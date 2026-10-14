//! The viewer proper: a decode thread fed by the IPC pipe, and a render loop
//! on the main thread that always shows the newest decoded frame.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use couch_codec_ffmpeg::{DecoderKind, FfmpegDecoder};
use couch_input::shortcuts::{Shortcut, Shortcuts};
use couch_media::{
    DecodedFrame, GpuNv12Frame, MediaError, Micros, Nv12Frame, VideoDecoder, barcode,
    mailbox::Mailbox,
};
use couch_proto::ipc::{FrameTiming, HudRole, HudSeat, HudState, LocalInput, NetStats, UserAction};
use sdl3::{
    event::{Event, WindowEvent},
    video::Window,
};

use crate::{
    audio::AudioOut,
    clock, gpu,
    hud::{Hud, HudAction},
    input::{InputCapture, button_code},
    now_us,
    pipe::{self, ForWindow, Incoming, Outgoing},
};

/// A decoded frame and what we know about its journey so far.
struct Decoded {
    picture: Picture,
    timing: FrameTiming,
}

/// A decoded frame copied to memory, or left on the GPU (M9c).
enum Picture {
    Cpu(Nv12Frame),
    Gpu(GpuNv12Frame),
}

impl Picture {
    fn size(&self) -> (u32, u32) {
        match self {
            Self::Cpu(f) => (f.width, f.height),
            Self::Gpu(f) => (f.width, f.height),
        }
    }
}

/// The device a decoder whose frames stay on the GPU decodes on.
#[cfg(windows)]
type DecoderDevice = crate::zero_copy::DecoderDevice;
#[cfg(not(windows))]
type DecoderDevice = std::convert::Infallible;

/// Where the stream window goes. Latency tests place it at a known spot and
/// at the stream's own size, so it shows the stream 1:1.
#[derive(Debug, Clone, Copy)]
pub struct WindowOptions {
    pub position: Option<(i32, i32)>,
    pub size: (u32, u32),
    /// Start with the HUD hidden (it would cover the latency barcode on screen).
    pub hide_hud: bool,
    /// Start with the stats panel closed (the app's viewers).
    pub hide_stats: bool,
    /// Show a made-up session in this role, without the pipe, to check the
    /// HUD by eye (`--hud-preview`).
    pub preview: Option<HudRole>,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            position: None,
            size: (1280, 720),
            hide_hud: false,
            hide_stats: false,
            preview: None,
        }
    }
}

/// An SDL window position (window coordinates) in the HUD's points.
fn hud_point(window: &Window, x: f32, y: f32) -> egui::Pos2 {
    let (w, _) = window.size();
    let (pw, _) = window.size_in_pixels();
    let per_coord = if w == 0 { 1.0 } else { pw as f32 / w as f32 };
    let scale = hud_scale(window);
    egui::pos2(x * per_coord / scale, y * per_coord / scale)
}

/// The HUD's pixels per point: the display's scale.
fn hud_scale(window: &Window) -> f32 {
    window.display_scale().clamp(0.5, 4.0)
}

pub fn run(options: WindowOptions) -> Result<()> {
    InputCapture::allow_background_pads();
    let sdl = sdl3::init().map_err(|e| anyhow!("SDL init: {e}"))?;
    // Audio first, so the pipe reader has somewhere to put packets.
    let (audio, audio_feed) = match AudioOut::open(&sdl) {
        Ok((out, feed)) => (Some(out), Some(feed)),
        Err(e) => {
            tracing::warn!("no audio: {e:#}");
            (None, None)
        }
    };

    let (in_tx, in_rx) = mpsc::channel();
    let (window_tx, window_rx) = mpsc::channel();
    let (out_tx, out_rx) = mpsc::channel();
    let net = Arc::new(std::sync::Mutex::new(None));
    // A preview has no main process: nothing is sent, and the decoder
    // waits for good instead of stopping when the pipe closes.
    let _keep_open = options.preview.map(|_| in_tx.clone());
    pipe::spawn_reader(in_tx, window_tx.clone(), net.clone(), audio_feed);
    let writer = match options.preview {
        None => Some(pipe::spawn_writer(out_rx)),
        Some(role) => {
            drop(out_rx);
            let _ = window_tx.send(ForWindow::Hud(preview_session(role)));
            None
        }
    };

    let video = sdl.video().map_err(|e| anyhow!("SDL video: {e}"))?;
    let mouse = sdl.mouse();
    let mut input = InputCapture::new(&sdl)?;
    let mut local = Vec::new();
    // Proof of life for the main process, from the thread that pumps input.
    const ALIVE_EVERY: Duration = Duration::from_millis(100);
    let mut last_alive = Instant::now() - ALIVE_EVERY;
    let mut last_audio_stats = Instant::now();
    let mut last_pad_check = Instant::now();
    let mut builder = video.window("Couch Co-App", options.size.0, options.size.1);
    match options.position {
        Some((x, y)) => builder.position(x, y),
        None => builder.position_centered(),
    };
    let mut window = builder.resizable().high_pixel_density().build()?;
    // SAFETY: `gpu` is dropped at the end of this function, before `window`
    // (locals drop in reverse declaration order).
    let mut gpu = unsafe { gpu::Gpu::new(&window)? };
    let mut video_pass = gpu::VideoPass::new(&gpu.device, gpu.config.format);

    // The decoder starts once the GPU is set up, so frames that stay on the
    // GPU can be decoded on its device. Frames arriving meanwhile wait in the
    // channel; the stale check doesn't count that wait (`is_stale`).
    let decoded = Arc::new(Mailbox::new());
    let decoder_name = Arc::new(std::sync::Mutex::new(String::from("starting")));
    // Frames decoded but never shown, a newer one already waiting.
    let behind = Arc::new(AtomicU64::new(0));
    // Set by the render thread when it can't bring a decoded frame in: the
    // decoder then goes back to frames through memory.
    let gpu_frames_failed = Arc::new(AtomicBool::new(false));
    {
        let decoded = decoded.clone();
        let out_tx = out_tx.clone();
        let decoder_name = decoder_name.clone();
        let behind = behind.clone();
        let gpu_frames_failed = gpu_frames_failed.clone();
        #[cfg(windows)]
        let device = gpu.decoder_device();
        #[cfg(not(windows))]
        let device = None;
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || {
                decode_loop(
                    in_rx,
                    &decoded,
                    &out_tx,
                    &decoder_name,
                    &behind,
                    &gpu_frames_failed,
                    device,
                )
            })?;
    }
    // Frames on the GPU: brought into the video pass, given back to the
    // decoder once drawn, and their barcode corner read back for latency.
    #[cfg(windows)]
    let mut importer = crate::zero_copy::FrameImporter::new();
    #[cfg(windows)]
    let mut strips = crate::zero_copy::BarcodeStrips::<FrameTiming>::default();
    #[cfg(windows)]
    let mut gpu_locator = BarcodeLocator::default();
    // The strip of the frame being shown, until its timing is attached.
    #[cfg(windows)]
    let mut strip_slot: Option<usize> = None;
    let mut hud = Hud::new(&gpu.device, gpu.config.format);
    hud.visible = !options.hide_hud;
    hud.show_stats = !options.hide_stats;
    let mut title = String::new();
    let mut events = sdl.event_pump().map_err(|e| anyhow!("SDL events: {e}"))?;
    let mut stats = Stats::default();
    // The size of the frame in the video textures, once there is one. It can
    // be drawn again whenever the window needs it: a static source sends no
    // new frames.
    let mut shown: Option<(u32, u32)> = None;
    let mut redraw = false;

    'running: loop {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => {
                    // The user closed the window: say so, so the main
                    // process doesn't take the exit for a crash and start
                    // the window again.
                    let _ = out_tx.send(Outgoing::Action(UserAction::Leave));
                    break 'running;
                }
                Event::Window { win_event, .. } => match win_event {
                    WindowEvent::PixelSizeChanged(w, h) => {
                        gpu.resize(w as u32, h as u32);
                        redraw = true;
                    }
                    WindowEvent::Exposed | WindowEvent::Restored | WindowEvent::Shown => {
                        redraw = true;
                    }
                    WindowEvent::MouseLeave => hud.pointer_gone(),
                    WindowEvent::FocusLost => hud.focus_lost(),
                    _ => {}
                },
                // A free pointer also drives the HUD; a click on its bar goes
                // nowhere else, so it is never forwarded and never captures
                // the mouse.
                Event::MouseMotion { x, y, .. } if !input.mouse_captured() => {
                    hud.pointer_moved(hud_point(&window, x, y));
                }
                Event::MouseButtonDown {
                    mouse_btn, x, y, ..
                } if !input.mouse_captured() => {
                    if let Some(button) = button_code(mouse_btn)
                        && hud.pointer_button(hud_point(&window, x, y), button, true)
                    {
                        continue;
                    }
                }
                Event::MouseButtonUp {
                    mouse_btn, x, y, ..
                } => {
                    if let Some(button) = button_code(mouse_btn)
                        && hud.pointer_button(hud_point(&window, x, y), button, false)
                    {
                        continue;
                    }
                }
                _ => {}
            }
            match input.event(&event, &mut local) {
                Some(Shortcut::ToggleHud) => hud.visible = !hud.visible,
                Some(Shortcut::ToggleControl) => toggle_control(&mut hud, &out_tx),
                Some(Shortcut::Fullscreen) => toggle_fullscreen(&mut window, &mut hud),
                // The host's keys are never the viewer's (scope Host).
                Some(
                    Shortcut::FreeMouse
                    | Shortcut::Panic
                    | Shortcut::AcceptRequest
                    | Shortcut::DeclineRequest,
                )
                | None => continue,
            }
            redraw = true;
        }
        for action in hud.take_actions() {
            match action {
                HudAction::Session(action) => {
                    let _ = out_tx.send(Outgoing::Action(action));
                }
                HudAction::Fullscreen => toggle_fullscreen(&mut window, &mut hud),
            }
            redraw = true;
        }
        while let Ok(message) = window_rx.try_recv() {
            match message {
                ForWindow::Seat(seat) => {
                    input.set_seat(seat);
                    redraw = true;
                }
                ForWindow::Rumble(rumble) => input.rumble(rumble),
                ForWindow::Shortcuts(keys) => {
                    let (shortcuts, skipped) = Shortcuts::with_keys(&keys.keys);
                    for (id, e) in skipped {
                        tracing::warn!("shortcut {id}: {e}; its default applies");
                    }
                    input.set_shortcuts(shortcuts.clone());
                    hud.shortcuts = shortcuts;
                    redraw = true;
                }
                ForWindow::Hud(state) => {
                    let wanted = format!("{} · {} · Couch Co-App", state.title, state.host);
                    if wanted != title {
                        if let Err(e) = window.set_title(&wanted) {
                            tracing::debug!("window title: {e}");
                        }
                        title = wanted;
                    }
                    hud.set_session(state);
                    redraw = true;
                }
            }
        }
        redraw |= hud.needs_paint();
        input.sync_capture(&mouse, &window);
        if last_alive.elapsed() >= ALIVE_EVERY {
            last_alive = Instant::now();
            local.push(LocalInput::Alive);
        }
        for event in local.drain(..) {
            let _ = out_tx.send(Outgoing::Input(event));
        }
        if last_pad_check.elapsed() >= Duration::from_secs(1) {
            last_pad_check = Instant::now();
            input.drop_host_pads(&mut local);
        }
        if let Some(audio) = &audio
            && last_audio_stats.elapsed() >= Duration::from_secs(1)
        {
            last_audio_stats = Instant::now();
            let _ = out_tx.send(Outgoing::AudioStats(audio.stats()));
        }

        // Frames on the GPU whose copies finished go back to the decoder, and
        // timings whose barcode strip is read go out.
        #[cfg(windows)]
        if importer.held() > 0 || strips.busy() {
            let _ = gpu.device.poll(wgpu::PollType::Poll);
            importer.release_finished();
            for strip in strips.take_ready() {
                let mut timing = strip.value;
                if !strip.luma.is_empty() {
                    timing.barcode_us = gpu_locator
                        .read(&strip.luma, strip.size.0 as usize, strip.size)
                        .unwrap_or(0);
                }
                stats.record(&timing);
                if out_tx.send(Outgoing::Timing(timing)).is_err() {
                    break 'running;
                }
            }
        }

        let timing = match decoded.take_timeout(Duration::from_millis(4)) {
            Ok(Some(Decoded { picture, timing })) => {
                let size = picture.size();
                // A strip whose frame was never shown.
                #[cfg(windows)]
                if let Some(slot) = strip_slot.take() {
                    strips.forget(slot);
                }
                let brought_in = match picture {
                    Picture::Cpu(frame) => {
                        video_pass.upload(&gpu.device, &gpu.queue, &frame);
                        true
                    }
                    #[cfg(windows)]
                    Picture::Gpu(frame) => {
                        // In a submit of its own, before the draw: a frame
                        // that never gets drawn (the surface was lost) is
                        // still in the textures for the next redraw.
                        let mut encoder = gpu.device.create_command_encoder(&Default::default());
                        let (luma, chroma) = video_pass.planes(&gpu.device, size);
                        let result = importer.copy(
                            &gpu.device,
                            &gpu.queue,
                            &mut encoder,
                            frame,
                            luma,
                            chroma,
                        );
                        if result.is_ok() && gpu_locator.wants() {
                            strip_slot = strips.record(&gpu.device, &mut encoder, luma);
                        }
                        gpu.queue.submit([encoder.finish()]);
                        importer.submitted(&gpu.queue);
                        if let Some(slot) = strip_slot {
                            strips.submitted(slot);
                        }
                        match result {
                            Ok(()) => true,
                            Err(e) => {
                                // Frames through memory from the next
                                // keyframe on; until then the last frame
                                // brought in stays.
                                if !gpu_frames_failed.swap(true, Ordering::Relaxed) {
                                    tracing::warn!(
                                        "bringing a decoded frame in: {e:#}; frames go \
                                         through memory from now on"
                                    );
                                }
                                false
                            }
                        }
                    }
                    #[cfg(not(windows))]
                    Picture::Gpu(_) => false,
                };
                if brought_in {
                    shown = Some(size);
                    Some(timing)
                } else {
                    None
                }
            }
            Ok(None) => None,
            Err(_closed) => break,
        };
        // Before the first frame, a session's viewer still shows its HUD
        // over black; `couch-cli view` waits for the stream.
        if shown.is_none() && hud.session().is_none() {
            continue;
        }
        if timing.is_none() && !redraw {
            continue;
        }

        let surface_texture = match gpu.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                let (w, h) = window.size_in_pixels();
                gpu.reconfigure(w, h);
                // The frame is still in the textures; show it next time round.
                redraw = true;
                continue;
            }
            _ => continue,
        };
        let view = surface_texture.texture.create_view(&Default::default());
        let size = (gpu.config.width, gpu.config.height);
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if let Some(frame_size) = shown {
                video_pass.draw(&mut pass, gpu::letterbox(frame_size, size));
            }
        }
        let mut lines = stats.lines(
            shown.unwrap_or((0, 0)),
            &decoder_name.lock().unwrap(),
            &gpu.adapter_name,
            &format!("{:?} (latency {})", gpu.present_mode(), gpu.frame_latency()),
            decoded.replaced_count() + behind.load(Ordering::Relaxed),
            *net.lock().unwrap(),
        );
        lines.insert(2, input.hud_line());
        lines.insert(
            3,
            audio
                .as_ref()
                .map_or_else(|| "audio: no device".to_string(), AudioOut::hud_line),
        );
        hud.waiting = shown.is_none();
        let mut buffers = hud.paint(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &view,
            &egui_wgpu::ScreenDescriptor {
                size_in_pixels: [size.0, size.1],
                pixels_per_point: hud_scale(&window),
            },
            &lines,
        );
        buffers.push(encoder.finish());
        gpu.queue.submit(buffers);
        gpu.queue.present(surface_texture);
        redraw = false;

        // Only a new frame's first showing is measured. A frame on the GPU
        // whose barcode corner is being read back waits for it (above).
        if let Some(mut timing) = timing {
            timing.presented_us = now_us().0;
            #[cfg(windows)]
            if let Some(slot) = strip_slot.take() {
                strips.attach(slot, timing);
                continue;
            }
            stats.record(&timing);
            if out_tx.send(Outgoing::Timing(timing)).is_err() {
                break;
            }
        }
    }

    // Let go of anything still held, then stop the writer once it has sent
    // that. Waiting for the channel to close would hang: the decode thread
    // holds a sender while it waits for more video.
    input.release_all(&mut local);
    for event in local.drain(..) {
        let _ = out_tx.send(Outgoing::Input(event));
    }
    let _ = out_tx.send(Outgoing::Close);
    drop(out_tx);
    drop(audio);
    // Frames still held for the GPU go back once it has finished with them.
    #[cfg(windows)]
    {
        let _ = gpu.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(1)),
        });
        drop(importer);
    }
    drop(gpu);
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    Ok(())
}

/// A made-up multiplayer session for `--hud-preview`: two controllers and a
/// keyboard seat, the host on the first with their own controller. Playing,
/// the game is out of front on the host.
fn preview_session(role: HudRole) -> HudState {
    let seat = |label: &str, pad, holder: Option<&str>, mine, asked| HudSeat {
        label: label.into(),
        pad,
        keyboard_mouse: !pad,
        holder: holder.map(Into::into),
        host: holder == Some("Alice"),
        mine,
        asked,
    };
    let playing = role == HudRole::Controlling;
    HudState {
        title: "Fling to the Finish".into(),
        host: "Alice".into(),
        role,
        seats: vec![
            seat("Controller 1", true, Some("Alice"), false, false),
            seat(
                "Controller 2",
                true,
                playing.then_some("Bob"),
                playing,
                role == HudRole::Asking,
            ),
            seat("Keyboard and mouse", false, Some("Carol"), false, false),
        ],
        host_on_seats: true,
        viewers: 2,
        control_paused: false,
        game_in_front: !playing,
        note: match role {
            HudRole::Watching => String::new(),
            HudRole::Asking => "Asked the host for Controller 2".into(),
            HudRole::Controlling => "You have Controller 2".into(),
        },
    }
}

/// Ctrl+Alt+Shift+C: ask for a seat (the first free one), withdraw the
/// request, or give the seat back, depending on where this viewer stands.
/// Nothing without a session.
fn toggle_control(hud: &mut Hud, out: &mpsc::Sender<Outgoing>) {
    let Some(session) = hud.session() else {
        return;
    };
    let action = match session.role {
        HudRole::Watching if session.control_paused => {
            let note = format!("{} paused control for now", session.host);
            hud.note(note);
            return;
        }
        HudRole::Watching => match crate::hud::seat_to_ask(session) {
            Some(seat) => UserAction::RequestControl(seat),
            None => return,
        },
        HudRole::Asking | HudRole::Controlling => UserAction::CancelControl,
    };
    let _ = out.send(Outgoing::Action(action));
}

fn toggle_fullscreen(window: &mut Window, hud: &mut Hud) {
    match window.set_fullscreen(!hud.fullscreen) {
        Ok(()) => hud.fullscreen = !hud.fullscreen,
        Err(e) => tracing::warn!("full screen: {e}"),
    }
}

/// A frame that waited this long for the decoder means decoding can't keep
/// up (software decoding a large stream, say). Normal waits are a few ms.
const STALE: Duration = Duration::from_millis(200);

/// Whether a frame that arrived at `arrived_us` is stale at `now_us`: it
/// waited more than [`STALE`] since it arrived or since the decoder became
/// ready (`ready_us`), whichever is later. Waiting behind the decoder's own
/// start-up isn't falling behind; once started, a decoder slower than the
/// stream still is.
fn is_stale(arrived_us: u64, ready_us: u64, now_us: u64) -> bool {
    now_us.saturating_sub(arrived_us.max(ready_us)) > STALE.as_micros() as u64
}

/// The decoder for this stream window: on the GPU's device when frames can
/// stay on the GPU (falling back to D3D11VA if that can't start), else
/// D3D11VA, else software.
fn open_decoder(device: Option<DecoderDevice>) -> Result<FfmpegDecoder, MediaError> {
    #[cfg(windows)]
    if let Some(device) = device {
        match FfmpegDecoder::new_d3d12(device.into_raw()) {
            Ok(decoder) => return Ok(decoder),
            Err(e) => tracing::warn!("D3D12VA unavailable, frames go through memory: {e}"),
        }
    }
    #[cfg(not(windows))]
    let _ = device;
    FfmpegDecoder::new(true)
}

fn decoder_label(decoder: &FfmpegDecoder) -> String {
    match decoder.kind() {
        DecoderKind::D3d12 => "D3D12VA (GPU frames)",
        DecoderKind::D3d11 => "D3D11VA (copied)",
        DecoderKind::Software => "software",
    }
    .into()
}

fn decode_loop(
    rx: mpsc::Receiver<Incoming>,
    decoded: &Mailbox<Decoded>,
    out: &mpsc::Sender<Outgoing>,
    decoder_name: &std::sync::Mutex<String>,
    skipped: &AtomicU64,
    gpu_frames_failed: &AtomicBool,
    device: Option<DecoderDevice>,
) {
    let mut decoder = match open_decoder(device) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("no decoder: {e}");
            decoded.close();
            return;
        }
    };
    *decoder_name.lock().unwrap() = decoder_label(&decoder);
    tracing::info!("decoding with {}", decoder_label(&decoder));
    // Until the decoder has returned its first frame it is still starting
    // (the hardware decoder starts at the first keyframe): frames waiting
    // behind that aren't stale.
    let mut ready_us = u64::MAX;
    let mut locator = BarcodeLocator::default();
    // After a decode error, or when decoding falls behind, wait for the next
    // keyframe instead of showing garbage or ever older frames.
    let mut need_keyframe = false;
    // Decode errors in a row on a D3D12 decoder.
    let mut d3d12_errors = 0u32;
    let mut last_request: Option<Instant> = None;
    let mut request_keyframe = || {
        if last_request.is_none_or(|t| t.elapsed() > Duration::from_millis(500)) {
            last_request = Some(Instant::now());
            let _ = out.send(Outgoing::KeyframeRequest);
        }
    };

    // One message read ahead, to know whether a newer frame already waits.
    let mut queued: Option<Incoming> = None;
    loop {
        let message = match queued.take() {
            Some(message) => message,
            None => match rx.recv() {
                Ok(message) => message,
                Err(_) => break,
            },
        };
        let (header, data) = match message {
            Incoming::Video(header, data) => (header, data),
            Incoming::Shutdown => break,
        };
        // The render thread couldn't bring this decoder's frames in.
        if decoder.kind() == DecoderKind::D3d12 && gpu_frames_failed.load(Ordering::Relaxed) {
            if !reopen_in_memory(&mut decoder, decoder_name) {
                break;
            }
            ready_us = u64::MAX;
            need_keyframe = true;
        }
        queued = rx.try_recv().ok();
        // With a newer frame already waiting (a backlog: frames that queued
        // while the window started, or behind a slow decode), this one is
        // decoded, since later frames refer to it, but not shown: showing
        // it would only show the past.
        let newer_waiting = matches!(queued, Some(Incoming::Video(..)));
        // Frames queue here unseen by the host (its reports measure arrival),
        // so a decoder slower than the stream must catch up on its own.
        let now = now_us().0;
        if !need_keyframe && !header.keyframe && is_stale(header.arrived_us, ready_us, now) {
            let waited = Duration::from_micros(now.saturating_sub(header.arrived_us));
            tracing::warn!(
                seq = header.seq,
                ?waited,
                "decoding fell behind; skipping to a keyframe"
            );
            need_keyframe = true;
        }
        if need_keyframe && !header.keyframe {
            request_keyframe();
            continue;
        }
        match decoder.decode(&data, Micros(header.capture_us)) {
            Ok(frames) => {
                need_keyframe = false;
                d3d12_errors = 0;
                for frame in frames {
                    if ready_us == u64::MAX {
                        ready_us = now_us().0;
                    }
                    if newer_waiting {
                        skipped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let (picture, barcode_us) = match frame {
                        DecodedFrame::CpuNv12(frame) => {
                            let barcode_us = if locator.wants() {
                                locator
                                    .read(
                                        &frame.y,
                                        frame.y_stride as usize,
                                        (frame.width, frame.height),
                                    )
                                    .unwrap_or(0)
                            } else {
                                0
                            };
                            (Picture::Cpu(frame), barcode_us)
                        }
                        // Its barcode is read by the render thread.
                        DecodedFrame::Gpu(frame) => (Picture::Gpu(frame), 0),
                        _ => continue,
                    };
                    let timing = FrameTiming {
                        epoch: header.epoch,
                        seq: header.seq,
                        capture_us: header.capture_us,
                        arrived_us: header.arrived_us,
                        decoded_us: now_us().0,
                        presented_us: 0,
                        barcode_us,
                    };
                    decoded.put(Decoded { picture, timing });
                }
            }
            // The D3D12 decoder couldn't start on this stream: decode it
            // as before, from the next keyframe.
            Err(MediaError::Unsupported(e)) if decoder.kind() == DecoderKind::D3d12 => {
                tracing::warn!("{e}; frames go through memory from now on");
                if !reopen_in_memory(&mut decoder, decoder_name) {
                    break;
                }
                ready_us = u64::MAX;
                need_keyframe = true;
                request_keyframe();
            }
            Err(e) => {
                tracing::warn!(seq = header.seq, "decode failed: {e}");
                d3d12_errors += u32::from(decoder.kind() == DecoderKind::D3d12);
                if d3d12_errors >= D3D12_ERRORS {
                    tracing::warn!(
                        "D3D12VA failed {d3d12_errors} times in a row; frames go through \
                         memory from now on"
                    );
                    if !reopen_in_memory(&mut decoder, decoder_name) {
                        break;
                    }
                    d3d12_errors = 0;
                    ready_us = u64::MAX;
                }
                need_keyframe = true;
                request_keyframe();
            }
        }
    }
    decoded.close();
}

/// Decode errors in a row after which a D3D12 decoder is given up for one
/// decoding through memory: it decodes, but not this stream.
const D3D12_ERRORS: u32 = 3;

/// Replaces `decoder` with one decoding through memory (D3D11VA, or
/// software). `false` if there is none.
fn reopen_in_memory(decoder: &mut FfmpegDecoder, name: &std::sync::Mutex<String>) -> bool {
    match FfmpegDecoder::new(true) {
        Ok(d) => {
            *decoder = d;
            *name.lock().unwrap() = decoder_label(decoder);
            tracing::info!("decoding with {}", decoder_label(decoder));
            true
        }
        Err(e) => {
            tracing::error!("no decoder: {e}");
            false
        }
    }
}

/// Finds the latency clock's barcode once, then reads it at that spot.
#[derive(Default)]
struct BarcodeLocator {
    position: Option<(u32, u32)>,
    misses: u32,
}

impl BarcodeLocator {
    /// Whether this frame should be read: every frame once the barcode is
    /// found; while searching, only every 30th (not every stream shows the
    /// clock).
    fn wants(&mut self) -> bool {
        if self.position.is_some() {
            return true;
        }
        // Counted here, not when read: a frame on the GPU is read a frame or
        // two later, and the frames between must not ask again.
        let due = self.misses.is_multiple_of(30);
        self.misses += 1;
        due
    }

    /// Reads a frame [`Self::wants`], from its luma plane (or the plane's
    /// top-left corner: the barcode is searched for within `3 × MARGIN` of it).
    fn read(&mut self, luma: &[u8], stride: usize, size: (u32, u32)) -> Option<u64> {
        if let Some((x, y)) = self.position {
            if let Some(v) = barcode::read_luma(luma, stride, size, x, y, clock::CELL) {
                self.misses = 0;
                return Some(v);
            }
            self.misses += 1;
            if self.misses < 30 {
                return None;
            }
            // Lost: search now, then every 30th frame.
            self.position = None;
            self.misses += 1;
        }
        let (x, y, v) = barcode::locate_luma(luma, stride, size, clock::CELL, clock::MARGIN * 3)?;
        self.position = Some((x, y));
        self.misses = 0;
        Some(v)
    }
}

/// Rolling statistics for the HUD.
#[derive(Default)]
struct Stats {
    presents: VecDeque<u64>,
    capture_to_present_ms: VecDeque<f32>,
    clock_to_present_ms: VecDeque<f32>,
    decode_ms: VecDeque<f32>,
}

const WINDOW: usize = 240;

fn push(q: &mut VecDeque<f32>, v: f32) {
    if q.len() == WINDOW {
        q.pop_front();
    }
    q.push_back(v);
}

fn median(q: &VecDeque<f32>) -> Option<f32> {
    let mut v: Vec<f32> = q.iter().copied().collect();
    v.sort_by(f32::total_cmp);
    v.get(v.len() / 2).copied()
}

impl Stats {
    fn record(&mut self, t: &FrameTiming) {
        if self.presents.len() == WINDOW {
            self.presents.pop_front();
        }
        self.presents.push_back(t.presented_us);
        let ms = |later: u64, earlier: u64| later.saturating_sub(earlier) as f32 / 1000.0;
        push(
            &mut self.capture_to_present_ms,
            ms(t.presented_us, t.capture_us),
        );
        push(&mut self.decode_ms, ms(t.decoded_us, t.arrived_us));
        if t.barcode_us != 0 {
            push(
                &mut self.clock_to_present_ms,
                ms(t.presented_us, t.barcode_us),
            );
        }
    }

    fn fps(&self) -> f32 {
        match (self.presents.front(), self.presents.back()) {
            (Some(first), Some(last)) if last > first => {
                (self.presents.len() - 1) as f32 * 1e6 / (last - first) as f32
            }
            _ => 0.0,
        }
    }

    fn lines(
        &self,
        (width, height): (u32, u32),
        decoder: &str,
        adapter: &str,
        presentation: &str,
        dropped: u64,
        net: Option<NetStats>,
    ) -> Vec<String> {
        let fmt = |v: Option<f32>| v.map_or("-".to_string(), |v| format!("{v:.1} ms"));
        let path = net.map_or("path -".to_string(), |n| {
            format!(
                "{}  rtt {:.1} ms  {:.1} Mbit/s",
                if n.relayed { "Relay" } else { "Direct" },
                f64::from(n.rtt_us) / 1000.0,
                f64::from(n.receive_bps) / 1e6
            )
        });
        vec![
            format!("{width}x{height} @ {:.0} fps", self.fps()),
            path,
            format!(
                "capture -> present  {}",
                fmt(median(&self.capture_to_present_ms))
            ),
            format!(
                "clock -> present    {}",
                fmt(median(&self.clock_to_present_ms))
            ),
            format!("arrive -> decoded   {}", fmt(median(&self.decode_ms))),
            format!("decoder {decoder}, {presentation}, {dropped} skipped"),
            format!("GPU {adapter}   (Ctrl+Alt+Shift+H hides this)"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    #[test]
    fn frames_waiting_behind_the_decoders_start_are_not_stale() {
        // Not started yet: however long the wait.
        assert!(!is_stale(0, u64::MAX, 900 * MS));
        // Started at 400 ms; a frame from 0 ms decoded at 450 ms.
        assert!(!is_stale(0, 400 * MS, 450 * MS));
    }

    #[test]
    fn a_decoder_that_falls_behind_after_starting_still_skips() {
        // Ready at 400 ms; the frame from 0 ms still waiting at 650 ms.
        assert!(is_stale(0, 400 * MS, 650 * MS));
        // A frame that arrived after the start and waited 250 ms.
        assert!(is_stale(1000 * MS, 400 * MS, 1250 * MS));
        // A normal wait of a few ms.
        assert!(!is_stale(1000 * MS, 400 * MS, 1004 * MS));
    }

    #[test]
    fn the_locator_reads_every_frame_once_found_and_every_30th_while_searching() {
        // A grey frame: no barcode to find. `true` when the frame was read.
        let grey = vec![128u8; 1280 * 64];
        let frame = |l: &mut BarcodeLocator| {
            let wanted = l.wants();
            if wanted {
                assert_eq!(l.read(&grey, 1280, (1280, 64)), None);
            }
            wanted
        };
        let mut locator = BarcodeLocator::default();
        assert_eq!((0..90).filter(|_| frame(&mut locator)).count(), 3);
        // Found, then lost: read every frame until 30 misses (the last one
        // also searches), then searched every 30th frame, as before M9c.
        locator.position = Some((16, 16));
        locator.misses = 0;
        assert!((0..30).all(|_| frame(&mut locator)));
        assert!(locator.position.is_none());
        assert_eq!((0..60).filter(|_| frame(&mut locator)).count(), 2);
    }
}
