//! The Linux backend (M16c, ADR 0012): it watches (M16b), and hosts with
//! controller seats as M16c-1's parts arrive. So far: FFmpeg's Linux
//! encoders (VAAPI, NVENC, software), the test pattern and the tone, enough
//! for `couch-cli host --source testpattern`. Capture through the portal,
//! sound from PipeWire, `uinput` controllers and the host's keys come next,
//! and only then does it say it can host (`capabilities`).

use std::{any::Any, sync::Arc};

use couch_codec_ffmpeg::FfmpegEncoder;
use couch_input::{HidUsage, InputError, KbmInjector, Plugged, shortcuts::Shortcut};
use couch_media::{
    AudioSource, Capturer, EncoderConfig, GpuFrame, MediaError, VideoEncoder, audio::ToneSource,
    clock::Clock, testpattern::TestPattern,
};
use couch_platform_linux::clock::MonotonicClock;

use super::{
    AudioKind, Backend, CaptureRequest, Focus, NativeId, NoKbm, Source, Sources, ViewerOptions,
    ViewerProcess, viewer_process,
};

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

    fn plug_pad(&self, _: bool) -> Result<Plugged, InputError> {
        Err(InputError::DriverUnavailable(
            "virtual controllers on Linux (M16c)".into(),
        ))
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

    fn host_keys(
        &self,
        _: &[(HidUsage, Shortcut)],
        _: Box<dyn Fn(Shortcut) + Send>,
    ) -> Result<Box<dyn Any + Send>, MediaError> {
        Err(MediaError::Unsupported(
            "the host's keys on Linux (M16c)".into(),
        ))
    }
}
