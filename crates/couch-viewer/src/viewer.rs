//! The viewer proper: a decode thread fed by the IPC pipe, and a render loop
//! on the main thread that always shows the newest decoded frame.

use std::{
    collections::VecDeque,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use couch_codec_ffmpeg::FfmpegDecoder;
use couch_input::shortcuts::Shortcut;
use couch_media::{DecodedFrame, Micros, Nv12Frame, VideoDecoder, barcode, mailbox::Mailbox};
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
    frame: Nv12Frame,
    timing: FrameTiming,
}

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

    let decoded = Arc::new(Mailbox::new());
    let decoder_name = Arc::new(std::sync::Mutex::new(String::from("starting")));
    {
        let decoded = decoded.clone();
        let out_tx = out_tx.clone();
        let decoder_name = decoder_name.clone();
        thread::Builder::new()
            .name("decode".into())
            .spawn(move || decode_loop(in_rx, &decoded, &out_tx, &decoder_name))?;
    }

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
                Some(Shortcut::FreeMouse | Shortcut::Panic) | None => continue,
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

        let timing = match decoded.take_timeout(Duration::from_millis(4)) {
            Ok(Some(Decoded { frame, timing })) => {
                video_pass.upload(&gpu.device, &gpu.queue, &frame);
                shown = Some((frame.width, frame.height));
                Some(timing)
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
            gpu.present_mode(),
            decoded.replaced_count(),
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

        // Only a new frame's first showing is measured.
        if let Some(mut timing) = timing {
            timing.presented_us = now_us().0;
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
    drop(gpu);
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    Ok(())
}

/// A made-up multiplayer session for `--hud-preview`: two controllers and a
/// keyboard seat. Playing, the game is out of front on the host.
fn preview_session(role: HudRole) -> HudState {
    let seat = |label: &str, pad, holder: Option<&str>, mine, asked| HudSeat {
        label: label.into(),
        pad,
        keyboard_mouse: !pad,
        holder: holder.map(Into::into),
        mine,
        asked,
    };
    let playing = role == HudRole::Controlling;
    HudState {
        title: "Fling to the Finish".into(),
        host: "Alice".into(),
        role,
        seats: vec![
            seat("Controller 1", true, Some("Carol"), false, false),
            seat(
                "Controller 2",
                true,
                playing.then_some("Bob"),
                playing,
                role == HudRole::Asking,
            ),
            seat("Keyboard and mouse", false, None, false, false),
        ],
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

fn decode_loop(
    rx: mpsc::Receiver<Incoming>,
    decoded: &Mailbox<Decoded>,
    out: &mpsc::Sender<Outgoing>,
    decoder_name: &std::sync::Mutex<String>,
) {
    let mut decoder = match FfmpegDecoder::new(true) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("no decoder: {e}");
            decoded.close();
            return;
        }
    };
    *decoder_name.lock().unwrap() = if decoder.is_hardware() {
        "D3D11VA".into()
    } else {
        "software".into()
    };
    let mut locator = BarcodeLocator::default();
    // After a decode error, or when decoding falls behind, wait for the next
    // keyframe instead of showing garbage or ever older frames.
    let mut need_keyframe = false;
    let mut last_request: Option<Instant> = None;
    let mut request_keyframe = || {
        if last_request.is_none_or(|t| t.elapsed() > Duration::from_millis(500)) {
            last_request = Some(Instant::now());
            let _ = out.send(Outgoing::KeyframeRequest);
        }
    };

    for message in rx {
        let (header, data) = match message {
            Incoming::Video(header, data) => (header, data),
            Incoming::Shutdown => break,
        };
        // Frames queue here unseen by the host (its reports measure arrival),
        // so a decoder slower than the stream must catch up on its own.
        let waited = Duration::from_micros(now_us().0.saturating_sub(header.arrived_us));
        if !need_keyframe && !header.keyframe && waited > STALE {
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
                for frame in frames {
                    let DecodedFrame::CpuNv12(frame) = frame else {
                        continue;
                    };
                    let barcode_us = locator.read(&frame).unwrap_or(0);
                    let timing = FrameTiming {
                        epoch: header.epoch,
                        seq: header.seq,
                        capture_us: header.capture_us,
                        arrived_us: header.arrived_us,
                        decoded_us: now_us().0,
                        presented_us: 0,
                        barcode_us,
                    };
                    decoded.put(Decoded { frame, timing });
                }
            }
            Err(e) => {
                tracing::warn!(seq = header.seq, "decode failed: {e}");
                need_keyframe = true;
                request_keyframe();
            }
        }
    }
    decoded.close();
}

/// Finds the latency clock's barcode once, then reads it at that spot.
#[derive(Default)]
struct BarcodeLocator {
    position: Option<(u32, u32)>,
    misses: u32,
}

impl BarcodeLocator {
    fn read(&mut self, frame: &Nv12Frame) -> Option<u64> {
        let size = (frame.width, frame.height);
        let stride = frame.y_stride as usize;
        if let Some((x, y)) = self.position {
            if let Some(v) = barcode::read_luma(&frame.y, stride, size, x, y, clock::CELL) {
                self.misses = 0;
                return Some(v);
            }
            self.misses += 1;
            if self.misses < 30 {
                return None;
            }
            self.position = None;
        }
        // Not every stream shows the clock; only search occasionally.
        if !self.misses.is_multiple_of(30) {
            self.misses += 1;
            return None;
        }
        self.misses += 1;
        let (x, y, v) =
            barcode::locate_luma(&frame.y, stride, size, clock::CELL, clock::MARGIN * 3)?;
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
        present_mode: wgpu::PresentMode,
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
            format!("decoder {decoder}, {present_mode:?}, {dropped} skipped"),
            format!("GPU {adapter}   (Ctrl+Alt+Shift+H hides this)"),
        ]
    }
}
