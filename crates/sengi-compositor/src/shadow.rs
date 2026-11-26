//! The shadow around a framed window.
//!
//! There is no blur and no gradient. The shadow is a stack of flat
//! translucent rounded rectangles, each a little larger than the one before
//! and each a little more transparent, so the edge falls off in steps. All
//! of it is worked out per pixel here and painted once into a few small
//! pictures: four corners and four one-pixel strips, which the renderer
//! stretches along the sides. They do not depend on the size of any window,
//! so every window at a scale shares them.

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    utils::{Logical, Rectangle, Size, Transform},
};

use sengi_tokens::{Rgba, Tokens};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowStyle {
    /// How many rectangles; none turns shadows off.
    pub steps: u32,
    /// How far the outermost reaches past the frame.
    pub spread: i32,
    /// How far the shadow sits below the frame.
    pub offset: i32,
    /// How dark the nearest rectangles are, 0 to 1, for the focused window
    /// and for the others.
    pub focused: f32,
    pub unfocused: f32,
    pub colour: Rgba,
}

impl ShadowStyle {
    pub fn from_tokens(tokens: &Tokens) -> Self {
        let number = |name: &str| {
            tokens
                .px(name)
                .unwrap_or_else(|| panic!("tokens define {name}"))
        };
        Self {
            steps: number("shadow.steps"),
            spread: number("shadow.spread") as i32,
            offset: number("shadow.offset") as i32,
            focused: number("shadow.alpha-focused").min(100) as f32 / 100.0,
            unfocused: number("shadow.alpha-unfocused").min(100) as f32 / 100.0,
            colour: tokens
                .color("shadow.color")
                .expect("tokens define shadow.color"),
        }
    }

    /// The style at `scale` times its size.
    pub fn scaled(&self, scale: f64) -> Self {
        let px = |v: i32| (f64::from(v) * scale).round() as i32;
        Self {
            spread: px(self.spread),
            offset: px(self.offset),
            ..*self
        }
    }

    /// Whether anything would be drawn.
    pub fn visible(&self) -> bool {
        self.steps > 0 && self.spread > 0 && self.focused.max(self.unfocused) > 0.0
    }

    pub fn peak(&self, active: bool) -> f32 {
        if active {
            self.focused
        } else {
            self.unfocused
        }
    }
}

/// The stack of rectangles for one darkness: how far each reaches past the
/// frame and how opaque it is.
pub struct Falloff {
    rings: Vec<(f32, f32)>,
}

impl Falloff {
    pub fn new(style: &ShadowStyle, peak: f32) -> Self {
        let steps = style.steps.max(1) as usize;
        // The darkness wanted inside ring `b` and outside the one before:
        // full next to the frame, dropping towards nothing at the edge.
        let wanted = |b: usize| {
            if b >= steps {
                0.0
            } else {
                peak * ((steps - b) as f32 / steps as f32).powi(2)
            }
        };
        // The stacked rectangles have to add up to it, outermost first.
        let rings = (0..steps)
            .map(|b| {
                let reach = style.spread as f32 * (b + 1) as f32 / steps as f32;
                let alpha = 1.0 - (1.0 - wanted(b)) / (1.0 - wanted(b + 1));
                (reach, alpha)
            })
            .collect();
        Self { rings }
    }

    /// How dark the shadow is at distance `d` from the edge of its
    /// rectangle, negative inside it.
    fn alpha(&self, d: f32) -> f32 {
        let mut clear = 1.0;
        for &(reach, alpha) in &self.rings {
            let cover = (reach - d + 0.5).clamp(0.0, 1.0);
            clear *= 1.0 - alpha * cover;
        }
        1.0 - clear
    }

    /// The darkness at pixel `(x, y)`, measured from the top left corner
    /// of a frame `width` by `height` with corners of `radius`. The shadow
    /// is that frame moved down by `offset`.
    pub fn at(&self, width: i32, height: i32, radius: i32, offset: i32, x: i32, y: i32) -> f32 {
        let (half_w, half_h) = (width as f32 / 2.0, height as f32 / 2.0);
        let r = (radius as f32).min(half_w).min(half_h);
        let dx = ((x as f32 + 0.5) - half_w).abs() - (half_w - r);
        let dy = ((y as f32 + 0.5) - (half_h + offset as f32)).abs() - (half_h - r);
        let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
        self.alpha(outside + dx.max(dy).min(0.0) - r)
    }
}

/// The parts the shadow is cut into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Top,
    Bottom,
    Left,
    Right,
}

pub const PARTS: [Part; 8] = [
    Part::TopLeft,
    Part::TopRight,
    Part::BottomLeft,
    Part::BottomRight,
    Part::Top,
    Part::Bottom,
    Part::Left,
    Part::Right,
];

/// How far the parts reach into the frame: the corner radius, or the
/// offset if that is more, so no lit pixel below the frame is missed.
pub fn reach(style: &ShadowStyle, radius: i32) -> i32 {
    radius.max(style.offset).max(1)
}

/// A corner's side, and the thickness of a strip.
pub fn side(style: &ShadowStyle, radius: i32) -> i32 {
    style.spread + reach(style, radius)
}

/// One part as premultiplied ARGB8888 pixels, with its width and height.
/// A strip is three pixels wide or high, all alike: the renderer shows only
/// the middle one, stretched, and the others keep its edges from fading.
pub fn paint(style: &ShadowStyle, peak: f32, radius: i32, part: Part) -> (i32, i32, Vec<u8>) {
    let falloff = Falloff::new(style, peak);
    let reach = reach(style, radius);
    let k = side(style, radius);
    // A frame big enough for the corners not to meet.
    let size = 2 * reach + 4;
    let (left, right) = (-style.spread, size - reach);
    let (top, bottom) = (style.offset - style.spread, style.offset + size - reach);
    let (x0, y0, w, h) = match part {
        Part::TopLeft => (left, top, k, k),
        Part::TopRight => (right, top, k, k),
        Part::BottomLeft => (left, bottom, k, k),
        Part::BottomRight => (right, bottom, k, k),
        Part::Top => (size / 2 - 1, top, 3, k),
        Part::Bottom => (size / 2 - 1, bottom, 3, k),
        Part::Left => (left, size / 2 - 1, k, 3),
        Part::Right => (right, size / 2 - 1, k, 3),
    };
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let alpha = falloff.at(size, size, radius, style.offset, x, y);
            pixels.extend(premultiplied(style.colour, alpha));
        }
    }
    (w, h, pixels)
}

/// `colour` at `alpha` as one ARGB8888 pixel, blue first.
fn premultiplied(colour: Rgba, alpha: f32) -> [u8; 4] {
    let alpha = alpha.clamp(0.0, 1.0) * f32::from(colour.a) / 255.0;
    let scale = |v: u8| (f32::from(v) * alpha).round() as u8;
    [
        scale(colour.b),
        scale(colour.g),
        scale(colour.r),
        (alpha * 255.0).round() as u8,
    ]
}

/// The pictures of a shadow at one scale, for a window that is focused and
/// for one that is not. Eight small ones each, whatever the windows.
pub struct ShadowSet {
    /// Width, height and pixels, indexed by `active`, then by `PARTS`.
    pictures: [[(i32, i32, Vec<u8>); 8]; 2],
    buffer_scale: i32,
    /// A corner's side and a strip's thickness, and how far the parts reach
    /// into the frame, in logical pixels.
    pub side: i32,
    pub reach: i32,
}

/// The buffers of one window's shadow, in the order of `PARTS`. Each window
/// has its own: the renderer tells elements apart by their buffer.
pub type Buffers = [MemoryRenderBuffer; 8];

impl ShadowSet {
    /// `logical` and `radius` are in logical pixels, `scaled` and
    /// `scaled_radius` the same at `scale`, which is what is painted.
    pub fn new(
        logical: &ShadowStyle,
        radius: i32,
        scaled: &ShadowStyle,
        scaled_radius: i32,
        scale: f64,
    ) -> Self {
        let make = |peak: f32| PARTS.map(|part| paint(scaled, peak, scaled_radius, part));
        Self {
            pictures: [make(scaled.unfocused), make(scaled.focused)],
            buffer_scale: scale.round().max(1.0) as i32,
            side: side(logical, radius),
            reach: reach(logical, radius),
        }
    }

    pub fn buffers(&self, active: bool) -> Buffers {
        self.pictures[usize::from(active)]
            .each_ref()
            .map(|(w, h, pixels)| {
                MemoryRenderBuffer::from_slice(
                    pixels,
                    Fourcc::Argb8888,
                    (*w, *h),
                    self.buffer_scale,
                    Transform::Normal,
                    None,
                )
            })
    }

    /// The size of a part in logical pixels, which is how much of its
    /// buffer a renderer element is to show.
    pub fn source(&self, part: usize) -> Rectangle<f64, Logical> {
        let (w, h, _) = &self.pictures[0][part];
        let scale = f64::from(self.buffer_scale);
        let (w, h) = (f64::from(*w) / scale, f64::from(*h) / scale);
        match PARTS[part] {
            Part::Top | Part::Bottom => Rectangle::new((w / 3.0, 0.0).into(), (w / 3.0, h).into()),
            Part::Left | Part::Right => Rectangle::new((0.0, h / 3.0).into(), (w, h / 3.0).into()),
            _ => Rectangle::from_size(Size::from((w, h))),
        }
    }
}

/// What shows where the shadow lies over `behind`.
pub fn over(style: &ShadowStyle, alpha: f32, behind: Rgba) -> Rgba {
    let alpha = alpha.clamp(0.0, 1.0) * f32::from(style.colour.a) / 255.0;
    let mix = |shade: u8, below: u8| {
        (f32::from(shade) * alpha + f32::from(below) * (1.0 - alpha)).round() as u8
    };
    Rgba {
        r: mix(style.colour.r, behind.r),
        g: mix(style.colour.g, behind.g),
        b: mix(style.colour.b, behind.b),
        a: behind.a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> ShadowStyle {
        ShadowStyle::from_tokens(&Tokens::builtin())
    }

    #[test]
    fn the_tokens_give_a_visible_shadow() {
        let style = style();
        assert!(style.visible());
        assert!(style.focused > style.unfocused);
        assert_eq!(style.steps, 8);
    }

    #[test]
    fn zero_steps_draw_nothing() {
        let mut style = style();
        style.steps = 0;
        assert!(!style.visible());
    }

    #[test]
    fn the_shadow_falls_off_in_steps_and_ends() {
        let style = style();
        let falloff = Falloff::new(&style, style.focused);
        let mut last = f32::MAX;
        let mut levels = Vec::new();
        // Straight down from the middle of a large frame.
        for y in 0..40 {
            let alpha = falloff.at(200, 100, 6, style.offset, 100, 100 + y);
            assert!(alpha <= last + 1e-4, "darker further out at {y}");
            last = alpha;
            let level = (alpha * 255.0).round() as u32;
            if levels.last() != Some(&level) {
                levels.push(level);
            }
        }
        assert!(levels[0] > 25, "there at the frame: {levels:?}");
        assert!(levels[0] < 50, "faint: {levels:?}");
        assert_eq!(*levels.last().unwrap(), 0, "ends: {levels:?}");
        assert!(levels.len() >= 8, "stepped: {levels:?}");
    }

    #[test]
    fn it_is_darker_below_than_above() {
        let style = style();
        let falloff = Falloff::new(&style, style.focused);
        let above = falloff.at(200, 100, 6, style.offset, 100, -4);
        let below = falloff.at(200, 100, 6, style.offset, 100, 104);
        assert!(below > above, "{above} {below}");
    }

    #[test]
    fn the_strips_match_the_corners_where_they_meet() {
        let style = style();
        let radius = 6;
        let (w, _, top) = paint(&style, style.focused, radius, Part::TopLeft);
        let (_, _, strip) = paint(&style, style.focused, radius, Part::Top);
        // The last column of the corner is the strip, at the rows they share.
        let k = side(&style, radius);
        for y in 0..k as usize {
            let corner = top[(y * w as usize + w as usize - 1) * 4 + 3];
            assert!(
                corner.abs_diff(strip[y * 12 + 7]) <= 1,
                "row {y}: {corner} against {}",
                strip[y * 12 + 7]
            );
            // The three columns of a strip are alike.
            assert_eq!(strip[y * 12 + 3], strip[y * 12 + 7]);
            assert_eq!(strip[y * 12 + 11], strip[y * 12 + 7]);
        }
    }

    #[test]
    fn the_parts_have_pixels() {
        let style = style();
        for part in PARTS {
            let (w, h, pixels) = paint(&style, style.focused, 6, part);
            let most = pixels.chunks(4).map(|p| p[3]).max().unwrap();
            eprintln!("{part:?} {w}x{h} most {most}");
            assert!(most > 20, "{part:?}");
        }
    }

    #[test]
    fn building_the_parts_is_cheap() {
        let style = style().scaled(2.0);
        let started = std::time::Instant::now();
        for _ in 0..10 {
            for part in PARTS {
                for peak in [style.focused, style.unfocused] {
                    paint(&style, peak, 12, part);
                }
            }
        }
        let each = started.elapsed() / 10;
        eprintln!("all sixteen parts at scale 2: {each:?}");
        assert!(each.as_millis() < 100);
    }
}
