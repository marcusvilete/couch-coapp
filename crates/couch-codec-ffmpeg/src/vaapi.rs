//! VAAPI frames for an encoder (M16c): `h264_vaapi` takes only VAAPI
//! surfaces, so each frame in memory is converted to NV12 (swscale, BT.709
//! limited range, as for every other encoder) and uploaded into a surface
//! from a pool this encoder owns, on the system's default VAAPI device.

use couch_media::{BgraFrame, EncoderConfig};
use rsmpeg::{avutil::AVFrame, ffi, swscale::SwsContext};

use crate::options::av_error;

/// The device, the surface pool, and the NV12 frame in memory each frame is
/// converted into before its upload.
pub(crate) struct VaapiFrames {
    device: *mut ffi::AVBufferRef,
    frames: *mut ffi::AVBufferRef,
    staging: AVFrame,
    scaler: Option<SwsContext>,
}

// The buffers are FFmpeg's reference-counted ones; the encoder that owns
// this uses it from one thread at a time.
unsafe impl Send for VaapiFrames {}

impl VaapiFrames {
    /// For an encoder opened with `config`: the default VAAPI device (the
    /// first render node), and NV12 surfaces of its size.
    pub(crate) fn new(config: EncoderConfig) -> Result<Self, String> {
        // SAFETY: each FFmpeg object is checked, and every failure path
        // unrefs what was made so far.
        let (device, frames) = unsafe {
            let mut device: *mut ffi::AVBufferRef = std::ptr::null_mut();
            let ret = ffi::av_hwdevice_ctx_create(
                &mut device,
                ffi::AV_HWDEVICE_TYPE_VAAPI,
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            );
            if ret < 0 {
                return Err(format!("no VAAPI device: {}", av_error(ret)));
            }
            let mut frames = ffi::av_hwframe_ctx_alloc(device);
            if frames.is_null() {
                ffi::av_buffer_unref(&mut device);
                return Err("no VAAPI frames context".into());
            }
            let fc = (*frames).data.cast::<ffi::AVHWFramesContext>();
            (*fc).format = ffi::AV_PIX_FMT_VAAPI;
            (*fc).sw_format = ffi::AV_PIX_FMT_NV12;
            (*fc).width = config.width as i32;
            (*fc).height = config.height as i32;
            // A pool that grows as the encoder holds surfaces: a fixed one
            // would fail every upload once exhausted.
            (*fc).initial_pool_size = 0;
            let ret = ffi::av_hwframe_ctx_init(frames);
            if ret < 0 {
                ffi::av_buffer_unref(&mut frames);
                ffi::av_buffer_unref(&mut device);
                return Err(format!("VAAPI surfaces: {}", av_error(ret)));
            }
            (device, frames)
        };
        Self::on(device, frames, config)
    }

    /// Another encoder's frames on the same device and surfaces, for one
    /// reopened at a new bitrate: no new device each time the rate moves.
    pub(crate) fn share(&self, config: EncoderConfig) -> Result<Self, String> {
        // SAFETY: new references to buffers we hold; each is released by
        // the new value's `Drop` (or here, if the second can't be made).
        let (device, frames) = unsafe {
            let mut device = ffi::av_buffer_ref(self.device);
            let frames = ffi::av_buffer_ref(self.frames);
            if device.is_null() || frames.is_null() {
                let mut frames = frames;
                ffi::av_buffer_unref(&mut frames);
                ffi::av_buffer_unref(&mut device);
                return Err("out of memory".into());
            }
            (device, frames)
        };
        Self::on(device, frames, config)
    }

    /// Takes `device` and `frames` (one reference each), with a new NV12
    /// frame in memory and converter.
    fn on(
        device: *mut ffi::AVBufferRef,
        frames: *mut ffi::AVBufferRef,
        config: EncoderConfig,
    ) -> Result<Self, String> {
        let mut this = Self {
            device,
            frames,
            staging: AVFrame::new(),
            scaler: None,
        };
        this.staging.set_width(config.width as i32);
        this.staging.set_height(config.height as i32);
        this.staging.set_format(ffi::AV_PIX_FMT_NV12);
        this.staging
            .alloc_buffer()
            .map_err(|e| format!("the NV12 frame: {e}"))?;
        this.scaler = Some(
            crate::encoder::bgra_converter(config, ffi::AV_PIX_FMT_NV12)
                .map_err(|e| e.to_string())?,
        );
        Ok(this)
    }

    /// A new reference to the surface pool, for `AVCodecContext::hw_frames_ctx`.
    pub(crate) fn frames_ref(&self) -> Result<*mut ffi::AVBufferRef, String> {
        // SAFETY: `frames` is a live buffer we hold a reference to.
        let r = unsafe { ffi::av_buffer_ref(self.frames) };
        if r.is_null() {
            Err("out of memory".into())
        } else {
            Ok(r)
        }
    }

    /// `src`, converted and uploaded into a surface of the pool.
    pub(crate) fn upload(&mut self, src: &BgraFrame) -> Result<AVFrame, String> {
        crate::encoder::fill_frame(&mut self.staging, &mut self.scaler, src)
            .map_err(|e| e.to_string())?;
        let mut surface = AVFrame::new();
        // SAFETY: `frames` is a live, initialized frames context; `surface`
        // is a fresh frame that takes the surface's reference; the transfer
        // reads `staging`, an NV12 frame of the pool's size.
        unsafe {
            let ret = ffi::av_hwframe_get_buffer(self.frames, surface.as_mut_ptr(), 0);
            if ret < 0 {
                return Err(format!("no VAAPI surface: {}", av_error(ret)));
            }
            let ret = ffi::av_hwframe_transfer_data(surface.as_mut_ptr(), self.staging.as_ptr(), 0);
            if ret < 0 {
                return Err(format!("upload to VAAPI: {}", av_error(ret)));
            }
        }
        Ok(surface)
    }
}

impl Drop for VaapiFrames {
    fn drop(&mut self) {
        // SAFETY: our own references, released once; surfaces still held by
        // the encoder keep the pool alive through theirs.
        unsafe {
            ffi::av_buffer_unref(&mut self.frames);
            ffi::av_buffer_unref(&mut self.device);
        }
    }
}
