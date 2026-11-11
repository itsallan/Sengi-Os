//! Geometry of the window frame: title bar, buttons, border and resize grips.
//!
//! All plain arithmetic on rectangles, so it is tested without a compositor.

use crate::layout::Rect;

/// Sizes of the frame parts, all in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// Height of the title bar, including the border along its top.
    pub titlebar: i32,
    pub border: i32,
    /// Side of a square title bar button.
    pub button: i32,
    /// Space between buttons.
    pub gap: i32,
    /// Space between the frame edge and the first button.
    pub padding: i32,
    /// How far around the frame edge a press starts a resize.
    pub grip: i32,
    /// How far from a corner a press on an edge resizes both ways.
    pub corner: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Close,
    Minimise,
    Maximise,
}

/// Left to right, as drawn.
pub const BUTTONS: [Button; 3] = [Button::Close, Button::Minimise, Button::Maximise];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Edges {
    pub fn any(self) -> bool {
        self.left || self.right || self.top || self.bottom
    }
}

/// What part of a decorated window a point is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Resize(Edges),
    Button(Button),
    Titlebar,
    Client,
}

impl Metrics {
    /// The frame around a client area.
    pub fn frame(&self, client: Rect) -> Rect {
        Rect::new(
            client.x - self.border,
            client.y - self.titlebar,
            client.w + 2 * self.border,
            client.h + self.titlebar + self.border,
        )
    }

    /// The client area inside a frame.
    pub fn client(&self, frame: Rect) -> Rect {
        Rect::new(
            frame.x + self.border,
            frame.y + self.titlebar,
            (frame.w - 2 * self.border).max(1),
            (frame.h - self.titlebar - self.border).max(1),
        )
    }

    /// A button's rectangle relative to the frame's top-left corner.
    pub fn button_rect(&self, button: Button) -> Rect {
        let index = BUTTONS.iter().position(|b| *b == button).unwrap_or(0) as i32;
        Rect::new(
            self.border + self.padding + index * (self.button + self.gap),
            (self.titlebar - self.button) / 2,
            self.button,
            self.button,
        )
    }

    /// Where the buttons end, relative to the frame's left edge.
    pub fn buttons_end(&self) -> i32 {
        let last = self.button_rect(BUTTONS[BUTTONS.len() - 1]);
        last.x + last.w
    }

    /// The narrowest frame that still shows every button.
    pub fn min_frame_width(&self) -> i32 {
        self.buttons_end() + self.padding + self.border
    }

    /// What is at `(x, y)` for a window with the given frame. `resizable` is
    /// false for maximised windows, which have no grips.
    pub fn hit(&self, frame: Rect, x: i32, y: i32, resizable: bool) -> Option<Hit> {
        let grip = if resizable { self.grip } else { 0 };
        let reach = Rect::new(
            frame.x - grip,
            frame.y - grip,
            frame.w + 2 * grip,
            frame.h + 2 * grip,
        );
        if !reach.contains(x, y) {
            return None;
        }

        // Buttons win over the grip that overlaps their top edge.
        let (local_x, local_y) = (x - frame.x, y - frame.y);
        if let Some(button) = BUTTONS
            .into_iter()
            .find(|b| self.button_rect(*b).contains(local_x, local_y))
        {
            return Some(Hit::Button(button));
        }

        if resizable {
            let edges = self.edges(frame, x, y);
            if edges.any() {
                return Some(Hit::Resize(edges));
            }
        }
        if local_y < self.titlebar {
            Some(Hit::Titlebar)
        } else {
            Some(Hit::Client)
        }
    }

    fn edges(&self, frame: Rect, x: i32, y: i32) -> Edges {
        // The grip reaches outside the frame and covers the border inside.
        let inside = self.border.max(1);
        let mut edges = Edges {
            left: x < frame.x + inside,
            right: x >= frame.right() - inside,
            top: y < frame.y + inside,
            bottom: y >= frame.bottom() - inside,
        };
        if edges.top || edges.bottom {
            edges.left |= x < frame.x + self.corner;
            edges.right |= x >= frame.right() - self.corner;
        }
        if edges.left || edges.right {
            edges.top |= y < frame.y + self.corner;
            edges.bottom |= y >= frame.bottom() - self.corner;
        }
        // Tiny windows: opposite edges cannot both move.
        if edges.left && edges.right {
            edges.left = x - frame.x < frame.right() - x;
            edges.right = !edges.left;
        }
        if edges.top && edges.bottom {
            edges.top = y - frame.y < frame.bottom() - y;
            edges.bottom = !edges.top;
        }
        edges
    }
}

/// The client area after dragging `edges` by `(dx, dy)` from `initial`.
///
/// The edges that are not dragged stay where they were. `min` and `max` are
/// client sizes; a zero in `max` means no limit on that axis.
pub fn resize(
    initial: Rect,
    edges: Edges,
    delta: (i32, i32),
    min: (i32, i32),
    max: (i32, i32),
) -> Rect {
    let limit = |size: i32, min: i32, max: i32| {
        let min = min.max(1);
        let max = if max > 0 { max.max(min) } else { i32::MAX };
        size.clamp(min, max)
    };

    let mut rect = initial;
    if edges.left {
        rect.w = limit(initial.w - delta.0, min.0, max.0);
        rect.x = initial.right() - rect.w;
    } else if edges.right {
        rect.w = limit(initial.w + delta.0, min.0, max.0);
    }
    if edges.top {
        rect.h = limit(initial.h - delta.1, min.1, max.1);
        rect.y = initial.bottom() - rect.h;
    } else if edges.bottom {
        rect.h = limit(initial.h + delta.1, min.1, max.1);
    }
    rect
}

/// Where a client area belongs once it has the size `size`, given that it
/// was resized by `edges` from `initial`: the opposite edges stay put.
pub fn anchor(initial: Rect, edges: Edges, size: (i32, i32)) -> (i32, i32) {
    let x = if edges.left {
        initial.right() - size.0
    } else {
        initial.x
    };
    let y = if edges.top {
        initial.bottom() - size.1
    } else {
        initial.y
    };
    (x, y)
}

/// Like [`anchor`], but only while it is still wanted: `None` once the
/// settle time after the drag is over, and for drags of the right or bottom
/// edge alone, which never move the window's origin.
pub fn anchored(
    initial: Rect,
    edges: Edges,
    size: (i32, i32),
    expired: bool,
) -> Option<(i32, i32)> {
    if expired || !(edges.left || edges.top) {
        return None;
    }
    Some(anchor(initial, edges, size))
}

/// Cuts a resize drag short where it would push the title bar above the
/// usable area. `frame` is the frame when the drag started.
pub fn limit_resize(frame: Rect, edges: Edges, delta: (i32, i32), usable: Rect) -> (i32, i32) {
    let (dx, mut dy) = delta;
    if edges.top {
        dy = dy.max(usable.y - frame.y);
    }
    (dx, dy)
}

/// The nearest position for `frame` that keeps its title bar usable: never
/// above the usable area or below its bottom, and with at least `grab`
/// pixels of it left on screen sideways.
pub fn keep_reachable(usable: Rect, frame: Rect, titlebar: i32, grab: i32) -> (i32, i32) {
    let min_x = usable.x - (frame.w - grab).max(0);
    let max_x = (usable.right() - grab).max(min_x);
    let max_y = (usable.bottom() - titlebar).max(usable.y);
    (frame.x.clamp(min_x, max_x), frame.y.clamp(usable.y, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics {
        titlebar: 30,
        border: 1,
        button: 26,
        gap: 4,
        padding: 8,
        grip: 4,
        corner: 16,
    };
    const FRAME: Rect = Rect::new(100, 100, 402, 331);

    fn edges(left: bool, right: bool, top: bool, bottom: bool) -> Edges {
        Edges {
            left,
            right,
            top,
            bottom,
        }
    }

    #[test]
    fn frame_and_client_are_inverse() {
        let client = Rect::new(101, 130, 400, 300);
        assert_eq!(M.frame(client), FRAME);
        assert_eq!(M.client(FRAME), client);
    }

    #[test]
    fn buttons_are_on_the_left_in_order() {
        assert_eq!(M.button_rect(Button::Close), Rect::new(9, 2, 26, 26));
        assert_eq!(M.button_rect(Button::Minimise), Rect::new(39, 2, 26, 26));
        assert_eq!(M.button_rect(Button::Maximise), Rect::new(69, 2, 26, 26));
        assert_eq!(M.buttons_end(), 95);
        assert_eq!(M.min_frame_width(), 104);
    }

    #[test]
    fn hit_buttons() {
        assert_eq!(
            M.hit(FRAME, 110, 110, true),
            Some(Hit::Button(Button::Close))
        );
        assert_eq!(
            M.hit(FRAME, 150, 115, true),
            Some(Hit::Button(Button::Minimise))
        );
        assert_eq!(
            M.hit(FRAME, 180, 127, true),
            Some(Hit::Button(Button::Maximise))
        );
        // The gap between two buttons is plain title bar.
        assert_eq!(M.hit(FRAME, 136, 115, true), Some(Hit::Titlebar));
    }

    #[test]
    fn hit_titlebar_and_client() {
        assert_eq!(M.hit(FRAME, 300, 115, true), Some(Hit::Titlebar));
        assert_eq!(M.hit(FRAME, 300, 129, true), Some(Hit::Titlebar));
        assert_eq!(M.hit(FRAME, 300, 130, true), Some(Hit::Client));
        assert_eq!(M.hit(FRAME, 300, 300, true), Some(Hit::Client));
    }

    #[test]
    fn hit_edges_inside_and_outside_the_border() {
        let left = Some(Hit::Resize(edges(true, false, false, false)));
        assert_eq!(M.hit(FRAME, 100, 250, true), left);
        assert_eq!(M.hit(FRAME, 96, 250, true), left);
        assert_eq!(M.hit(FRAME, 95, 250, true), None);
        assert_eq!(M.hit(FRAME, 101, 250, true), Some(Hit::Client));

        let right = Some(Hit::Resize(edges(false, true, false, false)));
        assert_eq!(M.hit(FRAME, 501, 250, true), right);
        assert_eq!(M.hit(FRAME, 505, 250, true), right);
        assert_eq!(M.hit(FRAME, 506, 250, true), None);

        let top = Some(Hit::Resize(edges(false, false, true, false)));
        assert_eq!(M.hit(FRAME, 300, 100, true), top);
        assert_eq!(M.hit(FRAME, 300, 97, true), top);

        let bottom = Some(Hit::Resize(edges(false, false, false, true)));
        assert_eq!(M.hit(FRAME, 300, 430, true), bottom);
        assert_eq!(M.hit(FRAME, 300, 434, true), bottom);
        assert_eq!(M.hit(FRAME, 300, 435, true), None);
    }

    #[test]
    fn hit_corners() {
        let top_left = Some(Hit::Resize(edges(true, false, true, false)));
        assert_eq!(M.hit(FRAME, 98, 98, true), top_left);
        assert_eq!(M.hit(FRAME, 100, 112, true), top_left);
        assert_eq!(M.hit(FRAME, 104, 100, true), top_left);

        let bottom_right = Some(Hit::Resize(edges(false, true, false, true)));
        assert_eq!(M.hit(FRAME, 503, 432, true), bottom_right);
        assert_eq!(M.hit(FRAME, 490, 430, true), bottom_right);
        assert_eq!(M.hit(FRAME, 501, 420, true), bottom_right);

        let top_right = Some(Hit::Resize(edges(false, true, true, false)));
        assert_eq!(M.hit(FRAME, 501, 100, true), top_right);
        let bottom_left = Some(Hit::Resize(edges(true, false, false, true)));
        assert_eq!(M.hit(FRAME, 100, 430, true), bottom_left);
    }

    #[test]
    fn maximised_windows_have_no_grips() {
        assert_eq!(M.hit(FRAME, 100, 250, false), Some(Hit::Client));
        assert_eq!(M.hit(FRAME, 96, 250, false), None);
        assert_eq!(M.hit(FRAME, 300, 100, false), Some(Hit::Titlebar));
    }

    #[test]
    fn resize_moves_only_the_dragged_edges() {
        let start = Rect::new(100, 100, 400, 300);
        let none = (0, 0);
        assert_eq!(
            resize(
                start,
                edges(false, true, false, true),
                (50, -20),
                none,
                none
            ),
            Rect::new(100, 100, 450, 280)
        );
        assert_eq!(
            resize(
                start,
                edges(true, false, true, false),
                (50, -20),
                none,
                none
            ),
            Rect::new(150, 80, 350, 320)
        );
        assert_eq!(
            resize(
                start,
                edges(false, false, false, true),
                (50, 20),
                none,
                none
            ),
            Rect::new(100, 100, 400, 320)
        );
    }

    #[test]
    fn resize_respects_size_limits() {
        let start = Rect::new(100, 100, 400, 300);
        let shrunk = resize(
            start,
            edges(true, false, true, false),
            (1000, 1000),
            (120, 80),
            (0, 0),
        );
        // The right and bottom edges have not moved.
        assert_eq!(shrunk, Rect::new(380, 320, 120, 80));

        let grown = resize(
            start,
            edges(false, true, false, true),
            (1000, 1000),
            (0, 0),
            (600, 0),
        );
        assert_eq!(grown, Rect::new(100, 100, 600, 1300));

        let collapsed = resize(
            start,
            edges(false, true, false, true),
            (-1000, -1000),
            (0, 0),
            (0, 0),
        );
        assert_eq!((collapsed.w, collapsed.h), (1, 1));
    }

    #[test]
    fn anchor_keeps_the_opposite_edges() {
        let start = Rect::new(100, 100, 400, 300);
        // The client settled on a size a little off what was asked for.
        assert_eq!(
            anchor(start, edges(true, false, true, false), (344, 310)),
            (156, 90)
        );
        assert_eq!(
            anchor(start, edges(false, true, false, true), (344, 310)),
            (100, 100)
        );
    }

    #[test]
    fn anchor_applies_only_to_live_left_or_top_drags() {
        let start = Rect::new(100, 100, 400, 300);
        let size = (344, 310);
        assert_eq!(
            anchored(start, edges(true, false, false, false), size, false),
            Some((156, 100))
        );
        assert_eq!(
            anchored(start, edges(true, false, true, false), size, true),
            None
        );
        assert_eq!(
            anchored(start, edges(false, true, false, true), size, false),
            None
        );
    }

    #[test]
    fn top_edge_cannot_be_dragged_above_the_usable_area() {
        let usable = Rect::new(0, 28, 1280, 692);
        let frame = Rect::new(300, 100, 400, 300);
        let top = edges(false, false, true, false);
        assert_eq!(limit_resize(frame, top, (5, -50), usable), (5, -50));
        assert_eq!(limit_resize(frame, top, (5, -500), usable), (5, -72));
        // Other edges are not limited.
        let bottom = edges(false, false, false, true);
        assert_eq!(limit_resize(frame, bottom, (5, -500), usable), (5, -500));
    }

    #[test]
    fn tiny_frames_pick_the_nearer_edge() {
        let tiny = Rect::new(100, 100, 10, 10);
        assert_eq!(
            M.hit(tiny, 100, 100, true),
            Some(Hit::Resize(edges(true, false, true, false)))
        );
        assert_eq!(
            M.hit(tiny, 108, 109, true),
            Some(Hit::Resize(edges(false, true, false, true)))
        );
    }

    #[test]
    fn degenerate_sizes_do_not_panic_or_go_negative() {
        assert_eq!(M.client(Rect::new(0, 0, 0, 0)), Rect::new(1, 30, 1, 1));
        // A maximum below the minimum loses to the minimum.
        let start = Rect::new(0, 0, 400, 300);
        let grown = resize(
            start,
            edges(false, true, false, false),
            (50, 0),
            (500, 0),
            (200, 0),
        );
        assert_eq!(grown.w, 500);
        // No usable area at all still gives a position.
        let nowhere = Rect::new(0, 0, 0, 0);
        assert_eq!(
            keep_reachable(nowhere, Rect::new(50, 50, 400, 300), 30, 100),
            (-100, 0)
        );
        let huge = Rect::new(0, 0, i32::MAX / 4, i32::MAX / 4);
        assert_eq!(
            keep_reachable(Rect::new(0, 28, 1280, 692), huge, 30, 100).1,
            28
        );
    }

    #[test]
    fn windows_never_hide_under_the_bar() {
        let usable = Rect::new(0, 28, 1280, 692);
        assert_eq!(
            keep_reachable(usable, Rect::new(300, 10, 400, 300), 30, 100),
            (300, 28)
        );
        assert_eq!(
            keep_reachable(usable, Rect::new(300, 200, 400, 300), 30, 100),
            (300, 200)
        );
    }

    #[test]
    fn windows_keep_a_piece_of_title_bar_on_screen() {
        let usable = Rect::new(0, 28, 1280, 692);
        let frame = |x, y| Rect::new(x, y, 400, 300);
        assert_eq!(
            keep_reachable(usable, frame(-390, 100), 30, 100),
            (-300, 100)
        );
        assert_eq!(
            keep_reachable(usable, frame(1270, 100), 30, 100),
            (1180, 100)
        );
        assert_eq!(
            keep_reachable(usable, frame(100, 5000), 30, 100),
            (100, 690)
        );
        // Hanging off the bottom or the sides is fine.
        assert_eq!(
            keep_reachable(usable, frame(-200, 600), 30, 100),
            (-200, 600)
        );
    }
}
