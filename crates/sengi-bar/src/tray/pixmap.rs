//! Icons that arrive over the bus as raw pixels.
//!
//! `IconPixmap` is a list of images, each a width, a height and
//! `width * height` pixels of ARGB32 in network byte order. All three
//! numbers come from the app, so nothing is allocated until they agree with
//! each other and with our limits.

/// Largest pixmap we will look at, in pixels per side.
pub const MAX_SIDE: u32 = 1024;

/// How many of an item's pixmaps are considered. Real items send a handful.
pub const MAX_PIXMAPS: usize = 16;

/// Largest icon we will produce, in pixels per side.
pub const MAX_ICON: u32 = 256;

/// One entry of an `IconPixmap` array, as received.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixmap {
    pub width: i32,
    pub height: i32,
    pub data: Vec<u8>,
}

/// A square icon ready to draw: `size` by `size` pixels of premultiplied
/// RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub size: u32,
    pub data: Vec<u8>,
}

/// The size of a pixmap, if the claimed one is sane and matches the amount
/// of data that came with it.
fn checked_size(width: i32, height: i32, len: usize) -> Option<(u32, u32)> {
    let width = u32::try_from(width).ok()?;
    let height = u32::try_from(height).ok()?;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let bytes = u64::from(width) * u64::from(height) * 4;
    (bytes == len as u64).then_some((width, height))
}

/// Whether a pixmap of this claimed size and this much data is worth
/// keeping. Asked before the data is copied anywhere.
pub fn plausible(width: i32, height: i32, len: usize) -> bool {
    checked_size(width, height, len).is_some()
}

impl Pixmap {
    fn size(&self) -> Option<(u32, u32)> {
        checked_size(self.width, self.height, self.data.len())
    }
}

/// Lower is better: the exact size, then the nearest larger one (shrinking
/// keeps an icon sharp), then the largest smaller one.
fn rank(side: u32, wanted: u32) -> (u8, u32) {
    match side {
        side if side == wanted => (0, 0),
        side if side > wanted => (1, side - wanted),
        side => (2, wanted - side),
    }
}

/// The pixmap to draw at `wanted` pixels, among the usable ones.
pub fn choose(pixmaps: &[Pixmap], wanted: u32) -> Option<&Pixmap> {
    pixmaps
        .iter()
        .take(MAX_PIXMAPS)
        .filter_map(|pixmap| {
            let (width, height) = pixmap.size()?;
            Some((rank(width.max(height), wanted), pixmap))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, pixmap)| pixmap)
}

/// Chooses a pixmap and scales it to `wanted` pixels per side.
pub fn render(pixmaps: &[Pixmap], wanted: u32) -> Option<Pixels> {
    if wanted == 0 || wanted > MAX_ICON {
        return None;
    }
    let pixmap = choose(pixmaps, wanted)?;
    let (width, height) = pixmap.size()?;
    // Network order ARGB is the bytes A, R, G, B. Alpha is not
    // premultiplied on the wire; `fit` does that.
    let mut rgba = Vec::with_capacity(pixmap.data.len());
    for p in pixmap.data.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[p[1], p[2], p[3], p[0]]);
    }
    // Some toolkits send a fully transparent placeholder first.
    if rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        return None;
    }
    Some(fit(&rgba, width, height, wanted))
}

/// How much of source cell `cell` falls inside the span `[start, end)`.
fn overlap(cell: usize, start: f32, end: f32) -> f32 {
    let lo = (cell as f32).max(start);
    let hi = ((cell + 1) as f32).min(end);
    (hi - lo).max(0.0)
}

/// Scales straight RGBA to fit a `size` square, centred, keeping its shape,
/// and premultiplies. Each output pixel is the area average of the source
/// pixels it covers.
pub fn fit(rgba: &[u8], width: u32, height: u32, size: u32) -> Pixels {
    let scale = size as f32 / width.max(height) as f32;
    let out_w = ((width as f32 * scale).round() as u32).clamp(1, size);
    let out_h = ((height as f32 * scale).round() as u32).clamp(1, size);
    let left = (size - out_w) / 2;
    let top = (size - out_h) / 2;
    let step_x = width as f32 / out_w as f32;
    let step_y = height as f32 / out_h as f32;

    let mut data = vec![0u8; (size * size * 4) as usize];
    for oy in 0..out_h {
        let (y0, y1) = (oy as f32 * step_y, (oy + 1) as f32 * step_y);
        for ox in 0..out_w {
            let (x0, x1) = (ox as f32 * step_x, (ox + 1) as f32 * step_x);
            let mut sum = [0f32; 4];
            let mut area = 0f32;
            for sy in (y0 as usize)..(y1.ceil() as usize).min(height as usize) {
                let wy = overlap(sy, y0, y1);
                for sx in (x0 as usize)..(x1.ceil() as usize).min(width as usize) {
                    let weight = wy * overlap(sx, x0, x1);
                    let p = &rgba[(sy * width as usize + sx) * 4..][..4];
                    let alpha = p[3] as f32 / 255.0;
                    sum[0] += p[0] as f32 * alpha * weight;
                    sum[1] += p[1] as f32 * alpha * weight;
                    sum[2] += p[2] as f32 * alpha * weight;
                    sum[3] += p[3] as f32 * weight;
                    area += weight;
                }
            }
            if area > 0.0 {
                let at = (((top + oy) * size + left + ox) * 4) as usize;
                for (out, total) in data[at..at + 4].iter_mut().zip(sum) {
                    *out = (total / area).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    Pixels { size, data }
}

/// Draws `badge` over the bottom right corner of `base`. Both are
/// premultiplied, so this is plain source-over.
pub fn overlay(base: &mut Pixels, badge: &Pixels) {
    if badge.size == 0 || badge.size > base.size {
        return;
    }
    let offset = (base.size - badge.size) as usize;
    let (base_side, badge_side) = (base.size as usize, badge.size as usize);
    for y in 0..badge_side {
        for x in 0..badge_side {
            let src = &badge.data[(y * badge_side + x) * 4..][..4];
            let at = ((y + offset) * base_side + x + offset) * 4;
            let keep = 255 - u32::from(src[3]);
            for (dst, src) in base.data[at..at + 4].iter_mut().zip(src) {
                *dst = (u32::from(*src) + (u32::from(*dst) * keep + 127) / 255).min(255) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(side: i32, argb: [u8; 4]) -> Pixmap {
        Pixmap {
            width: side,
            height: side,
            data: std::iter::repeat_n(argb, (side * side) as usize)
                .flatten()
                .collect(),
        }
    }

    fn sides(pixmaps: &[Pixmap], wanted: u32) -> Option<i32> {
        choose(pixmaps, wanted).map(|pixmap| pixmap.width)
    }

    #[test]
    fn the_exact_size_is_chosen() {
        let set = [16, 22, 32, 48].map(|side| solid(side, [255; 4]));
        assert_eq!(sides(&set, 16), Some(16));
        assert_eq!(sides(&set, 32), Some(32));
    }

    #[test]
    fn otherwise_the_nearest_larger_then_the_largest_smaller() {
        let set = [16, 22, 48].map(|side| solid(side, [255; 4]));
        // Drawn at 16 px on a scale 2 output.
        assert_eq!(sides(&set, 32), Some(48));
        assert_eq!(sides(&set, 20), Some(22));
        assert_eq!(sides(&set, 64), Some(48));
        assert_eq!(sides(&[], 16), None);
    }

    #[test]
    fn argb_is_reordered_and_premultiplied() {
        // Opaque: A=255 R=10 G=20 B=30.
        let icon = render(&[solid(2, [255, 10, 20, 30])], 2).unwrap();
        assert_eq!(&icon.data[..4], [10, 20, 30, 255]);
        // Half transparent white comes out as half grey, half alpha.
        let icon = render(&[solid(2, [128, 255, 255, 255])], 2).unwrap();
        assert_eq!(&icon.data[..4], [128, 128, 128, 128]);
        // Colour under zero alpha does not leak.
        let mut mixed = solid(2, [255, 200, 0, 0]);
        mixed.data[..4].copy_from_slice(&[0, 255, 255, 255]);
        let icon = render(&[mixed], 2).unwrap();
        assert_eq!(&icon.data[..4], [0, 0, 0, 0]);
        assert_eq!(&icon.data[4..8], [200, 0, 0, 255]);
    }

    #[test]
    fn a_larger_pixmap_is_shrunk_to_the_drawn_size() {
        let icon = render(&[solid(64, [255, 1, 2, 3])], 16).unwrap();
        assert_eq!(icon.size, 16);
        assert_eq!(icon.data.len(), 16 * 16 * 4);
        assert!(icon
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [1, 2, 3, 255]));
    }

    #[test]
    fn a_wide_pixmap_is_centred() {
        let wide = Pixmap {
            width: 4,
            height: 2,
            data: [255, 9, 9, 9].repeat(8),
        };
        let icon = render(&[wide], 4).unwrap();
        let row = |y: usize| &icon.data[y * 16..][..4];
        assert_eq!(row(0), [0, 0, 0, 0]);
        assert_eq!(row(1), [9, 9, 9, 255]);
        assert_eq!(row(2), [9, 9, 9, 255]);
        assert_eq!(row(3), [0, 0, 0, 0]);
    }

    #[test]
    fn hostile_sizes_are_refused_before_anything_is_allocated() {
        let claim = |width, height, len| Pixmap {
            width,
            height,
            data: vec![255; len],
        };
        // Claims a hundred thousand pixels a side and carries four bytes.
        assert_eq!(render(&[claim(100_000, 100_000, 4)], 16), None);
        assert_eq!(render(&[claim(i32::MAX, i32::MAX, 16)], 16), None);
        assert_eq!(render(&[claim(-1, -1, 4)], 16), None);
        assert_eq!(render(&[claim(0, 0, 0)], 16), None);
        // The numbers multiply to the data length only after overflow.
        assert_eq!(render(&[claim(65536, 65536, 0)], 16), None);
        // Honest about its size, but short or long on data.
        assert_eq!(render(&[claim(16, 16, 16 * 16 * 4 - 1)], 16), None);
        assert_eq!(render(&[claim(16, 16, 16 * 16 * 4 + 4)], 16), None);
        // Just over the limit, with the data to match.
        let side = MAX_SIDE as i32 + 1;
        assert_eq!(render(&[claim(side, 1, side as usize * 4)], 16), None);
        // A good one after a bad one is still found.
        let set = [claim(100_000, 100_000, 4), solid(16, [255; 4])];
        assert_eq!(render(&set, 16).unwrap().size, 16);
        // And an absurd output size is refused.
        assert_eq!(render(&[solid(16, [255; 4])], MAX_ICON + 1), None);
        assert_eq!(render(&[solid(16, [255; 4])], 0), None);
    }

    #[test]
    fn implausible_pixmaps_are_told_apart_before_they_are_copied() {
        assert!(plausible(22, 22, 22 * 22 * 4));
        assert!(plausible(
            MAX_SIDE as i32,
            MAX_SIDE as i32,
            (MAX_SIDE * MAX_SIDE * 4) as usize
        ));
        assert!(!plausible(100_000, 100_000, 4));
        assert!(!plausible(1100, 1100, 1100 * 1100 * 4));
        assert!(!plausible(22, 22, 22 * 22 * 4 + 1));
        assert!(!plausible(-22, -22, 22 * 22 * 4));
        assert!(!plausible(0, 0, 0));
    }

    #[test]
    fn only_the_first_few_pixmaps_are_considered() {
        let mut set = vec![solid(8, [255; 4]); MAX_PIXMAPS];
        set.push(solid(16, [255; 4]));
        assert_eq!(sides(&set, 16), Some(8));
    }

    #[test]
    fn an_empty_placeholder_is_not_an_icon() {
        assert_eq!(render(&[solid(16, [0, 0, 0, 0])], 16), None);
    }

    #[test]
    fn a_badge_is_drawn_over_the_bottom_right_corner() {
        let mut base = render(&[solid(4, [255, 0, 0, 255])], 4).unwrap();
        let badge = render(&[solid(2, [255, 255, 0, 0])], 2).unwrap();
        overlay(&mut base, &badge);
        let pixel = |x: usize, y: usize| &base.data[(y * 4 + x) * 4..][..4];
        assert_eq!(pixel(0, 0), [0, 0, 255, 255]);
        assert_eq!(pixel(1, 3), [0, 0, 255, 255]);
        assert_eq!(pixel(2, 2), [255, 0, 0, 255]);
        assert_eq!(pixel(3, 3), [255, 0, 0, 255]);

        // A see-through badge leaves the base showing.
        let mut base = render(&[solid(2, [255, 0, 0, 200])], 2).unwrap();
        let glass = Pixels {
            size: 1,
            data: vec![0, 0, 0, 0],
        };
        overlay(&mut base, &glass);
        assert_eq!(&base.data[12..], [0, 0, 200, 255]);
        // One that is too big is ignored.
        let big = render(&[solid(4, [255; 4])], 4).unwrap();
        overlay(&mut base, &big);
        assert_eq!(&base.data[12..], [0, 0, 200, 255]);
    }
}
