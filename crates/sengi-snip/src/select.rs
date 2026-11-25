//! The arithmetic of picking part of a screen: from the two ends of a drag
//! to the rectangle that is kept, how it is written for the program that
//! asked, and how long a countdown has left.

/// A part of an output in logical pixels from its top left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A rectangle with a side shorter than this is a click that slipped, not a
/// choice.
pub const MIN_SIDE: i32 = 4;

/// The rectangle between two corners, in whichever order they were given,
/// kept inside an output of `bounds` size. `None` if it is too small.
pub fn from_drag(a: (f32, f32), b: (f32, f32), bounds: (u32, u32)) -> Option<Rect> {
    let (width, height) = (bounds.0 as f32, bounds.1 as f32);
    let clamp = |value: f32, high: f32| {
        if value.is_nan() {
            0.0
        } else {
            value.clamp(0.0, high)
        }
    };
    let (ax, bx) = (clamp(a.0, width), clamp(b.0, width));
    let (ay, by) = (clamp(a.1, height), clamp(b.1, height));
    let left = ax.min(bx).round() as i32;
    let top = ay.min(by).round() as i32;
    let right = ax.max(bx).round() as i32;
    let bottom = ay.max(by).round() as i32;
    let rect = Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    };
    (rect.width >= MIN_SIDE && rect.height >= MIN_SIDE).then_some(rect)
}

/// What the size label says: `640 × 480`.
pub fn size_label(width: i32, height: i32) -> String {
    format!("{width} \u{d7} {height}")
}

/// How a choice is handed over, on one line: the output's name (or `-`),
/// a space, and `X,Y,WIDTH,HEIGHT`. sengi-shot reads it back.
pub fn format_choice(output: Option<&str>, rect: Rect) -> String {
    let name = output.filter(|name| !name.is_empty() && !name.contains(char::is_whitespace));
    format!(
        "{} {},{},{},{}",
        name.unwrap_or("-"),
        rect.x,
        rect.y,
        rect.width,
        rect.height
    )
}

/// Whole seconds still to wait, rounded up, so a countdown from 5 says 5 for
/// the first second and 1 for the last.
pub fn seconds_left(total_seconds: u32, elapsed_millis: u64) -> u32 {
    let total = u64::from(total_seconds) * 1000;
    total.saturating_sub(elapsed_millis).div_ceil(1000) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (u32, u32) = (1366, 768);

    #[test]
    fn a_drag_in_any_direction_is_the_same_rectangle() {
        let want = Rect {
            x: 100,
            y: 50,
            width: 300,
            height: 200,
        };
        assert_eq!(from_drag((100.0, 50.0), (400.0, 250.0), SCREEN), Some(want));
        assert_eq!(from_drag((400.0, 250.0), (100.0, 50.0), SCREEN), Some(want));
        assert_eq!(from_drag((400.0, 50.0), (100.0, 250.0), SCREEN), Some(want));
        assert_eq!(from_drag((100.0, 250.0), (400.0, 50.0), SCREEN), Some(want));
    }

    #[test]
    fn a_drag_past_the_edge_stops_at_the_edge() {
        assert_eq!(
            from_drag((-50.0, -10.0), (2000.0, 900.0), SCREEN),
            Some(Rect {
                x: 0,
                y: 0,
                width: 1366,
                height: 768
            })
        );
        assert_eq!(
            from_drag((1300.0, 700.0), (1500.0, 800.0), SCREEN),
            Some(Rect {
                x: 1300,
                y: 700,
                width: 66,
                height: 68
            })
        );
    }

    #[test]
    fn fractions_round_to_whole_pixels() {
        assert_eq!(
            from_drag((10.4, 10.6), (110.5, 60.4), SCREEN),
            Some(Rect {
                x: 10,
                y: 11,
                width: 101,
                height: 49
            })
        );
    }

    #[test]
    fn a_click_or_a_sliver_is_not_a_choice() {
        assert_eq!(from_drag((50.0, 50.0), (50.0, 50.0), SCREEN), None);
        assert_eq!(from_drag((50.0, 50.0), (53.0, 300.0), SCREEN), None);
        assert_eq!(from_drag((50.0, 50.0), (300.0, 53.0), SCREEN), None);
        assert!(from_drag((50.0, 50.0), (54.0, 54.0), SCREEN).is_some());
        assert_eq!(from_drag((f32::NAN, 0.0), (3.0, 100.0), SCREEN), None);
        // A drag that started and ended outside, on the same side.
        assert_eq!(from_drag((-30.0, 10.0), (-5.0, 100.0), SCREEN), None);
    }

    #[test]
    fn the_size_label() {
        assert_eq!(size_label(640, 480), "640 \u{d7} 480");
    }

    #[test]
    fn the_choice_on_a_line() {
        let rect = Rect {
            x: 10,
            y: 20,
            width: 640,
            height: 480,
        };
        assert_eq!(format_choice(Some("eDP-1"), rect), "eDP-1 10,20,640,480");
        assert_eq!(format_choice(None, rect), "- 10,20,640,480");
        assert_eq!(format_choice(Some("a b"), rect), "- 10,20,640,480");
        assert_eq!(format_choice(Some(""), rect), "- 10,20,640,480");
    }

    #[test]
    fn the_countdown_rounds_up() {
        assert_eq!(seconds_left(5, 0), 5);
        assert_eq!(seconds_left(5, 1), 5);
        assert_eq!(seconds_left(5, 999), 5);
        assert_eq!(seconds_left(5, 1000), 4);
        assert_eq!(seconds_left(5, 4001), 1);
        assert_eq!(seconds_left(5, 4999), 1);
        assert_eq!(seconds_left(5, 5000), 0);
        assert_eq!(seconds_left(5, 99_999), 0);
        assert_eq!(seconds_left(0, 0), 0);
    }
}
