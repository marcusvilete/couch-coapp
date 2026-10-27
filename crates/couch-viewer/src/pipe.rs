//! The stdin/stdout link to the main process (`couch_proto::ipc`).

use std::{
    io::Write,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
};

use couch_proto::ipc::{
    self, AudioPacket, AudioStats, ControllerNames, FrameTiming, HudState, LocalInput, NetStats,
    RumbleState, SeatState, ShortcutKeys, StreamVolume, UserAction, VideoFrameHeader,
    WindowRequest, WindowState,
};

use crate::audio::AudioFeed;

/// What the main process sends for the decoder.
pub enum Incoming {
    Video(VideoFrameHeader, Vec<u8>),
    Shutdown,
}

/// What the main process sends for the window's thread.
pub enum ForWindow {
    Seat(SeatState),
    Rumble(RumbleState),
    Hud(HudState),
    Shortcuts(ShortcutKeys),
    /// The profile's stream volume (P6), or the app's change to it (P11).
    Volume(StreamVolume),
    /// Something the app asks of the window (P11).
    Window(WindowRequest),
}

/// What the viewer sends back.
pub enum Outgoing {
    Timing(FrameTiming),
    KeyframeRequest,
    Input(LocalInput),
    AudioStats(AudioStats),
    Action(UserAction),
    /// The user changed the stream volume (P6).
    Volume(StreamVolume),
    /// The window's full screen, bar or stats changed (P11).
    Window(WindowState),
    /// The controllers' names, after each change of them (P12).
    ControllerNames(ControllerNames),
    /// Write what is queued before this, then stop. Other threads (the
    /// decoder) still hold senders, so the channel alone never closes.
    Close,
}

/// Reads stdin until the main process closes it; then the channel closes too.
/// Network stats only feed the HUD, so they go straight to `net`; audio goes
/// straight to the player's buffer (dropped if there is no audio device).
pub fn spawn_reader(
    tx: mpsc::Sender<Incoming>,
    window_tx: mpsc::Sender<ForWindow>,
    net: Arc<Mutex<Option<NetStats>>>,
    audio: Option<AudioFeed>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ipc-read".into())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            loop {
                let frame = match ipc::read_frame(&mut stdin) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::error!("IPC read failed: {e}");
                        break;
                    }
                };
                let message = match frame.kind {
                    ipc::kinds::VIDEO_FRAME => match VideoFrameHeader::read(&frame.payload) {
                        Some((header, data)) => Incoming::Video(header, data.to_vec()),
                        None => {
                            tracing::warn!("malformed video frame on the IPC pipe");
                            continue;
                        }
                    },
                    ipc::kinds::SHUTDOWN => Incoming::Shutdown,
                    ipc::kinds::AUDIO_PACKET => {
                        match ipc::from_payload::<AudioPacket>(&frame.payload) {
                            Some(packet) => {
                                if let Some(audio) = &audio {
                                    audio.push(packet);
                                }
                            }
                            None => tracing::warn!("malformed audio packet on the IPC pipe"),
                        }
                        continue;
                    }
                    ipc::kinds::NET_STATS => {
                        *net.lock().unwrap() = NetStats::read(&frame.payload);
                        continue;
                    }
                    ipc::kinds::SEAT
                    | ipc::kinds::RUMBLE
                    | ipc::kinds::HUD_STATE
                    | ipc::kinds::SHORTCUTS
                    | ipc::kinds::VOLUME
                    | ipc::kinds::WINDOW => {
                        let message = match frame.kind {
                            ipc::kinds::SEAT => {
                                ipc::from_payload(&frame.payload).map(ForWindow::Seat)
                            }
                            ipc::kinds::RUMBLE => {
                                ipc::from_payload(&frame.payload).map(ForWindow::Rumble)
                            }
                            ipc::kinds::SHORTCUTS => {
                                ipc::from_payload(&frame.payload).map(ForWindow::Shortcuts)
                            }
                            // Set here, before the sound it comes ahead of
                            // plays; the window only shows it.
                            ipc::kinds::VOLUME => ipc::from_payload::<StreamVolume>(&frame.payload)
                                .map(|volume| {
                                    let volume = volume.clamped();
                                    if let Some(audio) = &audio {
                                        audio.set_volume(volume);
                                    }
                                    ForWindow::Volume(volume)
                                }),
                            ipc::kinds::WINDOW => {
                                ipc::from_payload(&frame.payload).map(ForWindow::Window)
                            }
                            _ => ipc::from_payload(&frame.payload).map(ForWindow::Hud),
                        };
                        match message {
                            Some(m) => {
                                let _ = window_tx.send(m);
                            }
                            None => tracing::warn!("malformed IPC kind {:#04x}", frame.kind),
                        }
                        continue;
                    }
                    ipc::kinds::HELLO => {
                        // The viewer ships with the app, so a mismatch is a
                        // broken install: stop rather than misread frames.
                        let version = frame
                            .payload
                            .get(..2)
                            .map(|v| u16::from_be_bytes([v[0], v[1]]));
                        if version != Some(ipc::IPC_VERSION) {
                            tracing::error!(
                                ?version,
                                ours = ipc::IPC_VERSION,
                                "the main process speaks another IPC version"
                            );
                            break;
                        }
                        continue;
                    }
                    other => {
                        tracing::debug!("ignoring IPC kind {other:#04x}");
                        continue;
                    }
                };
                let shutdown = matches!(message, Incoming::Shutdown);
                if tx.send(message).is_err() || shutdown {
                    break;
                }
            }
        })
        .expect("spawning the IPC reader")
}

/// Writes replies to stdout, the only code in the viewer allowed to.
pub fn spawn_writer(rx: mpsc::Receiver<Outgoing>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("ipc-write".into())
        .spawn(move || {
            let mut stdout = std::io::stdout().lock();
            let mut buf = Vec::new();
            let mut payload = Vec::new();
            for message in rx {
                buf.clear();
                payload.clear();
                let kind = match message {
                    Outgoing::Timing(timing) => {
                        timing.write(&mut payload);
                        ipc::kinds::FRAME_TIMING
                    }
                    Outgoing::KeyframeRequest => ipc::kinds::KEYFRAME_REQUEST,
                    Outgoing::Input(input) => {
                        payload = ipc::to_payload(&input);
                        ipc::kinds::INPUT
                    }
                    Outgoing::AudioStats(stats) => {
                        payload = ipc::to_payload(&stats);
                        ipc::kinds::AUDIO_STATS
                    }
                    Outgoing::Action(action) => {
                        payload = ipc::to_payload(&action);
                        ipc::kinds::USER_ACTION
                    }
                    Outgoing::Volume(volume) => {
                        payload = ipc::to_payload(&volume);
                        ipc::kinds::VOLUME_CHANGED
                    }
                    Outgoing::Window(state) => {
                        payload = ipc::to_payload(&state);
                        ipc::kinds::WINDOW_STATE
                    }
                    Outgoing::ControllerNames(names) => {
                        payload = ipc::to_payload(&names);
                        ipc::kinds::CONTROLLER_NAMES
                    }
                    Outgoing::Close => break,
                };
                ipc::encode(kind, &payload, &mut buf).expect("small IPC frames");
                if stdout
                    .write_all(&buf)
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
        })
        .expect("spawning the IPC writer")
}
