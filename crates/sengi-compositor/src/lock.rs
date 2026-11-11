//! ext-session-lock: the lock screen.
//!
//! While `State::lock` is set nothing but the lock surface is drawn and
//! nothing else receives input. That is decided in three places, all of
//! which ask `is_locked`: `render::output`, the pointer hit test in
//! `input.rs`, and `State::update_keyboard_focus`. If the locker dies
//! without unlocking, the session stays locked and blank, as the protocol
//! demands, and the compositor starts its own locker for it. Only a locker
//! the compositor started may take over a dead one's session; while a
//! locker lives, a second is turned away.

use std::time::Duration;

use smithay::{
    delegate_session_lock,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1,
        wayland_server::{
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
            Resource,
        },
    },
    utils::SERIAL_COUNTER,
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};

use crate::state::State;

pub struct Lock {
    pub surface: Option<LockSurface>,
    /// The lock object of the client holding the lock. When that client
    /// dies this goes dead, and another locker may take over.
    locker: ExtSessionLockV1,
}

/// Who holds the session lock when a client asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    Nobody,
    /// A locker that is still connected.
    Alive,
    /// The session is locked but its locker is gone.
    Dead,
}

/// Whether a lock request is granted. `ours` says the client is a locker
/// this compositor started itself.
///
/// Anyone may lock an unlocked session; that is what swayidle's locker
/// does. Nobody may lock over a live locker. A dead locker's session can
/// only be taken over by our own locker: otherwise any client could step
/// in and unlock it.
pub fn may_lock(holder: Holder, ours: bool) -> bool {
    match holder {
        Holder::Nobody => true,
        Holder::Alive => false,
        Holder::Dead => ours,
    }
}

/// How long to wait before starting a locker for a session whose locker
/// died, and between attempts if that one fails too.
const RELOCK_DELAY: Duration = Duration::from_secs(1);

impl State {
    fn lock_holder(&self) -> Holder {
        match &self.lock {
            None => Holder::Nobody,
            Some(lock) if lock.locker.is_alive() => Holder::Alive,
            Some(_) => Holder::Dead,
        }
    }

    /// Starts the configured locker and remembers it as ours.
    pub fn spawn_locker(&mut self) {
        let command = self.locker_command.clone();
        if let Some(pid) = self.spawn(&command) {
            self.locker_pids.push(pid);
        }
    }

    /// A locked session whose locker died must not stay a blank screen
    /// with no way in: start our locker for it, and keep trying.
    pub fn watch_locker(&mut self) {
        let running = self.child_pids();
        self.locker_pids.retain(|pid| running.contains(pid));
        let orphaned = self.lock_holder() == Holder::Dead && self.locker_pids.is_empty();
        if !orphaned || self.relock_armed {
            return;
        }
        let timer = Timer::from_duration(RELOCK_DELAY);
        let armed = self.loop_handle.insert_source(timer, |_, _, state| {
            state.relock_armed = false;
            if state.lock_holder() == Holder::Dead && state.locker_pids.is_empty() {
                state.spawn_locker();
            }
            TimeoutAction::Drop
        });
        self.relock_armed = armed.is_ok();
    }

    pub fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// The lock screen's surface, once the locker has made one.
    pub fn lock_surface(&self) -> Option<WlSurface> {
        let surface = self.lock.as_ref()?.surface.as_ref()?;
        surface.alive().then(|| surface.wl_surface().clone())
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Dropping the confirmation tells a refused client that it did not
        // get the lock.
        let pid = confirmation
            .ext_session_lock()
            .client()
            .and_then(|client| client.get_credentials(&self.display_handle).ok())
            .map(|credentials| credentials.pid as u32);
        let ours = pid.is_some_and(|pid| self.locker_pids.contains(&pid));
        if !may_lock(self.lock_holder(), ours) {
            return;
        }
        // Whatever the pointer or keyboard was in the middle of ends here.
        self.drag = None;
        self.hovered_button = None;
        self.frame_cursor = None;
        self.cycle = None;
        self.layer_focus = None;
        let time = self.start_time.elapsed().as_millis() as u32;
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), time);
        keyboard.unset_grab(self);

        self.lock = Some(Lock {
            surface: None,
            locker: confirmation.ext_session_lock().clone(),
        });
        self.update_keyboard_focus();
        self.sync_pointer_focus();
        // From here on every frame shows the lock or nothing. The client is
        // told so once the first such frame is out, see `frame_presented`.
        self.pending_lock = Some(confirmation);
        self.request_redraw();
    }

    fn unlock(&mut self) {
        self.lock = None;
        self.pending_lock = None;
        self.request_redraw();
        self.update_keyboard_focus();
        self.sync_pointer_focus();
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        let size = self
            .output()
            .and_then(|output| self.space.output_geometry(&output))
            .map(|geometry| geometry.size)
            .unwrap_or_default();
        surface.with_pending_state(|state| {
            state.size = Some((size.w.max(0) as u32, size.h.max(0) as u32).into());
        });
        surface.send_configure();

        if let Some(lock) = &mut self.lock {
            lock.surface = Some(surface);
        }
        self.request_redraw();
        self.update_keyboard_focus();
        self.sync_pointer_focus();
    }
}

delegate_session_lock!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlocked_session_can_be_locked_by_anyone() {
        assert!(may_lock(Holder::Nobody, false));
        assert!(may_lock(Holder::Nobody, true));
    }

    #[test]
    fn a_live_locker_is_never_replaced() {
        assert!(!may_lock(Holder::Alive, false));
        assert!(!may_lock(Holder::Alive, true));
    }

    #[test]
    fn only_our_own_locker_takes_over_a_dead_one() {
        assert!(!may_lock(Holder::Dead, false));
        assert!(may_lock(Holder::Dead, true));
    }
}
