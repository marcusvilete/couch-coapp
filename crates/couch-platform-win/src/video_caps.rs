//! Which video codecs this PC decodes in hardware (M12c), for a viewer to
//! tell the host: the default adapter's D3D11 video decoder profiles. The
//! stream window decodes on that adapter (D3D12VA or D3D11VA).

use std::sync::OnceLock;

use windows::Win32::{
    Foundation::HMODULE,
    Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
        Direct3D11::{
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
            D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11VideoDevice,
        },
    },
};
use windows_core::{GUID, Interface};

/// Whether the GPU decodes HEVC Main (8-bit). Asked once per process.
pub fn hevc_decode() -> bool {
    static HEVC: OnceLock<bool> = OnceLock::new();
    *HEVC.get_or_init(|| {
        let found = has_profile(&D3D11_DECODER_PROFILE_HEVC_VLD_MAIN);
        tracing::info!(
            "HEVC decoding in hardware: {}",
            if found { "yes" } else { "no" }
        );
        found
    })
}

fn has_profile(wanted: &GUID) -> bool {
    let mut device = None;
    // SAFETY: out-parameters are live locals.
    let created = unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    };
    let Some(device) = created.ok().and(device) else {
        return false;
    };
    let Ok(video) = device.cast::<ID3D11VideoDevice>() else {
        return false;
    };
    // SAFETY: plain getters on a live video device; indices below the count.
    unsafe {
        (0..video.GetVideoDecoderProfileCount())
            .any(|i| video.GetVideoDecoderProfile(i).is_ok_and(|p| p == *wanted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This PC's answer; needs a GPU. By hand:
    /// `cargo test -p couch-platform-win video_caps -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a GPU"]
    fn this_gpu_says_whether_it_decodes_hevc() {
        eprintln!("HEVC decode: {}", hevc_decode());
    }
}
