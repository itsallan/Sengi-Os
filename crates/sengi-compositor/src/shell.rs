//! wl_compositor and xdg-shell handling: toplevels and popups.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_xdg_shell,
    desktop::{layer_map_for_output, PopupKind, Window},
    reexports::wayland_server::{
        protocol::{wl_seat::WlSeat, wl_surface::WlSurface},
        Client,
    },
    utils::Serial,
    wayland::{
        compositor::{
            get_parent, is_sync_subsurface, CompositorClientState, CompositorHandler,
            CompositorState,
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        },
    },
};

use crate::{
    layout::{self, Rect},
    state::{ClientState, State},
};

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("every client is inserted with ClientState")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for_surface(&root) {
                window.on_commit();
            }
        }

        self.toplevel_commit(surface);
        self.popup_commit(surface);
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.pending_windows
            .push(Window::new_wayland_window(surface));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.remove_window(&window);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            eprintln!("sengi-compositor: popup for a dead surface: {err}");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    // Popup grabs need pointer input, which no backend delivers yet.
    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}
}

impl State {
    /// The window whose toplevel is `surface`, mapped or not.
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.space
            .elements()
            .chain(self.pending_windows.iter())
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned()
    }

    fn toplevel_commit(&mut self, surface: &WlSurface) {
        let Some(index) = self
            .pending_windows
            .iter()
            .position(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        else {
            return;
        };
        let window = self.pending_windows[index].clone();
        let Some(toplevel) = window.toplevel() else {
            return;
        };

        if !toplevel.is_initial_configure_sent() {
            toplevel.send_configure();
            return;
        }

        // Wait for the first buffer; before that the window has no size.
        let size = window.geometry().size;
        if size.w <= 0 || size.h <= 0 {
            return;
        }

        self.pending_windows.remove(index);
        let step = self
            .tokens
            .px("size.titlebar-height")
            .expect("tokens define size.titlebar-height") as i32;
        let location = layout::place(self.usable_area(), (size.w, size.h), self.placed, step);
        self.placed += 1;
        self.space.map_element(window.clone(), location, true);
        self.focus_window(&window);
    }

    fn popup_commit(&mut self, surface: &WlSurface) {
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                // Cannot fail: this is the first configure of a live popup.
                let _ = popup.send_configure();
            }
        }
    }

    /// The part of the output windows may cover.
    pub fn usable_area(&self) -> Rect {
        let Some(output) = self.output() else {
            return Rect::new(0, 0, 0, 0);
        };
        let origin = self
            .space
            .output_geometry(&output)
            .map(|geo| geo.loc)
            .unwrap_or_default();
        let zone = layer_map_for_output(&output).non_exclusive_zone();
        Rect::new(
            origin.x + zone.loc.x,
            origin.y + zone.loc.y,
            zone.size.w,
            zone.size.h,
        )
    }
}

delegate_compositor!(State);
delegate_xdg_shell!(State);
