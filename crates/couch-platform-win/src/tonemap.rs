//! HDR frames to SDR on the GPU (M29). A monitor in HDR is captured in FP16
//! scRGB (`wgc::Format::Fp16`), and each frame is drawn into an 8-bit BGRA
//! texture by a pixel shader: divided by the monitor's SDR white, clipped,
//! sRGB-encoded. It takes the place of the copy into the texture pool
//! ([`crate::gpu_frames::TexturePool::convert`]), so everything after the
//! pool (NVENC, the read-back, the other encoders) sees BGRA as before.
//!
//! The maths is `couch_media::hdr::scrgb_to_srgb`'s, which the tests hold
//! this shader to; keep the two alike.

use std::sync::OnceLock;

use couch_media::MediaError;
use windows::{
    Win32::Graphics::{
        Direct3D::{
            D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile},
            ID3DBlob, ID3DInclude,
        },
        Direct3D11::{
            D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_BUFFER_DESC,
            D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VIEWPORT, ID3D11Buffer, ID3D11Device,
            ID3D11DeviceContext, ID3D11Multithread, ID3D11PixelShader, ID3D11RenderTargetView,
            ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
        },
    },
    core::{Interface, PCSTR, s},
};

/// One full-screen triangle; each pixel reads its own pixel of the frame
/// (no scaling: the target is the frame's top-left).
const SHADER: &str = r"
cbuffer Params : register(b0) { float inv_white; float3 unused; };
Texture2D<float4> frame : register(t0);

float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}

float4 ps(float4 pos : SV_Position) : SV_Target {
    float3 v = saturate(frame.Load(int3(pos.xy, 0)).rgb * inv_white);
    float3 low = v * 12.92;
    float3 high = 1.055 * pow(v, 1.0 / 2.4) - 0.055;
    return float4(v <= 0.0031308 ? low : high, 1);
}
";

/// The shaders' bytecode, compiled once per process.
struct Bytecode {
    vs: Vec<u8>,
    ps: Vec<u8>,
}

fn bytecode() -> Result<&'static Bytecode, MediaError> {
    static COMPILED: OnceLock<Result<Bytecode, String>> = OnceLock::new();
    COMPILED
        .get_or_init(|| {
            Ok(Bytecode {
                vs: compile(s!("vs"), s!("vs_4_0"))?,
                ps: compile(s!("ps"), s!("ps_4_0"))?,
            })
        })
        .as_ref()
        .map_err(|e| MediaError::Backend(e.clone()))
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, String> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source, entry and target outlive the call; the blobs are
    // out-parameters.
    let compiled = unsafe {
        D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            s!("tonemap"),
            None,
            None::<&ID3DInclude>,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    let text = |blob: &ID3DBlob| String::from_utf8_lossy(blob_bytes(blob)).into_owned();
    match (compiled, code) {
        (Ok(()), Some(code)) => Ok(blob_bytes(&code).to_vec()),
        (result, _) => Err(format!(
            "the HDR shader didn't compile: {}",
            errors
                .as_ref()
                .map(text)
                .unwrap_or_else(|| format!("{result:?}"))
        )),
    }
}

fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: a blob's pointer and size describe its own memory, live while
    // the blob is.
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()) }
}

/// How many frame textures' views are kept: Windows' frame pool has two
/// buffers, remade on a resize.
const SOURCES: usize = 4;
/// And targets': a texture pool's (at most 12) or the converted frame of a
/// capture to memory.
const TARGETS: usize = 16;

/// Draws FP16 scRGB frames into BGRA textures on one device.
pub struct Converter {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    multithread: ID3D11Multithread,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    params: ID3D11Buffer,
    /// What `params` holds now.
    white: f32,
    /// Views of textures, each holding its texture: a texture can't go, nor
    /// its address be reused, while it is here.
    sources: Vec<(ID3D11Texture2D, ID3D11ShaderResourceView)>,
    targets: Vec<(ID3D11Texture2D, ID3D11RenderTargetView)>,
    /// A copy of the frame, for frame textures a shader can't read.
    readable: Option<ID3D11Texture2D>,
}

// SAFETY: D3D11 devices are free-threaded, and the immediate context is used
// only under the device's lock (multithread protection, turned on below).
// A converter is used from one thread at a time.
unsafe impl Send for Converter {}

impl Converter {
    pub fn new(device: &ID3D11Device) -> Result<Self, MediaError> {
        let code = bytecode()?;
        let multithread: ID3D11Multithread = device.cast().map_err(backend)?;
        // SAFETY: plain calls on a live device; the bytecode and description
        // outlive them, and the out-parameters are live locals.
        unsafe {
            // The capture's device is shared with encoder threads.
            let _ = multithread.SetMultithreadProtected(true);
            let context = device.GetImmediateContext().map_err(backend)?;
            let mut vs = None;
            device
                .CreateVertexShader(&code.vs, None, Some(&mut vs))
                .map_err(backend)?;
            let mut ps = None;
            device
                .CreatePixelShader(&code.ps, None, Some(&mut ps))
                .map_err(backend)?;
            let mut params = None;
            device
                .CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: 16,
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut params),
                )
                .map_err(backend)?;
            let missing = || MediaError::Backend("D3D11 made nothing".into());
            let mut this = Self {
                device: device.clone(),
                context,
                multithread,
                vs: vs.ok_or_else(missing)?,
                ps: ps.ok_or_else(missing)?,
                params: params.ok_or_else(missing)?,
                white: f32::NAN,
                sources: Vec::new(),
                targets: Vec::new(),
                readable: None,
            };
            this.set_white(1.0);
            Ok(this)
        }
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Lets go of the frame textures' views, and so of the textures: frames
    /// aren't HDR any more (the monitor left HDR), and Windows' FP16 pool
    /// would otherwise stay alive with them.
    pub fn forget_sources(&mut self) {
        self.sources.clear();
        self.readable = None;
    }

    /// Lets go of `texture`'s view: it is being replaced.
    pub fn forget(&mut self, texture: &ID3D11Texture2D) {
        self.targets.retain(|(t, _)| t != texture);
    }

    fn set_white(&mut self, white: f32) {
        if white == self.white {
            return;
        }
        self.white = white;
        let data = [1.0 / white, 0.0, 0.0, 0.0];
        // SAFETY: 16 bytes into a 16-byte constant buffer of this device.
        // A multithread-protected context takes its lock for the call; in
        // `convert` its caller holds it already.
        unsafe {
            self.context
                .UpdateSubresource(&self.params, 0, None, data.as_ptr().cast(), 0, 0);
        }
    }

    /// Draws the top-left `width` x `height` of `from` (FP16 scRGB) into the
    /// top-left of `to` (8-bit BGRA, unorm), as SDR whose white is `white`
    /// (scRGB). Queued on the GPU, not waited for; the caller flushes.
    pub fn convert(
        &mut self,
        from: &ID3D11Texture2D,
        to: &ID3D11Texture2D,
        width: u32,
        height: u32,
        white: f32,
    ) -> Result<(), MediaError> {
        // SAFETY: paired with the Leave below; nothing between returns early
        // (the body is a closure whose result is returned after).
        unsafe { self.multithread.Enter() };
        let result = (|| {
            let source = self.source(from, width, height)?;
            let target = self.target(to)?;
            self.set_white(white);
            let viewport = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: width as f32,
                Height: height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            let c = &self.context;
            // SAFETY: every object is live and of this device. Nothing else
            // draws on this context (encoders only copy and encode), but
            // every stage the draw passes through is set anyway, and the
            // views are unbound after, so `to` is free for them.
            unsafe {
                c.IASetInputLayout(None);
                c.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                c.VSSetShader(&self.vs, None);
                c.HSSetShader(None, None);
                c.DSSetShader(None, None);
                c.GSSetShader(None, None);
                c.PSSetShader(&self.ps, None);
                c.OMSetDepthStencilState(None, 0);
                c.PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
                c.PSSetShaderResources(0, Some(&[Some(source)]));
                c.OMSetRenderTargets(Some(&[Some(target)]), None);
                c.OMSetBlendState(None, None, u32::MAX);
                c.RSSetState(None);
                c.RSSetViewports(Some(&[viewport]));
                c.Draw(3, 0);
                c.PSSetShaderResources(0, Some(&[None]));
                c.OMSetRenderTargets(None, None);
            }
            Ok(())
        })();
        // SAFETY: entered above.
        unsafe { self.multithread.Leave() };
        result
    }

    /// A view a shader reads `from` through: its own, or one of a copy of
    /// it when Windows made it without that binding.
    fn source(
        &mut self,
        from: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<ID3D11ShaderResourceView, MediaError> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: fills a local description of a live texture.
        unsafe { from.GetDesc(&mut desc) };
        let from = if desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0 {
            from.clone()
        } else {
            self.copy_readable(from, &desc, width, height)?
        };
        if let Some((_, view)) = self.sources.iter().find(|(t, _)| *t == from) {
            return Ok(view.clone());
        }
        // A new frame texture: Windows' pool was remade (a resize), so the
        // views of textures of another size let theirs go (FP16 at 4K is
        // 66 MB each).
        let size = |t: &ID3D11Texture2D| {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: fills a local description of a live texture.
            unsafe { t.GetDesc(&mut d) };
            (d.Width, d.Height)
        };
        let new_size = size(&from);
        self.sources.retain(|(t, _)| size(t) == new_size);
        let mut view = None;
        // SAFETY: a live texture of this device; the view is an
        // out-parameter.
        unsafe {
            self.device
                .CreateShaderResourceView(&from, None, Some(&mut view))
                .map_err(backend)?;
        }
        let view = view.ok_or_else(|| MediaError::Backend("no shader resource view".into()))?;
        if self.sources.len() == SOURCES {
            self.sources.remove(0);
        }
        self.sources.push((from, view.clone()));
        Ok(view)
    }

    fn copy_readable(
        &mut self,
        from: &ID3D11Texture2D,
        desc: &D3D11_TEXTURE2D_DESC,
        width: u32,
        height: u32,
    ) -> Result<ID3D11Texture2D, MediaError> {
        let fits = self.readable.as_ref().is_some_and(|t| {
            let mut have = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: as above.
            unsafe { t.GetDesc(&mut have) };
            (have.Width, have.Height, have.Format) == (width, height, desc.Format)
        });
        if !fits {
            let mut made = None;
            let copy = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                ..*desc
            };
            // SAFETY: the description outlives the call.
            unsafe {
                self.device
                    .CreateTexture2D(&copy, None, Some(&mut made))
                    .map_err(backend)?;
            }
            self.readable = made;
            tracing::info!("the HDR frames can't be read by a shader: copied first");
        }
        let readable = self
            .readable
            .clone()
            .ok_or_else(|| MediaError::Backend("no texture for the copy".into()))?;
        let region = D3D11_BOX {
            right: width,
            bottom: height,
            back: 1,
            ..Default::default()
        };
        // SAFETY: the same format, the region within both, under the lock.
        unsafe {
            self.context
                .CopySubresourceRegion(&readable, 0, 0, 0, 0, from, 0, Some(&region));
        }
        Ok(readable)
    }

    fn target(&mut self, to: &ID3D11Texture2D) -> Result<ID3D11RenderTargetView, MediaError> {
        if let Some((_, view)) = self.targets.iter().find(|(t, _)| t == to) {
            return Ok(view.clone());
        }
        let mut view = None;
        // SAFETY: a live texture of this device, made with the render
        // target binding; the view is an out-parameter.
        unsafe {
            self.device
                .CreateRenderTargetView(to, None, Some(&mut view))
                .map_err(backend)?;
        }
        let view = view.ok_or_else(|| MediaError::Backend("no render target view".into()))?;
        if self.targets.len() == TARGETS {
            self.targets.remove(0);
        }
        self.targets.push((to.clone(), view.clone()));
        Ok(view)
    }
}

/// The GPU's time, in milliseconds, for copying one `width` x `height` BGRA
/// frame (the pool's copy) and for converting one FP16 frame (this module's
/// draw), each the median of `rounds`; on a device like the capture's (M29:
/// `couch-cli hdr-probe --bench`).
pub fn bench(width: u32, height: u32, rounds: usize) -> Result<(f64, f64), MediaError> {
    let device = crate::wgc::new_device().map_err(backend)?;
    let made = |format, bind: u32| -> Result<ID3D11Texture2D, MediaError> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut t = None;
        // SAFETY: the description outlives the call.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }.map_err(backend)?;
        t.ok_or_else(|| MediaError::Backend("no texture".into()))
    };
    use windows::Win32::Graphics::{
        Direct3D11::D3D11_BIND_RENDER_TARGET,
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT},
    };
    let both = (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32;
    let hdr = made(DXGI_FORMAT_R16G16B16A16_FLOAT, both)?;
    let sdr = made(DXGI_FORMAT_B8G8R8A8_UNORM, both)?;
    let to = made(DXGI_FORMAT_B8G8R8A8_UNORM, both)?;
    let mut converter = Converter::new(&device)?;
    let context = converter.context.clone();
    let region = D3D11_BOX {
        right: width,
        bottom: height,
        back: 1,
        ..Default::default()
    };
    let (mut copies, mut draws) = (Vec::new(), Vec::new());
    for _ in 0..rounds {
        copies.push(gpu_time(&device, &context, &to, || {
            // SAFETY: the same format and size, the region within both.
            unsafe { context.CopySubresourceRegion(&to, 0, 0, 0, 0, &sdr, 0, Some(&region)) };
            Ok(())
        })?);
        draws.push(gpu_time(&device, &context, &to, || {
            converter.convert(&hdr, &to, width, height, 3.0)
        })?);
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    Ok((median(&mut copies), median(&mut draws)))
}

/// The GPU's time for what `work` queues, in milliseconds: timestamps around
/// it, waited for by reading a pixel of `to` back.
fn gpu_time(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    to: &ID3D11Texture2D,
    work: impl FnOnce() -> Result<(), MediaError>,
) -> Result<f64, MediaError> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_QUERY_DATA_TIMESTAMP_DISJOINT,
        D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP, D3D11_QUERY_TIMESTAMP_DISJOINT, ID3D11Query,
    };
    let query = |kind| -> Result<ID3D11Query, MediaError> {
        let mut q = None;
        // SAFETY: a local description; the query is an out-parameter.
        unsafe {
            device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: kind,
                    MiscFlags: 0,
                },
                Some(&mut q),
            )
        }
        .map_err(backend)?;
        q.ok_or_else(|| MediaError::Backend("no query".into()))
    };
    let (disjoint, start, end) = (
        query(D3D11_QUERY_TIMESTAMP_DISJOINT)?,
        query(D3D11_QUERY_TIMESTAMP)?,
        query(D3D11_QUERY_TIMESTAMP)?,
    );
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: fills a local description of a live texture.
    unsafe { to.GetDesc(&mut desc) };
    let staging = crate::gpu_frames::staging_texture(
        device,
        &D3D11_TEXTURE2D_DESC {
            Width: 1,
            Height: 1,
            ..desc
        },
    )?;
    // SAFETY: live queries and textures of this device, used on this thread
    // alone; the Map waits for everything queued before it, the queries
    // included, so their data is there after.
    unsafe {
        context.Begin(&disjoint);
        context.End(&start);
        work()?;
        context.End(&end);
        context.End(&disjoint);
        let one = D3D11_BOX {
            right: 1,
            bottom: 1,
            back: 1,
            ..Default::default()
        };
        context.CopySubresourceRegion(&staging, 0, 0, 0, 0, to, 0, Some(&one));
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(backend)?;
        context.Unmap(&staging, 0);
        let mut clock = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
        let (mut t0, mut t1) = (0u64, 0u64);
        context
            .GetData(
                &disjoint,
                Some((&raw mut clock).cast()),
                size_of_val(&clock) as u32,
                0,
            )
            .map_err(backend)?;
        context
            .GetData(&start, Some((&raw mut t0).cast()), 8, 0)
            .map_err(backend)?;
        context
            .GetData(&end, Some((&raw mut t1).cast()), 8, 0)
            .map_err(backend)?;
        if clock.Disjoint.as_bool() || clock.Frequency == 0 {
            return Err(MediaError::Backend("the GPU's clock jumped".into()));
        }
        Ok(t1.saturating_sub(t0) as f64 * 1000.0 / clock.Frequency as f64)
    }
}

fn backend(e: impl std::fmt::Display) -> MediaError {
    MediaError::Backend(e.to_string())
}

#[cfg(test)]
mod tests {
    use couch_media::hdr::scrgb_to_srgb;
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
                D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
                D3D11CreateDevice,
            },
            Dxgi::Common::{
                DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
                DXGI_SAMPLE_DESC,
            },
        },
    };

    use super::*;
    use crate::{gpu_frames::staging_texture, hdr::f32_to_half};

    /// Windows' software adapter: every Windows has it, no GPU needed.
    fn warp() -> ID3D11Device {
        let mut device = None;
        // SAFETY: out-parameters are live locals.
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .unwrap();
        }
        device.unwrap()
    }

    fn texture(
        device: &ID3D11Device,
        format: DXGI_FORMAT,
        (width, height): (u32, u32),
        bind: u32,
        initial: Option<(&[u16], u32)>,
    ) -> ID3D11Texture2D {
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
            BindFlags: bind,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = initial.map(|(pixels, pitch)| D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: pitch,
            SysMemSlicePitch: 0,
        });
        let mut made = None;
        // SAFETY: the description and data outlive the call.
        unsafe {
            device
                .CreateTexture2D(
                    &desc,
                    data.as_ref().map(std::ptr::from_ref),
                    Some(&mut made),
                )
                .unwrap();
        }
        made.unwrap()
    }

    /// BGRA pixels of `t`, as [r, g, b] over 255.
    fn read(device: &ID3D11Device, t: &ID3D11Texture2D, (w, h): (u32, u32)) -> Vec<[f32; 3]> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a live texture; the mapping is read, then unmapped.
        unsafe {
            t.GetDesc(&mut desc);
            let staging = staging_texture(device, &desc).unwrap();
            let context = device.GetImmediateContext().unwrap();
            context.CopyResource(&staging, t);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .unwrap();
            let bytes = std::slice::from_raw_parts(
                mapped.pData.cast::<u8>(),
                (mapped.RowPitch * h) as usize,
            );
            let out = (0..h)
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let p = &bytes[(y * mapped.RowPitch + x * 4) as usize..][..4];
                    [p[2], p[1], p[0]].map(|v| f32::from(v) / 255.0)
                })
                .collect();
            context.Unmap(&staging, 0);
            out
        }
    }

    /// Every value the measuring pattern shows, and the edges of the
    /// curve, drawn on the GPU as the reference computes them.
    #[test]
    fn the_shader_converts_as_the_reference_does() {
        let device = warp();
        let mut values: Vec<[f32; 3]> = crate::hdr::patches(640, 360, 3.0)
            .into_iter()
            .map(|p| p.scrgb)
            .collect();
        values.extend([
            [0.0; 3],
            [0.0001, 0.003, 0.004],
            [1.562; 3],
            [f32::NAN, f32::INFINITY, -1.0],
        ]);
        // One value per pixel in a 8-wide picture, then a column beyond the
        // converted area that must stay untouched.
        let (w, h) = (8u32, values.len().div_ceil(8) as u32);
        let mut pixels = vec![0u16; (w * h * 4) as usize];
        for (i, v) in values.iter().enumerate() {
            for c in 0..3 {
                pixels[i * 4 + c] = f32_to_half(v[c]);
            }
            pixels[i * 4 + 3] = f32_to_half(1.0);
        }
        let from = texture(
            &device,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            (w, h),
            D3D11_BIND_SHADER_RESOURCE.0 as u32,
            Some((&pixels, w * 8)),
        );
        let to = texture(
            &device,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            (w + 1, h),
            (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            None,
        );
        let mut converter = Converter::new(&device).unwrap();
        converter.convert(&from, &to, w, h, 3.0).unwrap();
        let out = read(&device, &to, (w + 1, h));
        for (i, v) in values.iter().enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            let got = out[(y * (w + 1) + x) as usize];
            // The reference on the value FP16 holds, not the f32 asked for.
            let held = v.map(|c| crate::hdr::half_to_f32(f32_to_half(c)));
            let want = scrgb_to_srgb(held, 3.0);
            for c in 0..3 {
                assert!(
                    (got[c] - want[c]).abs() <= 0.6 / 255.0,
                    "{v:?}: got {got:?}, want {want:?}"
                );
            }
        }
        for y in 0..h {
            assert_eq!(
                out[(y * (w + 1) + w) as usize],
                [0.0; 3],
                "outside, row {y}"
            );
        }

        // A frame Windows made without the shader binding is copied first.
        let unreadable = texture(
            &device,
            DXGI_FORMAT_R16G16B16A16_FLOAT,
            (w, h),
            D3D11_BIND_RENDER_TARGET.0 as u32,
            Some((&pixels, w * 8)),
        );
        converter.convert(&unreadable, &to, w, h, 1.0).unwrap();
        let out = read(&device, &to, (w + 1, h));
        let grey_80_nits = out[1];
        assert!(
            (grey_80_nits[0] - 1.0).abs() < 0.01,
            "{grey_80_nits:?} at white 1.0"
        );
    }
}
