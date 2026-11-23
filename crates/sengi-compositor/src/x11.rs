//! X11 programs, through Xwayland, and the windows they make.
//!
//! Xwayland is started once, at the start of a normal session (not on the
//! login screen), so that the dock, bar and launcher, and everything they
//! start, find a DISPLAY. Programs started while it comes up are held until it
//! is ready, or for a few seconds at most; then they run without a DISPLAY.
//! It stays up for the whole session. If it exits, its windows go with it and
//! the next program started brings it back.
//!
//! Smithay starts Xwayland and makes its connection to us as a client; the
//! window manager on the other end of that connection is `X11Wm`, and the
//! handlers below are what it asks of us. An X11 window is a `Window` like
//! any other: it is framed, focused, switched to and closed as an xdg one is.
//! Menus from X11 toolkits are override-redirect windows: they are put where
//! the client asks, drawn, and never framed, focused or listed.

use std::{
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

use smithay::{
    delegate_xwayland_shell,
    desktop::Window,
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            RegistrationToken,
        },
        wayland_server::{protocol::wl_surface::WlSurface, Client},
    },
    utils::{Logical, Rectangle},
    wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    xwayland::{
        xwm::{Reorder, ResizeEdge, WmWindowProperty, X11Window, XwmId},
        X11Surface, X11Wm, XWayland, XWaylandEvent, XwmHandler,
    },
};

use crate::{layout::Rect, state::State, window};

/// How long a program is held for Xwayland before it runs without a DISPLAY.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Xwayland, from the moment it is asked for until it is gone.
pub struct Session {
    /// Which Xwayland this is, for the timer that watches it come up.
    generation: u64,
    started: Instant,
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
        // Nobody is logged in on the login screen, so there is nothing for
        // X11 programs to run for.
        if self.x11.is_some() || self.login_screen || self.x11_gave_up {
            return false;
        }
        let spawned = XWayland::spawn(
            &self.display_handle,
            None,
            std::iter::empty::<(&str, &str)>(),
            // No abstract socket: any process that shares the network
            // namespace could connect to it, sandboxed ones too. Only the
            // socket in /tmp/.X11-unix is used.
            false,
            std::process::Stdio::null(),
            std::process::Stdio::inherit(),
            |_| {},
        );
        let (server, client) = match spawned {
            Ok(spawned) => spawned,
            Err(err) => {
                crate::say!("cannot start Xwayland, X11 programs will not run: {err}");
                self.x11_gave_up = true;
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
                self.x11_generation += 1;
                let generation = self.x11_generation;
                let timer = Timer::from_duration(READY_TIMEOUT);
                let _ = self.loop_handle.insert_source(timer, move |_, _, state| {
                    state.xwayland_overdue(generation);
                    TimeoutAction::Drop
                });
                self.x11 = Some(Session {
                    generation,
                    started: Instant::now(),
                    token,
                    client,
                    display: None,
                    wm: None,
                });
                true
            }
            Err(err) => {
                crate::say!("cannot watch Xwayland, X11 programs will not run: {err}");
                self.x11_gave_up = true;
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
                let took = session.started.elapsed().as_millis();
                crate::say!("Xwayland is up on :{display}");
                crate::say!("Xwayland took {took} ms to report ready");
            }
            Err(err) => {
                crate::say!(
                    "Xwayland is up but cannot be managed, X11 programs will not run: {err}"
                );
                self.x11_gave_up = true;
                self.stop_xwayland();
            }
        }
        self.start_held_programs();
    }

    /// Xwayland did not come up. The programs held for it run without it,
    /// so that a broken Xwayland costs the X11 programs and nothing else.
    fn xwayland_failed(&mut self) {
        crate::say!("Xwayland did not start, X11 programs will not run");
        self.x11_gave_up = true;
        self.stop_xwayland();
        self.start_held_programs();
    }

    /// The timer of Xwayland number `generation` ran out. If that one is
    /// still not ready it is given up, and the programs waiting for it run
    /// without a DISPLAY.
    fn xwayland_overdue(&mut self, generation: u64) {
        let waiting = self
            .x11
            .as_ref()
            .is_some_and(|session| session.generation == generation && session.display.is_none());
        if waiting {
            crate::say!("Xwayland did not report ready in time, X11 programs will not run");
            self.x11_gave_up = true;
            self.stop_xwayland();
            self.start_held_programs();
        }
    }

    /// Xwayland is gone, by its own exit or by ours. Its windows go with it.
    /// It is left down: no socket is kept for an X11 client to wake it, so
    /// the next program the user starts is what brings it back.
    pub fn stop_xwayland(&mut self) {
        let Some(session) = self.x11.take() else {
            return;
        };
        // Removing the event source ends the Xwayland process.
        self.loop_handle.remove(session.token);
        drop(session.wm);
        self.forget_x11_windows();
        self.request_redraw();
    }

    fn start_held_programs(&mut self) {
        let held = std::mem::take(&mut self.held_commands);
        for (command, is_session) in held {
            let pid = self.spawn_plain(&command);
            if is_session {
                self.session_started(pid);
            }
        }
    }

    fn forget_x11_windows(&mut self) {
        let windows: Vec<Window> = self
            .space
            .elements()
            .chain(self.minimised.iter())
            .filter(|window| window::is_x11(window))
            .cloned()
            .collect();
        for window in windows {
            self.remove_window(&window);
        }
    }

    /// The window that is `x11` on the desktop or minimised.
    fn window_for_x11(&self, x11: &X11Surface) -> Option<Window> {
        self.space
            .elements()
            .chain(self.minimised.iter())
            .find(|window| window.x11_surface() == Some(x11))
            .cloned()
    }

    fn forget_x11(&mut self, x11: &X11Surface) {
        if let Some(window) = self.window_for_x11(x11) {
            self.remove_window(&window);
        }
    }

    /// Maps a window that asked to be shown: placed like an xdg window, with
    /// the frame the client's Motif hints ask for, and focused.
    fn map_x11(&mut self, x11: X11Surface) {
        let window = Window::new_x11_window(x11.clone());
        let decorated = window::wants_frame(&window);
        window::data(&window).decorated.set(decorated);
        let size = x11.geometry().size;
        let (w, h) = (size.w.max(1), size.h.max(1));
        let metrics = self.theme.metrics;
        let client = Rect::new(0, 0, w, h);
        let outline = if decorated {
            metrics.frame(client)
        } else {
            client
        };
        let (x, y) = crate::layout::place(
            self.usable_area(),
            (outline.w, outline.h),
            self.placed,
            metrics.titlebar,
        );
        self.placed += 1;
        let origin = (x - outline.x, y - outline.y);
        self.space.map_element(window.clone(), origin, true);
        window::place_x11(&window, origin.0, origin.1, (w, h));
        self.focus_window(&window);
        // What the client asked for before it was shown.
        if x11.is_maximized() {
            self.set_maximised(&window, true);
        }
        if x11.is_fullscreen() {
            self.set_fullscreen(&window, true, None);
        }
    }

    /// Puts a menu of an X11 toolkit where it asked to be. It is not framed
    /// and not focused, and it is not in the window list.
    fn map_menu(&mut self, x11: X11Surface) {
        if self.window_for_x11(&x11).is_some() {
            return;
        }
        let location = x11.geometry().loc;
        self.space
            .map_element(Window::new_x11_window(x11), location, false);
        self.request_redraw();
    }
}

impl XWaylandShellHandler for State {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    /// A window that was focused before its surface was known gets the
    /// keyboard now.
    fn surface_associated(&mut self, _xwm: XwmId, _surface: WlSurface, _window: X11Surface) {
        self.update_keyboard_focus();
        self.request_redraw();
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

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if self.window_for_x11(&window).is_some() || window.set_mapped(true).is_err() {
            return;
        }
        self.map_x11(window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.map_menu(window);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.forget_x11(&window);
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        self.forget_x11(&window);
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let current = window.geometry();
        let size = (
            w.map_or(current.size.w, |w| w as i32),
            h.map_or(current.size.h, |h| h as i32),
        );
        match self.window_for_x11(&window) {
            // On the desktop the compositor says where a window is, and the
            // size it asks for is what it gets, as far as the usable area
            // goes, as for an xdg window. A fullscreen one is not held to it.
            Some(managed) if self.space.element_location(&managed).is_some() => {
                let data = window::data(&managed);
                let size = if data.fullscreen.get().is_some() {
                    size
                } else {
                    // Held to the output the window is on.
                    let usable = self
                        .output_of_window(&managed)
                        .map_or_else(|| self.usable_area(), |output| self.usable_area_of(&output));
                    let most = if data.decorated.get() {
                        self.theme.metrics.client(usable)
                    } else {
                        usable
                    };
                    (
                        size.0.clamp(1, most.w.max(1)),
                        size.1.clamp(1, most.h.max(1)),
                    )
                };
                window::request_size(&managed, size, false);
            }
            _ => {
                let loc = (x.unwrap_or(current.loc.x), y.unwrap_or(current.loc.y));
                let _ = window.configure(Rectangle::new(loc.into(), size.into()));
            }
        }
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<X11Window>,
    ) {
        // A menu is put where the client moved it; the others are ours to place.
        if !window.is_override_redirect() {
            return;
        }
        if let Some(menu) = self.window_for_x11(&window) {
            self.move_window(&menu, geometry.loc.x, geometry.loc.y);
        }
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        let Some(managed) = self.window_for_x11(&window) else {
            return;
        };
        if property == WmWindowProperty::MotifHints && !window::is_override(&managed) {
            let decorated = window::wants_frame(&managed);
            if window::data(&managed).decorated.replace(decorated) != decorated {
                self.decoration_changed(&managed);
            }
        }
        self.request_redraw();
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(managed) = self.on_screen_x11(&window) {
            self.set_maximised(&managed, true);
        }
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(managed) = self.on_screen_x11(&window) {
            self.set_maximised(&managed, false);
        }
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(managed) = self.on_screen_x11(&window) {
            self.set_fullscreen(&managed, true, None);
        }
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(managed) = self.on_screen_x11(&window) {
            self.set_fullscreen(&managed, false, None);
        }
    }

    fn minimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(managed) = self.window_for_x11(&window) {
            self.set_minimised(&managed, true);
        }
    }

    fn unminimize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        if let Some(managed) = self.window_for_x11(&window) {
            self.set_minimised(&managed, false);
        }
    }

    // Interactive moves and resizes that a client starts itself need the
    // pointer press that began them, which X11 does not hand to us. The
    // compositor's own title bar does both.
    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _edge: ResizeEdge,
    ) {
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}

    /// Xwayland has exited: its connection is gone. The next program started
    /// brings it back.
    fn disconnected(&mut self, _xwm: XwmId) {
        self.stop_xwayland();
    }
}

impl State {
    /// The window that is `x11` if it is on the desktop, not minimised.
    fn on_screen_x11(&self, x11: &X11Surface) -> Option<Window> {
        self.window_for_x11(x11)
            .filter(|window| self.space.element_location(window).is_some())
    }
}
