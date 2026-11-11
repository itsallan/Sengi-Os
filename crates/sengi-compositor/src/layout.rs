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

/// Where the next output goes: to the right of the ones already there.
pub fn next_output_x(outputs: &[Rect]) -> i32 {
    outputs.iter().map(Rect::right).max().unwrap_or(0)
}

/// The point nearest to `(x, y)` that lies on one of `outputs`, so the
/// pointer can cross from one screen to the next but not leave them.
pub fn clamp_to_outputs(outputs: &[Rect], x: f64, y: f64) -> (f64, f64) {
    let mut best: Option<((f64, f64), f64)> = None;
    for output in outputs.iter().filter(|output| output.w > 0 && output.h > 0) {
        let nearest_x = x.clamp(f64::from(output.x), f64::from(output.right() - 1));
        let nearest_y = y.clamp(f64::from(output.y), f64::from(output.bottom() - 1));
        let distance = (nearest_x - x).powi(2) + (nearest_y - y).powi(2);
        if best.is_none_or(|(_, least)| distance < least) {
            best = Some(((nearest_x, nearest_y), distance));
        }
    }
    best.map_or((x, y), |(point, _)| point)
}

/// Where a window frame goes when its screen is gone: `None` while the
/// middle of its title bar is still on one of `outputs`, otherwise a place
/// inside `usable`, as close to where it was as fits.
pub fn rehome(frame: Rect, outputs: &[Rect], usable: Rect) -> Option<(i32, i32)> {
    let handle = (frame.x + frame.w / 2, frame.y);
    if outputs
        .iter()
        .any(|output| output.contains(handle.0, handle.1))
    {
        return None;
    }
    let max_x = usable.x + (usable.w - frame.w).max(0);
    let max_y = usable.y + (usable.h - frame.h).max(0);
    Some((
        frame.x.clamp(usable.x, max_x),
        frame.y.clamp(usable.y, max_y),
    ))
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

    #[test]
    fn outputs_line_up_left_to_right() {
        assert_eq!(next_output_x(&[]), 0);
        let first = Rect::new(0, 0, 1920, 1080);
        assert_eq!(next_output_x(&[first]), 1920);
        // A gap left by an unplugged screen is not filled.
        let third = Rect::new(3200, 0, 1280, 800);
        assert_eq!(next_output_x(&[first, third]), 4480);
    }

    #[test]
    fn the_pointer_crosses_between_outputs_but_stays_on_them() {
        let outputs = [Rect::new(0, 0, 1920, 1080), Rect::new(1920, 0, 1280, 800)];
        assert_eq!(clamp_to_outputs(&outputs, 2000.0, 400.0), (2000.0, 400.0));
        assert_eq!(clamp_to_outputs(&outputs, -5.0, 400.0), (0.0, 400.0));
        assert_eq!(clamp_to_outputs(&outputs, 4000.0, 400.0), (3199.0, 400.0));
        // Below the smaller screen: back onto it, not across to the other.
        assert_eq!(clamp_to_outputs(&outputs, 2500.0, 900.0), (2500.0, 799.0));
        // Just right of the tall screen's lower part: stay on the tall one.
        assert_eq!(clamp_to_outputs(&outputs, 1925.0, 1000.0), (1919.0, 1000.0));
        assert_eq!(clamp_to_outputs(&[], 7.0, 8.0), (7.0, 8.0));
    }

    #[test]
    fn a_window_whose_screen_went_away_comes_back() {
        let left = Rect::new(0, 0, 1920, 1080);
        let usable = Rect::new(0, 28, 1920, 1000);
        // Still reachable where it is.
        assert_eq!(rehome(Rect::new(100, 100, 700, 500), &[left], usable), None);
        // Was on a screen to the right that is gone.
        assert_eq!(
            rehome(Rect::new(2100, 100, 700, 500), &[left], usable),
            Some((1220, 100))
        );
        // Larger than what is left: top-left corner stays visible.
        assert_eq!(
            rehome(Rect::new(2100, 0, 2400, 1400), &[left], usable),
            Some((0, 28))
        );
    }
}
