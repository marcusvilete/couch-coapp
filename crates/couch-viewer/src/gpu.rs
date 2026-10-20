//! wgpu setup and the NV12 video pass.
//!
//! Present mode prefers Mailbox (newest frame wins, no tearing), then
//! Immediate; frame latency is capped at one queued frame.
//!
//! With zero-copy decoding (M9c), wgpu runs on DX12 so the decoder can write
//! into its device; otherwise on wgpu's default backend (Vulkan on NVIDIA).

use anyhow::{Context, Result, anyhow, bail};
use couch_media::{Nv12Frame, YuvMatrix};

/// `COUCH_VIEWER_ZERO_COPY`: whether decoded frames stay on the GPU (M9c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeroCopy {
    /// `0`: decoded frames are copied through memory, as before M9c.
    Off,
    /// Unset: on for NVIDIA GPUs, the only ones it was tested on.
    Nvidia,
    /// `1`: on for any GPU, for trying it on AMD or Intel.
    Any,
}

impl ZeroCopy {
    pub fn from_env() -> Self {
        if !cfg!(windows) {
            return Self::Off;
        }
        match std::env::var("COUCH_VIEWER_ZERO_COPY").as_deref() {
            Ok("0") => Self::Off,
            Ok("1") => Self::Any,
            _ => Self::Nvidia,
        }
    }
}

const NVIDIA: u32 = 0x10de;

pub struct Gpu {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    pub adapter_name: String,
    /// The device takes decoded frames on the GPU (DX12, NV12 textures).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub zero_copy: bool,
}

impl Gpu {
    /// # Safety
    /// `window` must outlive the returned value (declare `Gpu` before the
    /// window in any struct holding both, so it drops first).
    pub unsafe fn new(window: &sdl3::video::Window) -> Result<Self> {
        let wanted = ZeroCopy::from_env();
        if wanted != ZeroCopy::Off {
            // SAFETY: as for this function.
            match unsafe { Self::open(window, Some(wanted)) } {
                Ok(gpu) => return Ok(gpu),
                Err(e) => tracing::info!("decoded frames go through memory: {e:#}"),
            }
        } else if cfg!(windows) {
            tracing::info!("decoded frames go through memory: COUCH_VIEWER_ZERO_COPY=0");
        }
        // SAFETY: as for this function.
        unsafe { Self::open(window, None) }
    }

    /// # Safety
    /// As for [`Self::new`].
    unsafe fn open(window: &sdl3::video::Window, zero_copy: Option<ZeroCopy>) -> Result<Self> {
        // WGPU_BACKEND (e.g. dx12, vulkan) picks the backend, for measuring.
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        // Validation stays off in debug builds too (wgpu turns it on there),
        // so the development app's latency is the installed app's: on DX12
        // the debug layer cost 0.6 ms or more per frame (M9c). WGPU_VALIDATION=1
        // turns it on.
        descriptor.flags = wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL.with_env();
        if zero_copy.is_some() {
            if !descriptor.backends.contains(wgpu::Backends::DX12) {
                bail!("WGPU_BACKEND leaves out DX12");
            }
            descriptor.backends = wgpu::Backends::DX12;
        }
        let instance = wgpu::Instance::new(descriptor);
        let target = unsafe { wgpu::SurfaceTargetUnsafe::from_display_and_window(window, window) }
            .map_err(|e| anyhow!("window handle: {e}"))?;
        let surface =
            unsafe { instance.create_surface_unsafe(target) }.context("creating surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no GPU adapter")?;
        let info = adapter.get_info();
        let mut required_features = wgpu::Features::empty();
        if let Some(mode) = zero_copy {
            if mode == ZeroCopy::Nvidia && info.vendor != NVIDIA {
                bail!(
                    "zero-copy decoding is on by default only for NVIDIA, not {} \
                     (COUCH_VIEWER_ZERO_COPY=1 tries it)",
                    info.name
                );
            }
            if !adapter
                .features()
                .contains(wgpu::Features::TEXTURE_FORMAT_NV12)
            {
                bail!("{} has no NV12 textures on DX12", info.name);
            }
            required_features = wgpu::Features::TEXTURE_FORMAT_NV12;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("couch-viewer"),
            required_features,
            ..Default::default()
        }))
        .context("creating GPU device")?;

        let (width, height) = window.size_in_pixels();
        let mut config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or_else(|| anyhow!("surface not supported by this adapter"))?;
        let caps = surface.get_capabilities(&adapter);
        // Our shader writes gamma-encoded RGB, so the target must not be sRGB.
        if let Some(format) = caps.formats.iter().copied().find(|f| !f.is_srgb()) {
            config.format = format;
        }
        config.present_mode = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
            .into_iter()
            .find(|m| caps.present_modes.contains(m))
            .unwrap_or(wgpu::PresentMode::Fifo);
        // Moonlight warns that a maximum frame latency of 1 makes
        // non-blocking presents wait on the compositor. Measured in M9a on
        // NVIDIA: 1, 2 and 3 give the same latency at 60 and 120 fps, so 1
        // stays. COUCH_VIEWER_FRAME_LATENCY (1 to 3) overrides it, for
        // measuring on other GPUs (Moonlight saw AMD behave differently).
        config.desired_maximum_frame_latency = std::env::var("COUCH_VIEWER_FRAME_LATENCY")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|v| (1..=3).contains(v))
            .unwrap_or(1);
        surface.configure(&device, &config);
        tracing::info!(
            "GPU {} ({:?}), present mode {:?} of {:?}, frame latency {}{}",
            info.name,
            info.backend,
            config.present_mode,
            caps.present_modes,
            config.desired_maximum_frame_latency,
            if zero_copy.is_some() {
                ", decoded frames stay on the GPU"
            } else {
                ""
            }
        );

        Ok(Self {
            surface,
            device,
            queue,
            config,
            adapter_name: format!("{} ({:?})", info.name, info.backend),
            zero_copy: zero_copy.is_some(),
        })
    }

    /// The device for a decoder whose frames stay on the GPU, or `None` when
    /// this device doesn't take them.
    #[cfg(windows)]
    pub fn decoder_device(&self) -> Option<crate::zero_copy::DecoderDevice> {
        if self.zero_copy {
            crate::zero_copy::DecoderDevice::of(&self.device)
        } else {
            None
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if (width, height) != (self.config.width, self.config.height) {
            self.reconfigure(width, height);
        }
    }

    /// Configures the surface again, as after it went out of date. A zero
    /// size (minimized) keeps the old configuration; restoring resizes it.
    pub fn reconfigure(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.config.present_mode
    }

    /// The surface's maximum frame latency, for the stats line.
    pub fn frame_latency(&self) -> u32 {
        self.config.desired_maximum_frame_latency
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var y_tex: texture_2d<f32>;
@group(0) @binding(1) var uv_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// One triangle covering the viewport.
@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

// Luma by Catmull-Rom (P8): the 4x4 texels around the point, weighted by the
// cubic that passes through them, sharper than bilinear. The middle two
// taps of each row and column are merged into one bilinear read at the
// point between them that gives both their weights, so 16 texels take 9
// reads (the well-known technique, as Matt Pettineo describes it in
// "Catmull-Rom filtering using 9 texture samples"). Its negative lobes may overshoot a little at hard edges; the
// colour is clamped at the end.
fn luma_bicubic(uv: vec2<f32>) -> f32 {
    let size = vec2<f32>(textureDimensions(y_tex));
    let at = uv * size;
    let t1 = floor(at - 0.5) + 0.5;
    let f = at - t1;
    let w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    let w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    let w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    let w3 = f * f * (-0.5 + 0.5 * f);
    let w12 = w1 + w2;
    let t0 = (t1 - 1.0) / size;
    let t12 = (t1 + w2 / w12) / size;
    let t3 = (t1 + 2.0) / size;
    var sum = 0.0;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t0.x, t0.y), 0.0).r * w0.x * w0.y;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t12.x, t0.y), 0.0).r * w12.x * w0.y;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t3.x, t0.y), 0.0).r * w3.x * w0.y;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t0.x, t12.y), 0.0).r * w0.x * w12.y;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t12.x, t12.y), 0.0).r * w12.x * w12.y;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t3.x, t12.y), 0.0).r * w3.x * w12.y;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t0.x, t3.y), 0.0).r * w0.x * w3.y;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t12.x, t3.y), 0.0).r * w12.x * w3.y;
    sum += textureSampleLevel(y_tex, samp, vec2<f32>(t3.x, t3.y), 0.0).r * w3.x * w3.y;
    return sum;
}

// Limited range: luma and chroma (Cb, Cr), centred. Chroma is at half the
// size and the eye resolves it less: bilinear in both scalers.
fn planes(uv: vec2<f32>, bicubic: bool) -> vec3<f32> {
    var luma: f32;
    if bicubic {
        luma = luma_bicubic(uv);
    } else {
        luma = textureSampleLevel(y_tex, samp, uv, 0.0).r;
    }
    let y = (luma - 0.0627451) * 1.1643836;
    let c = textureSampleLevel(uv_tex, samp, uv, 0.0).rg - vec2<f32>(0.5019608, 0.5019608);
    return vec3<f32>(y, c);
}

// BT.709 (what our converter writes for the encoders other than NVENC).
fn bt709(p: vec3<f32>) -> vec4<f32> {
    let rgb = vec3<f32>(
        p.x + 1.7927411 * p.z,
        p.x - 0.2132486 * p.y - 0.5329093 * p.z,
        p.x + 2.1124018 * p.y,
    );
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

// BT.601 (NVENC's conversion of RGB, M12c).
fn bt601(p: vec3<f32>) -> vec4<f32> {
    let rgb = vec3<f32>(
        p.x + 1.5960268 * p.z,
        p.x - 0.3917623 * p.y - 0.8129676 * p.z,
        p.x + 2.0172321 * p.y,
    );
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}

@fragment
fn fs_bt709(in: VsOut) -> @location(0) vec4<f32> {
    return bt709(planes(in.uv, false));
}

@fragment
fn fs_bt601(in: VsOut) -> @location(0) vec4<f32> {
    return bt601(planes(in.uv, false));
}

@fragment
fn fs_bt709_bicubic(in: VsOut) -> @location(0) vec4<f32> {
    return bt709(planes(in.uv, true));
}

@fragment
fn fs_bt601_bicubic(in: VsOut) -> @location(0) vec4<f32> {
    return bt601(planes(in.uv, true));
}
"#;

/// How the stream window scales the picture to its size (P8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scaler {
    /// One bilinear read per pixel, as before P8.
    Bilinear,
    /// Catmull-Rom on luma: sharper, 9 reads per pixel, no measurable cost.
    /// The default: it looked best of the three tried (P8; AMD's FSR 1 was
    /// tried too, looked no better, and was dropped).
    Bicubic,
}

impl Scaler {
    /// `COUCH_VIEWER_SCALER` (`bilinear`, `bicubic`), for comparing; the
    /// default otherwise.
    pub fn from_env() -> Self {
        match std::env::var("COUCH_VIEWER_SCALER").as_deref() {
            Ok(v) if v.eq_ignore_ascii_case("bilinear") => Self::Bilinear,
            Ok(v) if v.eq_ignore_ascii_case("bicubic") => Self::Bicubic,
            Ok(other) => {
                tracing::warn!("COUCH_VIEWER_SCALER={other:?} isn't a scaler; bicubic");
                Self::Bicubic
            }
            Err(_) => Self::Bicubic,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::Bicubic => "bicubic",
        }
    }
}

fn render_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    fragment: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(fragment),
            targets: &[Some(format.into())],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

struct Planes {
    size: (u32, u32),
    y: wgpu::Texture,
    uv: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

pub struct VideoPass {
    bt709: wgpu::RenderPipeline,
    bt601: wgpu::RenderPipeline,
    bt709_bicubic: wgpu::RenderPipeline,
    bt601_bicubic: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    planes: Option<Planes>,
    /// The matrix of the frame in the planes, as its stream declares.
    pub matrix: YuvMatrix,
    pub scaler: Scaler,
}

impl VideoPass {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("nv12"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("nv12"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("nv12"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let pipeline = |fragment| {
            render_pipeline(
                device,
                fragment,
                &pipeline_layout,
                &shader,
                fragment,
                format,
            )
        };
        let (bt709, bt601) = (pipeline("fs_bt709"), pipeline("fs_bt601"));
        let (bt709_bicubic, bt601_bicubic) =
            (pipeline("fs_bt709_bicubic"), pipeline("fs_bt601_bicubic"));
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nv12"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            bt709,
            bt601,
            bt709_bicubic,
            bt601_bicubic,
            layout,
            sampler,
            planes: None,
            matrix: YuvMatrix::default(),
            scaler: Scaler::from_env(),
        }
    }

    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &Nv12Frame) {
        self.matrix = frame.matrix;
        let size = (frame.width, frame.height);
        let (y, uv) = self.planes(device, size);
        let chroma = (frame.width.div_ceil(2), frame.height.div_ceil(2));
        write_plane(queue, y, &frame.y, frame.y_stride, size);
        write_plane(queue, uv, &frame.uv, frame.uv_stride, chroma);
    }

    /// The luma (R8) and chroma (RG8) textures the pass draws, made for
    /// frames of `size`, for a frame on the GPU to be copied into.
    pub fn planes(
        &mut self,
        device: &wgpu::Device,
        size: (u32, u32),
    ) -> (&wgpu::Texture, &wgpu::Texture) {
        if self.planes.as_ref().map(|p| p.size) != Some(size) {
            self.planes = Some(self.create_planes(device, size));
        }
        let planes = self.planes.as_ref().expect("planes created above");
        (&planes.y, &planes.uv)
    }

    fn create_planes(&self, device: &wgpu::Device, (w, h): (u32, u32)) -> Planes {
        let texture = |label, format, width, height| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                // Copied into (uploads, or a decoded frame's planes), and
                // read from for the latency barcode.
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let y = texture("luma", wgpu::TextureFormat::R8Unorm, w, h);
        let uv = texture(
            "chroma",
            wgpu::TextureFormat::Rg8Unorm,
            w.div_ceil(2),
            h.div_ceil(2),
        );
        let y_view = y.create_view(&Default::default());
        let uv_view = uv.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("nv12"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        Planes {
            size: (w, h),
            y,
            uv,
            bind_group,
        }
    }

    /// Draws the last uploaded frame into `viewport` (x, y, width, height).
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, viewport: [f32; 4]) {
        let Some(planes) = &self.planes else {
            return;
        };
        let [x, y, w, h] = viewport;
        pass.set_viewport(x, y, w, h, 0.0, 1.0);
        pass.set_pipeline(match (self.matrix, self.scaler) {
            (YuvMatrix::Bt601, Scaler::Bilinear) => &self.bt601,
            (YuvMatrix::Bt709, Scaler::Bilinear) => &self.bt709,
            (YuvMatrix::Bt601, Scaler::Bicubic) => &self.bt601_bicubic,
            (YuvMatrix::Bt709, Scaler::Bicubic) => &self.bt709_bicubic,
        });
        pass.set_bind_group(0, &planes.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn write_plane(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    data: &[u8],
    stride: u32,
    (w, h): (u32, u32),
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
}

/// The largest rectangle with the frame's aspect ratio that fits the target.
pub fn letterbox((fw, fh): (u32, u32), (tw, th): (u32, u32)) -> [f32; 4] {
    let scale = (tw as f32 / fw as f32).min(th as f32 / fh as f32);
    let (w, h) = (fw as f32 * scale, fh as f32 * scale);
    [(tw as f32 - w) / 2.0, (th as f32 - h) / 2.0, w, h]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses and validates a shader as wgpu would when the window opens,
    /// so a mistake fails here rather than on someone's screen.
    fn validate(source: &str) -> naga::Module {
        let module = naga::front::wgsl::parse_str(source).unwrap_or_else(|e| {
            panic!("{}", e.emit_to_string(source));
        });
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}"));
        module
    }

    /// P8: the video shader has every entry point a pipeline asks for.
    #[test]
    fn the_video_shader_is_valid_and_has_each_scaler() {
        let module = validate(SHADER);
        let entries: Vec<&str> = module
            .entry_points
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        for name in [
            "vs",
            "fs_bt709",
            "fs_bt601",
            "fs_bt709_bicubic",
            "fs_bt601_bicubic",
        ] {
            assert!(entries.contains(&name), "{name} missing: {entries:?}");
        }
    }
}
