//! Protocols that ordinary programs expect of a desktop, each of which
//! Smithay implements and this file only wires up and sets the rules for:
//!
//! - primary selection: what is selected can be pasted with the middle
//!   button, without copying;
//! - xdg-activation: a program that has the keyboard can pass it on to a
//!   window it started or points at, and no other program can take it;
//! - viewporter and fractional-scale, which let a client draw for a
//!   screen whose scale is not a whole number;
//! - pointer constraints and relative pointer, for programs that take the
//!   pointer for themselves: games, remote desktops, 3D viewers;
//! - the picture that follows the pointer during a drag.

use std::time::Duration;

use smithay::{
    delegate_fractional_scale, delegate_pointer_constraints, delegate_primary_selection,
    delegate_relative_pointer, delegate_viewporter, delegate_xdg_activation,
    input::{
        pointer::{PointerHandle, RelativeMotionEvent},
        Seat,
    },
    reexports::wayland_server::{
        protocol::{wl_data_source::WlDataSource, wl_surface::WlSurface},
        Resource,
    },
    utils::{Logical, Point},
    wayland::{
        compositor::with_states,
        fractional_scale::{with_fractional_scale, FractionalScaleHandler},
        pointer_constraints::{
            with_pointer_constraint, PointerConstraint, PointerConstraintsHandler,
        },
        selection::{
            data_device::{ClientDndGrabHandler, ServerDndGrabHandler},
            primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
        },
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};

use crate::state::State;

/// How long a token stays good for bringing a window forward.
const ACTIVATION_TIME: Duration = Duration::from_secs(10);

/// How many unused tokens one program may hold.
const TOKENS_EACH: usize = 8;

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    /// A token is only worth something when the program that asked for
    /// it has the keyboard and proves it with the serial of that focus:
    /// the launcher that is being typed into, a window the user is in.
    /// Anything else gets a token that activates nothing, which is what
    /// keeps a program in the background from putting itself in front.
    /// Tokens that were never used go when their time is up, and no
    /// program holds more than a few at once.
    fn token_created(&mut self, token: XdgActivationToken, data: XdgActivationTokenData) -> bool {
        self.xdg_activation_state
            .retain_tokens(|_, held| held.timestamp.elapsed() <= ACTIVATION_TIME);
        let held = self
            .xdg_activation_state
            .tokens()
            .filter(|(other, held)| **other != token && held.client_id == data.client_id)
            .count();
        if held >= TOKENS_EACH {
            return false;
        }
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let Some((serial, seat)) = data.serial else {
            return false;
        };
        // The surface with the keyboard belongs to whoever asks.
        let asker = keyboard
            .current_focus()
            .and_then(|surface| self.display_handle.get_client(surface.id()).ok())
            .map(|client| client.id());
        let focused = asker.is_some() && asker == data.client_id;
        focused
            && Seat::<Self>::from_resource(&seat).as_ref() == Some(&self.seat)
            && keyboard
                .last_enter()
                .is_some_and(|entered| serial.is_no_older_than(&entered))
    }

    fn request_activation(
        &mut self,
        token: XdgActivationToken,
        token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Good for one window, once: whoever else learns the token later
        // has nothing.
        self.xdg_activation_state.remove_token(&token);
        if token_data.timestamp.elapsed() > ACTIVATION_TIME || self.is_locked() {
            return;
        }
        let Some(window) = self.window_for_surface(&surface) else {
            return;
        };
        if self.minimised.contains(&window) {
            self.set_minimised(&window, false);
        }
        if self.space.element_location(&window).is_some() {
            self.focus_window(&window);
        }
    }
}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self.preferred_scale_of(&surface);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        });
    }
}

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {
        // It takes effect at once where that is allowed, and otherwise
        // when it becomes so.
        self.settle_pointer_constraint();
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
        // Where the program would like the pointer to be once it lets go.
        // It stays where it was locked.
    }
}

impl State {
    /// The pointing device moved by `delta`. Whoever asked is told the
    /// movement as it was; where the pointer goes is then up to what
    /// holds it. `unaccelerated` is the movement before the pointer speed
    /// was applied, `micros` the time of it.
    pub fn pointer_moved(
        &mut self,
        delta: Point<f64, Logical>,
        unaccelerated: Point<f64, Logical>,
        micros: u64,
        time: u32,
    ) {
        if self.note_activity() {
            return;
        }
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let here = pointer.current_location();
        let under = self.surface_under(here);
        pointer.relative_motion(
            self,
            under.clone(),
            &RelativeMotionEvent {
                delta,
                delta_unaccel: unaccelerated,
                utime: micros,
            },
        );

        let moved = here + delta;
        let moved = Point::from(crate::layout::clamp_to_outputs(
            &self.output_rects(),
            moved.x,
            moved.y,
        ));
        // A program that holds the pointer: locked, it does not move at
        // all; confined, it does not leave the surface, or the part of it
        // that was named.
        if let Some((surface, origin)) = under {
            // Looked up first: the constraint is read with the surface's
            // state held, which finding a surface needs too.
            let stays_on_it = self
                .surface_under(moved)
                .is_some_and(|(there, _)| there == surface);
            let mut held = false;
            with_pointer_constraint(&surface, &pointer, |constraint| {
                let Some(constraint) = constraint.filter(|constraint| constraint.is_active())
                else {
                    return;
                };
                held = match &*constraint {
                    PointerConstraint::Locked(_) => true,
                    PointerConstraint::Confined(confined) => {
                        let within = (moved - origin).to_i32_round();
                        let outside_region = confined
                            .region()
                            .is_some_and(|region| !region.contains(within));
                        outside_region || !stays_on_it
                    }
                };
            });
            if held {
                pointer.frame(self);
                return;
            }
        }
        self.pointer_seen = true;
        self.place_pointer(moved, time);
        self.settle_pointer_constraint();
    }

    /// Brings the pointer constraint in line with how things stand. Called
    /// when the pointer has moved, when the keyboard has gone to another
    /// program, when a constraint is asked for and when the session locks.
    ///
    /// A program may hold the pointer only while it is the one in use:
    /// its surface is under the pointer, it has the keyboard, the session
    /// is not locked, and the pointer is inside the region the constraint
    /// names, if it names one. Otherwise any window the pointer passed
    /// over could trap it. A constraint in force ends as soon as the
    /// first three no longer hold.
    pub fn settle_pointer_constraint(&mut self) {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let here = pointer.current_location();
        let allowed = (!self.is_locked())
            .then(|| self.surface_under(here))
            .flatten()
            .filter(|(surface, _)| {
                let owner = self.display_handle.get_client(surface.id()).ok();
                owner.is_some() && owner.map(|client| client.id()) == self.keyboard_client
            });
        let on = allowed.as_ref().map(|(surface, _)| surface);
        if let Some(left) = self.constrained.take_if(|held| Some(&*held) != on) {
            if left.is_alive() {
                with_pointer_constraint(&left, &pointer, |constraint| {
                    if let Some(constraint) = constraint.filter(|held| held.is_active()) {
                        constraint.deactivate();
                    }
                });
            }
        }
        let Some((surface, origin)) = allowed else {
            return;
        };
        let within = (here - origin).to_i32_round();
        with_pointer_constraint(&surface, &pointer, |constraint| {
            let Some(constraint) = constraint else { return };
            if constraint.is_active() {
                self.constrained = Some(surface.clone());
                return;
            }
            let in_region = constraint
                .region()
                .is_none_or(|region| region.contains(within));
            if in_region {
                constraint.activate();
                self.constrained = Some(surface.clone());
            }
        });
    }
}

impl ClientDndGrabHandler for State {
    fn started(
        &mut self,
        _source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.drag_icon = icon;
        self.autohide.carrying = true;
        self.request_redraw();
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.drag_icon = None;
        self.autohide.carrying = false;
        self.request_redraw();
    }
}

impl ServerDndGrabHandler for State {
    fn cancelled(&mut self, _seat: Seat<Self>) {
        self.drag_icon = None;
        self.request_redraw();
    }
}

delegate_primary_selection!(State);
delegate_xdg_activation!(State);
delegate_viewporter!(State);
delegate_fractional_scale!(State);
delegate_pointer_constraints!(State);
delegate_relative_pointer!(State);
