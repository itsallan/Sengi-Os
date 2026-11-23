//! X11 programs, through Xwayland.
//!
//! Xwayland is not started at boot. The first program the user starts while
//! it is not up starts it (see `State::spawn`). That program is held until
//! Xwayland is ready, so that it finds a DISPLAY, and the programs started
//! after it get the DISPLAY at once. When Xwayland goes away the next program
//! the user starts brings it back.
//!
//! Smithay starts Xwayland and makes its connection to us as a client. The
//! window manager on the other end of that connection is `X11Wm`, and the
//! handlers below are what it asks of us.

use std::os::unix::net::UnixStream;

use smithay::{
    delegate_xwayland_shell,
    reexports::{calloop::RegistrationToken, wayland_server::Client},
    utils::{Logical, Rectangle},
    wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    xwayland::{
        xwm::{Reorder, ResizeEdge, X11Window, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
    },
};

use crate::state::State;

/// Xwayland, from the moment it is asked for until it is gone.
pub struct Session {
    /// Where the event source of the Xwayland instance is, for removing it.
    token: RegistrationToken,
    /// Xwayland's connection to us as a client, kept for the window manager.
    client: Client,
    /// The display number, once Xwayland is ready.
    pub display: Option<u32>,
    /// The window manager on Xwayland's X connection, once it is ready.
    wm: Option<X11Wm>,
}

impl State {
    /// The DISPLAY programs started now are given, if Xwayland is ready.
    pub fn display_name(&self) -> Option<String> {
        self.x11
            .as_ref()?
            .display
            .map(|number| format!(":{number}"))
    }

    /// Starts Xwayland if it is not up or starting. Returns whether it is
    /// starting now, so that the programs waiting for it can be held.
    pub fn start_xwayland(&mut self) -> bool {
        if self.x11.is_some() {
            return false;
        }
        let spawned = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(&str, &str)>(),
            true,
            std::process::Stdio::null(),
            std::process::Stdio::inherit(),
            |_| {},
        );
        let (server, client) = match spawned {
            Ok(spawned) => spawned,
            Err(err) => {
                crate::say!("cannot start Xwayland, X11 programs will not run: {err}");
                return false;
            }
        };
        let inserted = self
            .loop_handle
            .insert_source(server, |event, _, state| match event {
                XWaylandEvent::Ready {
                    x11_socket,
                    display_number,
                } => state.xwayland_ready(display_number, x11_socket),
                XWaylandEvent::Error => state.xwayland_failed(),
            });
        match inserted {
            Ok(token) => {
                self.x11 = Some(Session {
                    token,
                    client,
                    display: None,
                    wm: None,
                });
                true
            }
            Err(err) => {
                crate::say!("cannot watch Xwayland, X11 programs will not run: {err}");
                false
            }
        }
    }

    fn xwayland_ready(&mut self, display: u32, socket: UnixStream) {
        let handle = self.loop_handle.clone();
        let Some(session) = self.x11.as_mut() else {
            return;
        };
        session.display = Some(display);
        match X11Wm::start_wm(handle, socket, session.client.clone()) {
            Ok(wm) => {
                session.wm = Some(wm);
                crate::say!("Xwayland is up on :{display}");
            }
            Err(err) => {
                crate::say!(
                    "Xwayland is up but cannot be managed, X11 programs will not run: {err}"
                );
                self.stop_xwayland();
            }
        }
        self.start_held_programs();
    }

    /// Xwayland did not come up. The programs held for it run without it,
    /// so that a broken Xwayland costs the X11 programs and nothing else.
    fn xwayland_failed(&mut self) {
        crate::say!("Xwayland did not start, X11 programs will not run");
        self.stop_xwayland();
        self.start_held_programs();
    }

    /// Xwayland is gone, by its own exit or by ours. It is left down: no
    /// socket is kept for an X11 client to wake it, so the next program the
    /// user starts is what brings it back.
    pub fn stop_xwayland(&mut self) {
        let Some(session) = self.x11.take() else {
            return;
        };
        // Removing the event source ends the Xwayland process.
        self.loop_handle.remove(session.token);
        drop(session.wm);
        self.request_redraw();
    }

    fn start_held_programs(&mut self) {
        let held = std::mem::take(&mut self.held_commands);
        for command in held {
            self.spawn_plain(&command);
        }
    }
}

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

delegate_xwayland_shell!(State);

impl XwmHandler for State {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        // The window manager is dropped only once its connection has closed,
        // so no event of it can arrive without it.
        self.x11
            .as_mut()
            .and_then(|session| session.wm.as_mut())
            .expect("an X11 event comes only while Xwayland is managed")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn unmapped_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn destroyed_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        _w: Option<u32>,
        _h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _geometry: Rectangle<i32, Logical>,
        _above: Option<X11Window>,
    ) {
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _edge: ResizeEdge,
    ) {
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}

    /// Xwayland has exited: its connection is gone.
    fn disconnected(&mut self, _xwm: XwmId) {
        self.stop_xwayland();
    }
}
