//! Three-finger swipes on a touchpad.
//!
//! Left and right walk through the windows the way Alt+Tab does: a swipe
//! to the left goes to the window used before this one, a longer one to
//! the one before that, and going back with the fingers undoes it. The
//! window is chosen when the fingers lift, as it is when Alt is let go.
//! A swipe up opens the switcher and leaves it open (there is no overview
//! of the windows here, so this is the nearest thing); left and right then
//! move its selection, a swipe down picks what it has selected, and Escape
//! closes it. Pinch gestures and swipes with other numbers of fingers are
//! not looked at.

use smithay::backend::input::{
    GestureBeginEvent, GestureEndEvent, GestureSwipeUpdateEvent, InputBackend, InputEvent,
};

use crate::{overlay::Overlay, state::State};

/// How far the fingers travel, in logical pixels of touchpad movement, for
/// one step through the windows.
pub const STEP: f64 = 70.0;
/// How far before a swipe counts as going sideways or up, and which.
pub const LOCK: f64 = 24.0;
/// How far up or down for the switcher to open or pick.
pub const VERTICAL: f64 = 60.0;
/// The number of fingers that make a gesture of ours.
pub const FINGERS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    Sideways,
    Vertical,
}

/// What a swipe asks for, as it grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// One more window along, or one back.
    Step { forward: bool },
    /// Open the switcher and keep it open.
    Show,
    /// Pick what the switcher has selected.
    Pick,
}

/// One swipe, from the fingers coming down to their lifting.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Swipe {
    dx: f64,
    dy: f64,
    axis: Option<Axis>,
    /// How many steps, net, have been asked for: forward is positive.
    steps: i32,
    vertical_done: bool,
}

impl Swipe {
    /// The touchpad moved the fingers by `dx`, `dy` more. What that asks for.
    pub fn update(&mut self, dx: f64, dy: f64) -> Vec<Act> {
        self.dx += dx;
        self.dy += dy;
        if self.axis.is_none() {
            let (ax, ay) = (self.dx.abs(), self.dy.abs());
            if ax.max(ay) < LOCK {
                return Vec::new();
            }
            self.axis = Some(if ax >= ay {
                Axis::Sideways
            } else {
                Axis::Vertical
            });
        }
        let mut acts = Vec::new();
        match self.axis {
            Some(Axis::Sideways) => {
                // To the left is forward: the windows follow the fingers.
                let wanted = (-self.dx / STEP).trunc() as i32;
                while self.steps != wanted {
                    let forward = wanted > self.steps;
                    self.steps += if forward { 1 } else { -1 };
                    acts.push(Act::Step { forward });
                }
            }
            Some(Axis::Vertical) if !self.vertical_done && self.dy.abs() >= VERTICAL => {
                self.vertical_done = true;
                acts.push(if self.dy < 0.0 { Act::Show } else { Act::Pick });
            }
            _ => {}
        }
        acts
    }

    /// Whether the swipe went sideways, so that lifting the fingers picks.
    fn sideways(&self) -> bool {
        self.axis == Some(Axis::Sideways)
    }
}

/// A swipe in progress, and whether its steps opened the switcher.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Active {
    swipe: Swipe,
    opened: bool,
}

impl State {
    pub fn process_gesture_event<I: InputBackend>(&mut self, event: &InputEvent<I>) {
        match event {
            InputEvent::GestureSwipeBegin { event } => self.swipe_begin(event.fingers()),
            InputEvent::GestureSwipeUpdate { event } => {
                let delta = event.delta();
                self.swipe_update(delta.x, delta.y);
            }
            InputEvent::GestureSwipeEnd { event } => self.swipe_end(event.cancelled()),
            // Pinch and hold are ignored.
            _ => {}
        }
    }

    pub fn swipe_begin(&mut self, fingers: u32) {
        self.swipe = None;
        if fingers != FINGERS || !self.config.touchpad.gestures {
            return;
        }
        // The touch that lights a dark screen is not a gesture.
        if self.note_activity() || self.is_locked() || self.login_screen {
            return;
        }
        self.swipe = Some(Active {
            swipe: Swipe::default(),
            opened: false,
        });
    }

    pub fn swipe_update(&mut self, dx: f64, dy: f64) {
        let Some(mut active) = self.swipe else { return };
        for act in active.swipe.update(dx, dy) {
            self.do_swipe(&mut active, act);
        }
        // The swipe may have been dropped while it ran, by the lock.
        if self.swipe.is_some() {
            self.swipe = Some(active);
        }
    }

    fn do_swipe(&mut self, active: &mut Active, act: Act) {
        let switcher = matches!(self.overlay, Some(Overlay::Switcher { .. }));
        // A menu or the shortcut sheet is somebody's, not ours to move.
        if self.overlay.is_some() && !switcher {
            return;
        }
        match act {
            Act::Step { forward } if switcher => self.step_switcher(forward),
            Act::Step { forward } => {
                self.open_switcher(forward);
                active.opened = matches!(self.overlay, Some(Overlay::Switcher { .. }));
            }
            Act::Show if !switcher => self.open_switcher(true),
            Act::Show => {}
            Act::Pick if switcher => self.activate_overlay(),
            Act::Pick => {}
        }
    }

    pub fn swipe_end(&mut self, cancelled: bool) {
        let Some(active) = self.swipe.take() else {
            return;
        };
        // Only a switcher this swipe opened by going sideways is settled
        // by the fingers lifting. One opened by a swipe up stays.
        if active.opened && active.swipe.sideways() {
            if cancelled {
                self.close_overlay();
            } else {
                self.activate_overlay();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(moves: &[(f64, f64)]) -> Vec<Act> {
        let mut swipe = Swipe::default();
        moves
            .iter()
            .flat_map(|&(x, y)| swipe.update(x, y))
            .collect()
    }

    #[test]
    fn a_short_movement_does_nothing() {
        assert!(run(&[(5.0, 3.0), (6.0, 2.0)]).is_empty());
    }

    #[test]
    fn a_swipe_left_steps_forward_once_per_distance() {
        let acts = run(&[(-30.0, 0.0), (-45.0, 0.0), (-70.0, 0.0)]);
        assert_eq!(
            acts,
            vec![Act::Step { forward: true }, Act::Step { forward: true }]
        );
    }

    #[test]
    fn a_swipe_right_steps_back_and_returning_undoes_it() {
        let acts = run(&[(80.0, 0.0), (-80.0, 0.0)]);
        assert_eq!(
            acts,
            vec![Act::Step { forward: false }, Act::Step { forward: true }]
        );
    }

    #[test]
    fn up_shows_and_down_picks_once() {
        assert_eq!(
            run(&[(1.0, -40.0), (0.0, -40.0), (0.0, -40.0)]),
            vec![Act::Show]
        );
        assert_eq!(run(&[(0.0, 70.0), (0.0, 70.0)]), vec![Act::Pick]);
    }

    #[test]
    fn once_the_direction_is_set_the_other_axis_is_ignored() {
        let acts = run(&[(-40.0, 5.0), (0.0, -200.0)]);
        assert!(acts.is_empty(), "{acts:?}");
        let acts = run(&[(0.0, -30.0), (-300.0, 0.0)]);
        assert!(acts.is_empty(), "{acts:?}");
    }
}
