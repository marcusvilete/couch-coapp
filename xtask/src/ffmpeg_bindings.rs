//! Generates the FFmpeg FFI bindings that rsmpeg is built against.
//!
//! rusty_ffmpeg (under rsmpeg) runs bindgen 0.71 at build time. With libclang
//! 22 and later, that bindgen emits some structs (`AVFormatContext`,
//! `AVBitStreamFilter`) without their fields. So we run a current bindgen once,
//! with rusty_ffmpeg 0.16.7's options, and point rusty_ffmpeg at the result
//! through `FFMPEG_BINDING_PATH` (.cargo/config.toml). Normal builds then need no
//! libclang at all. Re-run this after bumping FFmpeg in deps.rs.

use std::{fs, path::Path};

use anyhow::{Context, anyhow, bail};
use bindgen::callbacks::{MacroParsingBehavior, ParseCallbacks};

use crate::deps;

/// Where the generated bindings live, relative to the repository root.
pub const BINDING_FILE: &str =
    "crates/couch-codec-ffmpeg/bindings/ffmpeg-x86_64-pc-windows-msvc.rs";

/// The headers rusty_ffmpeg 0.16.7 binds, in its order. Headers missing from
/// the FFmpeg version at hand (such as `avfft.h`, removed in 8.0) are skipped.
const HEADERS: &[&str] = &[
    "libavcodec/ac3_parser.h",
    "libavcodec/adts_parser.h",
    "libavcodec/avcodec.h",
    "libavcodec/avdct.h",
    "libavcodec/avfft.h",
    "libavcodec/bsf.h",
    "libavcodec/codec.h",
    "libavcodec/codec_desc.h",
    "libavcodec/codec_id.h",
    "libavcodec/codec_par.h",
    "libavcodec/defs.h",
    "libavcodec/dirac.h",
    "libavcodec/dv_profile.h",
    "libavcodec/jni.h",
    "libavcodec/mediacodec.h",
    "libavcodec/packet.h",
    "libavcodec/version.h",
    "libavcodec/version_major.h",
    "libavcodec/vorbis_parser.h",
    "libavdevice/avdevice.h",
    "libavdevice/version.h",
    "libavdevice/version_major.h",
    "libavfilter/avfilter.h",
    "libavfilter/buffersink.h",
    "libavfilter/buffersrc.h",
    "libavfilter/version.h",
    "libavfilter/version_major.h",
    "libavformat/avformat.h",
    "libavformat/avio.h",
    "libavformat/version.h",
    "libavformat/version_major.h",
    "libavutil/adler32.h",
    "libavutil/aes.h",
    "libavutil/aes_ctr.h",
    "libavutil/ambient_viewing_environment.h",
    "libavutil/attributes.h",
    "libavutil/audio_fifo.h",
    "libavutil/avassert.h",
    "libavutil/avconfig.h",
    "libavutil/avstring.h",
    "libavutil/avutil.h",
    "libavutil/base64.h",
    "libavutil/blowfish.h",
    "libavutil/bprint.h",
    "libavutil/bswap.h",
    "libavutil/buffer.h",
    "libavutil/camellia.h",
    "libavutil/cast5.h",
    "libavutil/channel_layout.h",
    "libavutil/common.h",
    "libavutil/cpu.h",
    "libavutil/crc.h",
    "libavutil/csp.h",
    "libavutil/des.h",
    "libavutil/detection_bbox.h",
    "libavutil/dict.h",
    "libavutil/display.h",
    "libavutil/dovi_meta.h",
    "libavutil/downmix_info.h",
    "libavutil/encryption_info.h",
    "libavutil/error.h",
    "libavutil/eval.h",
    "libavutil/executor.h",
    "libavutil/ffversion.h",
    "libavutil/fifo.h",
    "libavutil/file.h",
    "libavutil/film_grain_params.h",
    "libavutil/frame.h",
    "libavutil/hash.h",
    "libavutil/hdr_dynamic_metadata.h",
    "libavutil/hdr_dynamic_vivid_metadata.h",
    "libavutil/hmac.h",
    "libavutil/hwcontext.h",
    "libavutil/imgutils.h",
    "libavutil/intfloat.h",
    "libavutil/intreadwrite.h",
    "libavutil/lfg.h",
    "libavutil/log.h",
    "libavutil/lzo.h",
    "libavutil/macros.h",
    "libavutil/mastering_display_metadata.h",
    "libavutil/mathematics.h",
    "libavutil/md5.h",
    "libavutil/mem.h",
    "libavutil/motion_vector.h",
    "libavutil/murmur3.h",
    "libavutil/opt.h",
    "libavutil/parseutils.h",
    "libavutil/pixdesc.h",
    "libavutil/pixelutils.h",
    "libavutil/pixfmt.h",
    "libavutil/random_seed.h",
    "libavutil/rational.h",
    "libavutil/rc4.h",
    "libavutil/replaygain.h",
    "libavutil/ripemd.h",
    "libavutil/samplefmt.h",
    "libavutil/sha.h",
    "libavutil/sha512.h",
    "libavutil/spherical.h",
    "libavutil/stereo3d.h",
    "libavutil/tea.h",
    "libavutil/threadmessage.h",
    "libavutil/time.h",
    "libavutil/timecode.h",
    "libavutil/timestamp.h",
    "libavutil/tree.h",
    "libavutil/twofish.h",
    "libavutil/tx.h",
    "libavutil/uuid.h",
    "libavutil/version.h",
    "libavutil/video_enc_params.h",
    "libavutil/video_hint.h",
    "libavutil/xtea.h",
    "libswresample/swresample.h",
    "libswresample/version.h",
    "libswresample/version_major.h",
    "libswscale/swscale.h",
    "libswscale/version.h",
    "libswscale/version_major.h",
];

/// Structs that came out fieldless with the old bindgen; checked after generating.
const SENTINEL_STRUCTS: &[&str] = &[
    "AVFormatContext",
    "AVBitStreamFilter",
    "AVCodecContext",
    "AVFrame",
];

/// `math.h` defines these as macros that clash with FFmpeg's; rusty_ffmpeg skips them too.
#[derive(Debug)]
struct IgnoreMathMacros;

impl ParseCallbacks for IgnoreMathMacros {
    fn will_parse_macro(&self, name: &str) -> MacroParsingBehavior {
        match name {
            "FP_NAN" | "FP_INFINITE" | "FP_ZERO" | "FP_SUBNORMAL" | "FP_NORMAL" => {
                MacroParsingBehavior::Ignore
            }
            _ => MacroParsingBehavior::Default,
        }
    }
}

/// The provenance line written at the top of the bindings, tying them to the pin.
pub fn provenance(ffmpeg_sha256: &str) -> String {
    format!("// FFmpeg archive sha256: {ffmpeg_sha256}")
}

pub fn generate(root: &Path) -> anyhow::Result<()> {
    let ffmpeg = deps::DEPS
        .iter()
        .find(|d| d.dir == "ffmpeg")
        .expect("FFmpeg is pinned in deps.rs");
    if !deps::is_installed(root, ffmpeg) {
        bail!("FFmpeg is missing; run `cargo xtask fetch-deps` first");
    }
    let include = deps::third_party(root).join("ffmpeg").join("include");

    let mut builder = bindgen::builder()
        .raw_line("// Generated by `cargo xtask gen-ffmpeg-bindings`. Do not edit.")
        .raw_line(format!("// {}", ffmpeg.name))
        .raw_line(provenance(ffmpeg.sha256))
        .impl_debug(true)
        .rust_target(bindgen::RustTarget::stable(68, 0).map_err(|e| anyhow!("{e}"))?)
        .parse_callbacks(Box::new(IgnoreMathMacros))
        .clang_arg("--target=x86_64-pc-windows-msvc")
        .clang_arg(format!("-I{}", include.display()))
        .blocklist_type("__mingw_ldbl_type_t")
        .prepend_enum_name(false);
    for header in HEADERS {
        let path = include.join(header);
        if path.exists() {
            builder = builder.header(path.to_string_lossy());
        } else {
            println!("skipping {header} (not in this FFmpeg version)");
        }
    }

    let bindings = builder
        .generate()
        .context("bindgen failed; is LLVM installed and LIBCLANG_PATH set?")?;
    let out = root.join(BINDING_FILE);
    fs::create_dir_all(out.parent().expect("binding file has a parent"))?;
    bindings.write_to_file(&out)?;

    let text = fs::read_to_string(&out)?;
    for name in SENTINEL_STRUCTS {
        if is_fieldless(&text, name) {
            bail!("{name} came out without fields; this bindgen/libclang pair is broken");
        }
    }
    println!("wrote {} ({} KiB)", out.display(), text.len() / 1024);
    Ok(())
}

/// True if `pub struct {name}` is emitted as a placeholder with no real fields.
fn is_fieldless(text: &str, name: &str) -> bool {
    let needle = format!("pub struct {name} {{");
    let Some(start) = text.find(&needle) else {
        return true;
    };
    let body = &text[start + needle.len()..];
    let body = &body[..body.find('}').unwrap_or(body.len())];
    body.trim() == "pub _address: u8," || body.trim().starts_with("_unused")
}
