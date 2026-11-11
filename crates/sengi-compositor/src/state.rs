//! Compositor state shared by every backend.

use std::{
    collections::HashMap,
    error::Error,
    ffi::OsString,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    process::{Child, Command},
    sync::Arc,
    time::Instant,
};

use sengi_tokens::{Rgba, Tokens};
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{utils::with_renderer_surface_state, Color32F},
    },
    delegate_cursor_shape, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    desktop::{layer_map_for_output, PopupManager, Space, Window, WindowSurfaceType},
    input::{
        pointer::{CursorIcon, CursorImageStatus},
        Seat, SeatHandler, SeatState,
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            ping::Ping,
            signals::{Signal, Signals},
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction,
        },
        wayland_protocols::ext::idle_notify::v1::server::{
            ext_idle_notification_v1::ExtIdleNotificationV1,
            ext_idle_notifier_v1::ExtIdleNotifierV1,
        },
        wayland_server::{
            backend::GlobalId,
            backend::{ClientData, ClientId, DisconnectReason},
            delegate_dispatch,
            protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
            Client, DataInit, Display, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Logical, Physical, Point, Size, Transform, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorState},
        cursor_shape::CursorShapeManagerState,
        dmabuf::DmabufState,
        idle_notify::{IdleNotificationUserData, IdleNotifierHandler, IdleNotifierState},
        output::{OutputHandler, OutputManagerState},
        security_context::SecurityContextState,
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        session_lock::SessionLockManagerState,
        shell::{
            wlr_layer::{KeyboardInteractivity, Layer, WlrLayerShellState},
            xdg::{decoration::XdgDecorationState, XdgShellState},
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
        tablet_manager::TabletSeatHandler,
    },
};

use crate::{
    cursor::{resize_cursor, Cursors},
    decoration::Theme,
    foreign_toplevel::ForeignToplevelState,
    frame::Button,
    input::Drag,
    keys::{self, Swallowed},
    layout::{self, FocusStack, Rect},
    lock::{Lock, Lockers},
    render, security,
    text::Font,
};

/// Environment variable through which started programs learn our pid.
pub const PID_VARIABLE: &str = "SENGI_COMPOSITOR_PID";

pub type ScreenshotHook = Box<dyn FnMut(&mut State)>;
pub type DmabufImport = Box<dyn FnMut(&Dmabuf) -> bool>;

/// Refresh rate reported for outputs that have no real one, in mHz.
pub const REFRESH_MHZ: i32 = 60_000;

pub struct State {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub loop_handle: LoopHandle<'static, Self>,
    /// Something visible changed since the last frame. Set through
    /// `request_redraw`, cleared by the backend when it draws.
    pub needs_redraw: bool,
    /// Wakes the backend to draw; each backend installs its own.
    redraw_ping: Option<Ping>,
    /// The slow tick for surfaces nobody can see is already scheduled.
    hidden_frames_armed: bool,
    /// A lock that is confirmed once the first locked frame is out.
    /// How many times the session has been locked.
    pub lock_count: u64,
    /// Set while a request of the lock's holder is being handled.
    pub lock_request_from_holder: bool,
    /// Which program may lock.
    pub lockers: Lockers,
    /// Switches to another virtual terminal, where the backend can.
    pub switch_vt: Option<Box<dyn FnMut(i32)>>,
    /// Frames shown on a real screen so far. Stays zero on backends that
    /// do not flip pages.
    pub page_flips: u64,
    /// How many of those had the cursor on a plane of its own.
    pub cursor_plane_frames: u64,
    /// Saves a picture of the screen on SIGUSR2, where the backend can.
    pub take_screenshot: Option<ScreenshotHook>,
    pub dmabuf_state: DmabufState,
    /// Tries a client's dmabuf on the renderer; set by a backend that
    /// offers linux-dmabuf.
    pub dmabuf_import: Option<DmabufImport>,
    pub dmabufs_imported: u64,
    /// What Super+L runs, and what is started for a session whose locker
    /// died.
    pub locker_command: String,
    /// Lockers we started ourselves that are still running.
    pub locker_pids: Vec<u32>,
    pub relock_armed: bool,
    pub tokens: Tokens,
    pub theme: Theme,
    /// The title bar typeface, if it is installed.
    pub font: Option<Font>,
    /// Set when the backend gave up, so the process exits non-zero.
    pub failed: bool,

    pub space: Space<Window>,
    pub popups: PopupManager,
    /// Toplevels that exist but have not committed a buffer yet. They are
    /// placed and mapped once their size is known.
    pub pending_windows: Vec<Window>,
    /// Windows hidden by minimise; they are not in `space` meanwhile.
    pub minimised: Vec<Window>,
    pub focus_stack: FocusStack<Window>,
    /// A layer surface with on-demand keyboard focus that was clicked.
    pub layer_focus: Option<WlSurface>,
    /// The title bar button under the pointer.
    pub hovered_button: Option<(Window, Button)>,
    /// A move, resize or button press the pointer is in the middle of.
    pub drag: Option<Drag>,
    /// The pointer may be over something else than its focus says, and
    /// has to be looked at again even though it did not move.
    pub pointer_stale: bool,
    pub swallowed_keys: Swallowed,
    /// Alt+Tab in progress: the windows in the order they had when it
    /// started, and which one is selected.
    pub cycle: Option<(Vec<Window>, usize)>,
    /// Programs we started and still have to collect when they exit.
    children: Vec<Child>,
    pub cursors: Cursors,
    /// The cursor the client under the pointer asked for.
    pub client_cursor: CursorImageStatus,
    /// The cursor for the frame part under the pointer, if it has its own.
    pub frame_cursor: Option<CursorIcon>,
    /// False until the pointer first moves, so a session without a mouse
    /// has no cursor parked in the corner.
    pub pointer_seen: bool,
    /// Counts mapped windows so new ones cascade.
    pub placed: usize,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    _xdg_decoration_state: XdgDecorationState,
    _cursor_shape_state: CursorShapeManagerState,
    pub layer_shell_state: WlrLayerShellState,
    pub foreign_toplevels: ForeignToplevelState,
    pub session_lock_state: SessionLockManagerState,
    /// Set while the session is locked. See `lock.rs`.
    pub lock: Option<Lock>,
    pub idle_notifier_state: IdleNotifierState<Self>,
    _security_context_state: SecurityContextState,
    /// How many security contexts each client has made.
    pub security_contexts: HashMap<ClientId, u32>,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    // Held so the xdg-output global stays alive.
    _output_manager_state: OutputManagerState,
    /// What announces each output to clients, to take back when it goes.
    output_globals: Vec<(Output, GlobalId)>,

    pub seat: Seat<Self>,
}

impl State {
    pub fn new(
        event_loop: &mut EventLoop<'static, Self>,
        display: Display<Self>,
    ) -> Result<Self, Box<dyn Error>> {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&dh);
        let foreign_toplevels = ForeignToplevelState::new(&dh);
        let cursor_shape_state = CursorShapeManagerState::new::<Self>(&dh);
        // Only the locker program gets to see the lock.
        let lockers = Lockers::default();
        let known = lockers.clone();
        let session_lock_state =
            SessionLockManagerState::new::<Self, _>(&dh, move |client| known.recognises(client));
        let idle_notifier_state = IdleNotifierState::<Self>::new(&dh, event_loop.handle());
        let layer_shell_state =
            WlrLayerShellState::new_with_filter::<Self, _>(&dh, security::trusted);
        let security_context_state = SecurityContextState::new::<Self, _>(&dh, security::trusted);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 300, 30)?;
        seat.add_pointer();

        // The way to log out: `sengi-compositor --exit`, or any of these
        // signals, ends the loop and with it the session.
        // SIGUSR1 reports how many frames have reached the screen, for
        // checking that an idle one shows none. SIGUSR2 saves a screenshot.
        let signals = Signals::new(&[
            Signal::SIGTERM,
            Signal::SIGINT,
            Signal::SIGHUP,
            Signal::SIGUSR1,
            Signal::SIGUSR2,
            Signal::SIGCHLD,
        ])?;
        event_loop
            .handle()
            .insert_source(signals, |event, _, state| match event.signal() {
                Signal::SIGUSR1 => {
                    let pointer = state.seat.get_pointer().expect("seat has a pointer");
                    let at = pointer.current_location();
                    crate::say!(
                        "{} page flips, {} with the cursor on its own plane, {} dmabufs imported, pointer at {:.0},{:.0}",
                        state.page_flips, state.cursor_plane_frames, state.dmabufs_imported, at.x, at.y
                    );
                    crate::say!("{}", state.lock_status());
                }
                // A program we started has ended; a dead locker must be
                // noticed without waiting for the next input.
                Signal::SIGCHLD => {
                    state.reap_children();
                    state.watch_locker();
                }
                Signal::SIGUSR2 => {
                    if let Some(mut shoot) = state.take_screenshot.take() {
                        shoot(state);
                        state.take_screenshot = Some(shoot);
                    }
                }
                _ => state.loop_signal.stop(),
            })?;

        let socket = ListeningSocketSource::new_auto()?;
        let socket_name = socket.socket_name().to_os_string();
        let handle = event_loop.handle();
        handle.insert_source(socket, |stream, _, state| {
            let data = ClientState::of(&stream, false);
            if let Err(err) = state.display_handle.insert_client(stream, Arc::new(data)) {
                crate::say!("rejected a client: {err}");
            }
        })?;
        handle.insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, state| {
                // Safety: the display is never dropped while the loop runs.
                unsafe { display.get_mut().dispatch_clients(state)? };
                Ok(PostAction::Continue)
            },
        )?;

        let tokens = Tokens::builtin();
        let theme = Theme::from_tokens(&tokens);
        let font = Font::load(&theme.font_family, theme.font_weight, theme.font_px);
        if font.is_none() {
            crate::say!(
                "{} is not installed, title bars will have no text",
                theme.font_family
            );
        }

        let cursors = Cursors::new(&theme);

        Ok(Self {
            start_time: Instant::now(),
            socket_name,
            display_handle: dh,
            loop_signal: event_loop.get_signal(),
            loop_handle: event_loop.handle(),
            needs_redraw: false,
            redraw_ping: None,
            hidden_frames_armed: false,
            lock_count: 0,
            lock_request_from_holder: false,
            lockers,
            switch_vt: None,
            page_flips: 0,
            cursor_plane_frames: 0,
            take_screenshot: None,
            dmabuf_state: DmabufState::new(),
            dmabuf_import: None,
            dmabufs_imported: 0,
            locker_command: keys::DEFAULT_LOCKER.to_string(),
            locker_pids: Vec::new(),
            relock_armed: false,
            tokens,
            theme,
            font,
            failed: false,
            space: Space::default(),
            popups: PopupManager::default(),
            pending_windows: Vec::new(),
            minimised: Vec::new(),
            focus_stack: FocusStack::default(),
            layer_focus: None,
            hovered_button: None,
            drag: None,
            pointer_stale: false,
            swallowed_keys: Swallowed::default(),
            cycle: None,
            children: Vec::new(),
            cursors,
            client_cursor: CursorImageStatus::default_named(),
            frame_cursor: None,
            pointer_seen: false,
            placed: 0,
            compositor_state,
            xdg_shell_state,
            _xdg_decoration_state: xdg_decoration_state,
            _cursor_shape_state: cursor_shape_state,
            layer_shell_state,
            foreign_toplevels,
            session_lock_state,
            lock: None,
            idle_notifier_state,
            _security_context_state: security_context_state,
            security_contexts: HashMap::new(),
            shm_state,
            seat_state,
            data_device_state,
            _output_manager_state: output_manager_state,
            output_globals: Vec::new(),
            seat,
        })
    }

    /// Runs until a backend stops the loop.
    pub fn run(mut self, mut event_loop: EventLoop<'static, Self>) -> Result<(), Box<dyn Error>> {
        event_loop.run(None, &mut self, |state| {
            state.space.refresh();
            state.popups.cleanup();
            state.sync_pointer_focus();
            state.refresh_foreign_toplevels();
            state.reap_children();
            state.watch_locker();
            let _ = state.display_handle.flush_clients();
        })?;

        if self.failed {
            return Err("rendering failed".into());
        }
        Ok(())
    }

    pub fn set_redraw_ping(&mut self, ping: Ping) {
        self.redraw_ping = Some(ping);
    }

    /// Asks for a frame. Everything that changes what is on screen calls
    /// this; nothing is drawn otherwise.
    pub fn request_redraw(&mut self) {
        if self.needs_redraw {
            return;
        }
        self.needs_redraw = true;
        if let Some(ping) = &self.redraw_ping {
            ping.ping();
        }
    }

    /// A client committed: besides the redraw, make sure it gets its frame
    /// callback eventually even if it turns out not to be visible.
    pub fn surface_committed(&mut self) {
        self.request_redraw();
        if self.hidden_frames_armed {
            return;
        }
        let timer = Timer::from_duration(render::HIDDEN_FRAME_INTERVAL);
        let armed = self.loop_handle.insert_source(timer, |_, _, state| {
            state.hidden_frames_armed = false;
            let mut outputs: Vec<Output> = state.space.outputs().cloned().collect();
            if outputs.is_empty() {
                // With every screen gone, clients are still owed the slow
                // tick; it is sent in the name of a screen that is not
                // there.
                outputs.push(Output::new(
                    "none".into(),
                    PhysicalProperties {
                        size: (0, 0).into(),
                        subpixel: Subpixel::Unknown,
                        make: "Sengi OS".into(),
                        model: "none".into(),
                    },
                ));
            }
            for output in outputs {
                render::send_frames(state, &output);
            }
            TimeoutAction::Drop
        });
        self.hidden_frames_armed = armed.is_ok();
    }

    /// The colour behind all windows.
    pub fn desktop_color(&self) -> Color32F {
        let desktop = self
            .tokens
            .color("color.desktop")
            .expect("tokens define color.desktop");
        self.color(desktop)
    }

    /// A token colour in the form the renderer takes: premultiplied floats.
    pub fn color(&self, c: Rgba) -> Color32F {
        let alpha = f32::from(c.a) / 255.0;
        let channel = |value: u8| f32::from(value) / 255.0 * alpha;
        Color32F::new(channel(c.r), channel(c.g), channel(c.b), alpha)
    }

    pub fn output(&self) -> Option<Output> {
        self.space.outputs().next().cloned()
    }

    /// Where every output is on the desktop.
    pub fn output_rects(&self) -> Vec<Rect> {
        self.space
            .outputs()
            .filter_map(|output| self.space.output_geometry(output))
            .map(|geo| Rect::new(geo.loc.x, geo.loc.y, geo.size.w, geo.size.h))
            .collect()
    }

    /// The output `location` is on, or the first one.
    pub fn output_at(&self, location: Point<f64, Logical>) -> Option<Output> {
        self.space
            .outputs()
            .find(|output| {
                self.space
                    .output_geometry(output)
                    .is_some_and(|geo| geo.to_f64().contains(location))
            })
            .cloned()
            .or_else(|| self.output())
    }

    /// Announces an output of `size` pixels to clients and puts it to the
    /// right of the ones already there.
    pub fn add_output(
        &mut self,
        name: &str,
        model: &str,
        size: Size<i32, Physical>,
        refresh_mhz: i32,
        transform: Transform,
    ) -> Output {
        let output = Output::new(
            name.into(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Sengi OS".into(),
                model: model.into(),
            },
        );
        let global = output.create_global::<Self>(&self.display_handle);
        self.output_globals.push((output.clone(), global));
        let x = layout::next_output_x(&self.output_rects());
        let mode = OutputMode {
            size,
            refresh: refresh_mhz,
        };
        output.change_current_state(Some(mode), Some(transform), None, Some((x, 0).into()));
        output.set_preferred(mode);
        self.space.map_output(&output, (x, 0));
        output
    }

    /// Takes an output that was unplugged off the desktop. Its bars are
    /// closed and its windows move to a screen that is still there.
    #[cfg_attr(not(feature = "drm"), allow(dead_code))]
    pub fn remove_output(&mut self, output: &Output) {
        for layer in layer_map_for_output(output).layers() {
            layer.layer_surface().send_close();
        }
        self.space.unmap_output(output);
        self.lock_output_removed(output);
        self.configure_lock_surfaces();
        if let Some(at) = self.output_globals.iter().position(|(o, _)| o == output) {
            let (_, global) = self.output_globals.remove(at);
            self.display_handle.remove_global::<Self>(global);
        }

        let remaining = self.output_rects();
        let usable = self.usable_area();
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(frame) = self.frame_rect(&window) else {
                continue;
            };
            if let Some((x, y)) = layout::rehome(frame, &remaining, usable) {
                self.move_frame_to(&window, x, y);
            }
        }
        self.usable_area_changed();

        // The pointer may have been on the screen that went.
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let at = pointer.current_location();
        let (x, y) = layout::clamp_to_outputs(&remaining, at.x, at.y);
        if (x, y) != (at.x, at.y) && !pointer.is_grabbed() {
            let time = self.start_time.elapsed().as_millis() as u32;
            self.place_pointer((x, y).into(), time);
        }
        self.update_keyboard_focus();
        self.request_redraw();
    }

    /// Applies a new size to `output`, for backends whose window can resize.
    #[cfg(feature = "winit")]
    pub fn resize_output(&mut self, output: &Output, size: Size<i32, Physical>) {
        let mode = OutputMode {
            size,
            refresh: REFRESH_MHZ,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);
        layer_map_for_output(output).arrange();
        self.configure_lock_surfaces();
        self.usable_area_changed();
        self.request_redraw();
    }

    /// Raises `window`, marks it active and gives it the keyboard.
    pub fn focus_window(&mut self, window: &Window) {
        if let Some(old) = self.focus_stack.current().cloned() {
            if old != *window {
                set_activated(&old, false);
            }
        }
        self.request_redraw();
        self.focus_stack.focus(window.clone());
        self.space.raise_element(window, true);
        set_activated(window, true);

        // Choosing a window takes the keyboard back from an on-demand layer.
        self.layer_focus = None;
        self.update_keyboard_focus();
    }

    /// Forgets `window` and hands focus to the one used before it.
    pub fn remove_window(&mut self, window: &Window) {
        self.request_redraw();
        self.pending_windows.retain(|w| w != window);
        self.space.unmap_elem(window);
        self.minimised.retain(|w| w != window);
        self.forget_pointer_state(window);
        if !self.focus_stack.remove(window) {
            return;
        }
        match self.focus_stack.current().cloned() {
            Some(next) => self.focus_window(&next),
            None => self.update_keyboard_focus(),
        }
    }

    /// Runs `command` with the shell as a client of this compositor.
    pub fn spawn(&mut self, command: &str) -> Option<u32> {
        let child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            // For the session's restart loops, and for `--exit`.
            .env(PID_VARIABLE, std::process::id().to_string())
            .spawn();
        match child {
            Ok(child) => {
                let pid = child.id();
                self.children.push(child);
                Some(pid)
            }
            Err(err) => {
                crate::say!("cannot start `{command}`: {err}");
                None
            }
        }
    }

    /// Starts `program` itself, without a shell in between.
    pub fn spawn_program(
        &mut self,
        program: &std::path::Path,
        arguments: &[String],
    ) -> Option<u32> {
        let child = Command::new(program)
            .args(arguments)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env(PID_VARIABLE, std::process::id().to_string())
            .spawn();
        match child {
            Ok(child) => {
                let pid = child.id();
                self.children.push(child);
                Some(pid)
            }
            Err(err) => {
                crate::say!("cannot start {}: {err}", program.display());
                None
            }
        }
    }

    /// The programs we started that are still running.
    pub fn child_pids(&self) -> Vec<u32> {
        self.children.iter().map(Child::id).collect()
    }

    /// Collects the programs that have exited, so they do not stay around
    /// as zombies.
    fn reap_children(&mut self) {
        self.children
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
    }

    /// What to draw at the pointer right now.
    pub fn cursor(&self) -> CursorImageStatus {
        if !self.pointer_seen {
            return CursorImageStatus::Hidden;
        }
        if let Some(Drag::Resize { edges, .. }) = &self.drag {
            return CursorImageStatus::Named(resize_cursor(*edges));
        }
        if let Some(icon) = self.frame_cursor {
            return CursorImageStatus::Named(icon);
        }
        // Over the desktop or a frame nobody sets a cursor, and whatever the
        // last client chose must not linger.
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if self.drag.is_some() || pointer.current_focus().is_none() {
            return CursorImageStatus::default_named();
        }
        match &self.client_cursor {
            CursorImageStatus::Surface(surface) if !surface.is_alive() => {
                CursorImageStatus::default_named()
            }
            other => other.clone(),
        }
    }

    /// Drops any hover or drag that involves `window`.
    pub fn forget_pointer_state(&mut self, window: &Window) {
        if self
            .hovered_button
            .as_ref()
            .is_some_and(|(w, _)| w == window)
        {
            self.hovered_button = None;
        }
        let dragged = match &self.drag {
            Some(Drag::Move { window: w, .. })
            | Some(Drag::Resize { window: w, .. })
            | Some(Drag::Press { window: w, .. }) => w == window,
            None => false,
        };
        if dragged {
            self.drag = None;
        }
    }

    /// Gives the keyboard to whoever should have it now: a layer surface
    /// that demands it (the launcher), else a layer surface the user clicked,
    /// else the focused window.
    pub fn update_keyboard_focus(&mut self) {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        if self.is_locked() {
            // Only the lock screen, or nobody until it has a surface.
            let target = self.lock_focus();
            if keyboard.current_focus() != target {
                keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
            }
            return;
        }
        // An open menu holds the keyboard until it is dismissed.
        if keyboard.is_grabbed() {
            return;
        }
        // An on-demand layer keeps the keyboard only while it still exists
        // and still wants it.
        let layer_focus_valid = self.layer_focus.as_ref().is_some_and(|surface| {
            surface.is_alive()
                && self.output_for_layer(surface).is_some_and(|output| {
                    layer_map_for_output(&output)
                        .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                        .is_some_and(|layer| layer.can_receive_keyboard_focus())
                })
        });
        if !layer_focus_valid {
            self.layer_focus = None;
        }
        let target = self
            .exclusive_layer()
            .or_else(|| self.layer_focus.clone())
            .or_else(|| {
                let toplevel = self.focus_stack.current()?.toplevel()?;
                Some(toplevel.wl_surface().clone())
            });

        if keyboard.current_focus() != target {
            keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
        }
    }

    /// Tells idle watchers such as swayidle that the user did something.
    pub fn note_activity(&mut self) {
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
        // The cursor moved, a button lit up, a binding did something.
        self.request_redraw();
    }

    /// The front-most layer surface asking for exclusive keyboard focus.
    /// Only the layers above windows may do that.
    fn exclusive_layer(&self) -> Option<WlSurface> {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        [Layer::Overlay, Layer::Top].into_iter().find_map(|kind| {
            outputs.iter().find_map(|output| {
                layer_map_for_output(output)
                    .layers_on(kind)
                    .rev()
                    .find(|layer| {
                        layer.cached_state().keyboard_interactivity
                            == KeyboardInteractivity::Exclusive
                            && has_buffer(layer.wl_surface())
                    })
                    .map(|layer| layer.wl_surface().clone())
            })
        })
    }
}

/// Whether a surface is mapped, that is, has content to show.
pub fn has_buffer(surface: &WlSurface) -> bool {
    with_renderer_surface_state(surface, |state| state.buffer().is_some()).unwrap_or(false)
}

pub fn set_activated(window: &Window, active: bool) {
    window.set_activated(active);
    if let Some(toplevel) = window.toplevel() {
        // Before the first configure the state simply rides along with it.
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// The client came in through a sandbox's security context.
    pub sandboxed: bool,
    /// The process that connected, as the kernel reported it then.
    pub pid: Option<i32>,
    /// A handle on that very process, where the kernel gives one. A pid
    /// can be handed to another process later; this cannot.
    pub process: Option<OwnedFd>,
}

impl ClientState {
    /// What is known of the client at the other end of `stream`.
    pub fn of(stream: &UnixStream, sandboxed: bool) -> Self {
        let pid = smithay::reexports::rustix::net::sockopt::socket_peercred(stream)
            .ok()
            .map(|credentials| credentials.pid.as_raw_nonzero().get());
        Self {
            sandboxed,
            pid,
            process: peer_pidfd(stream),
            ..Self::default()
        }
    }

    /// Whether the process that connected still exists. Without a handle
    /// on it there is only the pid to go by, and the answer is yes.
    pub fn still_running(&self) -> bool {
        let Some(process) = &self.process else {
            return true;
        };
        // The kernel shows the pid behind a pidfd, and -1 once the
        // process is gone.
        let info = format!("/proc/self/fdinfo/{}", process.as_raw_fd());
        std::fs::read_to_string(info).is_ok_and(|info| {
            info.lines()
                .find_map(|line| line.strip_prefix("Pid:"))
                .and_then(|pid| pid.trim().parse::<i32>().ok())
                .is_some_and(|pid| Some(pid) == self.pid)
        })
    }
}

/// A pidfd for the process at the other end of `stream`, from
/// SO_PEERPIDFD (Linux 6.5 and later).
fn peer_pidfd(stream: &UnixStream) -> Option<OwnedFd> {
    const SO_PEERPIDFD: libc::c_int = 77;
    let mut fd: libc::c_int = -1;
    let mut size = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // Safety: the kernel writes one int, which is what there is room for.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            SO_PEERPIDFD,
            (&raw mut fd).cast(),
            &raw mut size,
        )
    };
    // Safety: on success the descriptor is new and nobody else's.
    (result == 0 && fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // The clipboard follows the keyboard.
        let client = focused.and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.client_cursor = image;
        self.request_redraw();
    }
}

// The cursor-shape protocol also covers tablet tools, which we do not have.
impl TabletSeatHandler for State {}

impl IdleNotifierHandler for State {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier_state
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}

impl OutputHandler for State {}

delegate_shm!(State);
delegate_seat!(State);
// As `delegate_idle_notify!` would, except that sandboxed clients do not
// get to watch when the user is at the machine.
impl GlobalDispatch<ExtIdleNotifierV1, ()> for State {
    fn bind(
        state: &mut Self,
        handle: &DisplayHandle,
        client: &Client,
        resource: New<ExtIdleNotifierV1>,
        global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        <IdleNotifierState<Self> as GlobalDispatch<ExtIdleNotifierV1, (), Self>>::bind(
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
delegate_dispatch!(State: [ExtIdleNotifierV1: ()] => IdleNotifierState<State>);
delegate_dispatch!(State: [ExtIdleNotificationV1: IdleNotificationUserData] => IdleNotifierState<State>);
delegate_cursor_shape!(State);
delegate_data_device!(State);
delegate_output!(State);
