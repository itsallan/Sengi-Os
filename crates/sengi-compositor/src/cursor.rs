//! The pointer cursor: images from the system's XCursor theme, with an arrow
//! drawn here for systems that have none installed.

use std::{cell::RefCell, collections::HashMap, env, fs, rc::Rc};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::CursorIcon,
    utils::Transform,
};

use crate::{
    decoration::{Canvas, Theme},
    frame::Edges,
};

/// Used when `XCURSOR_SIZE` is not set; the usual desktop default.
const DEFAULT_SIZE: u32 = 24;

pub struct Sprite {
    pub buffer: MemoryRenderBuffer,
    /// The pixel of the image that sits on the pointer position.
    pub hotspot: (i32, i32),
}

pub struct Cursors {
    theme: xcursor::CursorTheme,
    size: u32,
    loaded: RefCell<HashMap<&'static str, Option<Rc<Sprite>>>>,
    fallback: Rc<Sprite>,
}

/// The cursor for dragging the given frame edges.
pub fn resize_cursor(edges: Edges) -> CursorIcon {
    match (edges.left, edges.right, edges.top, edges.bottom) {
        (true, _, true, _) => CursorIcon::NwResize,
        (_, true, true, _) => CursorIcon::NeResize,
        (true, _, _, true) => CursorIcon::SwResize,
        (_, true, _, true) => CursorIcon::SeResize,
        (true, ..) => CursorIcon::WResize,
        (_, true, ..) => CursorIcon::EResize,
        (_, _, true, _) => CursorIcon::NResize,
        (_, _, _, true) => CursorIcon::SResize,
        _ => CursorIcon::Default,
    }
}

/// Which of the sizes a cursor file offers is closest to `wanted`. Ties go
/// to the larger one, which scales down better than a small one scales up.
pub fn nearest_size(sizes: &[u32], wanted: u32) -> Option<usize> {
    sizes
        .iter()
        .enumerate()
        .min_by_key(|(_, &size)| (size.abs_diff(wanted), u32::MAX - size))
        .map(|(index, _)| index)
}

/// Whether the arrow covers the pixel at `(x, y)`: a triangle with its tip
/// at the origin, `height` tall, with the usual notch at the bottom.
fn in_arrow(x: i32, y: i32, height: i32) -> bool {
    let widest = height * 2 / 3;
    if x < 0 || y < 0 || y >= height {
        return false;
    }
    if y <= widest {
        x <= y
    } else {
        // Below the widest row the right side runs back to the left edge.
        x <= (height - 1 - y) * widest / (height - 1 - widest).max(1)
    }
}

/// An arrow in the theme's text colour with a dark outline.
pub fn draw_fallback(theme: &Theme) -> Canvas {
    let height = theme.fallback_cursor.max(4);
    let border = theme.metrics.border.max(1);
    let side = (height + 2 * border) as usize;
    let mut canvas = Canvas::new(side, side);

    for y in 0..side as i32 {
        for x in 0..side as i32 {
            let (ax, ay) = (x - border, y - border);
            let near = (-border..=border)
                .flat_map(|dy| (-border..=border).map(move |dx| (dx, dy)))
                .any(|(dx, dy)| in_arrow(ax + dx, ay + dy, height));
            if in_arrow(ax, ay, height) {
                canvas.blend(x as usize, y as usize, theme.text, 1.0);
            } else if near {
                canvas.blend(x as usize, y as usize, theme.inactive, 1.0);
            }
        }
    }
    canvas
}

fn buffer(pixels: &[u8], width: i32, height: i32) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        pixels,
        Fourcc::Argb8888,
        (width, height),
        1,
        Transform::Normal,
        None,
    )
}

impl Cursors {
    pub fn new(theme: &Theme) -> Self {
        let name = env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".into());
        let size = env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|size| size.parse().ok())
            .unwrap_or(DEFAULT_SIZE);

        let arrow = draw_fallback(theme);
        let side = arrow.width() as i32;
        let border = theme.metrics.border.max(1);
        let fallback = Rc::new(Sprite {
            buffer: buffer(&arrow.into_pixels(), side, side),
            hotspot: (border, border),
        });

        Self {
            theme: xcursor::CursorTheme::load(&name),
            size,
            loaded: RefCell::default(),
            fallback,
        }
    }

    /// The image for `icon`: from the theme if it has it, else the theme's
    /// default pointer, else the built-in arrow.
    pub fn get(&self, icon: CursorIcon) -> Rc<Sprite> {
        self.themed(icon.name())
            .or_else(|| icon.alt_names().iter().find_map(|name| self.themed(name)))
            .or_else(|| self.themed("default"))
            .or_else(|| self.themed("left_ptr"))
            .unwrap_or_else(|| self.fallback.clone())
    }

    fn themed(&self, name: &'static str) -> Option<Rc<Sprite>> {
        self.loaded
            .borrow_mut()
            .entry(name)
            .or_insert_with(|| self.load(name))
            .clone()
    }

    fn load(&self, name: &str) -> Option<Rc<Sprite>> {
        let path = self.theme.load_icon(name)?;
        let images = xcursor::parser::parse_xcursor(&fs::read(path).ok()?)?;
        let sizes: Vec<u32> = images.iter().map(|image| image.size).collect();
        // Animated cursors repeat a size per frame; the first frame will do.
        let image = &images[nearest_size(&sizes, self.size)?];

        // The file has RGBA bytes, the renderer wants BGRA.
        let mut pixels = image.pixels_rgba.clone();
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Some(Rc::new(Sprite {
            buffer: buffer(&pixels, image.width as i32, image.height as i32),
            hotspot: (image.xhot as i32, image.yhot as i32),
        }))
    }
}

#[cfg(test)]
mod tests {
    use sengi_tokens::Tokens;

    use super::*;

    fn edges(left: bool, right: bool, top: bool, bottom: bool) -> Edges {
        Edges {
            left,
            right,
            top,
            bottom,
        }
    }

    #[test]
    fn every_edge_and_corner_has_its_cursor() {
        assert_eq!(
            resize_cursor(edges(true, false, false, false)),
            CursorIcon::WResize
        );
        assert_eq!(
            resize_cursor(edges(false, true, false, false)),
            CursorIcon::EResize
        );
        assert_eq!(
            resize_cursor(edges(false, false, true, false)),
            CursorIcon::NResize
        );
        assert_eq!(
            resize_cursor(edges(false, false, false, true)),
            CursorIcon::SResize
        );
        assert_eq!(
            resize_cursor(edges(true, false, true, false)),
            CursorIcon::NwResize
        );
        assert_eq!(
            resize_cursor(edges(false, true, true, false)),
            CursorIcon::NeResize
        );
        assert_eq!(
            resize_cursor(edges(true, false, false, true)),
            CursorIcon::SwResize
        );
        assert_eq!(
            resize_cursor(edges(false, true, false, true)),
            CursorIcon::SeResize
        );
        assert_eq!(resize_cursor(Edges::default()), CursorIcon::Default);
    }

    #[test]
    fn nearest_size_prefers_the_closest_then_the_larger() {
        assert_eq!(nearest_size(&[24, 32, 48], 24), Some(0));
        assert_eq!(nearest_size(&[24, 32, 48], 30), Some(1));
        assert_eq!(nearest_size(&[24, 32, 48], 28), Some(1));
        assert_eq!(nearest_size(&[24, 32, 48], 200), Some(2));
        assert_eq!(nearest_size(&[], 24), None);
    }

    #[test]
    fn fallback_arrow_points_up_left_with_an_outline() {
        let theme = Theme::from_tokens(&Tokens::builtin());
        let canvas = draw_fallback(&theme);
        let border = theme.metrics.border.max(1) as usize;
        let text = [theme.text.r, theme.text.g, theme.text.b, 255];
        let outline = [theme.inactive.r, theme.inactive.g, theme.inactive.b, 255];

        // The tip is at the hotspot, outlined on the outside.
        assert_eq!(canvas.pixel(border, border), text);
        assert_eq!(canvas.pixel(border - 1, border), outline);
        assert_eq!(canvas.pixel(border, border + 5), text);
        assert_eq!(canvas.pixel(border + 3, border + 5), text);
        // Far to the right of the tip there is nothing.
        assert_eq!(canvas.pixel(canvas.width() - 1, border)[3], 0);
    }
}
