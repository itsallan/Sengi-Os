//! Drawing the title bar.
//!
//! The title bar is painted on the CPU into a small ARGB image whenever it
//! changes, and the renderer shows it like any other texture. Every size and
//! colour comes from the design tokens.

use sengi_tokens::{Rgba, Tokens};

use crate::{
    frame::{Button, Metrics, BUTTONS},
    layout::Rect,
};

#[derive(Debug, Clone)]
pub struct Theme {
    pub metrics: Metrics,
    pub radius: i32,
    /// Side of the square a button glyph is drawn in.
    pub glyph: i32,
    pub glyph_stroke: i32,
    /// Height of the arrow drawn when no cursor theme is installed.
    pub fallback_cursor: i32,
    pub font_family: String,
    pub font_weight: u32,
    pub font_px: u32,
    pub active: Rgba,
    pub inactive: Rgba,
    pub hover: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub border: Rgba,
    pub danger: Rgba,
}

impl Theme {
    pub fn from_tokens(tokens: &Tokens) -> Self {
        let px = |name: &str| {
            tokens
                .px(name)
                .unwrap_or_else(|| panic!("tokens define {name}")) as i32
        };
        let color = |name: &str| {
            tokens
                .color(name)
                .unwrap_or_else(|| panic!("tokens define {name}"))
        };
        let button = px("size.button");

        Self {
            metrics: Metrics {
                titlebar: px("size.titlebar-height"),
                border: px("border.width"),
                button,
                gap: px("space.xs"),
                padding: px("space.sm"),
                grip: px("space.xs"),
                corner: px("space.lg"),
            },
            radius: px("radius.base"),
            glyph: button - 2 * px("space.sm"),
            glyph_stroke: px("border.width"),
            fallback_cursor: px("size.bar-icon"),
            font_family: tokens
                .get("type.family")
                .expect("tokens define type.family")
                .to_string(),
            // A weight, not a length, so not read as pixels.
            font_weight: tokens
                .get("type.weight-medium")
                .and_then(|weight| weight.parse().ok())
                .expect("tokens define type.weight-medium"),
            font_px: px("type.size-md") as u32,
            active: color("color.surface"),
            inactive: color("color.bg"),
            hover: color("color.surface-hover"),
            text: color("color.text"),
            text_muted: color("color.text-muted"),
            border: color("color.border"),
            danger: color("color.danger"),
        }
    }
}

/// Text already turned into pixels: one coverage byte per pixel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextBitmap {
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<u8>,
}

/// Premultiplied ARGB8888 pixels, in the byte order wl_shm and DRM use.
pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

/// A rectangle with float edges and a radius per corner, clockwise from the
/// top left.
#[derive(Clone, Copy)]
struct Rounded {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radii: [f32; 4],
}

impl Rounded {
    fn new(rect: Rect, radii: [f32; 4]) -> Self {
        Self {
            x0: rect.x as f32,
            y0: rect.y as f32,
            x1: rect.right() as f32,
            y1: rect.bottom() as f32,
            radii,
        }
    }

    /// How much of the pixel at `(x, y)` the shape covers, 0 to 1.
    fn coverage(&self, x: usize, y: usize) -> f32 {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        if px < self.x0 || px > self.x1 || py < self.y0 || py > self.y1 {
            return 0.0;
        }
        let [top_left, top_right, bottom_right, bottom_left] = self.radii;
        let corners = [
            (top_left, self.x0 + top_left, self.y0 + top_left, -1.0, -1.0),
            (
                top_right,
                self.x1 - top_right,
                self.y0 + top_right,
                1.0,
                -1.0,
            ),
            (
                bottom_right,
                self.x1 - bottom_right,
                self.y1 - bottom_right,
                1.0,
                1.0,
            ),
            (
                bottom_left,
                self.x0 + bottom_left,
                self.y1 - bottom_left,
                -1.0,
                1.0,
            ),
        ];
        for (radius, cx, cy, sx, sy) in corners {
            let (dx, dy) = ((px - cx) * sx, (py - cy) * sy);
            if radius > 0.0 && dx > 0.0 && dy > 0.0 {
                let distance = (dx * dx + dy * dy).sqrt();
                return (radius - distance + 0.5).clamp(0.0, 1.0);
            }
        }
        1.0
    }
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height * 4],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    #[cfg(test)]
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        // Stored as blue, green, red, alpha.
        [
            self.pixels[i + 2],
            self.pixels[i + 1],
            self.pixels[i],
            self.pixels[i + 3],
        ]
    }

    /// Paints `color` over the pixel at `(x, y)`, weighted by `coverage`.
    pub fn blend(&mut self, x: usize, y: usize, color: Rgba, coverage: f32) {
        if x >= self.width || y >= self.height || coverage <= 0.0 {
            return;
        }
        let alpha = f32::from(color.a) / 255.0 * coverage.min(1.0);
        let i = (y * self.width + x) * 4;
        let source = [color.b, color.g, color.r, 255];
        for (channel, value) in source.into_iter().enumerate() {
            let below = f32::from(self.pixels[i + channel]);
            let mixed = f32::from(value) * alpha + below * (1.0 - alpha);
            self.pixels[i + channel] = mixed.round() as u8;
        }
    }

    fn fill(&mut self, shape: Rounded, color: Rgba) {
        let (x0, y0) = (shape.x0.max(0.0) as usize, shape.y0.max(0.0) as usize);
        let (x1, y1) = (shape.x1.ceil() as usize, shape.y1.ceil() as usize);
        for y in y0..y1.min(self.height) {
            for x in x0..x1.min(self.width) {
                self.blend(x, y, color, shape.coverage(x, y));
            }
        }
    }

    /// A straight line with round ends.
    fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Rgba) {
        let reach = width / 2.0 + 1.0;
        let x0 = (from.0.min(to.0) - reach).floor().max(0.0) as usize;
        let y0 = (from.1.min(to.1) - reach).floor().max(0.0) as usize;
        let x1 = (from.0.max(to.0) + reach).ceil() as usize;
        let y1 = (from.1.max(to.1) + reach).ceil() as usize;
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length_squared = dx * dx + dy * dy;

        for y in y0..y1.min(self.height) {
            for x in x0..x1.min(self.width) {
                let (px, py) = (x as f32 + 0.5 - from.0, y as f32 + 0.5 - from.1);
                let along = if length_squared > 0.0 {
                    ((px * dx + py * dy) / length_squared).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (ox, oy) = (px - along * dx, py - along * dy);
                let distance = (ox * ox + oy * oy).sqrt();
                self.blend(x, y, color, width / 2.0 + 0.5 - distance);
            }
        }
    }

    fn text(&mut self, bitmap: &TextBitmap, left: i32, top: i32, clip_right: i32, color: Rgba) {
        for row in 0..bitmap.height {
            for column in 0..bitmap.width {
                let (x, y) = (left + column as i32, top + row as i32);
                if x < 0 || y < 0 || x >= clip_right {
                    continue;
                }
                let coverage = f32::from(bitmap.coverage[row * bitmap.width + column]) / 255.0;
                self.blend(x as usize, y as usize, color, coverage);
            }
        }
    }
}

/// What a title bar looks like; it is repainted when any of this changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Titlebar {
    /// Full frame width in pixels.
    pub width: i32,
    pub active: bool,
    pub hovered: Option<Button>,
    /// Maximised windows fill the screen edge to edge and have square
    /// corners.
    pub square: bool,
    /// The frame is wider than `width`: this is only its left part, so the
    /// right end is a straight cut with no corner or border.
    pub cut: bool,
}

/// Paints a title bar: `width` by the title bar height, premultiplied ARGB.
pub fn draw_titlebar(theme: &Theme, bar: &Titlebar, title: Option<&TextBitmap>) -> Canvas {
    let metrics = &theme.metrics;
    let (width, height) = (bar.width.max(1), metrics.titlebar.max(1));
    let mut canvas = Canvas::new(width as usize, height as usize);

    // Border colour first, then the fill inset by the border width, which
    // leaves a border that follows the rounded corners.
    let radius = if bar.square { 0.0 } else { theme.radius as f32 };
    let border = metrics.border;
    let outer = Rect::new(0, 0, width, height);
    let right_border = if bar.cut { 0 } else { border };
    let inner = Rect::new(
        border,
        border,
        width - border - right_border,
        height - border,
    );
    let right_radius = if bar.cut { 0.0 } else { radius };
    let inner_right_radius = (right_radius - border as f32).max(0.0);
    let inner_radius = (radius - border as f32).max(0.0);
    let background = if bar.active {
        theme.active
    } else {
        theme.inactive
    };
    canvas.fill(
        Rounded::new(outer, [radius, right_radius, 0.0, 0.0]),
        theme.border,
    );
    canvas.fill(
        Rounded::new(inner, [inner_radius, inner_right_radius, 0.0, 0.0]),
        background,
    );

    let foreground = if bar.active {
        theme.text
    } else {
        theme.text_muted
    };
    let stroke = theme.glyph_stroke as f32;
    for button in BUTTONS {
        let rect = metrics.button_rect(button);
        if bar.hovered == Some(button) {
            let radius = theme.radius as f32;
            canvas.fill(Rounded::new(rect, [radius; 4]), theme.hover);
        }

        // Glyph corners sit on pixel centres so straight strokes stay crisp.
        let half = (theme.glyph / 2) as f32;
        let cx = rect.x as f32 + (rect.w / 2) as f32 + 0.5;
        let cy = rect.y as f32 + (rect.h / 2) as f32 + 0.5;
        let (left, right, top, bottom) = (cx - half, cx + half, cy - half, cy + half);
        match button {
            Button::Close => {
                canvas.line((left, top), (right, bottom), stroke, theme.danger);
                canvas.line((left, bottom), (right, top), stroke, theme.danger);
            }
            Button::Minimise => canvas.line((left, cy), (right, cy), stroke, foreground),
            Button::Maximise => {
                canvas.line((left, top), (right, top), stroke, foreground);
                canvas.line((right, top), (right, bottom), stroke, foreground);
                canvas.line((right, bottom), (left, bottom), stroke, foreground);
                canvas.line((left, bottom), (left, top), stroke, foreground);
            }
        }
    }

    if let Some(title) = title {
        // Centred on the whole bar, but never under the buttons.
        let first = metrics.buttons_end() + metrics.padding;
        let last = width - metrics.padding - border;
        let centred = (width - title.width as i32) / 2;
        let left = centred.max(first);
        let top = (height + border - title.height as i32) / 2;
        canvas.text(title, left, top, last, foreground);
    }
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::from_tokens(&Tokens::builtin())
    }

    fn rgba(c: Rgba) -> [u8; 4] {
        [c.r, c.g, c.b, c.a]
    }

    fn bar(active: bool) -> Titlebar {
        Titlebar {
            width: 400,
            active,
            hovered: None,
            square: false,
            cut: false,
        }
    }

    #[test]
    fn theme_follows_the_tokens() {
        let tokens = Tokens::builtin();
        let theme = theme();
        assert_eq!(
            theme.metrics.titlebar as u32,
            tokens.px("size.titlebar-height").unwrap()
        );
        assert_eq!(theme.radius as u32, tokens.px("radius.base").unwrap());
        assert_eq!(theme.active, tokens.color("color.surface").unwrap());
        assert_eq!(theme.inactive, tokens.color("color.bg").unwrap());
        assert_eq!(theme.font_family, "Inter");
        assert!(theme.glyph > 0 && theme.glyph < theme.metrics.button);
    }

    #[test]
    fn fill_and_border_use_the_theme_colours() {
        let theme = theme();
        let active = draw_titlebar(&theme, &bar(true), None);
        let inactive = draw_titlebar(&theme, &bar(false), None);
        let middle = (300, theme.metrics.titlebar as usize / 2);

        assert_eq!(active.pixel(middle.0, middle.1), rgba(theme.active));
        assert_eq!(inactive.pixel(middle.0, middle.1), rgba(theme.inactive));
        // Top edge away from the corners, and both sides.
        assert_eq!(active.pixel(200, 0), rgba(theme.border));
        assert_eq!(active.pixel(0, 20), rgba(theme.border));
        assert_eq!(active.pixel(399, 20), rgba(theme.border));
        assert_eq!(active.pixel(1, 20), rgba(theme.active));
    }

    #[test]
    fn top_corners_are_rounded_and_bottom_ones_are_not() {
        let theme = theme();
        let canvas = draw_titlebar(&theme, &bar(true), None);
        let bottom = theme.metrics.titlebar as usize - 1;

        assert_eq!(canvas.pixel(0, 0)[3], 0);
        assert_eq!(canvas.pixel(399, 0)[3], 0);
        assert_eq!(canvas.pixel(0, bottom)[3], 255);
        assert_eq!(canvas.pixel(399, bottom)[3], 255);
        // Just inside the curve the pixel is solid.
        let r = theme.radius as usize;
        assert_eq!(canvas.pixel(r, r)[3], 255);
    }

    #[test]
    fn maximised_bars_have_square_corners() {
        let theme = theme();
        let mut square = bar(true);
        square.square = true;
        let canvas = draw_titlebar(&theme, &square, None);
        assert_eq!(canvas.pixel(0, 0), rgba(theme.border));
    }

    #[test]
    fn cut_bars_end_straight_without_a_border() {
        let theme = theme();
        let mut cut = bar(true);
        cut.cut = true;
        let canvas = draw_titlebar(&theme, &cut, None);
        // Top right: border along the top, but no corner and no side border.
        assert_eq!(canvas.pixel(399, 0), rgba(theme.border));
        assert_eq!(canvas.pixel(399, 20), rgba(theme.active));
        // The left end is unchanged.
        assert_eq!(canvas.pixel(0, 0)[3], 0);
    }

    #[test]
    fn close_glyph_is_drawn_in_danger() {
        let theme = theme();
        let canvas = draw_titlebar(&theme, &bar(true), None);
        let rect = theme.metrics.button_rect(Button::Close);
        let centre = (
            (rect.x + rect.w / 2) as usize,
            (rect.y + rect.h / 2) as usize,
        );
        assert_eq!(canvas.pixel(centre.0, centre.1), rgba(theme.danger));

        // The minimise glyph is a line through the middle in the text colour.
        let rect = theme.metrics.button_rect(Button::Minimise);
        let centre = (
            (rect.x + rect.w / 2) as usize,
            (rect.y + rect.h / 2) as usize,
        );
        assert_eq!(canvas.pixel(centre.0, centre.1), rgba(theme.text));
        assert_eq!(canvas.pixel(centre.0, centre.1 - 3), rgba(theme.active));
    }

    #[test]
    fn hovered_button_gets_a_background() {
        let theme = theme();
        let mut hovered = bar(true);
        hovered.hovered = Some(Button::Maximise);
        let canvas = draw_titlebar(&theme, &hovered, None);

        let rect = theme.metrics.button_rect(Button::Maximise);
        let centre = (
            (rect.x + rect.w / 2) as usize,
            (rect.y + rect.h / 2) as usize,
        );
        // The maximise glyph is hollow, so its centre shows the background.
        assert_eq!(canvas.pixel(centre.0, centre.1), rgba(theme.hover));
        let other = theme.metrics.button_rect(Button::Minimise);
        assert_eq!(
            canvas.pixel(other.x as usize + 2, other.y as usize + 2),
            rgba(theme.active)
        );
    }

    #[test]
    fn title_is_centred_but_clear_of_the_buttons() {
        let theme = theme();
        let title = |width| TextBitmap {
            width,
            height: 10,
            coverage: vec![255; width * 10],
        };
        let row = (theme.metrics.titlebar as usize).div_ceil(2);

        let short = draw_titlebar(&theme, &bar(true), Some(&title(100)));
        assert_eq!(short.pixel(149, row), rgba(theme.active));
        assert_eq!(short.pixel(150, row), rgba(theme.text));
        assert_eq!(short.pixel(249, row), rgba(theme.text));
        assert_eq!(short.pixel(250, row), rgba(theme.active));

        // Too long to centre: starts after the buttons, cut before the edge.
        let long = draw_titlebar(&theme, &bar(true), Some(&title(390)));
        let first = (theme.metrics.buttons_end() + theme.metrics.padding) as usize;
        assert_eq!(long.pixel(first - 1, row), rgba(theme.active));
        assert_eq!(long.pixel(first, row), rgba(theme.text));
        assert_eq!(
            long.pixel(399 - theme.metrics.padding as usize, row),
            rgba(theme.active)
        );
    }
}
