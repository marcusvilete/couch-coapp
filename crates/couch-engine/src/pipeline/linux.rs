//! The Linux backend (M16c, ADR 0012): it watches (M16b), and hosts with
//! controller seats as M16c-1's parts arrive. So far: FFmpeg's Linux
//! encoders (VAAPI, NVENC, software), the test pattern and the tone, and
//! guests' virtual controllers through `uinput` (with its access set up from
//! Settings → Drivers), and the host's keys through the global-shortcuts
//! portal. Capture through the portal and sound from PipeWire come next, and
//! only then does it say it can host (`capabilities`).

use std::{any::Any, sync::Arc};

use couch_codec_ffmpeg::FfmpegEncoder;
use couch_input::{
    DEV_PAD_IDS, HidUsage, InputError, KbmInjector, Plugged, VirtualPadFactory, XBOX_360_IDS,
    shortcuts::Shortcut,
};
use couch_media::{
    AudioSource, Capturer, EncoderConfig, GpuFrame, MediaError, VideoEncoder, audio::ToneSource,
    clock::Clock, testpattern::TestPattern,
};
use couch_platform_linux::{clock::MonotonicClock, global_keys, pads};

use super::{
    AudioKind, Backend, CaptureRequest, Focus, HostKeyEvent, NativeId, NoKbm, Source, Sources,
    ViewerOptions, ViewerProcess, viewer_process,
};
use crate::{DriverName, DriverState};

pub(super) struct LinuxBackend;

impl Backend for LinuxBackend {
    /// The stream window's clock (`couch-viewer`'s `now_us`), so times from
    /// it compare with ours.
    fn clock(&self) -> Arc<dyn Clock> {
        Arc::new(MonotonicClock)
    }

    fn sources(&self) -> Result<Sources, MediaError> {
        Ok(Sources::default())
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
            _ => Err(MediaError::Unsupported(
                "capturing windows and screens on Linux (M16c)".into(),
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

    fn audio(&self, kind: AudioKind) -> Result<Box<dyn AudioSource>, MediaError> {
        match kind {
            AudioKind::Tone => Ok(Box::new(ToneSource::new(Arc::new(MonotonicClock)))),
            _ => Err(MediaError::Unsupported(
                "sound capture on Linux (M16c)".into(),
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
