//! idle-inhibit: a client, a video player say, asks that the session not
//! be taken for idle while one of its surfaces is on screen. swayidle then
//! does not switch the screen off or lock.

use smithay::{
    delegate_idle_inhibit,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    wayland::idle_inhibit::IdleInhibitHandler,
};

use crate::state::{has_buffer, State};

impl IdleInhibitHandler for State {
    fn inhibit(&mut self, surface: WlSurface) {
        if !self.idle_inhibitors.contains(&surface) {
            self.idle_inhibitors.push(surface);
        }
        self.refresh_idle_inhibit();
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.retain(|held| held != &surface);
        self.refresh_idle_inhibit();
    }
}

impl State {
    /// Works out afresh whether anything holds idleness off. A surface
    /// that is gone or shows nothing holds nothing off, and nothing does
    /// while the session is locked: what asked is behind the lock screen.
    pub fn refresh_idle_inhibit(&mut self) {
        self.idle_inhibitors.retain(Resource::is_alive);
        let inhibited = !self.is_locked() && self.idle_inhibitors.iter().any(has_buffer);
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }
}

delegate_idle_inhibit!(State);
