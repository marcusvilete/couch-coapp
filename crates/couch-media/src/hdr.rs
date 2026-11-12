//! A host whose monitor is in HDR (M29): what an SDR picture of it holds.
//!
//! Windows composes such a monitor in scRGB (linear, BT.709 primaries, 1.0
//! being 80 nits), SDR content raised to the monitor's SDR white level. The
//! host captures it so and converts it to the SDR picture viewers get:
//! divided by the SDR white (scRGB), clipped to [0, 1] (brighter than SDR
//! white is white, colours outside BT.709 at its edge; the author's choice),
//! then the sRGB curve. Windows' own 8-bit capture clips HDR content at 80
//! nits instead, without the SDR white: a game in HDR came out washed out.
//!
//! [`scrgb_to_srgb`] is the reference the GPU's conversion is checked against
//! (`couch_platform_win::tonemap`); keep the two alike.

/// The nits scRGB's 1.0 stands for.
pub const SCRGB_NITS: f32 = 80.0;

/// The scRGB value of an SDR white level in nits (240 nits: 3.0); 1.0 for a
/// level unknown or below the reference.
pub fn white_scale(sdr_white_nits: Option<f32>) -> f32 {
    sdr_white_nits
        .map(|nits| nits / SCRGB_NITS)
        .filter(|s| s.is_finite() && *s >= 1.0)
        .unwrap_or(1.0)
}

/// A linear scRGB colour as the sRGB-encoded values (0 to 1) of an SDR
/// picture whose white is `white` (scRGB, from [`white_scale`]).
pub fn scrgb_to_srgb(rgb: [f32; 3], white: f32) -> [f32; 3] {
    rgb.map(|v| srgb_encode(v / white))
}

/// The sRGB curve, linear to encoded, clipped to [0, 1] first.
pub fn srgb_encode(v: f32) -> f32 {
    // NaN clips to black, as the GPU's `saturate` does.
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.001)
    }

    #[test]
    fn sdr_content_comes_back_as_it_was() {
        // What hdr-probe measured: the 75% grey of colour bars, raised to an
        // SDR white of 240 nits, reads 1.562 in an FP16 capture.
        assert!(near(scrgb_to_srgb([1.562; 3], 3.0), [0.749; 3]));
        // 50% sRGB grey (linear 0.214) at that white.
        assert!(near(scrgb_to_srgb([0.642; 3], 3.0), [0.5; 3]));
        assert!(near(scrgb_to_srgb([3.0, 0.0, 0.0], 3.0), [1.0, 0.0, 0.0]));
        assert_eq!(scrgb_to_srgb([0.0; 3], 3.0), [0.0; 3]);
    }

    #[test]
    fn brighter_than_sdr_white_is_white_and_wide_colours_clip() {
        assert!(near(scrgb_to_srgb([12.5; 3], 3.0), [1.0; 3]));
        // BT.2020's green: its negative red and blue clip to none.
        assert!(near(
            scrgb_to_srgb([-1.763, 3.399, -0.302], 3.0),
            [0.0, 1.0, 0.0]
        ));
        assert!(near(
            scrgb_to_srgb([f32::NAN, f32::INFINITY, -1.0], 3.0),
            [0.0, 1.0, 0.0]
        ));
    }

    #[test]
    fn hdr_greys_below_sdr_white_keep_their_place() {
        // 80 and 160 nits under a 240-nit white: what Windows' 8-bit capture
        // makes white, the conversion keeps grey (hdr-probe --pattern).
        assert!(near(scrgb_to_srgb([1.0; 3], 3.0), [0.613; 3]));
        assert!(near(scrgb_to_srgb([2.0; 3], 3.0), [0.836; 3]));
    }

    #[test]
    fn the_white_scale_is_never_below_the_reference() {
        assert_eq!(white_scale(Some(240.0)), 3.0);
        assert_eq!(white_scale(Some(80.0)), 1.0);
        assert_eq!(white_scale(Some(40.0)), 1.0);
        assert_eq!(white_scale(Some(f32::NAN)), 1.0);
        assert_eq!(white_scale(None), 1.0);
    }
}
