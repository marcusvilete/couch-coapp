//! The Linux backend (M16c, ADR 0012): it watches (M16b), and hosts sessions
//! with controller seats (M16c-1): FFmpeg's Linux encoders (VAAPI, NVENC,
//! software); a window or screen the user shares through the desktop's
//! screen-sharing portal, and what the PC plays from PipeWire; guests'
//! virtual controllers through `uinput` (with its access set up from
//! Settings → Drivers); and the host's keys through the global-shortcuts
//! portal. No keyboard seat or desktop control yet (M16c-2).

use std::{
    any::Any,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError, atomic::AtomicBool},
    time::Duration,
};

use couch_codec_ffmpeg::FfmpegEncoder;
use couch_input::{
    DEV_PAD_IDS, HidUsage, InputError, KbmInjector, Plugged, VirtualPadFactory, XBOX_360_IDS,
    shortcuts::Shortcut,
};
use couch_media::{
    AudioSource, Capturer, EncoderConfig, GpuFrame, MediaError, VideoEncoder, audio::ToneSource,
    clock::Clock, testpattern::TestPattern,
};
use couch_platform_linux::{
    clock::MonotonicClock,
    global_keys, notices, pads, screencast,
    sound::{Sounds, SystemSound},
};

use super::{
    AudioKind, Backend, CaptureRequest, Focus, HostKeyEvent, NativeId, NoKbm, ScreenSource, Source,
    Sources, ViewerOptions, ViewerProcess, viewer_process,
};
use crate::{DriverName, DriverState, HostCapabilities, NoticeView};

/// The one source a Linux host lists: whatever the user shares through the
/// desktop's dialog, a window or a screen. A screen to the pipeline (no
/// window to keep in front, the PC's sound).
pub const SHARED: NativeId = 1;

/// The profile's file for the screen-sharing portal's restore token.
const PORTAL_FILE: &str = "portal.json";

pub(super) struct LinuxBackend {
    /// Where the portal's restore token is kept (the app's profile), so a
    /// choice is remembered across runs; `None` keeps it for this run only
    /// (`couch-cli`).
    portal_file: Option<PathBuf>,
    /// The token for the last choice. Each is good for one start, and each
    /// start gives the next.
    restore_token: Arc<Mutex<Option<String>>>,
}

impl LinuxBackend {
    pub(super) fn new(profile: Option<PathBuf>) -> Self {
        let portal_file = profile.map(|dir| dir.join(PORTAL_FILE));
        let restore_token = portal_file.as_deref().and_then(|file| {
            let bytes = std::fs::read(file).ok()?;
            let saved: PortalFile = serde_json::from_slice(&bytes).ok()?;
            saved.restore_token
        });
        Self {
            portal_file,
            restore_token: Arc::new(Mutex::new(restore_token)),
        }
    }

    /// A capture of what the user shares: the dialog, or the last choice
    /// again.
    fn share(&self, fps: u32) -> Result<Box<dyn Capturer>, MediaError> {
        let restore_token = self
            .restore_token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let (kept, file) = (self.restore_token.clone(), self.portal_file.clone());
        let on_token = Box::new(move |token: String| {
            if let Some(file) = &file {
                let saved = PortalFile {
                    restore_token: Some(token.clone()),
                };
                let bytes = serde_json::to_vec(&saved).expect("a token serializes");
                if let Err(e) = crate::store::write_atomic(file, &bytes) {
                    tracing::warn!("the shared window or screen won't be remembered: {e}");
                }
            }
            *kept.lock().unwrap_or_else(PoisonError::into_inner) = Some(token);
        });
        let capture = screencast::start(screencast::Options {
            fps,
            restore_token,
            on_token,
        })
        .map_err(MediaError::Backend)?;
        Ok(Box::new(capture))
    }
}

/// `portal.json`. This PC's own: never backed up (`store::BACKED_UP_FILES`
/// doesn't name it), nor logged.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PortalFile {
    #[serde(default)]
    restore_token: Option<String>,
}

impl Backend for LinuxBackend {
    /// The stream window's clock (`couch-viewer`'s `now_us`), so times from
    /// it compare with ours.
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(MonotonicClock)
    }

    /// Sessions with controller seats (M16c-1); no keyboard seat (the
    /// desktop can't say which window is in front, nor tell the host's keys
    /// from injected ones) nor desktop control until M16c-2.
    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities {
            sessions: true,
            keyboard_seat: false,
            desktop_control: false,
        }
    }

    /// One source: what the user shares in the desktop's dialog when the
    /// stream starts.
    fn sources(&self) -> Result<Sources, MediaError> {
        Ok(Sources {
            windows: Vec::new(),
            screens: vec![ScreenSource {
                id: SHARED,
                name: "A window or screen, chosen in your desktop's dialog".into(),
                width: 0,
                height: 0,
                primary: true,
                refresh_hz: None,
            }],
        })
    }

    fn capture(
        &self,
        source: &Source,
        request: CaptureRequest,
    ) -> Result<(Box<dyn Capturer>, Option<NativeId>), MediaError> {
        match *source {
            Source::TestPattern { width, height } => Ok((
                Box::new(TestPattern::new(
                    width,
                    height,
                    request.fps.max(1),
                    Arc::new(MonotonicClock),
                )),
                None,
            )),
            // `couch-cli host --source monitor` shares through the dialog too.
            Source::Screen(SHARED) | Source::PrimaryScreen => {
                Ok((self.share(request.fps.max(1))?, None))
            }
            Source::Screen(_) => Err(MediaError::Unsupported(
                "that screen: a Linux host shares what its desktop's dialog offers".into(),
            )),
            Source::Window(_) | Source::WindowTitle(_) => Err(MediaError::Unsupported(
                "a window by name: a Linux host shares what its desktop's dialog offers".into(),
            )),
        }
    }

    /// Frames in memory into the first Linux encoder that opens: VAAPI
    /// (AMD, Intel), NVENC, then software (`couch_codec_ffmpeg::probe`).
    fn encoder(
        &self,
        config: EncoderConfig,
        _gpu: Option<&GpuFrame>,
    ) -> Result<Box<dyn VideoEncoder>, MediaError> {
        // One encoder by FFmpeg's name, as on Windows: another machine's path
        // here (`libopenh264`, say).
        if let Ok(name) = std::env::var("COUCH_HOST_ENCODER")
            && !name.trim().is_empty()
        {
            tracing::info!("encoder {name}, frames in memory (COUCH_HOST_ENCODER)");
            return Ok(Box::new(FfmpegEncoder::open(name.trim(), config)?));
        }
        Ok(Box::new(FfmpegEncoder::open_best(config)?))
    }

    fn audio(
        &self,
        kind: AudioKind,
        stop: &AtomicBool,
    ) -> Result<Box<dyn AudioSource>, MediaError> {
        match kind {
            AudioKind::Tone => Ok(Box::new(ToneSource::new(Arc::new(MonotonicClock)))),
            // What the PC plays, this app left out.
            AudioKind::System => Ok(Box::new(SystemSound::start(Sounds::AllButOurs, stop)?)),
            AudioKind::Program(name) => {
                Ok(Box::new(SystemSound::start(Sounds::Program(name), stop)?))
            }
            AudioKind::Game(_) => Err(MediaError::Unsupported(
                "a window's own sound on Linux".into(),
            )),
        }
    }

    /// A virtual Xbox 360 controller through `uinput`, ready at once.
    fn plug_pad(&self, real_ids: bool) -> Result<Plugged, InputError> {
        let ids = if real_ids { XBOX_360_IDS } else { DEV_PAD_IDS };
        pads::UinputPads::new(ids).plug_pending()
    }

    /// Not a driver on Linux: whether this user may make input devices.
    fn drivers(&self) -> Vec<(DriverName, DriverState, bool)> {
        let state = if pads::available() {
            DriverState::Ready
        } else {
            DriverState::Missing
        };
        vec![(DriverName::Uinput, state, true)]
    }

    fn virtual_pads(&self) -> bool {
        pads::available()
    }

    /// Asks for the administrator's password (`pkexec`) and installs one
    /// fixed udev rule.
    fn install_driver(&self, driver: DriverName) -> Result<(), String> {
        match driver {
            DriverName::Uinput => pads::set_up(),
            _ => Err("not on Linux".into()),
        }
    }

    fn kbm(&self) -> Box<dyn KbmInjector> {
        Box::new(NoKbm)
    }

    fn focus(&self, _: NativeId) -> Focus {
        Focus::default()
    }

    fn window_exists(&self, _: NativeId) -> bool {
        false
    }

    fn launch_viewer(&self, options: &ViewerOptions) -> Result<ViewerProcess, MediaError> {
        viewer_process::launch(options)
    }

    /// The desktop's dialog comes when the host starts the stream, once:
    /// never over the game when a guest arrives, nor again each time
    /// someone comes back.
    fn captures_without_viewers(&self) -> bool {
        true
    }

    /// The portal doesn't say whose the shared window is: the host picks a
    /// program, or sends everything but this app.
    fn picks_sound(&self) -> bool {
        true
    }

    fn sound_programs(&self) -> Result<Vec<String>, MediaError> {
        couch_platform_linux::sound::programs()
    }

    fn show_notices(&self, list: Vec<NoticeView>) -> Result<(), MediaError> {
        let list = list
            .into_iter()
            .map(|n| notices::Notice {
                id: n.id,
                title: n.title,
                body: n.body,
                lasting: n.lasting,
            })
            .collect();
        notices::show(list).map_err(MediaError::Backend)
    }

    fn close_notices(&self) {
        notices::close_all(Duration::from_millis(500));
    }

    /// Through the desktop's global-shortcuts portal, each key with Ctrl,
    /// Alt and Shift as the preferred trigger (the desktop may let the user
    /// change it). No guest has keys to inject on a Linux host (no keyboard
    /// seat, `capabilities`), which Windows' hook needs to ignore. Returns at
    /// once: the desktop's answer comes later, as `Bound` or `Lost`.
    fn host_keys(
        &self,
        keys: &[(HidUsage, Shortcut)],
        on_event: Box<dyn Fn(HostKeyEvent) + Send>,
    ) -> Result<Box<dyn Any + Send>, MediaError> {
        let offered: Vec<global_keys::Key> = keys
            .iter()
            .filter_map(|&(usage, shortcut)| {
                let (id, description) = host_key(shortcut)?;
                Some(global_keys::Key {
                    id,
                    description,
                    trigger: global_keys::chord(usage.0)?,
                })
            })
            .collect();
        let shortcut = |id: &str| {
            Shortcut::HOST
                .iter()
                .copied()
                .find(|&s| host_key(s).map(|k| k.0) == Some(id))
        };
        let reported = move |event: global_keys::Event<'_>| match event {
            global_keys::Event::Pressed(id) => {
                if let Some(s) = shortcut(id) {
                    on_event(HostKeyEvent::Pressed(s));
                }
            }
            global_keys::Event::Bound(bound) => on_event(HostKeyEvent::Bound(
                bound
                    .into_iter()
                    .filter_map(|(id, keys)| Some((shortcut(&id)?, keys)))
                    .collect(),
            )),
            global_keys::Event::Failed(e) => on_event(HostKeyEvent::Lost(e)),
        };
        global_keys::bind(offered, Box::new(reported))
            .map(|keys| Box::new(keys) as Box<dyn Any + Send>)
            .map_err(MediaError::Unsupported)
    }
}

/// A host key's id and what it does, as the desktop shows them.
fn host_key(shortcut: Shortcut) -> Option<(&'static str, &'static str)> {
    Some(match shortcut {
        Shortcut::Panic => (
            "panic",
            "Couch Co-App: take every seat back from guests and pause control",
        ),
        Shortcut::AcceptRequest => ("acceptRequest", "Couch Co-App: accept a guest's request"),
        Shortcut::DeclineRequest => ("declineRequest", "Couch Co-App: decline a guest's request"),
        _ => return None,
    })
}
