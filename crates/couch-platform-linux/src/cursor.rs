//! The pointer drawn into a shared picture by us (a Linux host): the
//! screen-sharing portal sends it beside the picture (the cursor as
//! metadata), since a shared window sends a new picture only when it
//! redraws, and a pointer drawn in by the desktop then moved at a couple of
//! frames a second over a still window (GNOME, M16c-2's VM run). Drawing it
//! ourselves, a pointer that moves makes a new frame: the last picture with
//! the pointer put back where it was and drawn where it is.
//!
//! Pure pixels: pictures are packed BGRA (`width * 4` bytes a row), shapes
//! BGRA with straight alpha.

/// The largest pointer shape drawn, in pixels each way: Mutter's own
/// largest (KWin's is 256).
pub(crate) const MAX_SHAPE: u32 = 384;

/// How a shape's bitmap orders its channels, in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channels {
    Bgra,
    Rgba,
    Argb,
    Abgr,
}

/// A pointer's picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shape {
    width: u32,
    height: u32,
    /// The point of the shape that is the pointer's position.
    hotspot: (i32, i32),
    /// BGRA, straight alpha, packed.
    pixels: Vec<u8>,
}

impl Shape {
    /// From a bitmap of `width` by `height` pixels in `channels` order, rows
    /// `stride` bytes apart. `None` for an empty or oversized one, or `data`
    /// too short for it.
    pub(crate) fn new(
        channels: Channels,
        width: u32,
        height: u32,
        stride: usize,
        hotspot: (i32, i32),
        data: &[u8],
    ) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_SHAPE || height > MAX_SHAPE {
            return None;
        }
        let row = width as usize * 4;
        if stride < row {
            return None;
        }
        let data = data.get(..stride * (height as usize - 1) + row)?;
        let order: [usize; 4] = match channels {
            Channels::Bgra => [0, 1, 2, 3],
            Channels::Rgba => [2, 1, 0, 3],
            Channels::Argb => [3, 2, 1, 0],
            Channels::Abgr => [1, 2, 3, 0],
        };
        let mut pixels = Vec::with_capacity(row * height as usize);
        for line in data.chunks(stride).take(height as usize) {
            for p in line[..row].as_chunks::<4>().0 {
                pixels.extend(order.map(|i| p[i]));
            }
        }
        Some(Self {
            width,
            height,
            hotspot,
            pixels,
        })
    }
}

/// What a drawn pointer covered, to put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Under {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Draws `shape` with its hotspot at `at` into `picture` (`width` by
/// `height`, packed BGRA), cut to the picture; returns what it covered, or
/// `None` if none of it is inside.
pub(crate) fn draw(
    picture: &mut [u8],
    width: u32,
    height: u32,
    shape: &Shape,
    at: (i32, i32),
) -> Option<Under> {
    if picture.len() < width as usize * height as usize * 4 {
        return None;
    }
    // The shape's corner on the picture, and the part of it inside.
    let left = i64::from(at.0) - i64::from(shape.hotspot.0);
    let top = i64::from(at.1) - i64::from(shape.hotspot.1);
    let x0 = left.max(0);
    let y0 = top.max(0);
    let x1 = (left + i64::from(shape.width)).min(i64::from(width));
    let y1 = (top + i64::from(shape.height)).min(i64::from(height));
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let (x0, y0) = (x0 as usize, y0 as usize);
    let (sx, sy) = ((x0 as i64 - left) as usize, (y0 as i64 - top) as usize);
    let mut under = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let at = ((y0 + row) * width as usize + x0) * 4;
        let line = &mut picture[at..at + w * 4];
        under.extend_from_slice(line);
        let from = ((sy + row) * shape.width as usize + sx) * 4;
        let source = &shape.pixels[from..from + w * 4];
        for (dst, src) in line
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(source.as_chunks::<4>().0)
        {
            let alpha = u32::from(src[3]);
            for c in 0..3 {
                let blended =
                    (u32::from(src[c]) * alpha + u32::from(dst[c]) * (255 - alpha) + 127) / 255;
                dst[c] = blended as u8;
            }
        }
    }
    Some(Under {
        x: x0 as u32,
        y: y0 as u32,
        width: w as u32,
        height: h as u32,
        pixels: under,
    })
}

/// Puts back what a pointer drawn into `picture` (`width` pixels a row)
/// covered.
pub(crate) fn restore(picture: &mut [u8], width: u32, under: &Under) {
    let row = under.width as usize * 4;
    for (i, line) in under.pixels.chunks(row).enumerate() {
        let at = ((under.y as usize + i) * width as usize + under.x as usize) * 4;
        if let Some(dst) = picture.get_mut(at..at + row) {
            dst.copy_from_slice(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width` by `height` grey picture.
    fn grey(width: u32, height: u32) -> Vec<u8> {
        vec![100; width as usize * height as usize * 4]
    }

    fn pixel(picture: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * width + x) * 4) as usize;
        picture[at..at + 4].try_into().unwrap()
    }

    /// A 2x2 shape: opaque red, half-transparent white, clear, opaque blue
    /// (BGRA), its hotspot at its corner.
    fn shape() -> Shape {
        Shape::new(
            Channels::Bgra,
            2,
            2,
            8,
            (0, 0),
            &[
                0, 0, 255, 255, 255, 255, 255, 128, 9, 9, 9, 0, 255, 0, 0, 255,
            ],
        )
        .unwrap()
    }

    #[test]
    fn a_pointer_is_drawn_blended_and_put_back() {
        let (width, height) = (8, 6);
        let mut picture = grey(width, height);
        let clean = picture.clone();
        let under = draw(&mut picture, width, height, &shape(), (3, 2)).unwrap();
        assert_eq!(pixel(&picture, width, 3, 2), [0, 0, 255, 100]);
        // Half white over grey; the picture's own alpha is left alone.
        assert_eq!(pixel(&picture, width, 4, 2), [178, 178, 178, 100]);
        assert_eq!(pixel(&picture, width, 3, 3), [100, 100, 100, 100]);
        assert_eq!(pixel(&picture, width, 4, 3), [255, 0, 0, 100]);
        assert_eq!(pixel(&picture, width, 5, 2), [100, 100, 100, 100]);
        restore(&mut picture, width, &under);
        assert_eq!(picture, clean);
    }

    #[test]
    fn the_hotspot_places_it_and_the_edges_cut_it() {
        let (width, height) = (8, 6);
        let mut picture = grey(width, height);
        let clean = picture.clone();
        let mut s = shape();
        s.hotspot = (1, 1);
        // Hotspot on the corner pixel: only the shape's last pixel is in.
        let under = draw(&mut picture, width, height, &s, (0, 0)).unwrap();
        assert_eq!(pixel(&picture, width, 0, 0), [255, 0, 0, 100]);
        assert_eq!((under.width, under.height), (1, 1));
        restore(&mut picture, width, &under);
        // Past the far corner: only its first pixel.
        let under = draw(&mut picture, width, height, &s, (8, 6)).unwrap();
        assert_eq!(pixel(&picture, width, 7, 5), [0, 0, 255, 100]);
        restore(&mut picture, width, &under);
        assert_eq!(picture, clean);
        // Wholly outside: nothing drawn.
        assert_eq!(draw(&mut picture, width, height, &s, (-5, 3)), None);
        assert_eq!(draw(&mut picture, width, height, &s, (3, 40)), None);
        assert_eq!(
            draw(&mut picture, width, height, &s, (i32::MAX, i32::MIN)),
            None
        );
        assert_eq!(picture, clean);
    }

    #[test]
    fn channel_orders_become_bgra() {
        let red = |channels, bytes: [u8; 4]| {
            Shape::new(channels, 1, 1, 4, (0, 0), &bytes)
                .unwrap()
                .pixels
        };
        let want = vec![10, 20, 30, 40];
        assert_eq!(red(Channels::Bgra, [10, 20, 30, 40]), want);
        assert_eq!(red(Channels::Rgba, [30, 20, 10, 40]), want);
        assert_eq!(red(Channels::Argb, [40, 30, 20, 10]), want);
        assert_eq!(red(Channels::Abgr, [40, 10, 20, 30]), want);
    }

    #[test]
    fn bad_bitmaps_are_refused() {
        let data = [0u8; 64];
        assert!(Shape::new(Channels::Bgra, 0, 1, 4, (0, 0), &data).is_none());
        assert!(Shape::new(Channels::Bgra, MAX_SHAPE + 1, 1, 4096, (0, 0), &[0; 4096]).is_none());
        // A stride shorter than a row, or data too short.
        assert!(Shape::new(Channels::Bgra, 4, 1, 8, (0, 0), &data).is_none());
        assert!(Shape::new(Channels::Bgra, 4, 5, 16, (0, 0), &data).is_none());
        // Rows apart with padding: the last row needs no padding after it.
        assert!(Shape::new(Channels::Bgra, 2, 4, 16, (0, 0), &data[..56]).is_some());
    }
}
