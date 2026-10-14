//! D3D12 decoding for the viewer (M9c): an FFmpeg D3D12VA device context on
//! the viewer's own D3D12 device, and each decoded frame handed out as a
//! [`DecodedSurface`] that holds FFmpeg's frame (so its resource isn't decoded
//! into again) until the renderer is done with it.
//!
//! The structs below mirror FFmpeg 8.1's `libavutil/hwcontext_d3d12va.h`
//! (field layout only), which our generated bindings leave out.

use std::ffi::c_void;

use couch_media::DecodedSurface;
use rsmpeg::{avutil::AVFrame, ffi};

/// `AVD3D12VADeviceContext`.
#[repr(C)]
struct DeviceContext {
    device: *mut c_void,
    video_device: *mut c_void,
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    lock_ctx: *mut c_void,
    resource_flags: i32,
    heap_flags: i32,
}

/// `AVD3D12VASyncContext`.
#[repr(C)]
struct SyncContext {
    fence: *mut c_void,
    event: *mut c_void,
    fence_value: u64,
}

/// `AVD3D12VAFrame`, what `data[0]` of an `AV_PIX_FMT_D3D12` frame points at.
#[repr(C)]
struct Frame {
    texture: *mut c_void,
    subresource_index: i32,
    sync: SyncContext,
    flags: i32,
}

/// A reference to an FFmpeg device context, unreferenced when dropped.
pub(crate) struct DeviceRef(*mut ffi::AVBufferRef);

impl DeviceRef {
    /// The reference, now the caller's (`AVCodecContext::hw_device_ctx`,
    /// which the codec context unreferences when freed).
    pub(crate) fn into_raw(self) -> *mut ffi::AVBufferRef {
        let raw = self.0;
        std::mem::forget(self);
        raw
    }
}

impl Drop for DeviceRef {
    fn drop(&mut self) {
        // SAFETY: our own reference, unreferenced once.
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}

/// A D3D12VA device context on `device_owned`, an `ID3D12Device` carrying one
/// reference of its own. That reference is released when the context is
/// freed, or here if no context took it.
///
/// Unlike D3D11VA, FFmpeg's D3D12VA context releases the device only when
/// FFmpeg created the device itself (`d3d12va_device_free`, installed by
/// `av_hwdevice_ctx_create`). A context we fill in releases it through the
/// `free` callback we install, which FFmpeg calls once, on the last unref,
/// also after a failed init.
pub(crate) fn device_context(device_owned: *mut c_void) -> Result<DeviceRef, String> {
    if device_owned.is_null() {
        return Err("no D3D12 device".into());
    }
    // SAFETY: a plain allocation, checked for null. An FFmpeg built without
    // D3D12VA returns null; the device reference is then released here.
    let raw = unsafe { ffi::av_hwdevice_ctx_alloc(ffi::AV_HWDEVICE_TYPE_D3D12VA) };
    if raw.is_null() {
        // SAFETY: the caller handed us this reference to release.
        unsafe { release(device_owned) };
        return Err("this FFmpeg has no D3D12VA".into());
    }
    let device = DeviceRef(raw);
    // SAFETY: a D3D12VA device context, whose `hwctx` is an
    // `AVD3D12VADeviceContext`. The device pointer and the callback that
    // releases it are set before anything can free the context, so the
    // reference is the context's from here on.
    unsafe {
        let hw = (*raw).data.cast::<ffi::AVHWDeviceContext>();
        let d3d = (*hw).hwctx.cast::<DeviceContext>();
        (*d3d).device = device_owned;
        (*hw).free = Some(release_device);
        let ret = ffi::av_hwdevice_ctx_init(raw);
        if ret < 0 {
            return Err(format!("D3D12VA device: {}", crate::options::av_error(ret)));
        }
    }
    Ok(device)
}

/// `AVHWDeviceContext::free` for our D3D12VA contexts: releases the device's
/// reference. Called by FFmpeg once, from whichever thread frees the
/// context; never panics.
unsafe extern "C" fn release_device(hw: *mut ffi::AVHWDeviceContext) {
    // SAFETY: FFmpeg passes the context being freed, whose `hwctx` is ours.
    unsafe {
        let d3d = (*hw).hwctx.cast::<DeviceContext>();
        let device = std::mem::replace(&mut (*d3d).device, std::ptr::null_mut());
        if !device.is_null() {
            release(device);
        }
    }
}

/// `IUnknown::Release` on a COM pointer: the third entry of its vtable. (This
/// crate doesn't depend on `windows`.)
///
/// # Safety
/// `object` is a live COM interface pointer whose reference the caller owns.
unsafe fn release(object: *mut c_void) {
    type Release = unsafe extern "system" fn(*mut c_void) -> u32;
    // SAFETY: a COM object starts with its vtable pointer; IUnknown's
    // methods come first: QueryInterface, AddRef, Release.
    unsafe {
        let vtable = *object.cast::<*const Release>();
        (*vtable.add(2))(object);
    }
}

/// A decoded `AV_PIX_FMT_D3D12` frame, held until dropped.
pub(crate) struct D3d12Surface {
    frame: AVFrame,
}

// The frame's buffers are reference-counted with atomics; the last reference
// may be dropped on any thread. The surface is only read, never changed.
unsafe impl Send for D3d12Surface {}

impl D3d12Surface {
    /// `frame` must be an `AV_PIX_FMT_D3D12` frame from the decoder.
    pub(crate) fn new(frame: AVFrame) -> Result<Self, String> {
        if frame.format != ffi::AV_PIX_FMT_D3D12 || frame.data[0].is_null() {
            return Err(format!("not a D3D12 frame (format {})", frame.format));
        }
        // SAFETY: data[0] of a D3D12 frame points at its `AVD3D12VAFrame`.
        let raw = unsafe { &*frame.data[0].cast::<Frame>() };
        if raw.texture.is_null() || raw.sync.fence.is_null() || raw.subresource_index != 0 {
            // A texture array (never chosen for decoding today) would need
            // the slice handed on too.
            return Err("unexpected D3D12 frame layout".into());
        }
        Ok(Self { frame })
    }

    fn raw(&self) -> &Frame {
        // SAFETY: checked in `new`; the frame (and so its descriptor) lives
        // as long as we do, and nothing changes it while we hold it.
        unsafe { &*self.frame.data[0].cast::<Frame>() }
    }
}

impl DecodedSurface for D3d12Surface {
    fn d3d12_resource(&self) -> *mut c_void {
        self.raw().texture
    }

    fn d3d12_fence(&self) -> (*mut c_void, u64) {
        let sync = &self.raw().sync;
        (sync.fence, sync.fence_value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mirrors match FFmpeg's structs on 64-bit Windows (pointers 8
    /// bytes, enums 4): five pointers and two enums; a pointer, an int padded
    /// to 8, the sync context (two pointers and a u64) and an enum padded.
    #[test]
    fn mirrored_structs_have_ffmpegs_sizes() {
        assert_eq!(size_of::<DeviceContext>(), 5 * 8 + 2 * 4);
        assert_eq!(size_of::<SyncContext>(), 3 * 8);
        assert_eq!(size_of::<Frame>(), 8 + 8 + 3 * 8 + 8);
        assert_eq!(std::mem::offset_of!(Frame, sync), 16);
    }
}
