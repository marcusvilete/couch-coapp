//! Pictures on the board (M27e, ADR 0013; docs/protocol.md, Drawing on the
//! stream): made at the sender into what the board takes ([`prepare`]),
//! checked by the host ([`check`]) and decoded by each window ([`decode`]),
//! every decode with limits, so a small file can't claim a huge picture.
//!
//! On the wire a picture is a PNG or a JPEG of at most
//! [`MAX_PICTURE_SIDE`] pixels on its longer side and [`MAX_PICTURE_BYTES`];
//! its id is the SHA-256 of those bytes ([`picture_id`]). Re-encoding at the
//! sender also drops whatever a photo carried, such as where it was taken.

use std::io::Cursor;

use couch_proto::board::{MAX_PICTURE_BYTES, MAX_PICTURE_SIDE, PictureId};
use image::{
    DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits,
    codecs::{
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType as PngFilter, PngEncoder},
    },
    imageops::FilterType,
};
use sha2::{Digest, Sha256};

/// What a sender may start from at most: a dropped file or the clipboard's
/// picture, before it is made smaller.
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
/// ...and its sides at most: a photo from a camera fits, a decode bomb
/// doesn't.
pub const MAX_INPUT_SIDE: u32 = 16_384;
/// What decoding an input may take at most (a 16,384-pixel square in RGBA
/// is 1 GiB: an input that large must be narrow).
const MAX_INPUT_ALLOC: u64 = 384 * 1024 * 1024;
/// What decoding a picture from the wire may take: its pixels in RGBA, and
/// room for the decoder's own buffers.
const MAX_WIRE_ALLOC: u64 = 4 * (MAX_PICTURE_SIDE as u64) * (MAX_PICTURE_SIDE as u64) * 2;
/// The JPEG qualities tried, best first, before a picture is made smaller.
const JPEG_QUALITIES: [u8; 4] = [85, 75, 65, 55];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PictureError {
    /// Not a picture we read (PNG, JPEG, and from a sender BMP), or broken.
    #[error("not a picture the board takes")]
    Unreadable,
    /// Larger than the limits allow: too many bytes, or too many pixels.
    #[error("too large a picture")]
    TooLarge,
}

/// A picture as the board takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    /// A PNG or a JPEG, at most [`MAX_PICTURE_BYTES`].
    pub bytes: Vec<u8>,
    pub id: PictureId,
    pub width: u32,
    pub height: u32,
}

/// A picture decoded: its size and its pixels, RGBA, row after row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A picture's id: the SHA-256 of its bytes.
pub fn picture_id(bytes: &[u8]) -> PictureId {
    PictureId(Sha256::digest(bytes).into())
}

/// The sender's side: a picture from the clipboard or a file (PNG, JPEG or
/// BMP) made into what the board takes. Turned as a photo says it was
/// taken, scaled down to [`MAX_PICTURE_SIDE`] on its longer side, and
/// encoded again: a JPEG stays a JPEG (a photo), anything else becomes a
/// PNG, or a JPEG if that PNG is too large and has nothing see-through.
/// Smaller still, if needed, until it fits [`MAX_PICTURE_BYTES`].
pub fn prepare(input: &[u8]) -> Result<Prepared, PictureError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(PictureError::TooLarge);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_INPUT_SIDE);
    limits.max_image_height = Some(MAX_INPUT_SIDE);
    limits.max_alloc = Some(MAX_INPUT_ALLOC);
    let (format, mut image) = read(
        input,
        &[ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Bmp],
        limits,
        true,
    )?;
    if format == ImageFormat::Bmp {
        // A 32-bit bitmap from the clipboard often has nothing in its alpha:
        // that is an opaque picture, not an invisible one.
        image = opaque_if_invisible(image);
    }
    let longer = image.width().max(image.height());
    if longer > MAX_PICTURE_SIDE {
        image = image.resize(MAX_PICTURE_SIDE, MAX_PICTURE_SIDE, FilterType::Triangle);
    }
    let see_through = image.has_alpha() && !fully_opaque(&image);
    let photo = format == ImageFormat::Jpeg;
    loop {
        if !photo && let Some(png) = png(&image)? {
            return Ok(done(png, &image));
        }
        if !see_through {
            for quality in JPEG_QUALITIES {
                let jpeg = jpeg(&image, quality)?;
                if jpeg.len() <= MAX_PICTURE_BYTES {
                    return Ok(done(jpeg, &image));
                }
            }
        }
        // Still too large: three quarters the size, and again.
        let (w, h) = (image.width() * 3 / 4, image.height() * 3 / 4);
        if w < 16 || h < 16 {
            return Err(PictureError::TooLarge);
        }
        image = image.resize_exact(w, h, FilterType::Triangle);
    }
}

/// The host's side: whether `bytes` are a picture the board takes (a PNG or
/// a JPEG within the limits that decodes whole), and its size.
pub fn check(bytes: &[u8]) -> Result<(u32, u32), PictureError> {
    let decoded = decode(bytes)?;
    Ok((decoded.width, decoded.height))
}

/// A window's side, and the host's: a picture from the wire decoded, with
/// the same limits as [`check`].
pub fn decode(bytes: &[u8]) -> Result<Decoded, PictureError> {
    if bytes.is_empty() || bytes.len() > MAX_PICTURE_BYTES {
        return Err(PictureError::TooLarge);
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PICTURE_SIDE);
    limits.max_image_height = Some(MAX_PICTURE_SIDE);
    limits.max_alloc = Some(MAX_WIRE_ALLOC);
    let (_, image) = read(bytes, &[ImageFormat::Png, ImageFormat::Jpeg], limits, false)?;
    let rgba = image.into_rgba8();
    Ok(Decoded {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Decodes `bytes` if their format is one of `formats`, within `limits`;
/// turned as the picture says (`orient`) for a sender's input.
fn read(
    bytes: &[u8],
    formats: &[ImageFormat],
    limits: Limits,
    orient: bool,
) -> Result<(ImageFormat, DynamicImage), PictureError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| PictureError::Unreadable)?;
    let format = reader
        .format()
        .filter(|f| formats.contains(f))
        .ok_or(PictureError::Unreadable)?;
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(too_large_or_unreadable)?;
    let orientation = if orient {
        decoder.orientation().ok()
    } else {
        None
    };
    let mut image = DynamicImage::from_decoder(decoder).map_err(too_large_or_unreadable)?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    if image.width() == 0 || image.height() == 0 {
        return Err(PictureError::Unreadable);
    }
    Ok((format, image))
}

fn too_large_or_unreadable(e: image::ImageError) -> PictureError {
    match e {
        image::ImageError::Limits(_) => PictureError::TooLarge,
        _ => PictureError::Unreadable,
    }
}

fn done(bytes: Vec<u8>, image: &DynamicImage) -> Prepared {
    Prepared {
        id: picture_id(&bytes),
        bytes,
        width: image.width(),
        height: image.height(),
    }
}

/// The picture as a PNG, if it fits.
fn png(image: &DynamicImage) -> Result<Option<Vec<u8>>, PictureError> {
    let mut out = Vec::new();
    let encoder =
        PngEncoder::new_with_quality(&mut out, CompressionType::Default, PngFilter::Adaptive);
    let image = if image.has_alpha() {
        DynamicImage::ImageRgba8(image.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(image.to_rgb8())
    };
    image
        .write_with_encoder(encoder)
        .map_err(|_| PictureError::Unreadable)?;
    Ok((out.len() <= MAX_PICTURE_BYTES).then_some(out))
}

fn jpeg(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, PictureError> {
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(image.to_rgb8())
        .write_with_encoder(JpegEncoder::new_with_quality(&mut out, quality))
        .map_err(|_| PictureError::Unreadable)?;
    Ok(out)
}

fn fully_opaque(image: &DynamicImage) -> bool {
    image.to_rgba8().pixels().all(|p| p.0[3] == u8::MAX)
}

fn opaque_if_invisible(image: DynamicImage) -> DynamicImage {
    if !image.has_alpha() {
        return image;
    }
    let mut rgba = image.into_rgba8();
    if rgba.pixels().all(|p| p.0[3] == 0) {
        for p in rgba.pixels_mut() {
            p.0[3] = u8::MAX;
        }
    }
    DynamicImage::ImageRgba8(rgba)
}

#[cfg(test)]
mod tests {
    use image::{ImageEncoder, Rgb, RgbImage, Rgba, RgbaImage};

    use super::*;

    fn encode_png(image: &DynamicImage) -> Vec<u8> {
        let mut out = Vec::new();
        image.write_with_encoder(PngEncoder::new(&mut out)).unwrap();
        out
    }

    /// Noise: what compresses worst, like a photo's detail.
    fn noise(w: u32, h: u32) -> RgbImage {
        let mut x: u32 = 0x1234_5678;
        RgbImage::from_fn(w, h, |_, _| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            Rgb([x as u8, (x >> 8) as u8, (x >> 16) as u8])
        })
    }

    #[test]
    fn a_small_picture_stays_its_size_as_a_png() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(40, 30, Rgb([200, 10, 10])));
        let p = prepare(&encode_png(&image)).unwrap();
        assert_eq!((p.width, p.height), (40, 30));
        assert_eq!(&p.bytes[..4], b"\x89PNG");
        assert_eq!(p.id, picture_id(&p.bytes));
        assert_eq!(check(&p.bytes), Ok((40, 30)));
        let d = decode(&p.bytes).unwrap();
        assert_eq!(&d.rgba[..4], &[200, 10, 10, 255]);
    }

    #[test]
    fn a_large_picture_is_scaled_to_the_longer_side() {
        let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(4_000, 1_000, Rgb([1, 2, 3])));
        let p = prepare(&encode_png(&image)).unwrap();
        assert_eq!((p.width, p.height), (MAX_PICTURE_SIDE, 320));
        assert_eq!(check(&p.bytes), Ok((MAX_PICTURE_SIDE, 320)));
    }

    #[test]
    fn a_photo_stays_a_jpeg_and_fits() {
        let mut jpeg = Vec::new();
        DynamicImage::ImageRgb8(noise(3_000, 2_000))
            .write_with_encoder(JpegEncoder::new_with_quality(&mut jpeg, 95))
            .unwrap();
        assert!(jpeg.len() > MAX_PICTURE_BYTES);
        let p = prepare(&jpeg).unwrap();
        assert_eq!(&p.bytes[..2], &[0xff, 0xd8], "a JPEG");
        assert!(p.bytes.len() <= MAX_PICTURE_BYTES);
        assert!(p.width.max(p.height) <= MAX_PICTURE_SIDE);
        assert!(check(&p.bytes).is_ok());
    }

    #[test]
    fn a_large_opaque_png_becomes_a_jpeg_and_a_see_through_one_shrinks() {
        let opaque = DynamicImage::ImageRgb8(noise(1_200, 1_200));
        let p = prepare(&encode_png(&opaque)).unwrap();
        assert_eq!(&p.bytes[..2], &[0xff, 0xd8], "a JPEG");
        assert!(p.bytes.len() <= MAX_PICTURE_BYTES);

        let rgb = noise(1_200, 1_200);
        let see_through = DynamicImage::ImageRgba8(RgbaImage::from_fn(1_200, 1_200, |x, y| {
            let Rgb([r, g, b]) = *rgb.get_pixel(x, y);
            Rgba([r, g, b, if x < 600 { 255 } else { 90 }])
        }));
        let p = prepare(&encode_png(&see_through)).unwrap();
        assert_eq!(&p.bytes[..4], b"\x89PNG", "still see-through");
        assert!(p.bytes.len() <= MAX_PICTURE_BYTES);
        assert!(p.width < 1_200, "made smaller");
        let d = decode(&p.bytes).unwrap();
        assert!(d.rgba.chunks(4).any(|px| px[3] < 255));
    }

    #[test]
    fn a_bitmap_with_nothing_in_its_alpha_is_opaque() {
        let mut bmp = Vec::new();
        image::codecs::bmp::BmpEncoder::new(&mut bmp)
            .write_image(
                RgbaImage::from_pixel(8, 8, Rgba([10, 20, 30, 0])).as_raw(),
                8,
                8,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        let p = prepare(&bmp).unwrap();
        let d = decode(&p.bytes).unwrap();
        assert_eq!(&d.rgba[..4], &[10, 20, 30, 255]);
    }

    /// The decode bomb: a small file whose header claims a huge picture is
    /// refused before anything that size is made.
    #[test]
    fn a_small_file_claiming_huge_dimensions_is_refused() {
        // A real PNG of 1 by 1, its header then saying 60,000 by 60,000 (the
        // header's checksum mended so only the size is wrong).
        let mut png = encode_png(&DynamicImage::ImageRgb8(RgbImage::new(1, 1)));
        png[16..20].copy_from_slice(&60_000u32.to_be_bytes());
        png[20..24].copy_from_slice(&60_000u32.to_be_bytes());
        let crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(check(&png), Err(PictureError::TooLarge));
        assert_eq!(prepare(&png), Err(PictureError::TooLarge));

        // Within the input's sides but not the wire's: the host refuses it.
        let wide = encode_png(&DynamicImage::ImageRgb8(RgbImage::new(
            MAX_PICTURE_SIDE + 1,
            1,
        )));
        assert_eq!(check(&wide), Err(PictureError::TooLarge));
    }

    #[test]
    fn only_png_and_jpeg_come_from_the_wire() {
        let mut bmp = Vec::new();
        image::codecs::bmp::BmpEncoder::new(&mut bmp)
            .write_image(&[0; 3 * 4], 2, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert_eq!(check(&bmp), Err(PictureError::Unreadable));
        assert_eq!(check(b"not a picture"), Err(PictureError::Unreadable));
        assert_eq!(check(&[]), Err(PictureError::TooLarge));
        assert_eq!(
            check(&vec![0; MAX_PICTURE_BYTES + 1]),
            Err(PictureError::TooLarge)
        );
        // Cut short: refused, not half shown.
        let png = encode_png(&DynamicImage::ImageRgb8(noise(64, 64)));
        assert_eq!(check(&png[..png.len() / 2]), Err(PictureError::Unreadable));
    }

    #[test]
    fn a_photo_is_turned_as_it_was_taken_and_loses_its_metadata() {
        // A JPEG 4 wide and 2 high whose EXIF says "turn it a quarter".
        let mut jpeg = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut jpeg, 90);
        encoder.set_exif_metadata(exif_rotated_90()).expect("EXIF");
        encoder
            .write_image(noise(4, 2).as_raw(), 4, 2, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert!(jpeg.windows(4).any(|w| w == b"Exif"));
        let p = prepare(&jpeg).unwrap();
        assert_eq!((p.width, p.height), (2, 4), "turned");
        assert!(!p.bytes.windows(4).any(|w| w == b"Exif"), "no metadata");
    }

    /// A minimal EXIF block, big-endian, with one entry: orientation 6.
    fn exif_rotated_90() -> Vec<u8> {
        let mut e = Vec::new();
        e.extend_from_slice(b"MM\x00\x2a\x00\x00\x00\x08");
        e.extend_from_slice(&1u16.to_be_bytes()); // one entry
        e.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
        e.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        e.extend_from_slice(&1u32.to_be_bytes()); // one value
        e.extend_from_slice(&[0, 6, 0, 0]); // 6: turned 90° clockwise
        e.extend_from_slice(&0u32.to_be_bytes()); // no next directory
        e
    }

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
}
