//! ext-session-lock: the lock screen.
//!
//! While `State::lock` is set nothing but the lock surface is drawn and
//! nothing else receives input. That is decided in three places, all of
//! which ask `is_locked`: `render::output`, the pointer hit test in
//! `input.rs`, and `State::update_keyboard_focus`. If the locker dies
//! without unlocking, the session stays locked and blank, as the protocol
//! demands. A new locker can then take the lock over, and only it can
//! unlock; while the first one lives, a second is turned away.

use smithay::{
    delegate_session_lock,
    reexports::{
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
        // One locker at a time. Dropping the confirmation tells the
        // newcomer that it did not get the lock.
        if self
            .lock
            .as_ref()
            .is_some_and(|lock| lock.locker.is_alive())
        {
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
