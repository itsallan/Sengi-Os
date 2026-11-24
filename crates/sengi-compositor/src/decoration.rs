//! Drawing the title bar.
//!
//! The title bar is painted on the CPU into a small ARGB image whenever it
//! changes, and the renderer shows it like any other texture. Every size and
//! colour comes from the design tokens.

use sengi_tokens::{Rgba, Tokens};

use crate::{
    frame::{Button, Metrics, BUTTONS},
    layout::Rect,
    shadow::ShadowStyle,
};

/// The sizes of the cards the compositor draws over the desktop: the
/// switcher and the menus. All of them are tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardMetrics {
    /// Width of a menu.
    pub menu_width: i32,
    /// Height of a tile of the switcher.
    pub row: i32,
    /// Height of a row of a menu, the space between a menu's edge and its
    /// rows, and how far a separator is pulled in from the ends of a row.
    pub menu_row: i32,
    pub menu_pad: i32,
    pub menu_inset: i32,
    /// Space inside a card around its rows or tiles, across and down.
    pub pad_x: i32,
    pub pad_y: i32,
    /// Space between tiles, and between a row and the card's edge.
    pub gap: i32,
    /// Side of the square a window's badge is drawn in.
    pub badge: i32,
    /// Space left clear around a card at the edges of the screen.
    pub margin: i32,
}

/// How one round button looks: its fill, the ring around it and its glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Light {
    pub fill: Rgba,
    pub ring: Rgba,
    pub glyph: Rgba,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub metrics: Metrics,
    pub radius: i32,
    /// Side of the square a button glyph is drawn in.
    pub glyph: i32,
    pub glyph_stroke: f32,
    /// Width of the ring around a round button.
    pub ring: f32,
    /// How much darker a pressed button is, in percent.
    pub pressed_darken: u32,
    /// Close, minimise and maximise, in the order of `BUTTONS`.
    pub lights: [Light; 3],
    /// What all three buttons turn into in a window that is not focused.
    pub light_inactive: Rgba,
    /// Weight of the window title.
    pub title_weight: u32,
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
    pub accent: Rgba,
    pub accent_text: Rgba,
    /// The surface a card of settings sits on, one step above the bar.
    pub raised: Rgba,
    pub card: CardMetrics,
    pub shadow: ShadowStyle,
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
        let float = |name: &str| -> f32 {
            tokens
                .get(name)
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("tokens define {name}"))
        };
        let light = |name: &str| Light {
            fill: color(&format!("color.window-{name}")),
            ring: color(&format!("color.window-{name}-ring")),
            glyph: color(&format!("color.window-{name}-glyph")),
        };
        let button = px("window.button");

        Self {
            metrics: Metrics {
                titlebar: px("size.titlebar-height"),
                border: px("border.width"),
                button,
                gap: px("window.button-gap"),
                inset: px("window.button-inset"),
                padding: px("space.sm"),
                grip: px("space.xs"),
                corner: px("space.lg"),
            },
            radius: px("radius.base"),
            glyph: px("window.button-glyph"),
            glyph_stroke: float("window.button-glyph-stroke"),
            ring: float("window.button-ring"),
            pressed_darken: px("window.button-pressed-darken") as u32,
            lights: [light("close"), light("minimise"), light("maximise")],
            light_inactive: color("color.window-inactive"),
            title_weight: tokens
                .get("type.weight-bold")
                .and_then(|weight| weight.parse().ok())
                .expect("tokens define type.weight-bold"),
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
            accent: color("color.accent"),
            accent_text: color("color.accent-text"),
            raised: color("color.surface-raised"),
            shadow: ShadowStyle::from_tokens(tokens),
            card: CardMetrics {
                menu_width: px("size.menu-width"),
                row: px("size.list-row-height"),
                menu_row: px("menu.row"),
                menu_pad: px("menu.padding"),
                menu_inset: px("menu.separator-inset"),
                pad_x: px("space.md"),
                pad_y: px("space.sm"),
                gap: px("space.sm"),
                badge: px("size.button"),
                margin: px("space.lg"),
            },
        }
    }
}

impl Theme {
    /// This theme at `scale` times its size, for painting on a screen of
    /// that scale: every length is in that screen's pixels, so the frame
    /// comes out sharp. Layout and hit testing keep the unscaled theme.
    pub fn scaled(&self, scale: f64) -> Theme {
        let px = |v: i32| (f64::from(v) * scale).round() as i32;
        let m = &self.metrics;
        Theme {
            metrics: Metrics {
                titlebar: px(m.titlebar),
                border: px(m.border),
                button: px(m.button),
                gap: px(m.gap),
                inset: px(m.inset),
                padding: px(m.padding),
                grip: px(m.grip),
                corner: px(m.corner),
            },
            radius: px(self.radius),
            glyph: px(self.glyph),
            glyph_stroke: self.glyph_stroke * scale as f32,
            ring: self.ring * scale as f32,
            fallback_cursor: px(self.fallback_cursor),
            font_px: px(self.font_px as i32) as u32,
            accent: self.accent,
            accent_text: self.accent_text,
            raised: self.raised,
            shadow: self.shadow.scaled(scale),
            card: CardMetrics {
                menu_width: px(self.card.menu_width),
                row: px(self.card.row),
                menu_row: px(self.card.menu_row),
                menu_pad: px(self.card.menu_pad),
                menu_inset: px(self.card.menu_inset),
                pad_x: px(self.card.pad_x),
                pad_y: px(self.card.pad_y),
                gap: px(self.card.gap),
                badge: px(self.card.badge),
                margin: px(self.card.margin),
            },
            ..self.clone()
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

    /// A circle of `radius` around `(cx, cy)`.
    fn circle(cx: f32, cy: f32, radius: f32) -> Self {
        Self {
            x0: cx - radius,
            y0: cy - radius,
            x1: cx + radius,
            y1: cy + radius,
            radii: [radius; 4],
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

    pub fn height(&self) -> usize {
        self.height
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

    /// A rectangle with corners rounded at `radius`, filled with `colour`.
    pub fn round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: f32, colour: Rgba) {
        self.fill(Rounded::new(Rect::new(x, y, w, h), [radius; 4]), colour);
    }

    /// A line of text, its top left at `(left, top)`, in `colour`.
    pub fn label(&mut self, bitmap: &TextBitmap, left: i32, top: i32, colour: Rgba) {
        let right = self.width as i32;
        self.text(bitmap, left, top, right, colour);
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
    /// The pointer is over the group of buttons. Every button shows its
    /// glyph then, whichever of them the pointer is on.
    pub group_hovered: bool,
    /// The button that is held down, with the pointer still on it.
    pub pressed: Option<Button>,
    /// Maximised windows fill the screen edge to edge and have square
    /// corners.
    pub square: bool,
    /// The frame is wider than `width`: this is only its left part, so the
    /// right end is a straight cut with no corner or border.
    pub cut: bool,
}

/// One bottom corner of a window's frame, `size` pixels square: the border
/// around the rounded corner at `radius`, the desktop outside it, where the
/// client's own square corner would otherwise show. The left corner is at
/// the left of the canvas, or the right one when `right` is set. The rest of
/// the canvas is the border, or clear above it, so the client shows through.
#[cfg(test)]
pub fn draw_bottom_corner(
    size: i32,
    radius: f32,
    border: i32,
    edge: Rgba,
    desktop: Rgba,
    right: bool,
) -> Canvas {
    draw_bottom_corner_over(size, radius, border, edge, &|_, _| desktop, right)
}

/// As `draw_bottom_corner`, with what is behind the window at each pixel of
/// the corner given by `behind`: the desktop with the window's shadow on it.
pub fn draw_bottom_corner_over(
    size: i32,
    radius: f32,
    border: i32,
    edge: Rgba,
    behind: &dyn Fn(usize, usize) -> Rgba,
    right: bool,
) -> Canvas {
    let size = size.max(1);
    let mut canvas = Canvas::new(size as usize, size as usize);
    let inner_radius = (radius - border as f32).max(0.0);
    let (outer_radii, inner_radii, inner_x) = if right {
        ([0.0, 0.0, radius, 0.0], [0.0, 0.0, inner_radius, 0.0], 0)
    } else {
        (
            [0.0, 0.0, 0.0, radius],
            [0.0, 0.0, 0.0, inner_radius],
            border,
        )
    };
    let outer = Rounded::new(Rect::new(0, 0, size, size), outer_radii);
    let inner = Rounded::new(
        Rect::new(inner_x, 0, size - border, size - border),
        inner_radii,
    );
    for y in 0..size as usize {
        for x in 0..size as usize {
            let outside = outer.coverage(x, y);
            let inside = inner.coverage(x, y);
            canvas.blend(x, y, behind(x, y), 1.0 - outside);
            canvas.blend(x, y, edge, (outside - inside).max(0.0));
        }
    }
    canvas
}

/// `colour` darkened by `percent`.
fn darken(colour: Rgba, percent: u32) -> Rgba {
    let keep = 100 - percent.min(100);
    let scale = |v: u8| (u32::from(v) * keep / 100) as u8;
    Rgba {
        r: scale(colour.r),
        g: scale(colour.g),
        b: scale(colour.b),
        a: colour.a,
    }
}

/// One round button: a ring, the fill inside it and, while the pointer is
/// over the group, the glyph.
fn draw_light(canvas: &mut Canvas, theme: &Theme, bar: &Titlebar, button: Button, index: usize) {
    let rect = theme.metrics.button_rect(button);
    let look = theme.lights[index];
    let (mut fill, mut ring) = if bar.active {
        (look.fill, look.ring)
    } else {
        (theme.light_inactive, theme.light_inactive)
    };
    if bar.pressed == Some(button) {
        fill = darken(fill, theme.pressed_darken);
        ring = darken(ring, theme.pressed_darken);
    }
    let radius = rect.w as f32 / 2.0;
    let (cx, cy) = (rect.x as f32 + radius, rect.y as f32 + rect.h as f32 / 2.0);
    canvas.fill(Rounded::circle(cx, cy, radius), ring);
    canvas.fill(Rounded::circle(cx, cy, radius - theme.ring), fill);

    if !bar.group_hovered {
        return;
    }
    // The ends are pulled in by half the stroke, which the round caps give
    // back, so the glyph is as wide as `glyph` says.
    let stroke = theme.glyph_stroke;
    let half = (theme.glyph as f32 - stroke) / 2.0;
    let (left, right, top, bottom) = (cx - half, cx + half, cy - half, cy + half);
    match button {
        Button::Close => {
            canvas.line((left, top), (right, bottom), stroke, look.glyph);
            canvas.line((left, bottom), (right, top), stroke, look.glyph);
        }
        Button::Minimise => canvas.line((left, cy), (right, cy), stroke, look.glyph),
        Button::Maximise => {
            canvas.line((left, cy), (right, cy), stroke, look.glyph);
            canvas.line((cx, top), (cx, bottom), stroke, look.glyph);
        }
    }
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
    for (index, button) in BUTTONS.into_iter().enumerate() {
        draw_light(&mut canvas, theme, bar, button, index);
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
            group_hovered: false,
            pressed: None,
            square: false,
            cut: false,
        }
    }

    #[test]
    fn the_bottom_corners_are_the_desktop_and_the_edge_is_the_border() {
        let edge = Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        };
        let desktop = Rgba {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        };
        // Red, green, blue, alpha.
        let is_desktop = |p: [u8; 4]| p[2] > 200 && p[0] < 60;
        let is_edge = |p: [u8; 4]| p[0] > 200 && p[2] < 60;
        let left = draw_bottom_corner(8, 6.0, 2, edge, desktop, false);
        assert!(
            is_desktop(left.pixel(0, 7)),
            "left corner {:?}",
            left.pixel(0, 7)
        );
        assert!(
            is_edge(left.pixel(7, 7)),
            "bottom edge at the right of the piece"
        );
        assert!(is_edge(left.pixel(4, 7)), "bottom edge");
        assert_eq!(left.pixel(7, 0)[3], 0, "the client shows above the corner");
        let right = draw_bottom_corner(8, 6.0, 2, edge, desktop, true);
        assert!(
            is_desktop(right.pixel(7, 7)),
            "right corner {:?}",
            right.pixel(7, 7)
        );
        assert!(
            is_edge(right.pixel(0, 7)),
            "bottom edge at the left of the piece"
        );
        assert_eq!(right.pixel(0, 0)[3], 0, "the client shows above the corner");
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
    fn the_title_bar_is_sharp_at_a_scale_of_two() {
        let paint = theme().scaled(2.0);
        let bar = Titlebar {
            width: 604,
            active: true,
            group_hovered: false,
            pressed: None,
            square: false,
            cut: false,
        };
        let canvas = draw_titlebar(&paint, &bar, None);
        // Two rows of border at the top, then the fill with no blend between.
        assert_eq!(canvas.pixel(300, 0), rgba(paint.border));
        assert_eq!(canvas.pixel(300, 1), rgba(paint.border));
        for y in 2..(paint.metrics.titlebar as usize) {
            assert_eq!(canvas.pixel(300, y), rgba(paint.active), "row {y}");
        }
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

    fn centre_of(theme: &Theme, button: Button) -> (usize, usize) {
        let rect = theme.metrics.button_rect(button);
        (
            (rect.x + rect.w / 2) as usize,
            (rect.y + rect.h / 2) as usize,
        )
    }

    fn near(a: [u8; 4], b: Rgba) -> bool {
        within(a, b, 2)
    }

    fn within(a: [u8; 4], b: Rgba, tolerance: u8) -> bool {
        a[..3]
            .iter()
            .zip([b.r, b.g, b.b])
            .all(|(x, y)| x.abs_diff(y) <= tolerance)
    }

    #[test]
    fn the_buttons_are_twelve_pixels_eight_apart_from_twelve_in() {
        let theme = theme();
        let m = &theme.metrics;
        assert_eq!((m.button, m.gap, m.inset), (12, 8, 12));
        assert_eq!(m.button_rect(Button::Close).x, 12);
        assert_eq!(m.button_rect(Button::Minimise).x, 32);
        assert_eq!(m.button_rect(Button::Maximise).x, 52);
        // Centred in the bar.
        let rect = m.button_rect(Button::Close);
        assert_eq!(rect.y * 2 + rect.h, m.titlebar);
    }

    #[test]
    fn the_buttons_are_round() {
        let theme = theme();
        let canvas = draw_titlebar(&theme, &bar(true), None);
        for (index, button) in BUTTONS.into_iter().enumerate() {
            let rect = theme.metrics.button_rect(button);
            let (cx, cy) = centre_of(&theme, button);
            assert!(
                near(canvas.pixel(cx, cy), theme.lights[index].fill),
                "{button:?} fill"
            );
            // The corner of the square is not part of a circle.
            assert!(
                near(canvas.pixel(rect.x as usize, rect.y as usize), theme.active),
                "{button:?} is round"
            );
        }
    }

    /// The pixel at the left rim of a button, where the ring is.
    fn rim(canvas: &Canvas, theme: &Theme, button: Button) -> [u8; 4] {
        let rect = theme.metrics.button_rect(button);
        canvas.pixel(rect.x as usize, (rect.y + rect.h / 2) as usize)
    }

    #[test]
    fn the_ring_is_drawn_in_its_own_colour_at_the_rim() {
        // A ring two pixels wide makes the colours plain to read: the rim
        // pixel is all ring, the next one in all fill.
        let mut theme = theme();
        theme.ring = 2.0;
        let canvas = draw_titlebar(&theme, &bar(true), None);
        for (index, button) in BUTTONS.into_iter().enumerate() {
            let look = theme.lights[index];
            let rect = theme.metrics.button_rect(button);
            let y = (rect.y + rect.h / 2) as usize;
            assert!(
                within(rim(&canvas, &theme, button), look.ring, 8),
                "{button:?}"
            );
            assert!(
                near(canvas.pixel(rect.x as usize + 3, y), look.fill),
                "{button:?} inside the ring"
            );
            assert_ne!(look.ring, look.fill);
        }
    }

    #[test]
    fn the_token_ring_tints_the_rim_and_no_ring_does_not() {
        let with = theme();
        let mut without = theme();
        without.ring = 0.0;
        let drawn = draw_titlebar(&with, &bar(true), None);
        let bare = draw_titlebar(&without, &bar(true), None);
        for (index, button) in BUTTONS.into_iter().enumerate() {
            let look = with.lights[index];
            let (ring, plain) = (rim(&drawn, &with, button), rim(&bare, &without, button));
            assert_ne!(ring, plain, "{button:?} has a ring");
            // The rim with a ring is nearer the ring's colour.
            let away = |p: [u8; 4]| {
                (0..3)
                    .map(|i| {
                        u32::from(p[i])
                            .abs_diff(u32::from([look.ring.r, look.ring.g, look.ring.b][i]))
                    })
                    .sum::<u32>()
            };
            assert!(
                away(ring) < away(plain),
                "{button:?} rim tinted by the ring"
            );
        }
    }

    /// Which pixels of the button's square are not what the same button
    /// looks like without its glyph, and so belong to the glyph.
    fn glyph_pixels(theme: &Theme, button: Button) -> Vec<(i32, i32)> {
        let plain = draw_titlebar(theme, &bar(true), None);
        let mut hovered = bar(true);
        hovered.group_hovered = true;
        let shown = draw_titlebar(theme, &hovered, None);
        let rect = theme.metrics.button_rect(button);
        let mut found = Vec::new();
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                if shown.pixel(x as usize, y as usize) != plain.pixel(x as usize, y as usize) {
                    found.push((x - rect.x, y - rect.y));
                }
            }
        }
        found
    }

    #[test]
    fn each_button_has_its_own_glyph_in_the_glyph_colour() {
        let theme = theme();
        let (cross, minus, plus) = (
            glyph_pixels(&theme, Button::Close),
            glyph_pixels(&theme, Button::Minimise),
            glyph_pixels(&theme, Button::Maximise),
        );
        assert_ne!(cross, minus);
        assert_ne!(cross, plus);
        assert_ne!(minus, plus);

        // The button is 12 across, the centre between pixels 5 and 6.
        let has = |set: &[(i32, i32)], x, y| set.contains(&(x, y));
        // The cross covers the diagonals and not the middle of the edges.
        assert!(has(&cross, 4, 4) && has(&cross, 7, 7));
        assert!(has(&cross, 7, 4) && has(&cross, 4, 7));
        assert!(!has(&cross, 5, 2) && !has(&cross, 2, 5));
        // The minus is a bar across the middle and nothing above it.
        assert!(has(&minus, 4, 5) && has(&minus, 7, 6));
        assert!(
            minus.iter().all(|(_, y)| *y >= 5),
            "no pixel of the minus above the centre: {minus:?}"
        );
        assert!(minus.iter().all(|(_, y)| *y <= 6));
        // The plus has the bar and a stroke up and down through the centre.
        assert!(has(&plus, 4, 5) && has(&plus, 5, 3) && has(&plus, 6, 8));
        assert!(plus.iter().any(|(_, y)| *y < 5));
        assert!(!has(&plus, 3, 3));

        // The strokes are in the glyph colour: the pixel in the middle of
        // each is closer to it than to the fill it sits on.
        let canvas = {
            let mut hovered = bar(true);
            hovered.group_hovered = true;
            draw_titlebar(&theme, &hovered, None)
        };
        for (index, button) in BUTTONS.into_iter().enumerate() {
            let rect = theme.metrics.button_rect(button);
            let look = theme.lights[index];
            let (x, y) = (rect.x as usize + 5, rect.y as usize + 5);
            let got = canvas.pixel(x, y);
            let distance = |c: Rgba| {
                (0..3)
                    .map(|i| u32::from(got[i]).abs_diff(u32::from([c.r, c.g, c.b][i])))
                    .sum::<u32>()
            };
            assert!(distance(look.glyph) < distance(look.fill), "{button:?}");
        }
    }

    #[test]
    fn a_window_that_is_not_focused_has_grey_buttons() {
        let theme = theme();
        let canvas = draw_titlebar(&theme, &bar(false), None);
        for button in BUTTONS {
            let (cx, cy) = centre_of(&theme, button);
            assert_eq!(canvas.pixel(cx, cy), rgba(theme.light_inactive));
        }
        assert_eq!(rgba(theme.light_inactive), [0x4d, 0x4d, 0x4d, 255]);
    }

    #[test]
    fn a_pressed_button_is_fifteen_percent_darker() {
        let theme = theme();
        let mut held = bar(true);
        held.group_hovered = true;
        held.pressed = Some(Button::Close);
        let canvas = draw_titlebar(&theme, &held, None);
        // Away from the glyph, still inside the circle.
        let rect = theme.metrics.button_rect(Button::Close);
        let (x, y) = (rect.x as usize + 2, rect.y as usize + 6);
        let fill = theme.lights[0].fill;
        let want = darken(fill, 15);
        assert!(near(canvas.pixel(x, y), want), "{:?}", canvas.pixel(x, y));
        assert_eq!(want.r, 216);
        // The others stay as they were.
        let (cx, cy) = centre_of(&theme, Button::Maximise);
        assert_ne!(canvas.pixel(cx, cy), rgba(darken(theme.lights[2].fill, 15)));
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
