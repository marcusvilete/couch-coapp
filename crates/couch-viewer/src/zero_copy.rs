//! Decoded frames that stay on the GPU (M9c). FFmpeg's D3D12VA decoder runs
//! on wgpu's own D3D12 device, so a decoded frame is already a resource wgpu
//! can read. For each frame, in a submit of its own before the one that
//! draws it:
//! 1. the resource is wrapped as a wgpu texture (NV12, in the COMMON state
//!    FFmpeg leaves it in);
//! 2. the submit waits, on the GPU, for FFmpeg's fence;
//! 3. its two planes are copied into the video pass's own textures;
//! 4. it goes back to COMMON, the state FFmpeg's next use expects.
//!
//! The frame (and with it FFmpeg's reference) is kept until that submit has
//! finished, so FFmpeg never decodes into a picture the GPU is still copying.
//!
//! The `windows` types here only wrap and unwrap COM pointers for wgpu-hal
//! (reference counting) and read the resource's description; every other
//! Windows call stays in `couch-platform-win`.

use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result, anyhow, bail};
use couch_media::{GpuNv12Frame, barcode};
use wgpu::hal::{api::Dx12, dx12};
use windows::{
    Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Fence, ID3D12Resource},
    core::Interface,
};

/// The `ID3D12Device` under a wgpu device on the DX12 backend, with a
/// reference of its own, for the decoder thread. Released if never used.
pub struct DecoderDevice(ID3D12Device);

impl DecoderDevice {
    /// `None` on another backend.
    pub fn of(device: &wgpu::Device) -> Option<Self> {
        // SAFETY: the hal device is only borrowed to clone its COM pointer
        // (AddRef); nothing is done with it through wgpu's back.
        let hal = unsafe { device.as_hal::<Dx12>() }?;
        Some(Self(hal.raw_device().clone()))
    }

    /// The pointer, with its reference handed to the caller (FFmpeg, which
    /// releases it).
    pub fn into_raw(self) -> *mut c_void {
        self.0.into_raw()
    }
}

// ID3D12Device is free-threaded.
unsafe impl Send for DecoderDevice {}

/// Brings decoded frames into the video pass's textures and gives them back
/// to the decoder once the GPU is done with them.
pub struct FrameImporter {
    /// Frames copied in submits not yet known finished, with their submit's
    /// number.
    in_flight: VecDeque<(u64, GpuNv12Frame)>,
    /// Submits that carried a frame, so far.
    submitted: u64,
    /// The highest such submit the GPU has finished.
    finished: Arc<AtomicU64>,
    /// A frame copied in the encoder being recorded, before its submit.
    pending: Option<GpuNv12Frame>,
}

impl Default for FrameImporter {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameImporter {
    pub fn new() -> Self {
        Self {
            in_flight: VecDeque::new(),
            submitted: 0,
            finished: Arc::new(AtomicU64::new(0)),
            pending: None,
        }
    }

    /// Records copying `frame`'s planes into `luma` (R8Unorm) and `chroma`
    /// (Rg8Unorm), both the frame's size (chroma halved), and stages the wait
    /// for the decoder. The next `queue.submit` must carry `encoder`; call
    /// [`Self::submitted`] right after it.
    pub fn copy(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: GpuNv12Frame,
        luma: &wgpu::Texture,
        chroma: &wgpu::Texture,
    ) -> Result<()> {
        if self.pending.is_some() {
            bail!("a frame is already waiting for its submit");
        }
        let (width, height) = (frame.width, frame.height);
        let raw = frame.surface.d3d12_resource();
        // SAFETY: the surface keeps the resource alive while `frame` lives,
        // and the clone (AddRef) keeps it alive for wgpu's texture after.
        let resource = unsafe { ID3D12Resource::from_raw_borrowed(&raw) }
            .context("no D3D12 resource")?
            .clone();
        // SAFETY: a plain query of the resource's description.
        let desc = unsafe { resource.GetDesc() };
        if (desc.Width, desc.Height) != (u64::from(width), height) {
            bail!(
                "decoded resource is {}x{}, the frame {width}x{height}",
                desc.Width,
                desc.Height
            );
        }
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // SAFETY: the resource is a 2D NV12 texture of `size` on this device
        // (FFmpeg's decoder was given wgpu's device), one mip, one sample.
        let hal = unsafe {
            dx12::Device::texture_from_raw(
                resource,
                wgpu::TextureFormat::NV12,
                wgpu::TextureDimension::D2,
                size,
                1,
                1,
            )
        };
        let desc = wgpu::TextureDescriptor {
            label: Some("decoded"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::NV12,
            usage: wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };
        // SAFETY: made from this device's resource, as `desc` says. After
        // decoding, FFmpeg leaves it in COMMON, which is `PRESENT` to wgpu's
        // DX12 backend. A new wgpu texture per frame keeps wgpu's idea of the
        // state from ever disagreeing with what FFmpeg did since.
        let texture = unsafe {
            device.create_texture_from_hal::<Dx12>(hal, &desc, wgpu::TextureUses::PRESENT)
        };

        let (fence, value) = frame.surface.d3d12_fence();
        // SAFETY: the fence belongs to the frame's buffer in FFmpeg's pool,
        // which the frame we hold keeps alive; the clone (AddRef) keeps it
        // alive for the queue's staged wait after that.
        let fence = unsafe { ID3D12Fence::from_raw_borrowed(&fence) }
            .context("no D3D12 fence")?
            .clone();
        // SAFETY: only used to stage the wait for the next submit.
        let hal_queue =
            unsafe { queue.as_hal::<Dx12>() }.ok_or_else(|| anyhow!("not a DX12 queue"))?;
        hal_queue.add_wait_fence(fence, value);

        let plane = |aspect| wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect,
        };
        let whole = |texture| wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        };
        encoder.copy_texture_to_texture(plane(wgpu::TextureAspect::Plane0), whole(luma), size);
        encoder.copy_texture_to_texture(
            plane(wgpu::TextureAspect::Plane1),
            whole(chroma),
            wgpu::Extent3d {
                width: width / 2,
                height: height / 2,
                depth_or_array_layers: 1,
            },
        );
        encoder.transition_resources(
            std::iter::empty(),
            std::iter::once(wgpu::TextureTransition {
                texture: &texture,
                selector: None,
                state: wgpu::TextureUses::PRESENT,
            }),
        );
        self.pending = Some(frame);
        Ok(())
    }

    /// After the submit that carried the last [`Self::copy`]: the frame is
    /// kept until the GPU has finished that submit.
    pub fn submitted(&mut self, queue: &wgpu::Queue) {
        let Some(frame) = self.pending.take() else {
            return;
        };
        self.submitted += 1;
        let number = self.submitted;
        self.in_flight.push_back((number, frame));
        let finished = self.finished.clone();
        queue.on_submitted_work_done(move || {
            finished.fetch_max(number, Ordering::AcqRel);
        });
    }

    /// Gives back to the decoder every frame whose submit has finished, as
    /// far as the last `device.poll` knows (it runs the callbacks). Never
    /// waits.
    pub fn release_finished(&mut self) {
        let finished = self.finished.load(Ordering::Acquire);
        while self.in_flight.front().is_some_and(|(n, _)| *n <= finished) {
            self.in_flight.pop_front();
        }
    }

    /// Frames still held for the GPU.
    pub fn held(&self) -> usize {
        self.in_flight.len() + usize::from(self.pending.is_some())
    }
}

/// The latency barcode's corner of a frame on the GPU: the search area the
/// barcode locator uses (`3 × MARGIN` from the top left) plus one barcode.
const STRIP: (u32, u32) = (
    barcode::width(crate::clock::CELL) + 3 * crate::clock::MARGIN,
    crate::clock::CELL + 3 * crate::clock::MARGIN,
);
const STRIP_ROW: u32 = STRIP.0.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
/// Readings in flight at once; with none free, a frame goes unread.
const STRIP_SLOTS: usize = 3;

/// A frame's barcode strip, read back from the GPU.
pub struct Strip<T> {
    /// Tightly packed luma rows.
    pub luma: Vec<u8>,
    pub size: (u32, u32),
    /// What [`BarcodeStrips::attach`] put with it (the frame's timing).
    pub value: T,
}

/// Reads the barcode's corner of frames that stay on the GPU, without
/// waiting: a frame's strip is copied out in the submit that brings it in,
/// mapped once that is done, and handed back with the value attached to it
/// once both have happened.
pub struct BarcodeStrips<T> {
    slots: Vec<StripSlot<T>>,
}

/// A slot's map: not yet, done, or failed (then there is nothing to unmap).
const MAP_PENDING: u8 = 0;
const MAP_DONE: u8 = 1;
const MAP_FAILED: u8 = 2;

struct StripSlot<T> {
    buffer: wgpu::Buffer,
    busy: bool,
    mapped: Arc<std::sync::atomic::AtomicU8>,
    size: (u32, u32),
    value: Option<T>,
    /// The frame was never shown: free the slot once mapped.
    orphan: bool,
}

impl<T> Default for BarcodeStrips<T> {
    fn default() -> Self {
        Self { slots: Vec::new() }
    }
}

impl<T> BarcodeStrips<T> {
    /// Records copying `luma`'s strip, or `None` with every slot in use. Call
    /// [`Self::submitted`] after the submit that carries `encoder`.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        luma: &wgpu::Texture,
    ) -> Option<usize> {
        let slot = match self.slots.iter().position(|s| !s.busy) {
            Some(i) => i,
            None if self.slots.len() < STRIP_SLOTS => {
                self.slots.push(StripSlot {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("barcode strip"),
                        size: u64::from(STRIP_ROW * STRIP.1),
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    }),
                    busy: false,
                    mapped: Arc::default(),
                    size: (0, 0),
                    value: None,
                    orphan: false,
                });
                self.slots.len() - 1
            }
            None => return None,
        };
        let size = (STRIP.0.min(luma.width()), STRIP.1.min(luma.height()));
        encoder.copy_texture_to_buffer(
            luma.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.slots[slot].buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(STRIP_ROW),
                    rows_per_image: Some(STRIP.1),
                },
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        let s = &mut self.slots[slot];
        s.busy = true;
        s.size = size;
        s.value = None;
        s.orphan = false;
        s.mapped.store(MAP_PENDING, Ordering::Release);
        Some(slot)
    }

    /// After the submit carrying `slot`'s copy.
    pub fn submitted(&mut self, slot: usize) {
        let s = &self.slots[slot];
        let mapped = s.mapped.clone();
        s.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let state = if result.is_ok() { MAP_DONE } else { MAP_FAILED };
                mapped.store(state, Ordering::Release);
            });
    }

    /// The frame was shown: hand `value` back with its strip.
    pub fn attach(&mut self, slot: usize, value: T) {
        self.slots[slot].value = Some(value);
    }

    /// The frame was never shown: drop its strip.
    pub fn forget(&mut self, slot: usize) {
        self.slots[slot].orphan = true;
    }

    /// Readings in flight.
    pub fn busy(&self) -> bool {
        self.slots.iter().any(|s| s.busy)
    }

    /// Every strip that is both read back (or failed to be: then with no
    /// pixels, size 0) and attached, as far as the last `device.poll` knows.
    /// Never waits.
    pub fn take_ready(&mut self) -> Vec<Strip<T>> {
        let mut ready = Vec::new();
        for s in &mut self.slots {
            let state = s.mapped.load(Ordering::Acquire);
            if !s.busy || state == MAP_PENDING || (s.value.is_none() && !s.orphan) {
                continue;
            }
            let (mut luma, mut size) = (Vec::new(), (0, 0));
            if state == MAP_DONE {
                if !s.orphan
                    && let Ok(data) = s.buffer.slice(..).get_mapped_range()
                {
                    let (w, h) = s.size;
                    for row in 0..h {
                        luma.extend_from_slice(&data[(row * STRIP_ROW) as usize..][..w as usize]);
                    }
                    size = s.size;
                }
                s.buffer.unmap();
            }
            s.busy = false;
            if let (Some(value), false) = (s.value.take(), s.orphan) {
                ready.push(Strip { luma, size, value });
            }
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    //! The M9c spike: FFmpeg's D3D12VA frames read by wgpu. Needs a GPU with
    //! D3D12 video decoding and NVENC: `cargo test -p couch-viewer zero_copy
    //! -- --ignored --nocapture`.

    use std::sync::{Arc, Mutex};

    use couch_codec_ffmpeg::{DecoderKind, FfmpegDecoder, FfmpegEncoder};
    use couch_media::{
        Codec, DecodedFrame, EncoderConfig, EncoderInput, Nv12Frame, VideoDecoder, VideoEncoder,
        barcode,
        clock::ProcessClock,
        testpattern::{BARCODE_CELL, BARCODE_X, BARCODE_Y, TestPattern},
    };

    use super::*;

    struct Headless {
        device: wgpu::Device,
        queue: wgpu::Queue,
        errors: Arc<Mutex<Vec<String>>>,
    }

    /// wgpu on DX12 with the D3D12 debug layer and GPU-based validation,
    /// whose findings wgpu logs (shown with `--nocapture`).
    fn headless() -> Headless {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
            .with_test_writer()
            .try_init();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            flags: wgpu::InstanceFlags::advanced_debugging(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .expect("a DX12 adapter");
        eprintln!("adapter: {:?}", adapter.get_info());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("spike"),
            required_features: wgpu::Features::TEXTURE_FORMAT_NV12,
            ..Default::default()
        }))
        .expect("a DX12 device with NV12");
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
            sink.lock().unwrap().push(e.to_string());
        }));
        Headless {
            device,
            queue,
            errors,
        }
    }

    fn plane_texture(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        w: u32,
        h: u32,
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("plane"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// A COM object's reference count: AddRef, then Release, which returns it.
    fn com_refs(object: *mut c_void) -> u32 {
        type Count = unsafe extern "system" fn(*mut c_void) -> u32;
        // SAFETY: a live COM object; IUnknown's AddRef and Release are its
        // vtable's second and third entries, and they balance.
        unsafe {
            let vtable = *object.cast::<*const Count>();
            (*vtable.add(1))(object);
            (*vtable.add(2))(object)
        }
    }

    /// Reads a texture back, tightly packed (`bytes` per texel).
    fn read_back(gpu: &Headless, texture: &wgpu::Texture, bytes: u32) -> Vec<u8> {
        let (w, h) = (texture.width(), texture.height());
        let row = (w * bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            texture.size(),
        );
        gpu.queue.submit([encoder.finish()]);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let data = buffer.slice(..).get_mapped_range().expect("mapped");
        let mut out = Vec::with_capacity((w * h * bytes) as usize);
        for y in 0..h {
            out.extend_from_slice(&data[(y * row) as usize..][..(w * bytes) as usize]);
        }
        out
    }

    fn config(width: u32, height: u32) -> EncoderConfig {
        EncoderConfig {
            codec: Codec::H264,
            width,
            height,
            fps: 60,
            bitrate_bps: 20_000_000,
        }
    }

    /// The same stream decoded by D3D11VA (copied to memory, today's path)
    /// and by D3D12VA (left on the GPU, copied into our textures and read
    /// back) gives the same pictures. Frames are held up to 4 at a time
    /// before being brought in, and the size changes halfway, so FFmpeg's
    /// frames are reused while others are out.
    #[test]
    #[ignore = "needs a GPU with D3D12 video decoding and NVENC"]
    fn d3d12va_frames_match_d3d11va() {
        let gpu = headless();
        let mut d3d11 = FfmpegDecoder::new(true).expect("D3D11VA decoder");
        assert_eq!(d3d11.kind(), DecoderKind::D3d11);
        let raw_device = unsafe { gpu.device.as_hal::<Dx12>() }
            .unwrap()
            .raw_device()
            .as_raw();
        // A decoder gives back the device reference it was handed (FFmpeg
        // releases it only through the `free` callback we install). Checked
        // with nothing else on the device in between: its resources, wgpu's
        // included, each hold a reference too.
        let refs_before = com_refs(raw_device);
        let device = DecoderDevice::of(&gpu.device).expect("a D3D12 device");
        drop(FfmpegDecoder::new_d3d12(device.into_raw()).expect("D3D12VA decoder"));
        let refs_after = com_refs(raw_device);
        eprintln!("device references: {refs_before} before a decoder, {refs_after} after");
        assert_eq!(
            refs_after, refs_before,
            "the decoder leaked a device reference"
        );
        let device = DecoderDevice::of(&gpu.device).expect("a D3D12 device");
        let mut d3d12 = FfmpegDecoder::new_d3d12(device.into_raw()).expect("D3D12VA decoder");
        let mut importer = FrameImporter::new();
        let mut strips = BarcodeStrips::default();
        let clock = Arc::new(ProcessClock::new());
        let (mut compared, mut different, mut barcodes, mut most_held) = (0, 0usize, 0, 0);
        let mut held: VecDeque<(GpuNv12Frame, Nv12Frame)> = VecDeque::new();

        for (w, h) in [(1280, 720), (1920, 1080)] {
            let mut encoder = FfmpegEncoder::open("h264_nvenc", config(w, h)).expect("NVENC");
            let mut pattern = TestPattern::new(w, h, 60, clock.clone());
            let luma = plane_texture(&gpu.device, wgpu::TextureFormat::R8Unorm, w, h);
            let chroma = plane_texture(&gpu.device, wgpu::TextureFormat::Rg8Unorm, w / 2, h / 2);
            for i in 0..150 {
                let frame = pattern.render();
                let units = encoder
                    .encode(EncoderInput::Cpu(&frame), i == 0)
                    .expect("encode");
                for unit in units {
                    let cpu = match d3d11.decode(&unit.data, frame.captured_at).unwrap().pop() {
                        Some(DecodedFrame::CpuNv12(f)) => f,
                        _ => panic!("D3D11VA gave no frame in memory"),
                    };
                    let on_gpu = match d3d12.decode(&unit.data, frame.captured_at).unwrap().pop() {
                        Some(DecodedFrame::Gpu(f)) => f,
                        _ => panic!("D3D12VA gave no GPU frame"),
                    };
                    assert_eq!((on_gpu.width, on_gpu.height), (w, h));
                    held.push_back((on_gpu, cpu));
                    most_held = most_held.max(held.len() + importer.held());
                    if held.len() < 4 {
                        continue;
                    }
                    let (on_gpu, cpu) = held.pop_front().unwrap();
                    let mut enc = gpu.device.create_command_encoder(&Default::default());
                    importer
                        .copy(&gpu.device, &gpu.queue, &mut enc, on_gpu, &luma, &chroma)
                        .expect("bring the frame in");
                    let slot = strips
                        .record(&gpu.device, &mut enc, &luma)
                        .expect("a free strip slot");
                    gpu.queue.submit([enc.finish()]);
                    importer.submitted(&gpu.queue);
                    strips.submitted(slot);
                    strips.attach(slot, compared);
                    // (Waits for the GPU, so the strip is mapped too.)
                    let y = read_back(&gpu, &luma, 1);
                    let uv = read_back(&gpu, &chroma, 2);
                    importer.release_finished();
                    for strip in strips.take_ready() {
                        assert_eq!(strip.value, compared);
                        barcodes += u32::from(
                            barcode::read_luma(
                                &strip.luma,
                                strip.size.0 as usize,
                                strip.size,
                                BARCODE_X,
                                BARCODE_Y,
                                BARCODE_CELL,
                            )
                            .is_some(),
                        );
                    }
                    compared += 1;
                    let packed = |p: &[u8], stride: u32, row: u32, rows: u32| -> Vec<u8> {
                        (0..rows)
                            .flat_map(|r| p[(r * stride) as usize..][..row as usize].to_vec())
                            .collect()
                    };
                    different += y
                        .iter()
                        .zip(packed(&cpu.y, cpu.y_stride, w, h))
                        .filter(|(a, b)| **a != *b)
                        .count();
                    different += uv
                        .iter()
                        .zip(packed(&cpu.uv, cpu.uv_stride, w, h / 2))
                        .filter(|(a, b)| **a != *b)
                        .count();
                }
            }
            held.clear();
        }
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        importer.release_finished();
        drop(d3d12);
        // The debug layer's own record, read straight from the device (it is
        // active only if the device answers to ID3D12InfoQueue).
        {
            use windows::Win32::Graphics::Direct3D12::{
                D3D12_MESSAGE, D3D12_MESSAGE_SEVERITY_CORRUPTION, D3D12_MESSAGE_SEVERITY_ERROR,
                D3D12_MESSAGE_SEVERITY_WARNING, ID3D12InfoQueue,
            };
            let hal = unsafe { gpu.device.as_hal::<Dx12>() }.unwrap();
            match hal.raw_device().cast::<ID3D12InfoQueue>() {
                Err(e) => eprintln!("debug layer: not active ({e})"),
                Ok(queue) => unsafe {
                    let n = queue.GetNumStoredMessages();
                    eprintln!("debug layer: active, {n} messages stored");
                    for i in 0..n {
                        let mut len = 0;
                        let _ = queue.GetMessage(i, None, &mut len);
                        let mut buf = vec![0u8; len];
                        let msg = buf.as_mut_ptr().cast::<D3D12_MESSAGE>();
                        if queue.GetMessage(i, Some(msg), &mut len).is_ok() {
                            let m = &*msg;
                            if [
                                D3D12_MESSAGE_SEVERITY_CORRUPTION,
                                D3D12_MESSAGE_SEVERITY_ERROR,
                                D3D12_MESSAGE_SEVERITY_WARNING,
                            ]
                            .contains(&m.Severity)
                            {
                                let text = std::slice::from_raw_parts(
                                    m.pDescription,
                                    m.DescriptionByteLength.saturating_sub(1),
                                );
                                gpu.errors
                                    .lock()
                                    .unwrap()
                                    .push(String::from_utf8_lossy(text).into_owned());
                            }
                        }
                    }
                },
            }
        }
        let errors = gpu.errors.lock().unwrap().clone();
        eprintln!(
            "compared {compared} frames: {different} bytes differ; barcodes read {barcodes}; \
             most frames out at once {most_held}; still held {}; wgpu errors {errors:?}",
            importer.held()
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(different, 0, "D3D12VA's pictures differ from D3D11VA's");
        assert_eq!(barcodes, compared);
        assert_eq!(importer.held(), 0, "every frame went back to FFmpeg");
    }
}
