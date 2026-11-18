//! Loading NVENC, and what bindgen can't take from its header: the struct
//! versions (function-like macros) and the GUIDs (`static const`). A test
//! reads both back from the vendored header, so a header bump can't leave
//! them stale.

use std::{
    ffi::{CStr, c_void},
    sync::OnceLock,
};

use couch_media::MediaError;
use libloading::os::windows::{LOAD_LIBRARY_SEARCH_SYSTEM32, Library, Symbol};

use crate::ffi::{
    GUID, NV_ENC_ERR_DEVICE_NOT_EXIST, NV_ENC_ERR_ENCODER_BUSY, NV_ENC_ERR_ENCODER_NOT_INITIALIZED,
    NV_ENC_ERR_GENERIC, NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY, NV_ENC_ERR_INVALID_CALL,
    NV_ENC_ERR_INVALID_DEVICE, NV_ENC_ERR_INVALID_ENCODERDEVICE, NV_ENC_ERR_INVALID_PARAM,
    NV_ENC_ERR_INVALID_PTR, NV_ENC_ERR_INVALID_VERSION, NV_ENC_ERR_LOCK_BUSY,
    NV_ENC_ERR_NO_ENCODE_DEVICE, NV_ENC_ERR_OUT_OF_MEMORY, NV_ENC_ERR_RESOURCE_NOT_MAPPED,
    NV_ENC_ERR_RESOURCE_NOT_REGISTERED, NV_ENC_ERR_RESOURCE_REGISTER_FAILED,
    NV_ENC_ERR_UNIMPLEMENTED, NV_ENC_ERR_UNSUPPORTED_DEVICE, NV_ENC_ERR_UNSUPPORTED_PARAM,
    NV_ENC_SUCCESS, NV_ENCODE_API_FUNCTION_LIST, NVENCAPI_MAJOR_VERSION, NVENCAPI_MINOR_VERSION,
    NVENCAPI_VERSION, NVENCSTATUS,
};

/// `NVENCAPI_STRUCT_VERSION(ver)`.
const fn struct_version(ver: u32) -> u32 {
    NVENCAPI_VERSION | (ver << 16) | (0x7 << 28)
}

pub(crate) const CAPS_PARAM_VER: u32 = struct_version(1);
pub(crate) const CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
pub(crate) const CONFIG_VER: u32 = struct_version(9) | (1 << 31);
pub(crate) const INITIALIZE_PARAMS_VER: u32 = struct_version(7) | (1 << 31);
pub(crate) const RECONFIGURE_PARAMS_VER: u32 = struct_version(2) | (1 << 31);
pub(crate) const PRESET_CONFIG_VER: u32 = struct_version(5) | (1 << 31);
pub(crate) const PIC_PARAMS_VER: u32 = struct_version(7) | (1 << 31);
pub(crate) const LOCK_BITSTREAM_VER: u32 = struct_version(2) | (1 << 31);
pub(crate) const MAP_INPUT_RESOURCE_VER: u32 = struct_version(4);
pub(crate) const REGISTER_RESOURCE_VER: u32 = struct_version(5);
pub(crate) const OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = struct_version(1);
pub(crate) const FUNCTION_LIST_VER: u32 = struct_version(2);

const fn guid(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> GUID {
    GUID {
        Data1: data1,
        Data2: data2,
        Data3: data3,
        Data4: data4,
    }
}

pub(crate) const CODEC_H264: GUID = guid(
    0x6bc8_2762,
    0x4e63,
    0x4ca4,
    [0xaa, 0x85, 0x1e, 0x50, 0xf3, 0x21, 0xf6, 0xbf],
);
pub(crate) const CODEC_HEVC: GUID = guid(
    0x790c_dc88,
    0x4522,
    0x4d7b,
    [0x94, 0x25, 0xbd, 0xa9, 0x97, 0x5f, 0x76, 0x03],
);
pub(crate) const H264_PROFILE_HIGH: GUID = guid(
    0xe7cb_c309,
    0x4f7a,
    0x4b89,
    [0xaf, 0x2a, 0xd5, 0x37, 0xc9, 0x2b, 0xe3, 0x10],
);
pub(crate) const HEVC_PROFILE_MAIN: GUID = guid(
    0xb514_c39a,
    0xb55b,
    0x40fa,
    [0x87, 0x8f, 0xf1, 0x25, 0x3b, 0x4d, 0xfd, 0xec],
);
pub(crate) const PRESET_P1: GUID = guid(
    0xfc0a_8d3e,
    0x45f8,
    0x4cf8,
    [0x80, 0xc7, 0x29, 0x88, 0x71, 0x59, 0x0e, 0xbf],
);

/// The driver's NVENC: its function list, and the library kept loaded for
/// the life of the process (the pointers point into it).
pub(crate) struct Api {
    _library: Library,
    pub(crate) fns: NV_ENCODE_API_FUNCTION_LIST,
}

// SAFETY: the function list is filled once and only read afterwards; its
// pointers are to the driver's functions, callable from any thread.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

/// NVENC, loaded on first use; the error, if it can't be, every time after.
pub(crate) fn api() -> Result<&'static Api, MediaError> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(load)
        .as_ref()
        .map_err(|e| MediaError::Unsupported(e.clone()))
}

fn load() -> Result<Api, String> {
    type MaxVersion = unsafe extern "C" fn(*mut u32) -> NVENCSTATUS;
    type CreateInstance = unsafe extern "C" fn(*mut NV_ENCODE_API_FUNCTION_LIST) -> NVENCSTATUS;
    // The driver installs it in System32; searching only there keeps a DLL
    // planted beside the app, or in the working folder, from being loaded by
    // us. (A module of that name already in the process is reused whatever
    // the flags: only FFmpeg's NVENC loads it besides us, and it would run
    // that same DLL anyway.)
    // SAFETY: loading the driver's DLL runs only its own initialization.
    let library =
        unsafe { Library::load_with_flags("nvEncodeAPI64.dll", LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .map_err(|e| format!("no NVIDIA encoder driver ({e})"))?;
    // SAFETY: the symbols have these signatures (nvEncodeAPI.h), and are
    // used only while `library` lives (it moves into `Api`, kept forever).
    let (max_version, create): (Symbol<MaxVersion>, Symbol<CreateInstance>) = unsafe {
        (
            library
                .get(b"NvEncodeAPIGetMaxSupportedVersion\0")
                .map_err(|e| e.to_string())?,
            library
                .get(b"NvEncodeAPICreateInstance\0")
                .map_err(|e| e.to_string())?,
        )
    };
    let mut driver = 0u32;
    // SAFETY: writes one u32.
    check(
        unsafe { max_version(&mut driver) },
        "NvEncodeAPIGetMaxSupportedVersion",
    )
    .map_err(|e| e.to_string())?;
    let ours = (NVENCAPI_MAJOR_VERSION << 4) | NVENCAPI_MINOR_VERSION;
    if driver < ours {
        return Err(format!(
            "the NVIDIA driver offers NVENC API {}.{}, we need {}.{} (update the driver)",
            driver >> 4,
            driver & 0xf,
            NVENCAPI_MAJOR_VERSION,
            NVENCAPI_MINOR_VERSION
        ));
    }
    let mut fns = NV_ENCODE_API_FUNCTION_LIST {
        version: FUNCTION_LIST_VER,
        ..Default::default()
    };
    // SAFETY: fills the list, whose version says which layout it has.
    check(unsafe { create(&mut fns) }, "NvEncodeAPICreateInstance").map_err(|e| e.to_string())?;
    Ok(Api {
        _library: library,
        fns,
    })
}

/// A call's status as a result, naming the call.
pub(crate) fn check(status: NVENCSTATUS, call: &str) -> Result<(), MediaError> {
    if status == NV_ENC_SUCCESS {
        Ok(())
    } else {
        Err(MediaError::Backend(format!(
            "{call}: {}",
            status_name(status)
        )))
    }
}

/// Like [`check`], with the encoder's own last error text if it gives one.
pub(crate) fn check_on(
    api: &Api,
    encoder: *mut c_void,
    status: NVENCSTATUS,
    call: &str,
) -> Result<(), MediaError> {
    check(status, call).map_err(|e| {
        let detail = api
            .fns
            .nvEncGetLastErrorString
            // SAFETY: a session's pointer; the text is the driver's, read at once.
            .map(|f| unsafe { f(encoder) })
            .filter(|p| !p.is_null())
            // SAFETY: a NUL-terminated string owned by the driver.
            .map(|p| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty());
        match detail {
            Some(d) => MediaError::Backend(format!("{e} ({d})")),
            None => e,
        }
    })
}

fn status_name(status: NVENCSTATUS) -> String {
    let name = match status {
        NV_ENC_ERR_NO_ENCODE_DEVICE => "no encode device",
        NV_ENC_ERR_UNSUPPORTED_DEVICE => "unsupported device",
        NV_ENC_ERR_INVALID_ENCODERDEVICE => "invalid encoder device",
        NV_ENC_ERR_INVALID_DEVICE => "invalid device",
        NV_ENC_ERR_DEVICE_NOT_EXIST => "device gone",
        NV_ENC_ERR_INVALID_PTR => "invalid pointer",
        NV_ENC_ERR_INVALID_PARAM => "invalid parameter",
        NV_ENC_ERR_INVALID_CALL => "invalid call",
        NV_ENC_ERR_OUT_OF_MEMORY => "out of memory",
        NV_ENC_ERR_ENCODER_NOT_INITIALIZED => "encoder not initialized",
        NV_ENC_ERR_UNSUPPORTED_PARAM => "unsupported parameter",
        NV_ENC_ERR_LOCK_BUSY => "lock busy",
        NV_ENC_ERR_INVALID_VERSION => "invalid version",
        NV_ENC_ERR_GENERIC => "generic error",
        NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY => "incompatible client key",
        NV_ENC_ERR_UNIMPLEMENTED => "unimplemented",
        NV_ENC_ERR_RESOURCE_REGISTER_FAILED => "resource registration failed",
        NV_ENC_ERR_RESOURCE_NOT_REGISTERED => "resource not registered",
        NV_ENC_ERR_RESOURCE_NOT_MAPPED => "resource not mapped",
        NV_ENC_ERR_ENCODER_BUSY => "encoder busy (too many sessions?)",
        _ => return format!("NVENCSTATUS {status}"),
    };
    format!("{name} ({status})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> String {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../third_party/nv-codec-headers/nvEncodeAPI.h"
        );
        std::fs::read_to_string(path).expect("the vendored header")
    }

    /// `#define NAME (NVENCAPI_STRUCT_VERSION(n) | ( 1u<<31 ))` or without
    /// the high bit, as the header has it.
    fn header_version(text: &str, name: &str) -> u32 {
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("#define {name} ")))
            .unwrap_or_else(|| panic!("{name} isn't in the header"));
        let n: u32 = line
            .split("NVENCAPI_STRUCT_VERSION(")
            .nth(1)
            .and_then(|r| r.split(')').next())
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_else(|| panic!("can't read {line}"));
        struct_version(n) | if line.contains("1u<<31") { 1 << 31 } else { 0 }
    }

    #[test]
    fn struct_versions_match_the_header() {
        let text = header();
        for (name, ours) in [
            ("NV_ENC_CAPS_PARAM_VER", CAPS_PARAM_VER),
            (
                "NV_ENC_CREATE_BITSTREAM_BUFFER_VER",
                CREATE_BITSTREAM_BUFFER_VER,
            ),
            ("NV_ENC_CONFIG_VER", CONFIG_VER),
            ("NV_ENC_INITIALIZE_PARAMS_VER", INITIALIZE_PARAMS_VER),
            ("NV_ENC_RECONFIGURE_PARAMS_VER", RECONFIGURE_PARAMS_VER),
            ("NV_ENC_PRESET_CONFIG_VER", PRESET_CONFIG_VER),
            ("NV_ENC_PIC_PARAMS_VER", PIC_PARAMS_VER),
            ("NV_ENC_LOCK_BITSTREAM_VER", LOCK_BITSTREAM_VER),
            ("NV_ENC_MAP_INPUT_RESOURCE_VER", MAP_INPUT_RESOURCE_VER),
            ("NV_ENC_REGISTER_RESOURCE_VER", REGISTER_RESOURCE_VER),
            (
                "NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER",
                OPEN_ENCODE_SESSION_EX_PARAMS_VER,
            ),
            ("NV_ENCODE_API_FUNCTION_LIST_VER", FUNCTION_LIST_VER),
        ] {
            assert_eq!(ours, header_version(&text, name), "{name}");
        }
    }

    /// The GUID's two lines in the header, as numbers.
    fn header_guid(text: &str, name: &str) -> [u64; 11] {
        let start = text
            .find(&format!("{name} "))
            .unwrap_or_else(|| panic!("{name} isn't in the header"));
        let body = &text[start..];
        let body = &body[body.find('{').unwrap()..body.find("};").unwrap()];
        let numbers: Vec<u64> = body
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter_map(|t| t.strip_prefix("0x"))
            .map(|h| u64::from_str_radix(h, 16).unwrap())
            .collect();
        numbers.try_into().unwrap()
    }

    fn numbers(g: GUID) -> [u64; 11] {
        let mut out = [
            u64::from(g.Data1),
            u64::from(g.Data2),
            u64::from(g.Data3),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        for (o, b) in out[3..].iter_mut().zip(g.Data4) {
            *o = u64::from(b);
        }
        out
    }

    #[test]
    fn guids_match_the_header() {
        let text = header();
        for (name, ours) in [
            ("NV_ENC_CODEC_H264_GUID", CODEC_H264),
            ("NV_ENC_CODEC_HEVC_GUID", CODEC_HEVC),
            ("NV_ENC_H264_PROFILE_HIGH_GUID", H264_PROFILE_HIGH),
            ("NV_ENC_HEVC_PROFILE_MAIN_GUID", HEVC_PROFILE_MAIN),
            ("NV_ENC_PRESET_P1_GUID", PRESET_P1),
        ] {
            assert_eq!(numbers(ours), header_guid(&text, name), "{name}");
        }
    }
}
