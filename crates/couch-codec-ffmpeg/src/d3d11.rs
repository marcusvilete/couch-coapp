//! D3D11 frames for an encoder (M9b): an FFmpeg D3D11VA device context on the
//! capture's device, a frames context describing BGRA textures, and each
//! [`GpuFrame`] wrapped as an `AV_PIX_FMT_D3D11` frame that keeps the texture
//! out of its pool until FFmpeg lets go of the frame.
//!
//! The two structs below mirror FFmpeg 8.1's `libavutil/hwcontext_d3d11va.h`
//! (field layout only), which our generated bindings leave out.

use std::{any::Any, ffi::c_void, ptr::NonNull, sync::Arc};

use couch_media::GpuFrame;
use rsmpeg::{avutil::AVFrame, ffi};

/// `AVD3D11VADeviceContext`.
#[repr(C)]
struct DeviceContext {
    device: *mut c_void,
    device_context: *mut c_void,
    video_device: *mut c_void,
    video_context: *mut c_void,
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    lock_ctx: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
}

/// `AVD3D11VAFramesContext`. Only its size matters to us (left as FFmpeg
/// allocates it: FFmpeg's own pool is never used).
#[repr(C)]
#[allow(dead_code)]
struct FramesContext {
    texture: *mut c_void,
    bind_flags: u32,
    misc_flags: u32,
    texture_infos: *mut c_void,
}

/// A device context on the frames' device and a BGRA frames context, for one
/// encoder.
pub(crate) struct D3d11Frames {
    device: *mut ffi::AVBufferRef,
    frames: *mut ffi::AVBufferRef,
    width: u32,
    height: u32,
    /// The texture pool frames must come from (`GpuFrame::source`).
    source: u64,
    /// That pool's textures, kept alive until after the encoder (which drops
    /// its codec context first) has unregistered them.
    _textures: Arc<dyn Any + Send + Sync>,
}

impl D3d11Frames {
    /// For frames like `frame`: its device, size and texture pool.
    pub(crate) fn new(frame: &GpuFrame) -> Result<Self, String> {
        // SAFETY: each FFmpeg object is checked for null, and every failure
        // path unrefs what was made so far. The device pointer carries one
        // reference of its own (`d3d11_device_owned`), which the device
        // context releases when freed, including on a failed init.
        unsafe {
            let mut device = ffi::av_hwdevice_ctx_alloc(ffi::AV_HWDEVICE_TYPE_D3D11VA);
            if device.is_null() {
                return Err("no D3D11VA device context".into());
            }
            let hw = (*device).data.cast::<ffi::AVHWDeviceContext>();
            let d3d = (*hw).hwctx.cast::<DeviceContext>();
            // Taken only now, so every path below hands it to FFmpeg.
            (*d3d).device = frame.texture().d3d11_device_owned();
            if (*d3d).device.is_null() {
                // (FFmpeg would dereference it; a fake texture has none.)
                ffi::av_buffer_unref(&mut device);
                return Err("the frame has no D3D11 device".into());
            }
            let ret = ffi::av_hwdevice_ctx_init(device);
            if ret < 0 {
                ffi::av_buffer_unref(&mut device);
                return Err(format!("D3D11VA device: {}", crate::options::av_error(ret)));
            }
            let mut frames = ffi::av_hwframe_ctx_alloc(device);
            if frames.is_null() {
                ffi::av_buffer_unref(&mut device);
                return Err("no D3D11 frames context".into());
            }
            let fc = (*frames).data.cast::<ffi::AVHWFramesContext>();
            (*fc).format = ffi::AV_PIX_FMT_D3D11;
            (*fc).sw_format = ffi::AV_PIX_FMT_BGRA;
            (*fc).width = frame.width as i32;
            (*fc).height = frame.height as i32;
            (*fc).initial_pool_size = 0;
            let ret = ffi::av_hwframe_ctx_init(frames);
            if ret < 0 {
                ffi::av_buffer_unref(&mut frames);
                ffi::av_buffer_unref(&mut device);
                return Err(format!("D3D11 frames: {}", crate::options::av_error(ret)));
            }
            Ok(Self {
                device,
                frames,
                width: frame.width,
                height: frame.height,
                source: frame.source,
                _textures: frame.texture().keep_alive(),
            })
        }
    }

    /// A new reference to the frames context, for `AVCodecContext::hw_frames_ctx`.
    pub(crate) fn frames_ref(&self) -> Result<*mut ffi::AVBufferRef, String> {
        // SAFETY: `frames` is a live buffer we hold a reference to.
        let r = unsafe { ffi::av_buffer_ref(self.frames) };
        if r.is_null() {
            Err("out of memory".into())
        } else {
            Ok(r)
        }
    }

    /// `frame` as an `AV_PIX_FMT_D3D11` frame. The frame holds a clone of the
    /// `GpuFrame` (so its texture stays out of the pool) until FFmpeg frees it.
    pub(crate) fn wrap(&self, frame: &GpuFrame) -> Result<AVFrame, String> {
        if (frame.width, frame.height) != (self.width, self.height) {
            return Err(format!(
                "frame is {}x{}, encoder was opened for {}x{}",
                frame.width, frame.height, self.width, self.height
            ));
        }
        // Another pool's texture may be another device's, or sit at the
        // address of one of ours that was freed: the encoder must reopen.
        if frame.source != self.source {
            return Err(format!(
                "frame from texture pool {}, encoder opened for pool {}",
                frame.source, self.source
            ));
        }
        let keep = Box::into_raw(Box::new(frame.clone()));
        // SAFETY: `raw` is checked for null and is ours until handed to
        // `AVFrame::from_raw`. `buf[0]` owns `keep`: `release` frees it when
        // the frame's last reference goes, and on the failure paths below we
        // free it ourselves since no buffer took it.
        unsafe {
            let raw = ffi::av_frame_alloc();
            if raw.is_null() {
                drop(Box::from_raw(keep));
                return Err("out of memory".into());
            }
            (*raw).format = ffi::AV_PIX_FMT_D3D11;
            (*raw).width = frame.width as i32;
            (*raw).height = frame.height as i32;
            // AVD3D11FrameDescriptor's layout in data[]: the texture, then its
            // index in a texture array (0: ours are single textures).
            (*raw).data[0] = frame.texture().d3d11_texture().cast();
            (*raw).data[1] = std::ptr::null_mut();
            (*raw).buf[0] = ffi::av_buffer_create(
                keep.cast(),
                size_of::<GpuFrame>(),
                Some(release),
                keep.cast(),
                0,
            );
            if (*raw).buf[0].is_null() {
                drop(Box::from_raw(keep));
                let mut raw = raw;
                ffi::av_frame_free(&mut raw);
                return Err("out of memory".into());
            }
            (*raw).hw_frames_ctx = ffi::av_buffer_ref(self.frames);
            let wrapped = AVFrame::from_raw(NonNull::new_unchecked(raw));
            if (*raw).hw_frames_ctx.is_null() {
                return Err("out of memory".into());
            }
            Ok(wrapped)
        }
    }
}

/// Frees the `GpuFrame` a wrapped frame held. Called by FFmpeg, from whichever
/// thread drops the frame's last reference; never panics.
unsafe extern "C" fn release(opaque: *mut c_void, _data: *mut u8) {
    // SAFETY: `opaque` is the box `wrap` leaked for this buffer, freed once.
    drop(unsafe { Box::from_raw(opaque.cast::<GpuFrame>()) });
}

impl Drop for D3d11Frames {
    fn drop(&mut self) {
        // SAFETY: our own references, released once.
        unsafe {
            ffi::av_buffer_unref(&mut self.frames);
            ffi::av_buffer_unref(&mut self.device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mirrors match FFmpeg's structs on 64-bit Windows (pointers 8
    /// bytes): 7 pointers and two UINTs; a pointer, two UINTs and a pointer.
    #[test]
    fn mirrored_structs_have_ffmpegs_sizes() {
        assert_eq!(size_of::<DeviceContext>(), 7 * 8 + 2 * 4);
        assert_eq!(size_of::<FramesContext>(), 8 + 2 * 4 + 8);
    }
}
