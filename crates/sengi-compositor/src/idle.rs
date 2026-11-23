//! idle-inhibit: a client, a video player say, asks that the session not
//! be taken for idle while one of its surfaces is on screen. swayidle then
//! does not switch the screen off or lock.

use smithay::{
    desktop::{layer_map_for_output, WindowSurfaceType},
    reexports::{
        wayland_protocols::wp::idle_inhibit::zv1::server::{
            zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
            zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
        },
        wayland_server::{
            delegate_dispatch, protocol::wl_surface::WlSurface, Client, DataInit, DisplayHandle,
            GlobalDispatch, New, Resource,
        },
    },
    wayland::{
        compositor::get_parent,
        idle_inhibit::{
            inhibitor::IdleInhibitorState, IdleInhibitHandler, IdleInhibitManagerState,
        },
        seat::WaylandFocus,
    },
};

use crate::{
    security,
    state::{has_buffer, State},
};

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
    /// that is gone, shows nothing, is minimised or is on no screen holds
    /// nothing off, and nothing does while the session is locked: what
    /// asked is behind the lock screen.
    pub fn refresh_idle_inhibit(&mut self) {
        self.idle_inhibitors.retain(Resource::is_alive);
        let inhibited = !self.is_locked()
            && self
                .idle_inhibitors
                .iter()
                .any(|surface| has_buffer(surface) && self.is_on_screen(surface));
        self.idle_notifier_state.set_is_inhibited(inhibited);
    }

    /// Whether the window or layer surface that `surface` belongs to is
    /// mapped on an output. A minimised window is not in the space.
    fn is_on_screen(&self, surface: &WlSurface) -> bool {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let window = self
            .space
            .elements()
            .find(|window| window.wl_surface().is_some_and(|own| *own == root));
        if let Some(window) = window {
            return !self.space.outputs_for_element(window).is_empty();
        }
        self.space.outputs().any(|output| {
            layer_map_for_output(output)
                .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                .is_some()
        })
    }
}

// As `delegate_idle_inhibit!` would, except that sandboxed clients do not
// get the global: a Flatpak app must not be able to keep the screen from
// locking.
impl GlobalDispatch<ZwpIdleInhibitManagerV1, ()> for State {
    fn bind(
        state: &mut Self,
        handle: &DisplayHandle,
        client: &Client,
        resource: New<ZwpIdleInhibitManagerV1>,
        global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        <IdleInhibitManagerState as GlobalDispatch<ZwpIdleInhibitManagerV1, (), Self>>::bind(
            state,
            handle,
            client,
            resource,
            global_data,
            data_init,
        );
    }

    fn can_view(client: Client, _: &()) -> bool {
        security::trusted(&client)
    }
}
delegate_dispatch!(State: [ZwpIdleInhibitManagerV1: ()] => IdleInhibitManagerState);
delegate_dispatch!(State: [ZwpIdleInhibitorV1: IdleInhibitorState] => IdleInhibitManagerState);
