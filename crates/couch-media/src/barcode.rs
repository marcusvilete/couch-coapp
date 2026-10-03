//! A timestamp drawn into frames as black and white cells, so a viewer can
//! read the capture time back out of decoded video and measure latency
//! end to end, including capture and encode.
//!
//! Layout, left to right: a white sync cell, a black sync cell, then 64 data
//! cells (MSB first): a 56-bit value followed by an 8-bit checksum. Cells are
//! square and large enough to survive H.264 compression; 16 px matches the
//! codec's macroblocks.

use crate::BgraFrame;

pub const DATA_BITS: u32 = 64;
pub const SYNC_CELLS: u32 = 2;
pub const TOTAL_CELLS: u32 = SYNC_CELLS + DATA_BITS;

/// Largest value that fits (56 bits, about 2283 years of microseconds).
pub const MAX_VALUE: u64 = (1 << 56) - 1;

/// Width in pixels of a barcode drawn with `cell`-pixel cells.
pub fn width(cell: u32) -> u32 {
    TOTAL_CELLS * cell
}

fn checksum(value: u64) -> u8 {
    value.to_be_bytes()[1..]
        .iter()
        .fold(0x5a_u8, |acc, b| acc.rotate_left(1) ^ b)
}

fn bits(value: u64) -> u64 {
    debug_assert!(value <= MAX_VALUE);
    ((value & MAX_VALUE) << 8) | u64::from(checksum(value & MAX_VALUE))
}

/// Draws `value` into a BGRA buffer at `(x, y)`. The caller ensures the
/// barcode fits: `x + width(cell) <= frame width` and `y + cell <= height`.
pub fn draw(data: &mut [u8], stride: u32, x: u32, y: u32, cell: u32, value: u64) {
    let payload = bits(value);
    for i in 0..TOTAL_CELLS {
        let white = match i {
            0 => true,
            1 => false,
            _ => (payload >> (DATA_BITS - 1 - (i - SYNC_CELLS))) & 1 == 1,
        };
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
    let lit = |i: u32| {
        let [b, g, r, _] = frame.pixel(x + i * cell + cell / 2, y + cell / 2);
        // Integer luma approximation (BT.601).
        (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 > 127
    };
    if !lit(0) || lit(1) {
        return None;
    }
    let payload = (0..DATA_BITS).fold(0u64, |acc, i| (acc << 1) | u64::from(lit(SYNC_CELLS + i)));
    let value = payload >> 8;
    (checksum(value) == payload as u8).then_some(value)
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
