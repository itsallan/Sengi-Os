//! Window placement and focus order.
//!
//! Nothing in here knows about Wayland, so it can be tested on its own.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// One past the last column.
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    /// One past the last row.
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub const fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// How many windows are cascaded before the pattern starts over.
const CASCADE_STEPS: usize = 8;

/// Where the top-left corner of a new window goes.
///
/// The first window is centred in `usable` (the output minus bar and dock).
/// Each following one moves down and right by `step`, so earlier title bars
/// stay visible. A window never starts above or left of the usable area, and
/// is pulled back inside it when it would stick out at the bottom or right.
pub fn place(usable: Rect, size: (i32, i32), nth: usize, step: i32) -> (i32, i32) {
    let (w, h) = size;
    let offset = (nth % CASCADE_STEPS) as i32 * step;
    let x = usable.x + (usable.w - w) / 2 + offset;
    let y = usable.y + (usable.h - h) / 2 + offset;
    let max_x = usable.x + (usable.w - w).max(0);
    let max_y = usable.y + (usable.h - h).max(0);
    (x.clamp(usable.x, max_x), y.clamp(usable.y, max_y))
}

/// Windows in the order they were last focused, most recent last.
#[derive(Debug)]
pub struct FocusStack<T> {
    items: Vec<T>,
}

impl<T> Default for FocusStack<T> {
    fn default() -> Self {
        Self { items: Vec::new() }
    }
}

impl<T: PartialEq> FocusStack<T> {
    /// Moves `item` to the front, adding it if it is new.
    pub fn focus(&mut self, item: T) {
        self.items.retain(|other| *other != item);
        self.items.push(item);
    }

    /// Drops `item`. Returns true when it was the focused one, in which case
    /// `current` now names the window that should take over.
    pub fn remove(&mut self, item: &T) -> bool {
        let was_current = self.current() == Some(item);
        self.items.retain(|other| other != item);
        was_current
    }

    pub fn current(&self) -> Option<&T> {
        self.items.last()
    }

    /// Every window, the focused one first, then by how recently each was
    /// used. This is the order Alt+Tab steps through.
    pub fn recent_first(&self) -> Vec<T>
    where
        T: Clone,
    {
        self.items.iter().rev().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USABLE: Rect = Rect::new(0, 28, 1280, 692);

    #[test]
    fn first_window_is_centred_in_the_usable_area() {
        assert_eq!(place(USABLE, (640, 400), 0, 30), (320, 28 + 146));
    }

    #[test]
    fn later_windows_cascade() {
        let first = place(USABLE, (640, 400), 0, 30);
        let second = place(USABLE, (640, 400), 1, 30);
        assert_eq!(second, (first.0 + 30, first.1 + 30));
    }

    #[test]
    fn cascade_starts_over() {
        assert_eq!(
            place(USABLE, (640, 400), CASCADE_STEPS, 30),
            place(USABLE, (640, 400), 0, 30)
        );
    }

    #[test]
    fn cascade_never_leaves_the_usable_area() {
        for nth in 0..CASCADE_STEPS * 2 {
            let (x, y) = place(USABLE, (1000, 600), nth, 30);
            assert!(x >= USABLE.x && x + 1000 <= USABLE.x + USABLE.w, "x={x}");
            assert!(y >= USABLE.y && y + 600 <= USABLE.y + USABLE.h, "y={y}");
        }
    }

    #[test]
    fn oversized_window_is_pinned_below_the_bar() {
        assert_eq!(place(USABLE, (2000, 1000), 3, 30), (0, 28));
    }

    #[test]
    fn focusing_moves_a_window_to_the_front() {
        let mut stack = FocusStack::default();
        stack.focus('a');
        stack.focus('b');
        stack.focus('c');
        stack.focus('a');
        assert_eq!(stack.current(), Some(&'a'));
        assert!(stack.remove(&'a'));
        assert_eq!(stack.current(), Some(&'c'));
    }

    #[test]
    fn recent_first_lists_the_focused_window_first() {
        let mut stack = FocusStack::default();
        stack.focus('a');
        stack.focus('b');
        stack.focus('c');
        stack.focus('b');
        assert_eq!(stack.recent_first(), vec!['b', 'c', 'a']);
    }

    #[test]
    fn closing_the_focused_window_hands_focus_back() {
        let mut stack = FocusStack::default();
        stack.focus('a');
        stack.focus('b');
        assert!(stack.remove(&'b'));
        assert_eq!(stack.current(), Some(&'a'));
    }

    #[test]
    fn closing_a_background_window_keeps_focus() {
        let mut stack = FocusStack::default();
        stack.focus('a');
        stack.focus('b');
        assert!(!stack.remove(&'a'));
        assert_eq!(stack.current(), Some(&'b'));
        assert!(stack.remove(&'b'));
        assert_eq!(stack.current(), None);
    }
}
