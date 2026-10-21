//! Captured frames kept on the GPU (M9b): a pool of BGRA textures on the
//! capture's D3D11 device, each handed out as a [`GpuFrame`] that an encoder
//! with GPU input (NVENC) reads directly, without a trip through memory.
//!
//! Windows.Graphics.Capture reuses its one texture as soon as the frame
//! callback returns, so every frame is copied, GPU to GPU, into a texture of
//! the pool. A texture is free again once no frame points at it (FFmpeg holds
//! a frame until NVENC has read it).
//!
//! A pool's textures live as long as the pool, and as long as any encoder
//! holding its [`GpuTexture::keep_alive`]. The pool is replaced only for a new
//! device or size, under a new `source` id. NVENC remembers each texture it
//! registered by address: an encoder reopens on a new `source`, so it never
//! meets a freed texture's address reused by a new one, and it keeps its
//! pool's textures until it closes, so none is destroyed while registered.

use std::{
    any::Any,
    ffi::c_void,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use bytes::Bytes;
use couch_media::{BgraFrame, GpuFrame, GpuTexture, MediaError, Micros};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
                D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
                D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice,
                ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
            },
            Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
        },
    },
    core::Interface,
};

/// The most textures one pool holds: frames waiting in each viewer's mailbox
/// are the same few frames, and each encoder holds at most one more.
const MAX_TEXTURES: usize = 12;

/// Pool ids; 0 is never used.
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

/// The textures of one capture device and size.
pub struct TexturePool {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// The device's lock (multithread protection), held around each copy.
    multithread: ID3D11Multithread,
    format: DXGI_FORMAT,
    width: u32,
    height: u32,
    source: u64,
    /// For the free test: a texture only the pool holds is free.
    textures: Vec<Arc<PooledTexture>>,
    /// Every texture again, for encoders to keep alive past the pool.
    kept: Arc<Kept>,
    /// Staging textures for frames read back mapped (M12a).
    mapped: Arc<StagingSet>,
    /// The last copy's timings, in microseconds: waiting for the device's
    /// lock, queuing the copy, and flushing it (M9f: `COUCH_CAPTURE_TRACE`).
    pub last_timings: (u64, u64, u64),
}

// SAFETY: D3D11 devices are free-threaded, and the device's immediate context
// is serialized by the multithread protection `TexturePool::new` turns on.
// The pool is used from one thread at a time (the capture callback).
unsafe impl Send for TexturePool {}

/// A pool's textures, held by whatever must outlive the pool: an encoder
/// (NVENC) keeps every texture it may have registered until it closes, even
/// once the capture has moved on to a new pool.
struct Kept(Mutex<Vec<ID3D11Texture2D>>);

// SAFETY: the textures are only held here, never used; COM reference counts
// are thread-safe.
unsafe impl Send for Kept {}
unsafe impl Sync for Kept {}

impl TexturePool {
    /// A pool for `width` x `height` frames (even) in `format` on `device`.
    pub fn new(
        device: &ID3D11Device,
        format: DXGI_FORMAT,
        width: u32,
        height: u32,
    ) -> Result<Self, MediaError> {
        let multithread = protect(device)?;
        // SAFETY: a plain getter on a live device.
        let context = unsafe { device.GetImmediateContext() }.map_err(backend)?;
        Ok(Self {
            device: device.clone(),
            context,
            multithread,
            format,
            width,
            height,
            source: NEXT_SOURCE.fetch_add(1, Ordering::Relaxed),
            textures: Vec::new(),
            kept: Arc::new(Kept(Mutex::new(Vec::new()))),
            mapped: Arc::new(StagingSet {
                free: Mutex::new(Vec::new()),
                made: std::sync::atomic::AtomicUsize::new(0),
                gave_up: AtomicBool::new(false),
            }),
            last_timings: (0, 0, 0),
        })
    }

    /// Whether frames of this device, format and size fit this pool.
    pub fn fits(
        &self,
        device: &ID3D11Device,
        format: DXGI_FORMAT,
        width: u32,
        height: u32,
    ) -> bool {
        self.device == *device
            && self.format == format
            && (self.width, self.height) == (width, height)
    }

    /// Copies the top-left of `from` (at least the pool's size) into a free
    /// texture. `None` when every texture is still in use: the frame is
    /// dropped, as the capture mailbox would drop it.
    pub fn copy(
        &mut self,
        from: &ID3D11Texture2D,
        captured_at: Micros,
    ) -> Result<Option<GpuFrame>, MediaError> {
        // A frame dropped below (every texture in use) or failed shows no
        // timings, not the last frame's.
        self.last_timings = (0, 0, 0);
        let free = self
            .textures
            .iter()
            // Only the pool holds it, and nothing else can clone it: free.
            .find(|t| Arc::strong_count(t) == 1)
            .cloned();
        let slot = match free {
            Some(slot) => slot,
            None if self.textures.len() < MAX_TEXTURES => {
                let made = texture(&self.device, self.format, self.width, self.height, None)?;
                self.kept
                    .0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(made.clone());
                let slot = Arc::new(PooledTexture {
                    device: self.device.clone(),
                    texture: made,
                    height: self.height,
                    staging: Mutex::new(None),
                    mapped: self.mapped.clone(),
                    kept: self.kept.clone(),
                });
                self.textures.push(slot.clone());
                slot
            }
            None => return Ok(None),
        };
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: self.width,
            bottom: self.height,
            back: 1,
        };
        let started = std::time::Instant::now();
        // The device's lock taken explicitly, to time the wait for it apart
        // from the calls (they take it again, which nests).
        // SAFETY: paired with the Leave below; nothing between can return.
        unsafe { self.multithread.Enter() };
        let locked = started.elapsed();
        // SAFETY: both textures are live on this device, in the same format,
        // and the region lies within both. Flush hands the copy to the GPU
        // now, ahead of the encoder's work on the same device.
        unsafe {
            self.context
                .CopySubresourceRegion(&slot.texture, 0, 0, 0, 0, from, 0, Some(&region));
        }
        let copied = started.elapsed();
        // SAFETY: as above.
        unsafe { self.context.Flush() };
        let flushed = started.elapsed();
        // SAFETY: entered above.
        unsafe { self.multithread.Leave() };
        self.last_timings = (
            locked.as_micros() as u64,
            (copied - locked).as_micros() as u64,
            (flushed - copied).as_micros() as u64,
        );
        Ok(Some(GpuFrame::new(
            slot,
            self.width,
            self.height,
            captured_at,
            self.source,
        )))
    }
}

/// One texture of a pool.
struct PooledTexture {
    device: ID3D11Device,
    texture: ID3D11Texture2D,
    height: u32,
    /// For [`GpuTexture::download`] by memcpy, made on first use.
    staging: Mutex<Option<ID3D11Texture2D>>,
    /// The pool's staging textures handed out mapped, as frames (M12a).
    mapped: Arc<StagingSet>,
    /// Its pool's textures, for [`GpuTexture::keep_alive`].
    kept: Arc<Kept>,
}

/// How many staging textures one pool hands out mapped at once (M12a):
/// each stays mapped while any viewer's encoder holds its frame (with 4
/// viewers about 5 to 9 at a time). Beyond that, frames are copied out by
/// memcpy as before.
const MAX_MAPPED: usize = 16;
/// And how much memory they may take together: 16 at 1080p (8 MB each),
/// 8 at 4K (33 MB each). They stay made while the pool lives.
const MAX_MAPPED_BYTES: usize = 256 << 20;

/// A frame read back is handed out as its staging texture's own mapped
/// memory, with no memcpy (M12a: at 1080p with 4 encoders busy, the memcpy
/// was 2.9 ms of a 4.8 ms read-back), unless `COUCH_HOST_MAPPED_FRAMES=0`,
/// the switch for measuring against the memcpy. Read once.
fn mapped_frames() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("COUCH_HOST_MAPPED_FRAMES").map_or(true, |v| v.trim() != "0"))
}

/// A pool's staging textures for frames handed out mapped.
struct StagingSet {
    free: Mutex<Vec<ID3D11Texture2D>>,
    /// How many were made.
    made: std::sync::atomic::AtomicUsize,
    /// Making one failed (memory short, say): no more are made, and frames
    /// beyond those there go by memcpy rather than fail.
    gave_up: AtomicBool,
}

// SAFETY: the textures are used only through the multithread-protected
// device (Map and Unmap on its immediate context), one frame at a time each:
// a texture is either in `free` or owned by exactly one `MappedStaging`.
unsafe impl Send for StagingSet {}
unsafe impl Sync for StagingSet {}

impl StagingSet {
    /// The most this set makes for textures like `desc`.
    fn limit(desc: &D3D11_TEXTURE2D_DESC) -> usize {
        let each = (desc.Width as usize * desc.Height as usize * 4).max(1);
        (MAX_MAPPED_BYTES / each).clamp(1, MAX_MAPPED)
    }

    /// A free staging texture, or a new one while fewer than [`Self::limit`]
    /// were made; `None` when every one is out, or one couldn't be made (the
    /// caller then copies by memcpy, as before M12a).
    fn take(&self, device: &ID3D11Device, desc: &D3D11_TEXTURE2D_DESC) -> Option<ID3D11Texture2D> {
        if let Some(t) = self
            .free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
        {
            return Some(t);
        }
        if self.gave_up.load(Ordering::Relaxed) {
            return None;
        }
        if self.made.fetch_add(1, Ordering::Relaxed) >= Self::limit(desc) {
            self.made.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        match staging_texture(device, desc) {
            Ok(t) => Some(t),
            Err(e) => {
                self.made.fetch_sub(1, Ordering::Relaxed);
                if !self.gave_up.swap(true, Ordering::Relaxed) {
                    tracing::warn!("no more mapped frames ({e}): frames read back are copied out");
                }
                None
            }
        }
    }

    fn give_back(&self, texture: ID3D11Texture2D) {
        self.free
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(texture);
    }
}

/// A frame's pixels: a staging texture's mapped memory, unmapped and given
/// back to its set when the last `Bytes` pointing into it drops.
struct MappedStaging {
    context: ID3D11DeviceContext,
    staging: Option<ID3D11Texture2D>,
    set: Arc<StagingSet>,
    data: *const u8,
    len: usize,
}

// SAFETY: the mapping stays valid until `Drop` unmaps it, on the device's
// multithread-protected immediate context, from whichever thread drops the
// last reference; nothing writes to it meanwhile (the texture is in no
// set while mapped, and the GPU never writes a mapped resource).
unsafe impl Send for MappedStaging {}
unsafe impl Sync for MappedStaging {}

impl AsRef<[u8]> for MappedStaging {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `len` bytes from `Map`, valid until `Drop` unmaps them.
        unsafe { std::slice::from_raw_parts(self.data, self.len) }
    }
}

impl Drop for MappedStaging {
    fn drop(&mut self) {
        if let Some(staging) = self.staging.take() {
            // SAFETY: mapped by `download`, unmapped once, here; no slice of
            // it outlives this owner.
            unsafe { self.context.Unmap(&staging, 0) };
            self.set.give_back(staging);
        }
    }
}

// SAFETY: as for `TexturePool`; the texture is only used through the
// multithread-protected device, and the staging texture behind its mutex.
unsafe impl Send for PooledTexture {}
unsafe impl Sync for PooledTexture {}

impl GpuTexture for PooledTexture {
    fn d3d11_texture(&self) -> *mut c_void {
        self.texture.as_raw()
    }

    fn d3d11_device_owned(&self) -> *mut c_void {
        // The clone adds the reference the caller now owns.
        self.device.clone().into_raw()
    }

    fn keep_alive(&self) -> Arc<dyn Any + Send + Sync> {
        self.kept.clone()
    }

    fn download(&self) -> Result<(u32, Bytes), MediaError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: fills a local description of a live texture.
        unsafe { self.texture.GetDesc(&mut desc) };
        if mapped_frames()
            && let Some(staging) = self.mapped.take(&self.device, &desc)
        {
            return self.download_mapped(staging);
        }
        let mut staging = self.staging.lock().unwrap_or_else(PoisonError::into_inner);
        let staging = match &*staging {
            Some(staging) => staging.clone(),
            None => {
                let made = staging_texture(&self.device, &desc)?;
                *staging = Some(made.clone());
                made
            }
        };
        // SAFETY: a plain getter on a live device.
        let context = unsafe { self.device.GetImmediateContext() }.map_err(backend)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: same device, size and format; Map waits for the copy, and
        // the mapping is read and then unmapped before anything else uses it
        // (the staging texture is behind its mutex).
        unsafe {
            context.CopyResource(&staging, &self.texture);
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(backend)?;
            let len = mapped.RowPitch as usize * self.height as usize;
            let data =
                Bytes::copy_from_slice(std::slice::from_raw_parts(mapped.pData.cast::<u8>(), len));
            context.Unmap(&staging, 0);
            Ok((mapped.RowPitch, data))
        }
    }
}

impl PooledTexture {
    /// The texture copied into `staging`, mapped, and handed out as the
    /// frame's memory itself (M12a): no memcpy.
    fn download_mapped(&self, staging: ID3D11Texture2D) -> Result<(u32, Bytes), MediaError> {
        // SAFETY: a plain getter on a live device.
        let context = unsafe { self.device.GetImmediateContext() }.map_err(backend)?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: same device, size and format; `staging` is this call's
        // alone (taken from the set), and Map waits for the copy. On failure
        // it goes back to the set unmapped.
        let result = unsafe {
            context.CopyResource(&staging, &self.texture);
            context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        };
        if let Err(e) = result {
            self.mapped.give_back(staging);
            return Err(backend(e));
        }
        let owner = MappedStaging {
            context,
            staging: Some(staging),
            set: self.mapped.clone(),
            data: mapped.pData.cast::<u8>().cast_const(),
            len: mapped.RowPitch as usize * self.height as usize,
        };
        Ok((mapped.RowPitch, Bytes::from_owner(owner)))
    }
}

/// Lets several threads use the device's immediate context: the capture
/// thread copies on it while encoder threads run NVENC on the same device.
/// FFmpeg turns this on only for devices it creates itself.
fn protect(device: &ID3D11Device) -> Result<ID3D11Multithread, MediaError> {
    let multithread: ID3D11Multithread = device.cast().map_err(backend)?;
    // SAFETY: a plain setter on a live device; the previous value is ignored.
    let _ = unsafe { multithread.SetMultithreadProtected(true) };
    Ok(multithread)
}

fn texture(
    device: &ID3D11Device,
    format: DXGI_FORMAT,
    width: u32,
    height: u32,
    initial: Option<&D3D11_SUBRESOURCE_DATA>,
) -> Result<ID3D11Texture2D, MediaError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        // Render target too, for encoders (or a conversion pass) that want it.
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut made = None;
    // SAFETY: the description and initial data (if any) outlive the call.
    unsafe {
        device
            .CreateTexture2D(&desc, initial.map(std::ptr::from_ref), Some(&mut made))
            .map_err(backend)?;
    }
    made.ok_or_else(|| MediaError::Backend("CreateTexture2D returned no texture".into()))
}

pub(crate) fn staging_texture(
    device: &ID3D11Device,
    desc: &D3D11_TEXTURE2D_DESC,
) -> Result<ID3D11Texture2D, MediaError> {
    let desc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..*desc
    };
    let mut made = None;
    // SAFETY: the description outlives the call.
    unsafe {
        device
            .CreateTexture2D(&desc, None, Some(&mut made))
            .map_err(backend)?;
    }
    made.ok_or_else(|| MediaError::Backend("CreateTexture2D returned no texture".into()))
}

/// Frames from memory onto a device of our own, through a [`TexturePool`],
/// as the capture would hand them over: for GPU tests and `couch-cli probe`.
pub struct Uploader {
    pool: TexturePool,
}

impl Uploader {
    /// A device on the default adapter, as the capture makes one (`wgc::new_device`).
    pub fn new(width: u32, height: u32) -> Result<Self, MediaError> {
        let mut device = None;
        // SAFETY: out-parameters are live locals.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .map_err(backend)?;
        }
        let device =
            device.ok_or_else(|| MediaError::Backend("D3D11CreateDevice made no device".into()))?;
        Ok(Self {
            pool: TexturePool::new(&device, DXGI_FORMAT_B8G8R8A8_UNORM, width, height)?,
        })
    }

    /// A new pool on the same device: its frames have a new `source`.
    pub fn new_pool(&mut self) -> Result<(), MediaError> {
        let p = &self.pool;
        self.pool = TexturePool::new(&p.device, p.format, p.width, p.height)?;
        Ok(())
    }

    /// `frame` (the uploader's size) as a GPU frame, or `None` when every
    /// texture of the pool is in use.
    pub fn upload(&mut self, frame: &BgraFrame) -> Result<Option<GpuFrame>, MediaError> {
        let p = &self.pool;
        if (frame.width, frame.height) != (p.width, p.height) {
            return Err(MediaError::Backend(format!(
                "a {}x{} frame for a {}x{} uploader",
                frame.width, frame.height, p.width, p.height
            )));
        }
        let initial = D3D11_SUBRESOURCE_DATA {
            pSysMem: frame.data.as_ptr().cast(),
            SysMemPitch: frame.stride,
            SysMemSlicePitch: 0,
        };
        let source = texture(&p.device, p.format, p.width, p.height, Some(&initial))?;
        self.pool.copy(&source, frame.captured_at)
    }
}

fn backend(e: impl std::fmt::Display) -> MediaError {
    MediaError::Backend(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, at: u64) -> BgraFrame {
        let data: Vec<u8> = (0..width * height)
            .flat_map(|i| [(i % 251) as u8, (i % 13) as u8, (i % 7) as u8, 255])
            .collect();
        BgraFrame {
            width,
            height,
            stride: width * 4,
            data: Bytes::from(data),
            captured_at: Micros(at),
        }
    }

    /// Needs a D3D11 device: any GPU, or Windows' software adapter.
    #[test]
    #[ignore = "needs a D3D11 device"]
    fn frames_come_back_from_the_gpu_and_textures_are_reused() {
        let mut uploader = Uploader::new(64, 32).unwrap();
        let first = uploader.upload(&frame(64, 32, 1)).unwrap().unwrap();
        let back = first.cpu().unwrap();
        let source = frame(64, 32, 1);
        for (x, y) in [(0, 0), (63, 31), (17, 9)] {
            assert_eq!(back.pixel(x, y), source.pixel(x, y), "({x}, {y})");
        }
        assert_eq!(back.captured_at, Micros(1));

        // While a frame is held its texture isn't reused; once dropped it is.
        let held: Vec<_> = (0..MAX_TEXTURES - 1)
            .map(|i| uploader.upload(&frame(64, 32, i as u64)).unwrap().unwrap())
            .collect();
        assert!(uploader.upload(&frame(64, 32, 99)).unwrap().is_none());
        drop(first);
        assert!(uploader.upload(&frame(64, 32, 100)).unwrap().is_some());
        assert_eq!(uploader.pool.textures.len(), MAX_TEXTURES);

        let old = held[0].source;
        uploader.new_pool().unwrap();
        let next = uploader.upload(&frame(64, 32, 101)).unwrap().unwrap();
        assert_ne!(next.source, old);
    }

    /// M12a: a frame read back as its staging texture's mapped memory has
    /// the right pixels; its staging texture is out of the set while the GPU
    /// frame (which keeps its copy in memory) or any copy of its pixels
    /// lives, back once the last drops; and when every one is out, frames
    /// are copied by memcpy instead.
    #[test]
    #[ignore = "needs a D3D11 device"]
    fn frames_read_back_mapped_give_their_staging_back() {
        if !mapped_frames() {
            eprintln!("COUCH_HOST_MAPPED_FRAMES=0 is set: skipped");
            return;
        }
        let mut uploader = Uploader::new(64, 32).unwrap();
        let set = uploader.pool.mapped.clone();
        let free = || set.free.lock().unwrap().len();
        let source = frame(64, 32, 1);

        let first = uploader.upload(&source).unwrap().unwrap();
        let back = first.cpu().unwrap();
        for (x, y) in [(0, 0), (63, 31), (17, 9)] {
            assert_eq!(back.pixel(x, y), source.pixel(x, y), "({x}, {y})");
        }
        assert_eq!(set.made.load(Ordering::SeqCst), 1);
        assert_eq!(free(), 0, "mapped while the frame lives");
        // The GPU frame keeps its copy in memory, so it keeps it mapped.
        drop(back);
        assert_eq!(free(), 0, "the GPU frame still holds its copy");
        // And so does a copy of the pixels, once the GPU frame is gone.
        let pixels = first.cpu().unwrap().data.clone();
        drop(first);
        assert_eq!(free(), 0, "a copy of its pixels still points into it");
        assert_eq!(pixels[..4], source.data[..4]);
        drop(pixels);
        assert_eq!(free(), 1, "given back");

        // Taken again rather than made anew.
        let again = uploader.upload(&source).unwrap().unwrap();
        let held = again.cpu().unwrap();
        assert_eq!(set.made.load(Ordering::SeqCst), 1);
        drop((held, again));

        // A 64x32 frame is small: the count caps, not the bytes.
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: 64,
            Height: 32,
            ..Default::default()
        };
        assert_eq!(StagingSet::limit(&desc), MAX_MAPPED);
        (desc.Width, desc.Height) = (3840, 2160);
        assert_eq!(StagingSet::limit(&desc), 8, "4K: the bytes cap");

        // Every one out: copied by memcpy, still the right pixels.
        let mut out = Vec::new();
        for i in 0..MAX_MAPPED {
            let f = uploader.upload(&frame(64, 32, i as u64)).unwrap().unwrap();
            out.push(f.cpu().unwrap());
        }
        assert_eq!(set.made.load(Ordering::SeqCst), MAX_MAPPED);
        let extra = uploader.upload(&source).unwrap().unwrap();
        let copied = extra.cpu().unwrap();
        assert_eq!(copied.pixel(17, 9), source.pixel(17, 9));
        assert_eq!(
            set.made.load(Ordering::SeqCst),
            MAX_MAPPED,
            "none made beyond"
        );
        drop(out);
        assert_eq!(free(), MAX_MAPPED);
    }
}
