//! ext-session-lock: the lock screen.
//!
//! While `State::lock` is set nothing but the lock surface is drawn and
//! nothing else receives input. That is decided in three places, all of
//! which ask `is_locked`: `render::output`, the pointer hit test in
//! `input.rs`, and `State::update_keyboard_focus`. If the locker dies
//! without unlocking, the session stays locked and blank, as the protocol
//! demands; a new locker can take over.

use smithay::{
    delegate_session_lock,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    utils::SERIAL_COUNTER,
    wayland::session_lock::{
        LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
    },
};

use crate::state::State;

#[derive(Default)]
pub struct Lock {
    pub surface: Option<LockSurface>,
}

impl State {
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

        self.lock = Some(Lock::default());
        self.update_keyboard_focus();
        self.sync_pointer_focus();
        // From here on every frame shows the lock or nothing.
        confirmation.lock();
    }

    fn unlock(&mut self) {
        self.lock = None;
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
        self.update_keyboard_focus();
        self.sync_pointer_focus();
    }
}

delegate_session_lock!(State);
