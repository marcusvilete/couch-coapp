//! A timestamp drawn into frames as black and white cells, so a viewer can
//! read the capture time back out of decoded video and measure latency
//! end to end, including capture and encode.
//!
//! Layout, left to right: a white sync cell, a black sync cell, then 64 data
//! cells (MSB first): a 48-bit value followed by a 16-bit CRC. Cells are
//! square and large enough to survive H.264 compression; 16 px matches the
//! codec's macroblocks. The CRC keeps whole-screen scans from finding
//! barcodes in ordinary pixels.

use crate::BgraFrame;

pub const DATA_BITS: u32 = 64;
pub const SYNC_CELLS: u32 = 2;
pub const TOTAL_CELLS: u32 = SYNC_CELLS + DATA_BITS;

/// Largest value that fits (48 bits, about 8.9 years of microseconds of uptime).
pub const MAX_VALUE: u64 = (1 << 48) - 1;

/// Width in pixels of a barcode drawn with `cell`-pixel cells.
pub fn width(cell: u32) -> u32 {
    TOTAL_CELLS * cell
}

/// CRC-16/CCITT-FALSE over the value's six bytes.
fn checksum(value: u64) -> u16 {
    value.to_be_bytes()[2..]
        .iter()
        .fold(0xffff_u16, |mut crc, &byte| {
            crc ^= u16::from(byte) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x1021
                } else {
                    crc << 1
                };
            }
            crc
        })
}

fn bits(value: u64) -> u64 {
    debug_assert!(value <= MAX_VALUE);
    ((value & MAX_VALUE) << 16) | u64::from(checksum(value & MAX_VALUE))
}

/// The cells encoding `value`, left to right; `true` is white.
pub fn cells(value: u64) -> impl Iterator<Item = bool> {
    let payload = bits(value);
    (0..TOTAL_CELLS).map(move |i| match i {
        0 => true,
        1 => false,
        _ => (payload >> (DATA_BITS - 1 - (i - SYNC_CELLS))) & 1 == 1,
    })
}

/// Draws `value` into a BGRA buffer at `(x, y)`. The caller ensures the
/// barcode fits: `x + width(cell) <= frame width` and `y + cell <= height`.
pub fn draw(data: &mut [u8], stride: u32, x: u32, y: u32, cell: u32, value: u64) {
    for (i, white) in (0u32..).zip(cells(value)) {
        let shade = if white { 255 } else { 0 };
        for row in y..y + cell {
            let start = (row * stride + (x + i * cell) * 4) as usize;
            for px in data[start..start + (cell * 4) as usize]
                .as_chunks_mut::<4>()
                .0
            {
                *px = [shade, shade, shade, 255];
            }
        }
    }
}

/// Reads a value drawn by [`draw`] at the same position and cell size.
/// Returns `None` if the sync cells or the checksum don't match.
pub fn read(frame: &BgraFrame, x: u32, y: u32, cell: u32) -> Option<u64> {
    if x + width(cell) > frame.width || y + cell > frame.height {
        return None;
    }
    read_with(
        |px, py| {
            let [b, g, r, _] = frame.pixel(px, py);
            // Integer luma approximation (BT.601).
            ((u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000) as u8
        },
        x,
        y,
        cell,
    )
}

/// Reads a barcode from a luma plane (the Y plane of a decoded NV12 frame).
pub fn read_luma(
    plane: &[u8],
    stride: usize,
    (width_px, height_px): (u32, u32),
    x: u32,
    y: u32,
    cell: u32,
) -> Option<u64> {
    if x + width(cell) > width_px || y + cell > height_px {
        return None;
    }
    read_with(
        |px, py| {
            plane
                .get(py as usize * stride + px as usize)
                .copied()
                .unwrap_or(0)
        },
        x,
        y,
        cell,
    )
}

/// Finds a barcode within `search` pixels of the top-left corner of a luma
/// plane, for when the capture adds an unknown offset (window borders).
/// Returns its position and value.
pub fn locate_luma(
    plane: &[u8],
    stride: usize,
    size: (u32, u32),
    cell: u32,
    search: u32,
) -> Option<(u32, u32, u64)> {
    (0..search).find_map(|y| {
        (0..search).find_map(|x| read_luma(plane, stride, size, x, y, cell).map(|v| (x, y, v)))
    })
}

fn read_with(luma_at: impl Fn(u32, u32) -> u8, x: u32, y: u32, cell: u32) -> Option<u64> {
    let lit = |i: u32| luma_at(x + i * cell + cell / 2, y + cell / 2) > 127;
    if !lit(0) || lit(1) {
        return None;
    }
    let payload = (0..DATA_BITS).fold(0u64, |acc, i| (acc << 1) | u64::from(lit(SYNC_CELLS + i)));
    let value = payload >> 16;
    (checksum(value) == payload as u16).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Micros;
    use proptest::prelude::*;

    fn frame_with(value: u64, cell: u32) -> BgraFrame {
        let (w, h) = (width(cell) + 10, cell + 6);
        let stride = w * 4;
        let mut data = vec![128u8; (stride * h) as usize];
        draw(&mut data, stride, 5, 3, cell, value);
        BgraFrame {
            width: w,
            height: h,
            stride,
            data: data.into(),
            captured_at: Micros(0),
        }
    }

    #[test]
    fn reads_back_what_was_drawn() {
        let frame = frame_with(123_456_789, 16);
        assert_eq!(read(&frame, 5, 3, 16), Some(123_456_789));
    }

    #[test]
    fn luma_plane_reads_and_locates() {
        let frame = frame_with(987_654_321, 8);
        // Build a luma plane from the BGRA frame, as a decoder would produce.
        let luma: Vec<u8> = (0..frame.height)
            .flat_map(|y| (0..frame.width).map(move |x| (x, y)))
            .map(|(x, y)| frame.pixel(x, y)[0])
            .collect();
        let size = (frame.width, frame.height);
        let stride = frame.width as usize;
        assert_eq!(read_luma(&luma, stride, size, 5, 3, 8), Some(987_654_321));
        // Any offset whose sample points land inside the cells reads the same
        // value, so `locate` may find a position near (5, 3) rather than on it.
        let (x, y, value) = locate_luma(&luma, stride, size, 8, 8).expect("barcode found");
        assert_eq!(value, 987_654_321);
        assert_eq!(read_luma(&luma, stride, size, x, y, 8), Some(value));
    }

    #[test]
    fn wrong_position_reads_nothing() {
        let frame = frame_with(42, 8);
        assert_eq!(read(&frame, 0, 0, 8), None);
    }

    proptest! {
        #[test]
        fn roundtrip_any(value in 0..=MAX_VALUE, cell in 2u32..20) {
            let frame = frame_with(value, cell);
            prop_assert_eq!(read(&frame, 5, 3, cell), Some(value));
        }
    }
}
