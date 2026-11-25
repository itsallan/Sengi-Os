//! Snap hints: while a window is dragged to the edge of a screen, an
//! outline shows where it will land when it is let go. The place is worked
//! out by the same function that places the window on release, so the
//! hint and the result cannot differ.

use smithay::{
    backend::allocator::Fourcc,
    backend::renderer::{
        element::{
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            Kind,
        },
        ImportMem, Renderer, Texture,
    },
    desktop::Window,
    output::Output,
    utils::{Logical, Physical, Point, Size, Transform},
};

use crate::{
    decoration::{Canvas, Theme},
    keys::Edge,
    layout::Rect,
    render::OutputElement,
    state::State,
};

/// How close to the edge of the screen the pointer has to be, in pixels:
/// labwc's default.
pub const RANGE: f64 = 1.0;

/// What letting go of a window at some place does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Snap {
    /// At the top of the screen: filling the usable area.
    Maximise,
    /// At a side or the bottom: half of the screen.
    Half(Edge),
}

/// What happens to a window let go of at `at`, on a screen whose edges are
/// at `left`, `top`, `right` and `bottom`. The top wins in the corners.
pub fn snap_at(at: (f64, f64), left: f64, top: f64, right: f64, bottom: f64) -> Option<Snap> {
    let (x, y) = at;
    if y <= top + RANGE {
        Some(Snap::Maximise)
    } else if x <= left + RANGE {
        Some(Snap::Half(Edge::Left))
    } else if x >= right - RANGE {
        Some(Snap::Half(Edge::Right))
    } else if y >= bottom - RANGE {
        Some(Snap::Half(Edge::Down))
    } else {
        None
    }
}

/// The outline on screen: where, and the picture of it, made once for the
/// place and the scale it is at.
pub struct Hint {
    /// Where the window will be, on the desktop.
    pub frame: Rect,
    pub output: Output,
    scale_key: u32,
    buffer: MemoryRenderBuffer,
}

/// A rounded rectangle's distance to a point: negative inside it.
fn distance(x: f32, y: f32, w: f32, h: f32, radius: f32) -> f32 {
    let (cx, cy) = (x - w / 2.0, y - h / 2.0);
    let (qx, qy) = (cx.abs() - (w / 2.0 - radius), cy.abs() - (h / 2.0 - radius));
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

/// The outline as a picture `w` by `h` pixels: a flat translucent fill and
/// a line around it, both the accent. Corners are the theme's radius.
fn draw(theme: &Theme, w: i32, h: i32) -> Canvas {
    let mut canvas = Canvas::new(w.max(1) as usize, h.max(1) as usize);
    let (wf, hf) = (w as f32, h as f32);
    let radius = (theme.radius as f32).min(wf / 2.0).min(hf / 2.0);
    // Heavier than a window's border, so it reads over any picture.
    let edge = (theme.metrics.border * 2).max(2) as f32;
    for y in 0..h.max(0) as usize {
        for x in 0..w.max(0) as usize {
            let d = distance(x as f32 + 0.5, y as f32 + 0.5, wf, hf, radius);
            let inside = (0.5 - d).clamp(0.0, 1.0);
            if inside <= 0.0 {
                continue;
            }
            canvas.blend(x, y, theme.snap_fill, inside);
            let ring = inside * (d + edge + 0.5).clamp(0.0, 1.0);
            canvas.blend(x, y, theme.snap_edge, ring);
        }
    }
    canvas
}

impl State {
    /// What letting go at `at` does, and on which screen.
    pub fn snap_for(&self, at: (f64, f64)) -> Option<(Output, Snap)> {
        let output = self.output_at(at.into())?;
        let screen = self.space.output_geometry(&output)?;
        let (left, top) = (f64::from(screen.loc.x), f64::from(screen.loc.y));
        let right = left + f64::from(screen.size.w);
        let bottom = top + f64::from(screen.size.h);
        let snap = snap_at(at, left, top, right, bottom)?;
        Some((output, snap))
    }

    /// The place `window` takes when it is snapped, as a frame on the desktop.
    pub fn snap_frame(&self, window: &Window, snap: Snap) -> Option<Rect> {
        match snap {
            Snap::Maximise => {
                let frame = self.frame_rect(window)?;
                Some(self.usable_area_near(frame))
            }
            Snap::Half(edge) => self.half_frame(window, edge),
        }
    }

    /// While `window` is dragged and the pointer is at `at`: shows or moves
    /// the outline, or takes it away when nothing would snap.
    pub fn update_snap_hint(&mut self, window: &Window, at: (f64, f64)) {
        let wanted = self.snap_for(at).and_then(|(output, snap)| {
            // Not for a window that cannot be snapped.
            if !self.can_snap(window) {
                return None;
            }
            Some((output, self.snap_frame(window, snap)?))
        });
        let Some((output, frame)) = wanted else {
            self.clear_snap_hint();
            return;
        };
        let scale = self.scale_of(&output);
        let key = crate::outputs::scale_key(scale);
        if let Some(hint) = &self.snap_hint {
            if hint.frame == frame && hint.output == output && hint.scale_key == key {
                return;
            }
        }
        let theme = &self.painter(scale).theme;
        let w = (f64::from(frame.w) * scale).round() as i32;
        let h = (f64::from(frame.h) * scale).round() as i32;
        let canvas = draw(theme, w, h);
        let buffer = MemoryRenderBuffer::from_slice(
            &canvas.into_pixels(),
            Fourcc::Argb8888,
            (w.max(1), h.max(1)),
            1,
            Transform::Normal,
            None,
        );
        self.snap_hint = Some(Hint {
            frame,
            output,
            scale_key: key,
            buffer,
        });
        self.request_redraw();
    }

    /// One line for the SIGUSR1 report.
    pub fn snap_status(&self) -> String {
        match &self.snap_hint {
            Some(hint) => format!(
                "snap hint: {}x{} at {},{} on {}",
                hint.frame.w,
                hint.frame.h,
                hint.frame.x,
                hint.frame.y,
                hint.output.name()
            ),
            None => "snap hint: none".to_string(),
        }
    }

    /// The pointer let go or the drag ended: the outline goes.
    pub fn clear_snap_hint(&mut self) {
        if self.snap_hint.take().is_some() {
            self.request_redraw();
        }
    }

    /// The outline, as a render element on `output`: nothing where it is
    /// not shown.
    pub fn snap_hint_element<R>(
        &self,
        output: &Output,
        renderer: &mut R,
    ) -> Option<OutputElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Clone + Send + Texture + 'static,
    {
        let hint = self
            .snap_hint
            .as_ref()
            .filter(|hint| &hint.output == output)?;
        let geometry = self.space.output_geometry(output)?;
        let scale = self.scale_of(output);
        let at = Point::<f64, Physical>::from((
            f64::from(hint.frame.x - geometry.loc.x) * scale,
            f64::from(hint.frame.y - geometry.loc.y) * scale,
        ));
        let logical = Size::<i32, Logical>::from((hint.frame.w, hint.frame.h));
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            at.to_i32_round::<i32>().to_f64(),
            &hint.buffer,
            None,
            None,
            Some(logical),
            Kind::Unspecified,
        )
        .ok()
        .map(OutputElement::Memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (f64, f64, f64, f64) = (0.0, 0.0, 1280.0, 720.0);

    fn at(x: f64, y: f64) -> Option<Snap> {
        snap_at((x, y), SCREEN.0, SCREEN.1, SCREEN.2, SCREEN.3)
    }

    #[test]
    fn the_edges_of_a_screen_snap_and_the_middle_does_not() {
        assert_eq!(at(640.0, 0.0), Some(Snap::Maximise));
        assert_eq!(at(0.0, 300.0), Some(Snap::Half(Edge::Left)));
        assert_eq!(at(1279.0, 300.0), Some(Snap::Half(Edge::Right)));
        assert_eq!(at(640.0, 719.0), Some(Snap::Half(Edge::Down)));
        assert_eq!(at(640.0, 300.0), None);
        assert_eq!(at(5.0, 300.0), None);
    }

    #[test]
    fn the_top_wins_in_a_corner() {
        assert_eq!(at(0.0, 0.0), Some(Snap::Maximise));
        assert_eq!(at(1279.0, 0.0), Some(Snap::Maximise));
        assert_eq!(at(0.0, 719.0), Some(Snap::Half(Edge::Left)));
    }

    #[test]
    fn a_second_screen_is_judged_by_its_own_edges() {
        let right = |x, y| snap_at((x, y), 1280.0, 0.0, 2560.0, 720.0);
        assert_eq!(right(1280.0, 300.0), Some(Snap::Half(Edge::Left)));
        assert_eq!(right(1290.0, 300.0), None);
        assert_eq!(right(2559.0, 300.0), Some(Snap::Half(Edge::Right)));
    }

    #[test]
    fn the_outline_is_translucent_inside_and_clear_outside_its_corners() {
        let theme = crate::decoration::Theme::from_tokens(&sengi_tokens::Tokens::builtin());
        let canvas = draw(&theme, 200, 100);
        // The very corner pixel is outside the rounded shape.
        assert_eq!(canvas.pixel(0, 0)[3], 0);
        // The middle holds the fill, which is see-through.
        let middle = canvas.pixel(100, 50);
        assert!(middle[3] > 0 && middle[3] < 128, "alpha {}", middle[3]);
        // The line along the edge is firmer than the fill.
        let edge = canvas.pixel(100, 0);
        assert!(edge[3] > middle[3], "edge {} fill {}", edge[3], middle[3]);
    }
}
