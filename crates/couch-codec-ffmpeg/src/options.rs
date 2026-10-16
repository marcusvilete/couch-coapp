//! Each encoder's low-latency options, as an ordered list of sets (M9a).
//!
//! The encoder opens with the first set the driver accepts. The **last set of
//! every list is the one we shipped before M9a**, so a driver that rejects a
//! newer option can't leave us worse off than before; never add a set after
//! it, and never make a list without it. Most of the newer options come from
//! Sunshine's tuning (`docs/research/sunshine-moonlight.md`, item 2); they are
//! FFmpeg's public option names, checked against our FFmpeg build
//! (`ffmpeg -h encoder=<name>`).

use std::ffi::{CStr, c_char};

use rsmpeg::{avcodec::AVCodecContext, ffi};

/// One way to open an encoder.
#[derive(Debug, Clone, Copy)]
pub struct OptionSet {
    /// For the logs (`EncoderCaps::options`).
    pub label: &'static str,
    pub options: &'static [(&'static CStr, &'static CStr)],
}

impl OptionSet {
    /// Opens `ctx` with these options. On success, returns the options FFmpeg
    /// didn't recognize (none, if the names are right).
    ///
    /// This calls `avcodec_open2` itself rather than rsmpeg's
    /// `AVCodecContext::open`: when the open fails, FFmpeg frees the options
    /// dictionary it was given and hands back a new one, and rsmpeg 0.18 then
    /// frees the original again. That double free crashed the process on any
    /// failed open with options, e.g. AMF or Quick Sync on a PC without that
    /// GPU (found in M9a).
    pub fn open(&self, ctx: &mut AVCodecContext) -> Result<Vec<String>, String> {
        let mut dict: *mut ffi::AVDictionary = std::ptr::null_mut();
        for (key, value) in self.options {
            // SAFETY: `dict` starts null and is only ever what av_dict_set
            // returns through it; the strings are NUL-terminated and copied.
            let set = unsafe { ffi::av_dict_set(&mut dict, key.as_ptr(), value.as_ptr(), 0) };
            if set < 0 {
                // SAFETY: frees whatever was built so far (null is fine).
                unsafe { ffi::av_dict_free(&mut dict) };
                return Err(format!(
                    "setting {}: {}",
                    key.to_string_lossy(),
                    av_error(set)
                ));
            }
        }
        // SAFETY: `ctx` is a valid, unopened context; FFmpeg may replace
        // `dict` (success or failure), and whatever it points at afterwards is
        // ours to free, exactly once, below.
        let opened = unsafe { ffi::avcodec_open2(ctx.as_mut_ptr(), std::ptr::null(), &mut dict) };
        let unused = unused_keys(dict);
        // SAFETY: `dict` is FFmpeg's current dictionary (or null), freed once.
        unsafe { ffi::av_dict_free(&mut dict) };
        if opened < 0 {
            return Err(av_error(opened));
        }
        Ok(unused)
    }
}

/// The keys left in `dict`: after `avcodec_open2`, the options it didn't use.
fn unused_keys(dict: *const ffi::AVDictionary) -> Vec<String> {
    let mut keys = Vec::new();
    let mut entry: *const ffi::AVDictionaryEntry = std::ptr::null();
    loop {
        // SAFETY: iterates `dict` (null is fine) with the empty-key pattern
        // FFmpeg documents; entries stay valid until the dictionary is freed.
        entry = unsafe {
            ffi::av_dict_get(dict, c"".as_ptr(), entry, ffi::AV_DICT_IGNORE_SUFFIX as i32)
        };
        if entry.is_null() {
            return keys;
        }
        // SAFETY: a non-null entry has a NUL-terminated key.
        keys.push(
            unsafe { CStr::from_ptr((*entry).key) }
                .to_string_lossy()
                .into_owned(),
        );
    }
}

/// FFmpeg's description of an error code.
pub(crate) fn av_error(code: i32) -> String {
    let mut buf = [0 as c_char; 128];
    // SAFETY: the buffer and its length match.
    unsafe { ffi::av_strerror(code, buf.as_mut_ptr(), buf.len()) };
    // SAFETY: av_strerror always NUL-terminates within the buffer.
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

const NVENC: &[OptionSet] = &[OptionSet {
    label: "p1+ull",
    options: &[
        (c"profile", c"high"),
        (c"preset", c"p1"),
        (c"tune", c"ull"),
        (c"rc", c"cbr"),
        (c"zerolatency", c"1"),
        (c"delay", c"0"),
        (c"rc-lookahead", c"0"),
        (c"forced-idr", c"1"),
    ],
}];

/// NVIDIA's HEVC (M12c): H.264's settings, with HEVC's Main profile (8-bit).
const NVENC_HEVC: &[OptionSet] = &[OptionSet {
    label: "main+p1+ull",
    options: &[
        (c"profile", c"main"),
        (c"preset", c"p1"),
        (c"tune", c"ull"),
        (c"rc", c"cbr"),
        (c"zerolatency", c"1"),
        (c"delay", c"0"),
        (c"rc-lookahead", c"0"),
        (c"forced-idr", c"1"),
    ],
}];

/// AMD. `forced_idr` defaults to off in FFmpeg's AMF wrapper, so without it a
/// requested keyframe may be a plain I-frame, which a new epoch can't start
/// from. Rate control stays CBR: our rate controller sets the bitrate.
const AMF: &[OptionSet] = &[
    OptionSet {
        label: "ultralowlatency+latency+idr",
        options: &[
            (c"profile", c"high"),
            (c"usage", c"ultralowlatency"),
            (c"rc", c"cbr"),
            (c"latency", c"1"),
            (c"async_depth", c"1"),
            (c"forced_idr", c"1"),
            (c"frame_skipping", c"0"),
            (c"filler_data", c"0"),
        ],
    },
    // Some drivers refuse ultra-low-latency (AMF issue 410); Sunshine falls
    // back to low latency the same way.
    OptionSet {
        label: "lowlatency+latency+idr",
        options: &[
            (c"profile", c"high"),
            (c"usage", c"lowlatency"),
            (c"rc", c"cbr"),
            (c"latency", c"1"),
            (c"async_depth", c"1"),
            (c"forced_idr", c"1"),
            (c"frame_skipping", c"0"),
            (c"filler_data", c"0"),
        ],
    },
    OptionSet {
        label: "ultralowlatency+idr",
        options: &[
            (c"profile", c"high"),
            (c"usage", c"ultralowlatency"),
            (c"rc", c"cbr"),
            (c"forced_idr", c"1"),
        ],
    },
    OptionSet {
        label: "before M9a",
        options: &[
            (c"profile", c"high"),
            (c"usage", c"ultralowlatency"),
            (c"rc", c"cbr"),
        ],
    },
];

/// Intel. `low_power` uses the GPU's fixed-function encoder, which some
/// generations or drivers lack; Sunshine retries without it.
const QSV: &[OptionSet] = &[
    OptionSet {
        label: "veryfast+low_power",
        options: &[
            (c"profile", c"high"),
            (c"preset", c"veryfast"),
            (c"async_depth", c"1"),
            (c"low_delay_brc", c"1"),
            (c"forced_idr", c"1"),
            (c"low_power", c"1"),
        ],
    },
    OptionSet {
        label: "before M9a",
        options: &[
            (c"profile", c"high"),
            (c"preset", c"veryfast"),
            (c"async_depth", c"1"),
            (c"low_delay_brc", c"1"),
            (c"forced_idr", c"1"),
        ],
    },
];

const MEDIA_FOUNDATION: &[OptionSet] = &[OptionSet {
    label: "display_remoting",
    options: &[
        (c"profile", c"high"),
        (c"rate_control", c"cbr"),
        (c"scenario", c"display_remoting"),
        (c"hw_encoding", c"1"),
    ],
}];

const SOFTWARE: &[OptionSet] = &[OptionSet {
    label: "no skipped frames",
    options: &[(c"allow_skip_frames", c"0")],
}];

/// The option sets to try for `name`, best first.
pub fn option_sets(name: &str) -> &'static [OptionSet] {
    match name {
        "h264_nvenc" => NVENC,
        "hevc_nvenc" => NVENC_HEVC,
        "h264_amf" => AMF,
        "h264_qsv" => QSV,
        "h264_mf" => MEDIA_FOUNDATION,
        _ => SOFTWARE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::H264_ENCODERS;

    fn has(set: &OptionSet, key: &CStr, value: &CStr) -> bool {
        set.options.iter().any(|&(k, v)| k == key && v == value)
    }

    /// What each encoder opened with before M9a: the fallback that must
    /// always be there.
    fn before_m9a(name: &str) -> &'static [(&'static CStr, &'static CStr)] {
        match name {
            "h264_amf" => &[
                (c"profile", c"high"),
                (c"usage", c"ultralowlatency"),
                (c"rc", c"cbr"),
            ],
            "h264_qsv" => &[
                (c"profile", c"high"),
                (c"preset", c"veryfast"),
                (c"async_depth", c"1"),
                (c"low_delay_brc", c"1"),
                (c"forced_idr", c"1"),
            ],
            other => {
                let sets = option_sets(other);
                assert_eq!(sets.len(), 1, "{other} has no fallbacks yet");
                sets[0].options
            }
        }
    }

    #[test]
    fn every_encoder_ends_with_its_settings_from_before_m9a() {
        for &name in H264_ENCODERS {
            let sets = option_sets(name);
            assert!(!sets.is_empty(), "{name}");
            assert_eq!(sets.last().unwrap().options, before_m9a(name), "{name}");
            assert!(sets.iter().all(|s| !s.options.is_empty()), "{name}");
        }
    }

    #[test]
    fn nvidias_hevc_is_its_h264_with_the_main_profile() {
        let hevc = option_sets("hevc_nvenc");
        assert_eq!(hevc.len(), 1);
        assert!(has(&hevc[0], c"profile", c"main"));
        assert!(has(&hevc[0], c"forced-idr", c"1"));
        // Everything but the profile is H.264's: the same latency tuning.
        let rest = |set: &OptionSet| -> Vec<_> {
            set.options
                .iter()
                .filter(|(k, _)| *k != c"profile")
                .copied()
                .collect()
        };
        assert_eq!(rest(&hevc[0]), rest(&option_sets("h264_nvenc")[0]));
    }

    #[test]
    fn amd_and_intel_ask_for_real_keyframes_first() {
        assert!(has(&option_sets("h264_amf")[0], c"forced_idr", c"1"));
        assert!(has(&option_sets("h264_amf")[1], c"usage", c"lowlatency"));
        assert!(has(&option_sets("h264_qsv")[0], c"low_power", c"1"));
        assert!(has(&option_sets("h264_qsv")[0], c"forced_idr", c"1"));
    }

    #[test]
    fn labels_tell_the_sets_apart() {
        for &name in H264_ENCODERS {
            let labels: Vec<_> = option_sets(name).iter().map(|s| s.label).collect();
            let unique: std::collections::BTreeSet<_> = labels.iter().collect();
            assert_eq!(labels.len(), unique.len(), "{name}");
        }
    }
}
