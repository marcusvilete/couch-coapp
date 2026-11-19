//! One NVENC session on the capture's D3D11 device, reading its BGRA textures.
//!
//! Its stream is what FFmpeg 8.1's `h264_nvenc` and `hevc_nvenc` make for our
//! options (`couch_codec_ffmpeg::options`: p1, ultra-low-latency tuning, CBR,
//! `zerolatency`, `delay 0`, no lookahead, forced IDR, an infinite GOP, no
//! B-frames, a one-frame VBV), field by field as `libavcodec/nvenc.c` sets
//! them, so viewers see no difference; but a bitrate change reconfigures the
//! session in place, without the reset and keyframe FFmpeg asks for.

use std::{any::Any, collections::HashMap, ffi::c_void, ptr, sync::Arc, time::Instant};

use bytes::Bytes;
use couch_media::{
    Codec, EncodedFrame, EncoderCaps, EncoderConfig, EncoderInput, GpuFrame, LongTerm, MediaError,
    VideoEncoder,
};

use crate::{
    api::{self, Api, check, check_on},
    ffi::*,
};

/// Long-term reference frames, for continuing a stream after frames the
/// viewer never got (M12d-2, ADR 0016). Off by default: the stream is then
/// FFmpeg's exactly. On, the parameter sets change (the DPB, LTR's flags).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Continuation {
    /// Long-term frames NVENC keeps (`ltrNumFrames`); 0 leaves LTR off.
    pub ltr_frames: u32,
    /// The references a frame may predict from (`numRefL0`, 1 to 7); 0 is
    /// the driver's choice.
    pub refs_l0: u32,
}

/// What one frame asks of NVENC beyond its picture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameControl {
    pub keyframe: bool,
    /// Mark this frame long-term, in this slot.
    pub mark_ltr: Option<u32>,
}

/// What NVENC says it did with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameReport {
    /// The timestamp it was encoded under, which [`NvencEncoder::invalidate`]
    /// names it by.
    pub timestamp: u64,
    /// `NV_ENC_PIC_TYPE_*`: 0 P, 2 I, 3 IDR, 7 a P no frame refers to.
    pub picture_type: u32,
    /// Marked long-term, in this slot.
    pub ltr_marked: Option<u32>,
    /// The long-term slots it predicted from.
    pub ltr_used: u32,
}

/// What this GPU offers for continuing ([`Continuation`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContinuationCaps {
    pub invalidation: bool,
    pub max_ltr_frames: i32,
}

/// An NVENC session reading one texture pool's frames.
pub struct NvencEncoder {
    api: &'static Api,
    session: *mut c_void,
    /// The capture's `ID3D11Device`, one reference ours, released after the
    /// session is destroyed.
    device: *mut c_void,
    output: NV_ENC_OUTPUT_PTR,
    /// Each pool texture NVENC has registered, by its address. Only the pool
    /// `source` names is ever registered, and `_textures` keeps it alive.
    registered: HashMap<usize, NV_ENC_REGISTERED_PTR>,
    /// The parameters it was initialized with, `encodeConfig` pointing into
    /// `encode_config`: both boxed, so the pointer stays good.
    init: Box<NV_ENC_INITIALIZE_PARAMS>,
    encode_config: Box<NV_ENC_CONFIG>,
    pending_bitrate: Option<u32>,
    caps: EncoderCaps,
    config: EncoderConfig,
    source: u64,
    last_pts: u64,
    codec_guid: GUID,
    /// Opened with long-term frames ([`Continuation`]): it continues.
    long_term: bool,
    /// Every texture of the pool, kept alive until the session is gone
    /// (dropped after `drop` destroyed it).
    _textures: Arc<dyn Any + Send + Sync>,
}

// SAFETY: the session and the device are used only from the thread that owns
// the encoder; NVENC's D3D11 sessions aren't tied to the thread that made them.
unsafe impl Send for NvencEncoder {}

/// The name it goes by in logs and the stats line.
pub(crate) fn name(codec: Codec) -> &'static str {
    match codec {
        Codec::Hevc => "hevc_nvenc_direct",
        _ => "h264_nvenc_direct",
    }
}

impl NvencEncoder {
    /// A session on `frame`'s device, for frames from its pool at its size.
    /// Fails where NVENC can't open there (no NVIDIA driver, too old a one,
    /// another vendor's GPU, the session limit), so the caller can open
    /// FFmpeg's encoder instead.
    pub fn open_gpu(config: EncoderConfig, frame: &GpuFrame) -> Result<Self, MediaError> {
        Self::open_continuing(config, frame, Continuation::default())
    }

    /// [`open_gpu`](Self::open_gpu) with long-term frames as `continuation`
    /// asks.
    pub fn open_continuing(
        config: EncoderConfig,
        frame: &GpuFrame,
        mut continuation: Continuation,
    ) -> Result<Self, MediaError> {
        if (frame.width, frame.height) != (config.width, config.height) {
            return Err(MediaError::Backend(format!(
                "a {}x{} frame for a {}x{} encoder",
                frame.width, frame.height, config.width, config.height
            )));
        }
        let codec_guid = match config.codec {
            Codec::H264 => api::CODEC_H264,
            Codec::Hevc => api::CODEC_HEVC,
            other => return Err(MediaError::Unsupported(format!("{other:?} in NVENC"))),
        };
        let api = api::api()?;
        let fns = &api.fns;
        let device = frame.texture().d3d11_device_owned();
        if device.is_null() {
            return Err(MediaError::Backend("the frame has no D3D11 device".into()));
        }
        // From here on, `encoder`'s drop releases the device and whatever
        // else was made, whichever step fails.
        let mut encoder = Self {
            api,
            session: ptr::null_mut(),
            device,
            output: ptr::null_mut(),
            registered: HashMap::new(),
            init: Box::default(),
            encode_config: Box::default(),
            pending_bitrate: None,
            caps: EncoderCaps {
                name: name(config.codec).to_string(),
                options: "p1+ull".into(),
                hardware: true,
                gpu_input: true,
                // It opens only where the bitrate changes in place (below).
                bitrate_change_costs_keyframe: false,
            },
            config,
            source: frame.source,
            last_pts: 0,
            codec_guid,
            long_term: continuation.ltr_frames > 0,
            _textures: frame.texture().keep_alive(),
        };

        let mut open = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
            version: api::OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            deviceType: NV_ENC_DEVICE_TYPE_DIRECTX,
            device,
            apiVersion: NVENCAPI_VERSION,
            ..Default::default()
        };
        let open_session = fns.nvEncOpenEncodeSessionEx.ok_or_else(missing)?;
        // SAFETY: the parameters and the out pointer live across the call; the
        // device is a D3D11 device we hold a reference to. A session handed
        // back even on failure is destroyed by `drop`, as the API asks.
        let status = unsafe { open_session(&mut open, &mut encoder.session) };
        check(status, "NvEncOpenEncodeSessionEx")?;

        // Our reason to exist. Without it FFmpeg's encoder never changes the
        // bitrate at all (its `reconfig_encoder` skips it): take that, as
        // before M12d, rather than a path of our own nobody has tried.
        if encoder.cap(codec_guid, NV_ENC_CAPS_SUPPORT_DYN_BITRATE_CHANGE)? == 0 {
            return Err(MediaError::Unsupported(
                "this GPU can't change NVENC's bitrate in place".into(),
            ));
        }
        // Continuing needs invalidation and the long-term frames asked for
        // (M12d-2): without them, the plain stream, which recovers with
        // keyframes, rather than an encoder that might refer to frames a viewer
        // never got.
        if continuation.ltr_frames > 0 {
            let caps = encoder.continuation_caps()?;
            if !caps.invalidation || caps.max_ltr_frames < continuation.ltr_frames as i32 {
                tracing::info!(?caps, "NVENC can't continue here; keyframes for recoveries");
                continuation = Continuation::default();
            }
        }
        encoder.long_term = continuation.ltr_frames > 0;

        let mut preset = NV_ENC_PRESET_CONFIG {
            version: api::PRESET_CONFIG_VER,
            presetCfg: NV_ENC_CONFIG {
                version: api::CONFIG_VER,
                ..Default::default()
            },
            ..Default::default()
        };
        let preset_config = fns.nvEncGetEncodePresetConfigEx.ok_or_else(missing)?;
        // SAFETY: a live session; `preset` is written in place.
        let status = unsafe {
            preset_config(
                encoder.session,
                codec_guid,
                api::PRESET_P1,
                NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
                &mut preset,
            )
        };
        check_on(api, encoder.session, status, "NvEncGetEncodePresetConfigEx")?;
        *encoder.encode_config = preset.presetCfg;
        configure(&mut encoder.encode_config, config);
        continue_with(&mut encoder.encode_config, config.codec, continuation);

        let (dar_width, dar_height) = reduced(config.width, config.height);
        *encoder.init = NV_ENC_INITIALIZE_PARAMS {
            version: api::INITIALIZE_PARAMS_VER,
            encodeGUID: codec_guid,
            presetGUID: api::PRESET_P1,
            encodeWidth: config.width,
            encodeHeight: config.height,
            darWidth: dar_width,
            darHeight: dar_height,
            frameRateNum: config.fps.max(1),
            frameRateDen: 1,
            enableEncodeAsync: 0,
            enablePTD: 1,
            encodeConfig: &mut *encoder.encode_config,
            tuningInfo: NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY,
            ..Default::default()
        };
        let initialize = fns.nvEncInitializeEncoder.ok_or_else(missing)?;
        // SAFETY: a live session; `init` and the config it points to are
        // boxed and outlive the call.
        let status = unsafe { initialize(encoder.session, &mut *encoder.init) };
        check_on(api, encoder.session, status, "NvEncInitializeEncoder")?;

        let mut bitstream = NV_ENC_CREATE_BITSTREAM_BUFFER {
            version: api::CREATE_BITSTREAM_BUFFER_VER,
            ..Default::default()
        };
        let create_bitstream = fns.nvEncCreateBitstreamBuffer.ok_or_else(missing)?;
        // SAFETY: a live, initialized session; `bitstream` is written in place.
        let status = unsafe { create_bitstream(encoder.session, &mut bitstream) };
        check_on(api, encoder.session, status, "NvEncCreateBitstreamBuffer")?;
        encoder.output = bitstream.bitstreamBuffer;
        Ok(encoder)
    }

    pub fn config(&self) -> EncoderConfig {
        self.config
    }

    /// Whether this GPU invalidates references and keeps long-term frames.
    pub fn continuation_caps(&self) -> Result<ContinuationCaps, MediaError> {
        Ok(ContinuationCaps {
            invalidation: self.cap(self.codec_guid, NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION)? != 0,
            max_ltr_frames: self.cap(self.codec_guid, NV_ENC_CAPS_NUM_MAX_LTR_FRAMES)?,
        })
    }

    /// No later frame predicts from the frame encoded under `timestamp`, nor
    /// from any frame predicted from it (`NvEncInvalidateRefFrames`).
    pub fn invalidate(&mut self, timestamp: u64) -> Result<(), MediaError> {
        let invalidate = self.api.fns.nvEncInvalidateRefFrames.ok_or_else(missing)?;
        // SAFETY: a live session; the timestamp is a plain number.
        let status = unsafe { invalidate(self.session, timestamp) };
        check_on(self.api, self.session, status, "NvEncInvalidateRefFrames")
    }

    /// One GPU frame, with what `control` asks, and NVENC's report on it.
    pub fn encode_frame(
        &mut self,
        frame: &GpuFrame,
        control: FrameControl,
    ) -> Result<Option<(EncodedFrame, FrameReport)>, MediaError> {
        if (frame.width, frame.height) != (self.config.width, self.config.height) {
            return Err(MediaError::Backend(format!(
                "frame is {}x{}, encoder was opened for {}x{}",
                frame.width, frame.height, self.config.width, self.config.height
            )));
        }
        // Another pool's texture may sit at the address of one of ours that
        // was freed: NVENC would take it for the one it registered.
        if frame.source != self.source {
            return Err(MediaError::Backend(format!(
                "frame from texture pool {}, encoder opened for pool {}",
                frame.source, self.source
            )));
        }
        let started = Instant::now();
        if let Some(bps) = self.pending_bitrate.take() {
            self.reconfigure(bps)?;
        }
        let pts = frame.captured_at.0.max(self.last_pts.saturating_add(1));
        self.last_pts = pts;

        let registered = self.registered(frame)?;
        let mut map = NV_ENC_MAP_INPUT_RESOURCE {
            version: api::MAP_INPUT_RESOURCE_VER,
            registeredResource: registered,
            ..Default::default()
        };
        let map_input = self.api.fns.nvEncMapInputResource.ok_or_else(missing)?;
        let unmap_input = self.api.fns.nvEncUnmapInputResource.ok_or_else(missing)?;
        // SAFETY: a live session and a resource it registered; `frame` holds
        // the texture out of its pool until after the unmap below.
        let status = unsafe { map_input(self.session, &mut map) };
        check_on(self.api, self.session, status, "NvEncMapInputResource")?;
        let encoded = self.encode_mapped(map.mappedResource, map.mappedBufferFmt, pts, control);
        // SAFETY: mapped above, unmapped once, whatever the encode did.
        let unmapped = unsafe { unmap_input(self.session, map.mappedResource) };
        let encoded = encoded?;
        check_on(self.api, self.session, unmapped, "NvEncUnmapInputResource")?;

        Ok(encoded.map(|(data, report)| {
            (
                EncodedFrame {
                    data,
                    keyframe: report.picture_type == NV_ENC_PIC_TYPE_IDR as u32,
                    captured_at: frame.captured_at,
                    encode_time_us: started.elapsed().as_micros() as u32,
                },
                report,
            )
        }))
    }

    fn cap(&self, codec: GUID, which: NV_ENC_CAPS) -> Result<i32, MediaError> {
        let mut param = NV_ENC_CAPS_PARAM {
            version: api::CAPS_PARAM_VER,
            capsToQuery: which,
            ..Default::default()
        };
        let mut value = 0;
        let get_caps = self.api.fns.nvEncGetEncodeCaps.ok_or_else(missing)?;
        // SAFETY: a live session; both pointers live across the call.
        let status = unsafe { get_caps(self.session, codec, &mut param, &mut value) };
        check_on(self.api, self.session, status, "NvEncGetEncodeCaps")?;
        Ok(value)
    }

    /// The texture registered with NVENC, registering it the first time.
    fn registered(&mut self, frame: &GpuFrame) -> Result<NV_ENC_REGISTERED_PTR, MediaError> {
        let texture = frame.texture().d3d11_texture();
        if let Some(&r) = self.registered.get(&(texture as usize)) {
            return Ok(r);
        }
        let mut reg = NV_ENC_REGISTER_RESOURCE {
            version: api::REGISTER_RESOURCE_VER,
            resourceType: NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX,
            width: frame.width,
            height: frame.height,
            // D3D11 textures: 0, and our pool's are single textures.
            pitch: 0,
            subResourceIndex: 0,
            resourceToRegister: texture,
            bufferFormat: NV_ENC_BUFFER_FORMAT_ARGB,
            bufferUsage: NV_ENC_INPUT_IMAGE,
            ..Default::default()
        };
        let register = self.api.fns.nvEncRegisterResource.ok_or_else(missing)?;
        // SAFETY: a live session; the texture is one of the pool's, which
        // `_textures` keeps alive for as long as the registration lasts.
        let status = unsafe { register(self.session, &mut reg) };
        check_on(self.api, self.session, status, "NvEncRegisterResource")?;
        self.registered
            .insert(texture as usize, reg.registeredResource);
        Ok(reg.registeredResource)
    }

    /// The new bitrate, in place: no reset, no keyframe (FFmpeg asks for
    /// both).
    fn reconfigure(&mut self, bps: u32) -> Result<(), MediaError> {
        let mut encode_config = *self.encode_config;
        set_rate(&mut encode_config, bps, self.config.fps);
        let mut params = NV_ENC_RECONFIGURE_PARAMS {
            version: api::RECONFIGURE_PARAMS_VER,
            reInitEncodeParams: *self.init,
            ..Default::default()
        };
        params.reInitEncodeParams.encodeConfig = &mut encode_config;
        params.set_resetEncoder(0);
        params.set_forceIDR(0);
        let reconfigure = self.api.fns.nvEncReconfigureEncoder.ok_or_else(missing)?;
        // SAFETY: a live session; `params` and the config it points to live
        // across the call.
        let status = unsafe { reconfigure(self.session, &mut params) };
        check_on(self.api, self.session, status, "NvEncReconfigureEncoder")?;
        *self.encode_config = encode_config;
        self.init.encodeConfig = &mut *self.encode_config;
        self.config.bitrate_bps = bps;
        Ok(())
    }

    /// Encodes one mapped input and copies its bitstream out.
    fn encode_mapped(
        &mut self,
        input: NV_ENC_INPUT_PTR,
        format: NV_ENC_BUFFER_FORMAT,
        pts: u64,
        control: FrameControl,
    ) -> Result<Option<(Bytes, FrameReport)>, MediaError> {
        let fns = &self.api.fns;
        let mut pic = NV_ENC_PIC_PARAMS {
            version: api::PIC_PARAMS_VER,
            inputWidth: self.config.width,
            inputHeight: self.config.height,
            inputPitch: 0,
            // FFmpeg's `forced-idr=1`: a requested keyframe is an IDR, which
            // carries SPS and PPS (`repeatSPSPPS`).
            encodePicFlags: if control.keyframe {
                NV_ENC_PIC_FLAG_FORCEIDR as u32
            } else {
                0
            },
            inputTimeStamp: pts,
            inputBuffer: input,
            outputBitstream: self.output,
            bufferFmt: format,
            pictureStruct: NV_ENC_PIC_STRUCT_FRAME,
            ..Default::default()
        };
        // FFmpeg repeats the configured slice mode in every picture.
        // SAFETY: the union's member for the codec being encoded.
        unsafe {
            match self.config.codec {
                Codec::Hevc => {
                    let p = &mut pic.codecPicParams.hevcPicParams;
                    p.sliceMode = 3;
                    p.sliceModeData = 1;
                    if let Some(slot) = control.mark_ltr {
                        p.set_ltrMarkFrame(1);
                        p.ltrMarkFrameIdx = slot;
                    }
                }
                _ => {
                    let p = &mut pic.codecPicParams.h264PicParams;
                    p.sliceMode = 3;
                    p.sliceModeData = 1;
                    if let Some(slot) = control.mark_ltr {
                        p.set_ltrMarkFrame(1);
                        p.ltrMarkFrameIdx = slot;
                    }
                }
            }
        }
        let encode = fns.nvEncEncodePicture.ok_or_else(missing)?;
        // SAFETY: a live session, a mapped input and our bitstream buffer.
        let status = unsafe { encode(self.session, &mut pic) };
        if status == NV_ENC_ERR_NEED_MORE_INPUT {
            // Never without B-frames; nothing to read if it happens.
            return Ok(None);
        }
        check_on(self.api, self.session, status, "NvEncEncodePicture")?;

        let mut lock = NV_ENC_LOCK_BITSTREAM {
            version: api::LOCK_BITSTREAM_VER,
            outputBitstream: self.output,
            ..Default::default()
        };
        let lock_bitstream = fns.nvEncLockBitstream.ok_or_else(missing)?;
        let unlock_bitstream = fns.nvEncUnlockBitstream.ok_or_else(missing)?;
        // SAFETY: a live session; synchronous mode, so this waits for the
        // encode above.
        let status = unsafe { lock_bitstream(self.session, &mut lock) };
        check_on(self.api, self.session, status, "NvEncLockBitstream")?;
        let data = if lock.bitstreamBufferPtr.is_null() {
            Bytes::new()
        } else {
            // SAFETY: while locked, the driver's buffer holds this many bytes.
            Bytes::copy_from_slice(unsafe {
                std::slice::from_raw_parts(
                    lock.bitstreamBufferPtr.cast::<u8>(),
                    lock.bitstreamSizeInBytes as usize,
                )
            })
        };
        let report = FrameReport {
            timestamp: pts,
            picture_type: lock.pictureType as u32,
            ltr_marked: (lock.ltrFrame() != 0).then_some(lock.ltrFrameIdx),
            ltr_used: lock.ltrFrameBitmap,
        };
        // SAFETY: the buffer locked above, unlocked once.
        let status = unsafe { unlock_bitstream(self.session, self.output) };
        check_on(self.api, self.session, status, "NvEncUnlockBitstream")?;
        Ok(Some((data, report)))
    }
}

impl VideoEncoder for NvencEncoder {
    fn caps(&self) -> &EncoderCaps {
        &self.caps
    }

    fn encode(
        &mut self,
        input: EncoderInput<'_>,
        force_keyframe: bool,
    ) -> Result<Vec<EncodedFrame>, MediaError> {
        let EncoderInput::Gpu(frame) = input else {
            return Err(MediaError::Unsupported(
                "a frame in memory for an encoder opened for GPU frames".into(),
            ));
        };
        let control = FrameControl {
            keyframe: force_keyframe,
            ..Default::default()
        };
        Ok(self
            .encode_frame(frame, control)?
            .map(|(encoded, _)| encoded)
            .into_iter()
            .collect())
    }

    fn set_bitrate(&mut self, bps: u32) -> Result<(), MediaError> {
        self.pending_bitrate = (bps != self.config.bitrate_bps).then_some(bps);
        Ok(())
    }

    fn continues(&self) -> bool {
        self.long_term
    }

    fn encode_marking(
        &mut self,
        input: EncoderInput<'_>,
        force_keyframe: bool,
        mark: Option<u32>,
    ) -> Result<Vec<(EncodedFrame, Option<LongTerm>)>, MediaError> {
        let EncoderInput::Gpu(frame) = input else {
            return Err(MediaError::Unsupported(
                "a frame in memory for an encoder opened for GPU frames".into(),
            ));
        };
        let control = FrameControl {
            keyframe: force_keyframe,
            mark_ltr: mark.filter(|_| self.long_term),
        };
        Ok(self
            .encode_frame(frame, control)?
            .map(|(encoded, report)| {
                let long_term = LongTerm {
                    timestamp: report.timestamp,
                    marked: report.ltr_marked,
                };
                (encoded, Some(long_term))
            })
            .into_iter()
            .collect())
    }

    fn invalidate(&mut self, timestamp: u64) -> Result<(), MediaError> {
        NvencEncoder::invalidate(self, timestamp)
    }
}

/// What a continuing encoder opens with (ADR 0016, the spike): two long-term
/// frames, and three references, since with long-term frames on the driver
/// predicts from one short-term frame (H.264 lost 1 dB a frame; HEVC gained
/// 1 dB over the driver's choice).
pub const CONTINUING: Continuation = Continuation {
    ltr_frames: 2,
    refs_l0: 3,
};

impl Drop for NvencEncoder {
    fn drop(&mut self) {
        let fns = &self.api.fns;
        if !self.session.is_null() {
            // SAFETY: each registration and the buffer are this session's,
            // released once, before the session itself.
            unsafe {
                if let Some(unregister) = fns.nvEncUnregisterResource {
                    for &r in self.registered.values() {
                        unregister(self.session, r);
                    }
                }
                if !self.output.is_null()
                    && let Some(destroy) = fns.nvEncDestroyBitstreamBuffer
                {
                    destroy(self.session, self.output);
                }
                if let Some(destroy) = fns.nvEncDestroyEncoder {
                    destroy(self.session);
                }
            }
        }
        // SAFETY: the one reference `d3d11_device_owned` gave us, released
        // once, after the session that used the device is gone.
        unsafe { release_com(self.device) };
        // `_textures` drops after this: the pool outlives its registrations.
    }
}

fn missing() -> MediaError {
    MediaError::Backend("the NVIDIA driver lacks an NVENC function".into())
}

/// Our settings over the preset's, as FFmpeg 8.1 sets them for our options
/// (`nvenc_setup_encoder`, `nvenc_setup_rate_control`,
/// `nvenc_setup_h264_config`, `nvenc_setup_hevc_config`).
fn configure(c: &mut NV_ENC_CONFIG, config: EncoderConfig) {
    c.version = api::CONFIG_VER;
    // `gop_size` i32::MAX and no B-frames, as `FfmpegEncoder` opens it.
    c.gopLength = i32::MAX as u32;
    c.frameIntervalP = 1;
    c.frameFieldMode = NV_ENC_PARAMS_FRAME_FIELD_MODE_FRAME;
    c.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
    c.rcParams.multiPass = NV_ENC_MULTI_PASS_DISABLED;
    c.rcParams.cbQPIndexOffset = 0;
    c.rcParams.crQPIndexOffset = 0;
    // `zerolatency=1`; `rc-lookahead=0` leaves the preset's (none at p1).
    c.rcParams.set_zeroReorderDelay(1);
    set_rate(c, config.bitrate_bps, config.fps);
    let gop = c.gopLength;
    // SAFETY: the union's member for the codec being configured.
    unsafe {
        match config.codec {
            Codec::Hevc => {
                c.profileGUID = api::HEVC_PROFILE_MAIN;
                let hevc = &mut c.encodeCodecConfig.hevcConfig;
                vui(&mut hevc.hevcVUIParameters);
                hevc.sliceMode = 3;
                hevc.sliceModeData = 1;
                hevc.set_disableSPSPPS(0);
                hevc.set_repeatSPSPPS(1);
                hevc.set_outputAUD(0);
                hevc.maxNumRefFramesInDPB = 0;
                hevc.idrPeriod = gop;
                hevc.set_outputBufferingPeriodSEI(0);
                hevc.set_enableFillerDataInsertion(0);
                hevc.set_outputPictureTimingSEI(1);
                hevc.set_chromaFormatIDC(1);
                hevc.level = NV_ENC_LEVEL_AUTOSELECT as u32;
                hevc.tier = NV_ENC_TIER_HEVC_MAIN as u32;
                hevc.inputBitDepth = NV_ENC_BIT_DEPTH_8;
                hevc.outputBitDepth = NV_ENC_BIT_DEPTH_8;
                // FFmpeg's `refs` default for NVENC is 0: the driver's choice.
                hevc.numRefL0 = NV_ENC_NUM_REF_FRAMES_AUTOSELECT;
                hevc.numRefL1 = NV_ENC_NUM_REF_FRAMES_AUTOSELECT;
            }
            _ => {
                c.profileGUID = api::H264_PROFILE_HIGH;
                let h264 = &mut c.encodeCodecConfig.h264Config;
                vui(&mut h264.h264VUIParameters);
                // H.264 only (FFmpeg: `gopLength != 1`); HEVC keeps the preset's.
                h264.h264VUIParameters.bitstreamRestrictionFlag = 1;
                h264.sliceMode = 3;
                h264.sliceModeData = 1;
                h264.set_disableSPSPPS(0);
                h264.set_repeatSPSPPS(1);
                h264.set_outputAUD(0);
                h264.maxNumRefFrames = 0;
                h264.idrPeriod = gop;
                h264.set_outputBufferingPeriodSEI(0);
                h264.set_enableFillerDataInsertion(0);
                h264.set_outputPictureTimingSEI(1);
                h264.chromaFormatIDC = 1;
                h264.level = NV_ENC_LEVEL_AUTOSELECT as u32;
                h264.inputBitDepth = NV_ENC_BIT_DEPTH_8;
                h264.outputBitDepth = NV_ENC_BIT_DEPTH_8;
                h264.numRefL0 = NV_ENC_NUM_REF_FRAMES_AUTOSELECT;
                h264.numRefL1 = NV_ENC_NUM_REF_FRAMES_AUTOSELECT;
                // `s12m_tc` is on by default in `h264_nvenc` (a frame without
                // a timecode leaves its picture's empty).
                h264.set_enableTimeCode(1);
            }
        }
    }
}

/// Long-term frames in per-picture mode, where `continuation` asks for them.
fn continue_with(c: &mut NV_ENC_CONFIG, codec: Codec, continuation: Continuation) {
    if continuation == Continuation::default() {
        return;
    }
    let ltr = u32::from(continuation.ltr_frames > 0);
    let refs = if continuation.refs_l0 == 0 {
        NV_ENC_NUM_REF_FRAMES_AUTOSELECT
    } else {
        continuation.refs_l0 as NV_ENC_NUM_REF_FRAMES
    };
    // SAFETY: the union's member for the codec being configured.
    unsafe {
        match codec {
            Codec::Hevc => {
                let hevc = &mut c.encodeCodecConfig.hevcConfig;
                hevc.set_enableLTR(ltr);
                hevc.ltrTrustMode = 0;
                hevc.ltrNumFrames = continuation.ltr_frames;
                hevc.numRefL0 = refs;
            }
            _ => {
                let h264 = &mut c.encodeCodecConfig.h264Config;
                h264.set_enableLTR(ltr);
                h264.ltrTrustMode = 0;
                h264.ltrNumFrames = continuation.ltr_frames;
                h264.numRefL0 = refs;
            }
        }
    }
}

/// CBR with a one-frame rate buffer (`FfmpegEncoder`'s `set_rate_buffer`):
/// every frame, keyframes included, fits one frame's share of the bitrate.
fn set_rate(c: &mut NV_ENC_CONFIG, bps: u32, fps: u32) {
    c.rcParams.averageBitRate = bps;
    c.rcParams.maxBitRate = bps;
    c.rcParams.vbvBufferSize = bps / fps.max(1);
}

/// What FFmpeg writes for RGB input tagged BT.709 limited range: NVENC
/// converts with BT.601 and says so (the viewer follows the declared matrix).
fn vui(v: &mut NV_ENC_CONFIG_H264_VUI_PARAMETERS) {
    v.colourMatrix = NV_ENC_VUI_MATRIX_COEFFS_BT470BG;
    v.colourPrimaries = NV_ENC_VUI_COLOR_PRIMARIES_BT709;
    v.transferCharacteristics = NV_ENC_VUI_TRANSFER_CHARACTERISTIC_BT709;
    v.videoFullRangeFlag = 0;
    v.colourDescriptionPresentFlag = 1;
    v.videoSignalTypePresentFlag = 1;
}

/// The display aspect ratio of square pixels, reduced as FFmpeg's
/// `compute_dar` leaves it.
fn reduced(width: u32, height: u32) -> (u32, u32) {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 { a } else { gcd(b, a % b) }
    }
    let g = gcd(width, height).max(1);
    (width / g, height / g)
}

/// `IUnknown::Release` on a COM pointer we own a reference to.
///
/// # Safety
/// `object` is null or a live COM object with a reference that is ours.
unsafe fn release_com(object: *mut c_void) {
    #[repr(C)]
    struct Unknown {
        query_interface: *const c_void,
        add_ref: *const c_void,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }
    if !object.is_null() {
        // SAFETY: a COM object's first field points at its vtable, which
        // begins with IUnknown's three methods.
        unsafe { ((**object.cast::<*const Unknown>()).release)(object) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aspect_ratio_is_reduced_like_ffmpegs() {
        assert_eq!(reduced(1920, 1080), (16, 9));
        assert_eq!(reduced(1282, 752), (641, 376));
        assert_eq!(reduced(1281, 1), (1281, 1));
    }

    #[test]
    fn the_rate_buffer_holds_one_frame() {
        let mut c = NV_ENC_CONFIG::default();
        set_rate(&mut c, 20_000_000, 60);
        assert_eq!(c.rcParams.averageBitRate, 20_000_000);
        assert_eq!(c.rcParams.maxBitRate, 20_000_000);
        assert_eq!(c.rcParams.vbvBufferSize, 333_333);
    }
}
