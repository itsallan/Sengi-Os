//! sengi-snip: the two small screens sengi-shot asks for. One dims the
//! screen so a rectangle can be dragged on it; the other counts down to a
//! delayed shot. They are separate from sengi-shot so that taking a plain
//! screenshot links no toolkit. Neither stays running.

pub mod cli;
pub mod select;

pub mod ui {
    slint::include_modules!();
}

use slint::ComponentHandle;

use crate::select::Rect;
use crate::ui::{CountdownWindow, SnipWindow};

/// How often the countdown looks at the time.
const TICK_MILLIS: u64 = 100;

/// Makes the selection window do its job. `chosen` is called with the
/// rectangle when one is made, `None` when the choice is cancelled.
pub fn attach_select(window: &SnipWindow, chosen: impl Fn(Option<Rect>) + 'static) {
    let chosen = std::rc::Rc::new(chosen);
    window.on_size_text(|width, height| select::size_label(width, height).into());
    window.on_committed({
        let (chosen, weak) = (chosen.clone(), window.as_weak());
        move |ax, ay, bx, by, width, height| {
            let bounds = (width.max(0.0) as u32, height.max(0.0) as u32);
            match select::from_drag((ax, ay), (bx, by), bounds) {
                Some(rect) => chosen(Some(rect)),
                // Too small to be meant: back to the start, still choosing.
                None => {
                    if let Some(window) = weak.upgrade() {
                        window.set_dragging(false);
                    }
                }
            }
        }
    });
    window.on_cancelled(move || chosen(None));
}

/// Counts down in the window, and calls `done(true)` at zero or
/// `done(false)` if it is clicked. The timer lives as long as the value
/// returned.
pub fn attach_countdown(
    window: &CountdownWindow,
    seconds: u32,
    done: impl Fn(bool) + 'static,
) -> slint::Timer {
    let done = std::rc::Rc::new(done);
    window.set_seconds(seconds as i32);
    window.on_clicked({
        let done = done.clone();
        move || done(false)
    });
    let timer = slint::Timer::default();
    // Counted in ticks of the toolkit's own clock, which is the one the
    // timer runs on.
    let ticks = std::cell::Cell::new(0u64);
    let weak = window.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        std::time::Duration::from_millis(TICK_MILLIS),
        move || {
            ticks.set(ticks.get() + 1);
            let left = select::seconds_left(seconds, ticks.get() * TICK_MILLIS);
            if left == 0 {
                done(true);
            } else if let Some(window) = weak.upgrade() {
                window.set_seconds(left as i32);
            }
        },
    );
    timer
}
