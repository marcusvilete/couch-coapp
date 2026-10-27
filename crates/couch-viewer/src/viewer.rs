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
    Codec, DecodedFrame, GpuNv12Frame, MediaError, Micros, Nv12Frame, VideoDecoder, barcode,
    mailbox::Mailbox,
};
use couch_proto::{
    ipc::{
        FrameTiming, HudRole, HudSeat, HudState, LocalInput, NetStats, StreamVolume, UserAction,
        WindowRequest, WindowState,
    },
    session::StreamState,
};
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
    /// Start without the session bar, the rest of the HUD showing (P11:
    /// Settings → Stream window).
    pub hide_bar: bool,
    /// Start with the stats panel closed (the app's viewers).
    pub hide_stats: bool,
    /// Show a made-up session, without the pipe, to check the HUD by eye
    /// (`--hud-preview`).
    pub preview: Option<Preview>,
    /// The HUD's scale instead of the display's (`--hud-scale`, for the
    /// preview's shots at 150%).
    pub hud_scale: Option<f32>,
    /// A preview's pointer, in points, to see hovers and tooltips
    /// (`--hud-preview-pointer`).
    pub preview_pointer: Option<(f32, f32)>,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            position: None,
            size: (1280, 720),
            hide_hud: false,
            hide_bar: false,
            hide_stats: false,
            preview: None,
            hud_scale: None,
            preview_pointer: None,
        }
    }
}

/// What `--hud-preview` shows: each state of the bar worth seeing (M21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    Watching,
    Asking,
    /// Playing two seats, the game out of front on the host, a note.
    Controlling,
    /// Playing one seat and asking for another.
    AskingMore,
    Paused,
    /// The game closed: nothing streamed, the seats staying.
    Stopped,
    /// Nothing streamed yet.
    Waiting,
    /// The host plays beside the seats.
    Beside,
    /// One seat, nobody on it, the host playing beside it.
    Single,
    Muted,
    /// Four pads and a keyboard seat, a long title and long names.
    Crowded,
    /// Paused, the game out of front and a note, all at once.
    Worst,
    /// Playing two seats with two controllers, choosing which moves to a
    /// third (P13).
    Choosing,
}

impl Preview {
    pub const NAMES: [(&'static str, Preview); 13] = [
        ("watching", Preview::Watching),
        ("asking", Preview::Asking),
        ("controlling", Preview::Controlling),
        ("asking-more", Preview::AskingMore),
        ("paused", Preview::Paused),
        ("stopped", Preview::Stopped),
        ("waiting", Preview::Waiting),
        ("beside", Preview::Beside),
        ("single", Preview::Single),
        ("muted", Preview::Muted),
        ("crowded", Preview::Crowded),
        ("worst", Preview::Worst),
        ("choosing", Preview::Choosing),
    ];

    pub fn named(name: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| *p)
    }
}

/// An SDL window position (window coordinates) in the HUD's points.
fn hud_point(window: &Window, scale: Option<f32>, x: f32, y: f32) -> egui::Pos2 {
    let (w, _) = window.size();
    let (pw, _) = window.size_in_pixels();
    let per_coord = if w == 0 { 1.0 } else { pw as f32 / w as f32 };
    let scale = hud_scale(window, scale);
    egui::pos2(x * per_coord / scale, y * per_coord / scale)
}

/// The HUD's pixels per point: the display's scale, unless set.
fn hud_scale(window: &Window, scale: Option<f32>) -> f32 {
    scale
        .unwrap_or_else(|| window.display_scale())
        .clamp(0.5, 4.0)
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
        Some(preview) => {
            drop(out_rx);
            let _ = window_tx.send(ForWindow::Hud(preview_session(preview)));
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
    hud.show_bar = !options.hide_bar;
    hud.show_stats = !options.hide_stats;
    hud.preview = options.preview.is_some();
    if options.preview == Some(Preview::Muted) {
        hud.volume = StreamVolume {
            percent: 40,
            muted: true,
        };
    }
    if options.preview == Some(Preview::Choosing) {
        hud.controllers = vec![
            crate::hud::HudController {
                slot: 0,
                name: "DualSense Wireless Controller".into(),
                seat: Some(1),
            },
            crate::hud::HudController {
                slot: 1,
                name: "Xbox Wireless Controller".into(),
                seat: Some(2),
            },
        ];
        hud.ask(0);
    }
    // A preview's pointer, sent once the bar has been laid out.
    let mut preview_pointer = options.preview_pointer;
    let mut title = String::new();
    let mut events = sdl.event_pump().map_err(|e| anyhow!("SDL events: {e}"))?;
    let mut stats = Stats::default();
    // The stats card's rows and when they were last built.
    let mut stat_rows: Vec<(String, String)> = Vec::new();
    let mut stats_at: Option<Instant> = None;
    let mut was_captured = false;
    // The size of the frame in the video textures, once there is one. It can
    // be drawn again whenever the window needs it: a static source sends no
    // new frames.
    let mut shown: Option<(u32, u32)> = None;
    let mut redraw = false;
    // The stream volume (P6): the profile's, once the main process sends
    // it (the pipe reader has already set the sound to it). A change goes to
    // the main process to remember: a shortcut or Mute at once, the slider
    // once the user lets it rest (or the window closes), not at every step
    // of a drag.
    let mut volume = StreamVolume::default();
    let mut volume_unsaved: Option<Instant> = None;
    // The window's state as last told to the main process (P11).
    let mut reported: Option<WindowState> = None;

    'running: loop {
        for event in events.poll_iter() {
            match event {
                Event::Quit { .. } => {
                    // A volume change not yet remembered goes first.
                    if volume_unsaved.take().is_some() {
                        let _ = out_tx.send(Outgoing::Volume(volume));
                    }
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
                    hud.pointer_moved(hud_point(&window, options.hud_scale, x, y));
                }
                Event::MouseButtonDown {
                    mouse_btn, x, y, ..
                } if !input.mouse_captured() => {
                    if let Some(button) = button_code(mouse_btn)
                        && hud.pointer_button(
                            hud_point(&window, options.hud_scale, x, y),
                            button,
                            true,
                        )
                    {
                        continue;
                    }
                }
                Event::MouseButtonUp {
                    mouse_btn, x, y, ..
                } => {
                    if let Some(button) = button_code(mouse_btn)
                        && hud.pointer_button(
                            hud_point(&window, options.hud_scale, x, y),
                            button,
                            false,
                        )
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
                Some(shortcut @ (Shortcut::VolumeDown | Shortcut::VolumeUp | Shortcut::Mute)) => {
                    let new = match shortcut {
                        Shortcut::VolumeDown => volume.stepped(-VOLUME_STEP),
                        Shortcut::VolumeUp => volume.stepped(VOLUME_STEP),
                        _ => StreamVolume {
                            muted: !volume.muted,
                            ..volume
                        },
                    };
                    // One step at a time: remembered at once, so a session
                    // ending right after can't lose it.
                    change_volume(&mut volume, new, audio.as_ref(), &mut hud);
                    volume_unsaved = None;
                    let _ = out_tx.send(Outgoing::Volume(volume));
                    hud.note(volume_note(volume));
                }
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
                    // Leaving: a volume change not yet remembered goes first.
                    if action == UserAction::Leave && volume_unsaved.take().is_some() {
                        let _ = out_tx.send(Outgoing::Volume(volume));
                    }
                    let _ = out_tx.send(Outgoing::Action(action));
                }
                HudAction::Fullscreen => toggle_fullscreen(&mut window, &mut hud),
                // The mute button is remembered at once; the slider once
                // the user lets it rest, not at every step of a drag.
                HudAction::Volume(new) => {
                    let muting = new.muted != volume.muted;
                    change_volume(&mut volume, new, audio.as_ref(), &mut hud);
                    if muting {
                        volume_unsaved = None;
                        let _ = out_tx.send(Outgoing::Volume(volume));
                    } else {
                        volume_unsaved = Some(Instant::now());
                    }
                }
            }
            redraw = true;
        }
        if volume_unsaved.is_some_and(|t| t.elapsed() >= VOLUME_SAVE_AFTER) {
            volume_unsaved = None;
            let _ = out_tx.send(Outgoing::Volume(volume));
        }
        while let Ok(message) = window_rx.try_recv() {
            match message {
                ForWindow::Seat(seat) => {
                    input.set_seat(seat);
                    // A preview keeps its made-up controllers.
                    if options.preview.is_none() {
                        hud.controllers = input.hud_controllers();
                    }
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
                ForWindow::Volume(saved) => {
                    change_volume(&mut volume, saved, audio.as_ref(), &mut hud);
                    redraw = true;
                }
                ForWindow::Window(request) => {
                    window_request(&mut window, &mut hud, request);
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
        // Captured, the pointer reaches the game, not the HUD: no hover or
        // tooltip may stay drawn over the game. Either way the bar's mouse
        // hint changes.
        if input.mouse_captured() != was_captured {
            redraw = true;
            if input.mouse_captured() {
                hud.mouse_captured();
            }
        }
        was_captured = input.mouse_captured();
        // The app shows the window's state (P11), however it changed: a
        // shortcut, the bar, or the app's own request.
        let state = WindowState {
            fullscreen: hud.fullscreen,
            bar: hud.show_bar,
            stats: hud.show_stats,
        };
        if reported != Some(state) {
            reported = Some(state);
            let _ = out_tx.send(Outgoing::Window(state));
        }
        if last_alive.elapsed() >= ALIVE_EVERY {
            last_alive = Instant::now();
            local.push(LocalInput::Alive);
        }
        for event in local.drain(..) {
            // The controllers' names go just before the slots (P12), so the
            // host's patch bay hears a new one by name at once.
            if matches!(event, LocalInput::ControllerSlots(_)) {
                let _ = out_tx.send(Outgoing::ControllerNames(input.controller_names()));
                if options.preview.is_none() {
                    hud.controllers = input.hud_controllers();
                }
            }
            let _ = out_tx.send(Outgoing::Input(event));
        }
        if last_pad_check.elapsed() >= Duration::from_secs(1) {
            last_pad_check = Instant::now();
            input.recheck_host_pads(&mut local);
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
                        let matrix = frame.matrix;
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
                            Ok(()) => {
                                video_pass.matrix = matrix;
                                true
                            }
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
        // While the host streams nothing (M19), the last picture goes: the
        // window shows black and the HUD says why.
        if shown.is_some() && hud.session().is_some_and(|s| s.stream != StreamState::Live) {
            shown = None;
            redraw = true;
        }
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
        // The stats card's rows: built only while it shows, a few times a
        // second (the audio's take its thread's lock; numbers that change
        // every frame can't be read anyway).
        if hud.wants_stats() && stats_at.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
            stats_at = Some(Instant::now());
            stat_rows = stats.rows(
                shown.unwrap_or((0, 0)),
                &decoder_name.lock().unwrap(),
                &gpu.adapter_name,
                &format!(
                    "{:?} (latency {}), {} scaling",
                    gpu.present_mode(),
                    gpu.frame_latency(),
                    video_pass.scaler.name()
                ),
                decoded.replaced_count() + behind.load(Ordering::Relaxed),
                *net.lock().unwrap(),
            );
            stat_rows.insert(2, ("Input".into(), input.hud_line()));
            stat_rows.insert(
                3,
                (
                    "Audio".into(),
                    audio
                        .as_ref()
                        .map_or_else(|| "no device".to_string(), AudioOut::hud_line),
                ),
            );
        }
        hud.mouse_hint = input.mouse_hint();
        // A preview draws a test picture behind the HUD instead of waiting.
        hud.waiting = shown.is_none() && options.preview.is_none();
        let mut buffers = hud.paint(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            &view,
            &egui_wgpu::ScreenDescriptor {
                size_in_pixels: [size.0, size.1],
                pixels_per_point: hud_scale(&window, options.hud_scale),
            },
            &stat_rows,
        );
        buffers.push(encoder.finish());
        gpu.queue.submit(buffers);
        gpu.queue.present(surface_texture);
        redraw = false;
        if let Some((x, y)) = preview_pointer.take() {
            hud.pointer_moved(egui::pos2(x, y));
        }

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

/// A made-up session for `--hud-preview`: two controllers and a keyboard
/// seat, the host on the first with their own controller, unless the state
/// asks for something else.
fn preview_session(preview: Preview) -> HudState {
    use Preview as P;
    let seat = |label: &str, pad, holder: Option<&str>, mine, asked| HudSeat {
        label: label.into(),
        pad,
        keyboard_mouse: !pad,
        holder: holder.map(Into::into),
        host: holder == Some("Alice"),
        mine,
        asked,
    };
    let mut s = HudState {
        title: "Fling to the Finish".into(),
        host: "Alice".into(),
        role: HudRole::Watching,
        seats: vec![
            seat("Controller 1", true, Some("Alice"), false, false),
            seat("Controller 2", true, None, false, false),
            seat("Keyboard and mouse", false, Some("Carol"), false, false),
        ],
        host_on_seats: true,
        viewers: 2,
        control_paused: false,
        game_in_front: true,
        stream: StreamState::Live,
        note: String::new(),
    };
    let bob = |seat: &mut HudSeat| {
        seat.holder = Some("Bob".into());
        seat.mine = true;
    };
    match preview {
        P::Watching | P::Muted => {}
        P::Asking => {
            s.role = HudRole::Asking;
            s.seats[1].asked = true;
            s.note = "Asked Alice for Controller 2".into();
        }
        P::Choosing => {
            s.role = HudRole::Controlling;
            s.seats
                .insert(2, seat("Controller 3", true, None, false, false));
            bob(&mut s.seats[1]);
            bob(&mut s.seats[2]);
        }
        P::Controlling => {
            s.role = HudRole::Controlling;
            s.seats
                .insert(2, seat("Controller 3", true, None, false, false));
            bob(&mut s.seats[1]);
            bob(&mut s.seats[2]);
            s.game_in_front = false;
            s.note = "You have Controller 3".into();
        }
        P::AskingMore => {
            s.role = HudRole::Asking;
            bob(&mut s.seats[1]);
            s.seats[2].asked = true;
        }
        P::Paused => {
            s.control_paused = true;
            s.note = "Alice took control back".into();
        }
        P::Stopped => {
            s.role = HudRole::Controlling;
            bob(&mut s.seats[1]);
            s.stream = StreamState::Stopped(couch_proto::session::EndReason::GameExited);
        }
        P::Waiting => s.stream = StreamState::Waiting,
        P::Beside => {
            s.host_on_seats = false;
            s.seats[0] = seat("Controller 1", true, Some("Dan"), false, false);
            s.viewers = 3;
        }
        P::Single => {
            s.host_on_seats = false;
            s.seats = vec![seat("Controller", true, None, false, false)];
            s.viewers = 1;
        }
        P::Crowded => {
            s.title =
                "The Extraordinarily Long Title of a Game Nobody Remembers: Definitive Edition"
                    .into();
            s.host = "Alexandria-Maximiliana".into();
            s.role = HudRole::Controlling;
            s.seats = vec![
                seat(
                    "Controller 1",
                    true,
                    Some("Alexandria-Maximiliana"),
                    false,
                    false,
                ),
                seat(
                    "Controller 2",
                    true,
                    Some("Bartholomew the Third"),
                    false,
                    false,
                ),
                seat("Controller 3", true, Some("Bob"), true, false),
                seat("Controller 4", true, None, false, false),
                seat(
                    "Keyboard and mouse",
                    false,
                    Some("Constantinople"),
                    false,
                    false,
                ),
            ];
            s.seats[0].host = true;
            s.viewers = 7;
        }
        P::Worst => {
            s.role = HudRole::Controlling;
            bob(&mut s.seats[1]);
            s.control_paused = true;
            s.game_in_front = false;
            s.note =
                "Alice paused control with the panic key; it comes back when they allow it".into();
        }
    }
    s
}

/// How often the stats card's rows are built again while it shows.
const STATS_EVERY: Duration = Duration::from_millis(250);

/// How much a volume shortcut moves the stream volume, in percent (P6).
const VOLUME_STEP: i8 = 10;
/// A volume change is remembered once nothing has changed it for this long.
const VOLUME_SAVE_AFTER: Duration = Duration::from_millis(500);

/// Plays at `new` from now on and shows it on the bar.
fn change_volume(
    volume: &mut StreamVolume,
    new: StreamVolume,
    audio: Option<&AudioOut>,
    hud: &mut Hud,
) {
    *volume = new;
    hud.volume = new;
    if let Some(audio) = audio {
        audio.set_volume(new);
    }
}

/// The note a volume shortcut shows: "Volume 70%", "Muted".
fn volume_note(volume: StreamVolume) -> String {
    if volume.muted {
        format!("Muted (volume {}%)", volume.percent)
    } else {
        format!("Volume {}%", volume.percent)
    }
}

/// Ctrl+Alt+Shift+C: ask for a seat (the first free one), withdraw the
/// request, or give the seats back, depending on where this viewer stands.
/// Nothing without a session. Asking for one more seat while holding some
/// is the bar's (M18). With two controllers or more, the bar first asks
/// which one plays it (P13).
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
        HudRole::Watching => {
            if let Some(seat) = crate::hud::seat_to_ask(session) {
                hud.ask(seat);
            }
            return;
        }
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

/// Does what the app asks of the window (P11), for someone watching from
/// another monitor.
fn window_request(window: &mut Window, hud: &mut Hud, request: WindowRequest) {
    match request {
        WindowRequest::Fullscreen(on) => {
            if on != hud.fullscreen {
                toggle_fullscreen(window, hud);
            }
        }
        // The bar alone: the centre card and the stats stay (P11).
        WindowRequest::ShowBar(on) => hud.show_bar = on,
        WindowRequest::ShowStats(on) => hud.show_stats = on,
        // The app allowed this process to take the foreground just before
        // asking; without that, Windows only flashes the taskbar button.
        WindowRequest::Raise => {
            if window.is_minimized() {
                window.restore();
            }
            if !window.raise() {
                tracing::debug!("the window couldn't come to the front");
            }
        }
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
fn open_decoder(codec: Codec, device: Option<DecoderDevice>) -> Result<FfmpegDecoder, MediaError> {
    #[cfg(windows)]
    if let Some(device) = device {
        match FfmpegDecoder::new_d3d12(codec, device.into_raw()) {
            Ok(decoder) => return Ok(decoder),
            Err(e) => tracing::warn!("D3D12VA unavailable, frames go through memory: {e}"),
        }
    }
    #[cfg(not(windows))]
    let _ = device;
    FfmpegDecoder::new(codec, true)
}

fn decoder_label(decoder: &FfmpegDecoder) -> String {
    let codec = match decoder.codec() {
        Codec::Hevc => "HEVC",
        _ => "H.264",
    };
    let path = match decoder.kind() {
        DecoderKind::D3d12 => "D3D12VA (GPU frames)",
        DecoderKind::D3d11 => "D3D11VA (copied)",
        DecoderKind::Software => "software",
    };
    format!("{codec}, {path}")
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
    // H.264 until the stream says otherwise: the first frame's codec
    // reopens it if need be (M12c).
    let mut decoder = match open_decoder(Codec::H264, device.clone()) {
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
        // Another codec (the host chose it at join): a decoder for it, from
        // a keyframe; on the GPU still, unless frames already go through
        // memory.
        let codec = Codec::from_wire(header.codec).unwrap_or(Codec::H264);
        if codec != decoder.codec() {
            if !header.keyframe {
                need_keyframe = true;
                request_keyframe();
                continue;
            }
            let reopened = if decoder.kind() == DecoderKind::D3d12 {
                open_decoder(codec, device.clone())
            } else {
                FfmpegDecoder::new(codec, true)
            };
            match reopened {
                Ok(d) => {
                    decoder = d;
                    *decoder_name.lock().unwrap() = decoder_label(&decoder);
                    tracing::info!("decoding with {}", decoder_label(&decoder));
                    ready_us = u64::MAX;
                }
                Err(e) => {
                    tracing::error!("no {codec:?} decoder: {e}");
                    break;
                }
            }
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
    match FfmpegDecoder::new(decoder.codec(), true) {
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

    /// The stats card's rows, as (label, value), before Input and Audio,
    /// which the window loop adds.
    fn rows(
        &self,
        (width, height): (u32, u32),
        decoder: &str,
        adapter: &str,
        presentation: &str,
        dropped: u64,
        net: Option<NetStats>,
    ) -> Vec<(String, String)> {
        let fmt = |v: Option<f32>| v.map_or("–".to_string(), |v| format!("{v:.1} ms"));
        let path = net.map_or("–".to_string(), |n| {
            format!(
                "{} · rtt {:.1} ms · {:.1} Mbit/s",
                if n.relayed { "Relay" } else { "Direct" },
                f64::from(n.rtt_us) / 1000.0,
                f64::from(n.receive_bps) / 1e6
            )
        });
        let row = |label: &str, value: String| (label.to_string(), value);
        vec![
            row("Video", format!("{width}×{height} · {:.0} fps", self.fps())),
            row("Path", path),
            row(
                "Capture → present",
                fmt(median(&self.capture_to_present_ms)),
            ),
            row("Clock → present", fmt(median(&self.clock_to_present_ms))),
            row("Arrive → decoded", fmt(median(&self.decode_ms))),
            row(
                "Decoder",
                format!("{decoder}, {presentation}, {dropped} skipped"),
            ),
            row("GPU", adapter.to_string()),
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
