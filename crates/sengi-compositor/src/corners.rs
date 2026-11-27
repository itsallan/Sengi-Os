//! Cuts the square bottom corners off a framed window's client area, so
//! that whatever is behind the window shows around the frame's rounded
//! corners: the wallpaper, another window, the shadow.
//!
//! A client's picture is drawn as a few rectangles: everything above the
//! corners as one, and below that one for each run of rows that lose the
//! same number of pixels at each end.

use std::collections::HashMap;

use smithay::{
    backend::renderer::{
        element::{utils::CropRenderElement, Element, Id, Kind, RenderElement, UnderlyingStorage},
        utils::{CommitCounter, DamageSet, OpaqueRegions},
        Renderer,
    },
    utils::{Buffer, Physical, Rectangle, Scale, Transform},
};

/// Far past any screen, and small enough not to overflow when added.
const FAR: i32 = 1 << 24;

/// Where the corners are cut, in the output's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notch<'a> {
    /// The top left pixel of the rows that are cut, at the client's left
    /// edge.
    pub left: i32,
    pub top: i32,
    /// The pixel after the client's right edge.
    pub right: i32,
    /// How many pixels each row loses at each end, from `top` down.
    pub insets: &'a [i32],
    /// The width of the frame's border: content a client draws further out
    /// than that is left as it was.
    pub border: i32,
}

impl Notch<'_> {
    /// The rectangles that together are everything but the notches. The
    /// first is what lies above them.
    pub fn pieces(&self) -> Vec<Rectangle<i32, Physical>> {
        let rect = |x0: i32, y0: i32, x1: i32, y1: i32| {
            Rectangle::new((x0, y0).into(), ((x1 - x0).max(0), y1 - y0).into())
        };
        let first = self.insets.iter().position(|inset| *inset > 0);
        let Some(first) = first else {
            return vec![rect(-FAR, -FAR, FAR, FAR)];
        };
        let mut y = self.top + first as i32;
        let mut pieces = vec![rect(-FAR, -FAR, FAR, y)];
        for run in self.insets[first..].chunk_by(|a, b| a == b) {
            let next = y + run.len() as i32;
            let inset = run[0];
            pieces.push(rect(self.left + inset, y, self.right - inset, next));
            pieces.push(rect(-FAR, y, self.left - self.border, next));
            pieces.push(rect(self.right + self.border, y, FAR, next));
            y = next;
        }
        pieces.push(rect(-FAR, y, FAR, FAR));
        pieces
    }
}

/// The names of the pieces after the first, kept per window so that the
/// renderer knows each again in the next frame.
#[derive(Default)]
pub struct Ids(HashMap<(Id, usize), Id>);

impl Ids {
    /// More than a window with a few subsurfaces on two screens needs; past
    /// it the names are made anew, which only repaints the corners.
    const MOST: usize = 256;

    pub fn get(&mut self, surface: &Id, piece: usize) -> Id {
        if self.0.len() >= Self::MOST {
            self.0.clear();
        }
        self.0
            .entry((surface.clone(), piece))
            .or_insert_with(Id::new)
            .clone()
    }
}

/// One of the small pieces at the bottom of a client: a part of its surface
/// under a name of its own. It is always composed, never put on a plane.
pub struct Piece<E> {
    pub element: CropRenderElement<E>,
    pub id: Id,
}

impl<E: Element> Element for Piece<E> {
    fn id(&self) -> &Id {
        &self.id
    }

    fn current_commit(&self) -> CommitCounter {
        self.element.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.element.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        self.element.geometry(scale)
    }

    fn transform(&self) -> Transform {
        self.element.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.element.damage_since(scale, commit)
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.element.opaque_regions(scale)
    }

    fn alpha(&self) -> f32 {
        self.element.alpha()
    }

    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: Renderer, E: RenderElement<R>> RenderElement<R> for Piece<E> {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), R::Error> {
        self.element.draw(frame, src, dst, damage, opaque_regions)
    }

    fn underlying_storage(&self, _renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether the pixel is in one of the pieces, and in no more than one.
    fn kept(pieces: &[Rectangle<i32, Physical>], x: i32, y: i32) -> bool {
        let n = pieces.iter().filter(|piece| piece.contains((x, y))).count();
        assert!(n <= 1, "{x},{y} is in {n} pieces");
        n == 1
    }

    #[test]
    fn only_the_notches_are_left_out() {
        let notch = Notch {
            left: 10,
            top: 100,
            right: 60,
            insets: &[0, 0, 1, 1, 3],
            border: 1,
        };
        let pieces = notch.pieces();
        for y in 90..115 {
            for x in 0..70 {
                let inset = usize::try_from(y - 100)
                    .ok()
                    .and_then(|row| notch.insets.get(row))
                    .copied()
                    .unwrap_or(0);
                let left = (9..10 + inset).contains(&x);
                let right = (60 - inset..61).contains(&x);
                let notched = inset > 0 && (left || right);
                assert_eq!(kept(&pieces, x, y), !notched, "at {x},{y}");
            }
        }
    }

    #[test]
    fn without_insets_the_picture_is_whole() {
        let notch = Notch {
            left: 0,
            top: 0,
            right: 10,
            insets: &[0, 0],
            border: 1,
        };
        assert_eq!(notch.pieces().len(), 1);
    }
}
