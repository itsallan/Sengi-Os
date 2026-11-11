//! The shared-memory buffers of one surface, reused from frame to frame so
//! that only what changed is drawn, copied and reported to the compositor.
//!
//! Slint renders straight into a buffer and repaints only what is dirty,
//! which needs the buffer to hold the previous frame. The compositor may
//! still be showing the buffer of the previous frame, so there are two or
//! three, and each remembers which parts of it have fallen behind. Before a
//! buffer is drawn into, those parts are copied over from the newest one.

use slint::platform::software_renderer::{PremultipliedRgbaColor, TargetPixel};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::Shm;
use wayland_client::protocol::wl_shm;

/// Enough for the compositor to hold one, a second to be on its way to it,
/// and a third to draw into.
const MAX_BUFFERS: usize = 3;

/// The most bytes copied between two buffers in one go. The two live in
/// the same pool, which lends out one of them at a time, so the bytes pass
/// through a scratch buffer, and that should stay small however large the
/// frame is.
const COPY_AT_ONCE: usize = 64 * 1024;

/// More out-of-date areas than this are merged into the box around them.
const MAX_AREAS: usize = 8;

/// A pixel of a wl_shm Argb8888 buffer, which is stored little-endian: the
/// bytes run B, G, R, A. Colours are premultiplied.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Bgra([u8; 4]);

impl TargetPixel for Bgra {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let keep = u16::from(u8::MAX - color.alpha);
        let mix = |under: u8, over: u8| (u16::from(under) * keep / 255) as u8 + over;
        self.0 = [
            mix(self.0[0], color.blue),
            mix(self.0[1], color.green),
            mix(self.0[2], color.red),
            mix(self.0[3], color.alpha),
        ];
    }

    fn from_rgb(red: u8, green: u8, blue: u8) -> Bgra {
        Bgra([blue, green, red, u8::MAX])
    }

    // What shows where the window draws nothing: the desktop behind it.
    fn background() -> Bgra {
        Bgra([0; 4])
    }
}

/// Views the bytes of a buffer as pixels. Bytes past the last whole pixel
/// are left out.
fn as_pixels(bytes: &mut [u8]) -> &mut [Bgra] {
    let (pixels, _) = bytes.as_chunks_mut::<4>();
    // SAFETY: `Bgra` is a transparent wrapper around `[u8; 4]`, so the two
    // slice types have the same layout, and the new slice borrows `bytes`
    // for the same lifetime.
    unsafe { std::slice::from_raw_parts_mut(pixels.as_mut_ptr().cast::<Bgra>(), pixels.len()) }
}

/// A rectangle of a buffer, in buffer pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Area {
    /// The part of the area that lies inside a buffer of the given size.
    pub(crate) fn clipped(self, width: i32, height: i32) -> Option<Area> {
        let left = self.x.clamp(0, width);
        let top = self.y.clamp(0, height);
        let right = self.x.saturating_add(self.width).clamp(0, width);
        let bottom = self.y.saturating_add(self.height).clamp(0, height);
        (right > left && bottom > top).then_some(Area {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }

    fn around(areas: &[Area]) -> Option<Area> {
        let left = areas.iter().map(|area| area.x).min()?;
        let top = areas.iter().map(|area| area.y).min()?;
        let right = areas.iter().map(|area| area.x + area.width).max()?;
        let bottom = areas.iter().map(|area| area.y + area.height).max()?;
        Some(Area {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }
}

/// Adds areas to a list of out-of-date ones, keeping the list short.
fn add_areas(stale: &mut Vec<Area>, more: &[Area]) {
    stale.extend_from_slice(more);
    if stale.len() > MAX_AREAS {
        let around = Area::around(stale);
        stale.clear();
        stale.extend(around);
    }
}

/// Cuts areas into bands of at most `rows` rows.
fn bands(areas: &[Area], rows: i32) -> Vec<Area> {
    let rows = rows.max(1);
    let mut bands = Vec::new();
    for area in areas {
        let mut top = area.y;
        while top < area.y + area.height {
            let height = rows.min(area.y + area.height - top);
            bands.push(Area {
                y: top,
                height,
                ..*area
            });
            top += height;
        }
    }
    bands
}

/// Appends the bytes of the given areas of a buffer to `scratch`. The areas
/// must lie inside the buffer.
fn gather(source: &[u8], stride: usize, areas: &[Area], scratch: &mut Vec<u8>) {
    for area in areas {
        for row in area.y..area.y + area.height {
            let start = row as usize * stride + area.x as usize * 4;
            scratch.extend_from_slice(&source[start..start + area.width as usize * 4]);
        }
    }
}

/// Writes bytes collected by [`gather`] to the same areas of another buffer
/// of the same size.
fn scatter(scratch: &[u8], target: &mut [u8], stride: usize, areas: &[Area]) {
    let mut taken = 0;
    for area in areas {
        let length = area.width as usize * 4;
        for row in area.y..area.y + area.height {
            let start = row as usize * stride + area.x as usize * 4;
            target[start..start + length].copy_from_slice(&scratch[taken..taken + length]);
            taken += length;
        }
    }
}

struct Frame {
    buffer: Buffer,
    /// Where this buffer differs from the newest frame.
    stale: Vec<Area>,
}

impl Frame {
    /// True if the compositor is done with the buffer.
    fn free(&self) -> bool {
        !self.buffer.slot().has_active_buffers()
    }
}

/// What came of trying to paint a frame.
pub(crate) enum Painted<'frames> {
    /// The buffer to attach and the areas of it that changed.
    Frame(&'frames Buffer, Vec<Area>),
    /// Nothing on screen changed, so there is nothing to commit.
    Unchanged,
    /// The compositor holds every buffer. Try again when one comes back.
    Busy,
    /// No memory for a buffer.
    Failed,
}

/// The buffers of one surface.
#[derive(Default)]
pub(crate) struct Frames {
    /// The memory the buffers live in. A pool can grow but never shrink, so
    /// each surface has its own, sized for its buffers, and gives the whole
    /// of it back when the surface is hidden or changes size. Otherwise one
    /// full-screen frame would be paid for for the life of the program.
    pool: Option<SlotPool>,
    /// Buffer size in pixels. Buffers of another size are of no use.
    size: (i32, i32),
    frames: Vec<Frame>,
    /// The buffer holding the newest frame.
    newest: Option<usize>,
}

impl Frames {
    /// Forgets every buffer and frees their memory, as when the surface
    /// goes away. The next frame is painted in full.
    pub(crate) fn clear(&mut self) {
        self.frames.clear();
        self.newest = None;
        self.pool = None;
    }

    /// Paints a frame of the given size into a buffer the compositor is not
    /// using.
    ///
    /// `paint` is given the buffer's pixels, already holding the previous
    /// frame unless its second argument says to paint everything. It
    /// returns the areas it changed.
    pub(crate) fn paint(
        &mut self,
        shm: &Shm,
        scratch: &mut Vec<u8>,
        width: i32,
        height: i32,
        paint: impl FnOnce(&mut [Bgra], bool) -> Vec<Area>,
    ) -> Painted<'_> {
        if self.size != (width, height) {
            self.clear();
            self.size = (width, height);
        }
        let stride = width as usize * 4;
        let pool = match &mut self.pool {
            Some(pool) => pool,
            None => {
                // Room for the two buffers a surface normally alternates
                // between; a third makes the pool grow.
                let Ok(pool) = SlotPool::new(stride * height as usize * 2, shm) else {
                    return Painted::Failed;
                };
                self.pool.insert(pool)
            }
        };
        let everything = Area {
            x: 0,
            y: 0,
            width,
            height,
        };

        // The newest buffer needs no catching up, so it is the first
        // choice when the compositor has already let go of it.
        let free = self
            .newest
            .filter(|&newest| self.frames[newest].free())
            .or_else(|| self.frames.iter().position(Frame::free));
        let target = match free {
            Some(target) => target,
            None if self.frames.len() < MAX_BUFFERS => {
                let Ok((buffer, _)) =
                    pool.create_buffer(width, height, stride as i32, wl_shm::Format::Argb8888)
                else {
                    return Painted::Failed;
                };
                self.frames.push(Frame {
                    buffer,
                    stale: vec![everything],
                });
                self.frames.len() - 1
            }
            None => return Painted::Busy,
        };

        // Bring the buffer up to the newest frame.
        if let Some(newest) = self.newest.filter(|&newest| newest != target) {
            let stale = std::mem::take(&mut self.frames[target].stale);
            let source = self.frames[newest].buffer.slot();
            for band in bands(&stale, (COPY_AT_ONCE / stride.max(1)) as i32) {
                scratch.clear();
                gather(pool.raw_data_mut(&source), stride, &[band], scratch);
                if let Some(canvas) = self.frames[target].buffer.canvas(pool) {
                    scatter(scratch, canvas, stride, &[band]);
                }
            }
        }

        let in_full = self.newest.is_none();
        let Some(canvas) = self.frames[target].buffer.canvas(pool) else {
            return Painted::Busy;
        };
        let painted = paint(as_pixels(canvas), in_full);
        let changed: Vec<Area> = if in_full {
            vec![everything]
        } else {
            painted
                .into_iter()
                .filter_map(|area| area.clipped(width, height))
                .collect()
        };

        for (index, frame) in self.frames.iter_mut().enumerate() {
            if index == target {
                frame.stale.clear();
            } else {
                add_areas(&mut frame.stale, &changed);
            }
        }
        self.newest = Some(target);
        if changed.is_empty() {
            return Painted::Unchanged;
        }
        Painted::Frame(&self.frames[target].buffer, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: i32, y: i32, width: i32, height: i32) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    fn color(red: u8, green: u8, blue: u8, alpha: u8) -> PremultipliedRgbaColor {
        PremultipliedRgbaColor {
            red,
            green,
            blue,
            alpha,
        }
    }

    #[test]
    fn pixels_are_stored_as_bgra_bytes() {
        assert_eq!(Bgra::from_rgb(1, 2, 3), Bgra([3, 2, 1, 255]));
        let mut pixel = Bgra::background();
        assert_eq!(pixel, Bgra([0, 0, 0, 0]));
        pixel.blend(color(10, 20, 30, 40));
        assert_eq!(pixel, Bgra([30, 20, 10, 40]));
    }

    #[test]
    fn blending_matches_slints_own_pixel() {
        let cases = [
            (color(200, 100, 50, 255), color(10, 20, 30, 40)),
            (color(0, 0, 0, 0), color(128, 64, 32, 128)),
            (color(90, 90, 90, 90), color(255, 255, 255, 255)),
            (color(17, 99, 201, 230), color(0, 0, 0, 0)),
        ];
        for (under, over) in cases {
            let mut expected = under;
            expected.blend(over);
            let mut pixel = Bgra([under.blue, under.green, under.red, under.alpha]);
            pixel.blend(over);
            assert_eq!(
                pixel,
                Bgra([expected.blue, expected.green, expected.red, expected.alpha])
            );
        }
    }

    #[test]
    fn a_buffer_is_seen_as_whole_pixels() {
        let mut bytes = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let pixels = as_pixels(&mut bytes);
        assert_eq!(pixels.len(), 2);
        pixels[1] = Bgra::from_rgb(0, 0, 0);
        assert_eq!(bytes, [1, 2, 3, 4, 0, 0, 0, 255, 9, 10]);
    }

    #[test]
    fn areas_are_clipped_to_the_buffer() {
        assert_eq!(area(2, 3, 4, 5).clipped(10, 10), Some(area(2, 3, 4, 5)));
        assert_eq!(area(-2, -3, 6, 6).clipped(10, 10), Some(area(0, 0, 4, 3)));
        assert_eq!(area(8, 8, 6, 6).clipped(10, 10), Some(area(8, 8, 2, 2)));
        assert_eq!(area(10, 0, 5, 5).clipped(10, 10), None);
        assert_eq!(area(3, 3, 0, 5).clipped(10, 10), None);
        assert_eq!(
            area(0, 0, i32::MAX, i32::MAX).clipped(10, 10),
            Some(area(0, 0, 10, 10))
        );
    }

    #[test]
    fn a_long_list_of_areas_becomes_the_box_around_them() {
        let mut stale = vec![area(0, 0, 1, 1)];
        add_areas(&mut stale, &[area(5, 5, 2, 2)]);
        assert_eq!(stale, [area(0, 0, 1, 1), area(5, 5, 2, 2)]);

        let many: Vec<Area> = (0..MAX_AREAS as i32)
            .map(|n| area(n * 3, 10, 2, 4))
            .collect();
        add_areas(&mut stale, &many);
        assert_eq!(stale, [area(0, 0, (MAX_AREAS as i32 - 1) * 3 + 2, 14)]);
    }

    #[test]
    fn areas_are_cut_into_bands_that_cover_them_exactly() {
        assert_eq!(
            bands(&[area(1, 2, 5, 7)], 3),
            [area(1, 2, 5, 3), area(1, 5, 5, 3), area(1, 8, 5, 1)]
        );
        assert_eq!(bands(&[area(0, 0, 4, 2)], 10), [area(0, 0, 4, 2)]);
        // Never fewer than one row at a time, however wide the buffer.
        assert_eq!(bands(&[area(0, 0, 4, 2)], 0).len(), 2);
        assert_eq!(bands(&[], 3), []);
    }

    #[test]
    fn stale_areas_are_copied_from_one_buffer_to_another() {
        // Two 4 x 3 buffers; bytes of the source count upwards.
        let stride = 16;
        let source: Vec<u8> = (0..48).collect();
        let mut target = vec![200u8; 48];
        let areas = [area(1, 0, 2, 1), area(0, 2, 4, 1), area(3, 1, 1, 2)];

        let mut scratch = Vec::new();
        gather(&source, stride, &areas, &mut scratch);
        scatter(&scratch, &mut target, stride, &areas);

        for row in 0..3 {
            for column in 0..4 {
                let inside = areas.iter().any(|area| {
                    (area.x..area.x + area.width).contains(&column)
                        && (area.y..area.y + area.height).contains(&row)
                });
                let at = row as usize * stride + column as usize * 4;
                let expected = if inside { source[at] } else { 200 };
                assert_eq!(target[at], expected, "pixel {column},{row}");
            }
        }
    }
}
