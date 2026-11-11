//! wl_compositor and xdg-shell handling: toplevels and popups.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{layer_map_for_output, PopupKind, Window},
    reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
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
            decoration::XdgDecorationHandler, PopupSurface, PositionerState, ToplevelSurface,
            XdgShellHandler, XdgShellState,
        },
    },
};

use crate::{
    frame::Edges,
    layout::{self, Rect},
    state::{ClientState, State},
    window,
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
        self.layer_commit(surface);
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

    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            return;
        };
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        // Only a press on the window can start a drag.
        if self.drag.is_some() || !pointer.is_grabbed() {
            return;
        }
        let at = pointer.current_location();
        self.release_pointer_from_client();
        self.start_move(&window, (at.x.floor() as i32, at.y.floor() as i32));
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        edges: ResizeEdge,
    ) {
        let Some(window) = self.window_for_surface(surface.wl_surface()) else {
            return;
        };
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if self.drag.is_some() || !pointer.is_grabbed() {
            return;
        }
        let has = |part: ResizeEdge| edges as u32 & part as u32 != 0;
        let edges = Edges {
            left: has(ResizeEdge::Left),
            right: has(ResizeEdge::Right),
            top: has(ResizeEdge::Top),
            bottom: has(ResizeEdge::Bottom),
        };
        let at = pointer.current_location();
        self.release_pointer_from_client();
        self.start_resize(&window, edges, (at.x.floor() as i32, at.y.floor() as i32));
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        match self.window_for_surface(surface.wl_surface()) {
            Some(window) if self.space.element_location(&window).is_some() => {
                self.set_maximised(&window, true);
            }
            // Not on screen yet; the protocol still wants an answer.
            _ => {
                if surface.is_initial_configure_sent() {
                    surface.send_configure();
                }
            }
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.set_minimised(&window, true);
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.set_maximised(&window, false);
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
            .chain(self.minimised.iter())
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned()
    }

    fn toplevel_commit(&mut self, surface: &WlSurface) {
        let Some(index) = self
            .pending_windows
            .iter()
            .position(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        else {
            // Already mapped: the client may have switched decoration mode.
            if let Some(window) = self.window_for_surface(surface) {
                let decorated = window::wants_server_decoration(&window);
                window::data(&window).decorated.set(decorated);
                self.apply_resize_anchor(&window);
            }
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
        let decorated = window::wants_server_decoration(&window);
        window::data(&window).decorated.set(decorated);

        // Place the whole frame, then put the client area inside it.
        let metrics = self.theme.metrics;
        let client = Rect::new(0, 0, size.w, size.h);
        let outline = if decorated {
            metrics.frame(client)
        } else {
            client
        };
        let (x, y) = layout::place(
            self.usable_area(),
            (outline.w, outline.h),
            self.placed,
            metrics.titlebar,
        );
        self.placed += 1;
        self.space
            .map_element(window.clone(), (x - outline.x, y - outline.y), true);
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

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // Ours unless the client insists on its own.
        set_decoration_mode(&toplevel, Mode::ServerSide);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: Mode) {
        set_decoration_mode(&toplevel, mode);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        set_decoration_mode(&toplevel, Mode::ServerSide);
    }
}

fn set_decoration_mode(toplevel: &ToplevelSurface, mode: Mode) {
    toplevel.with_pending_state(|state| state.decoration_mode = Some(mode));
    // Before the first configure the mode simply rides along with it.
    if toplevel.is_initial_configure_sent() {
        toplevel.send_pending_configure();
    }
}

delegate_xdg_decoration!(State);
delegate_compositor!(State);
delegate_xdg_shell!(State);
