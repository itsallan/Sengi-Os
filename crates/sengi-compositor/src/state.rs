//! Compositor state shared by every backend.

use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    ffi::OsString,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::PathBuf,
    process::{Child, Command},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
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
        keyboard::XkbConfig,
        pointer::{CursorIcon, CursorImageStatus},
        Seat, SeatHandler, SeatState,
    },
    output::{Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            generic::Generic,
            ping::Ping,
            signals::{Signal, Signals},
            timer::{TimeoutAction, Timer},
            EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, RegistrationToken,
        },
        wayland_protocols::ext::idle_notify::v1::server::{
            ext_idle_notification_v1::ExtIdleNotificationV1,
            ext_idle_notifier_v1::ExtIdleNotifierV1,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            delegate_dispatch,
            protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
            Client, DataInit, Display, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorState},
        cursor_shape::CursorShapeManagerState,
        dmabuf::DmabufState,
        fractional_scale::FractionalScaleManagerState,
        idle_inhibit::IdleInhibitManagerState,
        idle_notify::{IdleNotificationUserData, IdleNotifierHandler, IdleNotifierState},
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::PointerConstraintsState,
        relative_pointer::RelativePointerManagerState,
        seat::WaylandFocus,
        security_context::SecurityContextState,
        selection::{
            data_device::{set_data_device_focus, DataDeviceHandler, DataDeviceState},
            primary_selection::{set_primary_focus, PrimarySelectionState},
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
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
        xwayland_shell::XWaylandShellState,
    },
};

use crate::{
    config::Config,
    cursor::resize_cursor,
    decoration::Theme,
    foreign_toplevel::ForeignToplevelState,
    frame::Button,
    input::Drag,
    keys::{self, Swallowed},
    layout::{FocusStack, Rect},
    lock::Lock,
    output_management::OutputManagementState,
    output_power::OutputPowerState,
    output_store::StoredLayout,
    outputs::{Head, HeadPlan, Painter},
    render, security, window, x11,
};

/// Environment variable through which started programs learn our pid.
pub const PID_VARIABLE: &str = "SENGI_COMPOSITOR_PID";

pub type ScreenshotHook = Box<dyn FnMut(&mut State)>;
pub type DmabufImport = Box<dyn FnMut(&Dmabuf) -> bool>;

/// Refresh rate reported for outputs that have no real one, in mHz.
pub const REFRESH_MHZ: i32 = 60_000;

/// How many connections to the control socket may be open without
/// having asked for anything.
const MAX_CONTROL_WAITING: usize = 16;

/// How many requests to lock may be waiting for their answer.
pub const MAX_LOCK_ASKERS: usize = 32;

/// Sets the pointing devices to what the settings say.
pub type ConfigureInputs = Box<dyn FnMut(&Config)>;
/// Says what each pointing device is set to.
pub type DescribeInputs = Box<dyn Fn() -> Vec<String>>;
/// Changes what a backend has to for a new arrangement of the outputs, or
/// says why it cannot.
pub type OutputBackend = Box<dyn FnMut(&mut State, &[HeadPlan]) -> Result<(), String>>;
/// A screen is plugged in (with its size) or pulled out (by its name).
pub type TestOutputs = Box<dyn FnMut(&mut State, TestOutput) -> Result<String, String>>;

pub enum TestOutput {
    Plug {
        size: (i32, i32),
        panel: bool,
        name: Option<String>,
    },
    Unplug {
        name: String,
    },
}

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
    /// What Settings set, as last read.
    pub config: Config,
    /// Sets the pointing devices to what the settings say, where the
    /// backend has devices.
    pub configure_inputs: Option<ConfigureInputs>,
    /// One line on each pointing device, for the SIGUSR1 report.
    pub describe_inputs: Option<DescribeInputs>,
    pub output_power: OutputPowerState,
    /// Running as the login screen's compositor, see `become_login_screen`.
    pub login_screen: bool,
    /// The program of `--session`: when it ends, so do we.
    session_child: Option<u32>,
    /// How the `--session` program ended, once it has.
    session_status: Option<std::process::ExitStatus>,
    /// Buttons whose press lit a dark screen and went no further; their
    /// release goes no further either.
    pub wake_buttons: Vec<u32>,
    /// Buttons whose press an open card took; their release is not sent to
    /// a client either, as the client never saw the press.
    pub overlay_buttons: Vec<u32>,
    /// Draws an output into memory for a screenshot.
    pub capture: Option<crate::screencopy::Capture>,
    _screencopy_state: crate::screencopy::ScreencopyState,
    pub primary_selection_state: PrimarySelectionState,
    pub xdg_activation_state: XdgActivationState,
    // Held so their globals stay.
    _viewporter_state: ViewporterState,
    _fractional_scale_state: FractionalScaleManagerState,
    _pointer_constraints_state: PointerConstraintsState,
    _relative_pointer_state: RelativePointerManagerState,
    /// What every output is scaled by: `--scale`.
    pub output_scale: f64,
    /// The program that has the keyboard.
    pub keyboard_client: Option<ClientId>,
    /// The surface whose pointer constraint is in force.
    pub constrained: Option<WlSurface>,
    /// What a drag carries along under the pointer.
    pub drag_icon: Option<WlSurface>,
    /// The surfaces that ask for the session not to go idle.
    pub idle_inhibitors: Vec<WlSurface>,
    _idle_inhibit_state: IdleInhibitManagerState,
    /// The locker's program file, if there is one to start.
    pub locker_program: Option<PathBuf>,
    /// Set once the connection of the locker we started last has closed.
    pub locker_gone: Option<Arc<AtomicBool>>,
    /// Those who asked for the lock and have not been answered yet.
    pub lock_askers: Vec<UnixStream>,
    /// Connections to the control socket that have not said yet what
    /// they want, oldest first.
    control_waiting: VecDeque<(u64, RegistrationToken)>,
    control_count: u64,
    /// How many lockers have been started for them so far.
    pub lock_tries: u32,
    /// Where `--lock` is asked for; the file goes when we do.
    _control: ControlSocket,
    /// The file that tells compositor.sh we are up; it goes with us.
    ready_marker: Option<ReadyMarker>,
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
    pub xwayland_shell_state: XWaylandShellState,
    /// Xwayland, while it is wanted: see `x11.rs`.
    pub x11: Option<x11::Session>,
    /// The display number Xwayland last ran on, kept so that it comes back
    /// on the same one and the DISPLAY of programs still running stays right.
    pub x11_display: Option<u32>,
    /// Counts the Xwayland instances, so that a late timer can tell whether
    /// it is for the one that runs now.
    pub x11_generation: u64,
    /// Xwayland did not come up once; it is not tried again this session.
    pub x11_gave_up: bool,
    /// Tries a client's dmabuf on the renderer; set by a backend that
    /// offers linux-dmabuf.
    pub dmabuf_import: Option<DmabufImport>,
    /// The linux-dmabuf global, where one is offered, so that it can be
    /// withdrawn if the renderer is given up.
    pub dmabuf_global: Option<smithay::wayland::dmabuf::DmabufGlobal>,
    pub dmabufs_imported: u64,
    /// What Super+L runs, and what is started for a session whose locker
    /// died.
    pub locker_command: String,
    /// Lockers we started ourselves that are still running.
    pub locker_pids: Vec<u32>,
    pub relock_armed: bool,
    pub tokens: Tokens,
    pub theme: Theme,
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
    /// The last press on a title bar: a second one right after maximises.
    pub titlebar_press: Option<(Window, Instant, (i32, i32))>,
    /// The pointer may be over something else than its focus says, and
    /// has to be looked at again even though it did not move.
    pub pointer_stale: bool,
    pub swallowed_keys: Swallowed,
    /// Alt+Tab in progress: the windows in the order they had when it
    /// started, and which one is selected.
    /// The window switcher or a menu, when one is open.
    pub overlay: Option<crate::overlay::Overlay>,
    /// Programs we started and still have to collect when they exit.
    children: Vec<Child>,
    /// What is painted with, for each scale an output is drawn at.
    pub painters: Vec<Painter>,
    /// Whether the pointer is drawn in the picture a screen copy takes,
    /// which a client asks for with overlay_cursor.
    pub draw_cursor: bool,
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
    /// Every screen there is, switched on or not. See `outputs.rs`.
    pub heads: Vec<Head>,
    /// An output's scale changed, so surfaces are told theirs again.
    pub scales_dirty: bool,
    /// Changes what a backend has to change when outputs are rearranged:
    /// modes, switching on and off. None where it cannot.
    pub output_backend: Option<OutputBackend>,
    pub output_management: OutputManagementState,
    /// The layout kept between sessions.
    pub stored_layout: StoredLayout,
    /// The timer that saves it, while a change waits to be saved.
    pub output_save: Option<RegistrationToken>,
    /// When the change that waits to be saved was made.
    pub output_save_since: Option<Instant>,
    /// Plugs and unplugs screens on the headless backend, for tests.
    pub test_outputs: Option<TestOutputs>,

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
        // Only a locker we started gets to see the lock.
        let session_lock_state =
            SessionLockManagerState::new::<Self, _>(&dh, crate::lock::started_as_locker);
        let idle_notifier_state = IdleNotifierState::<Self>::new(&dh, event_loop.handle());
        let idle_inhibit_state = IdleInhibitManagerState::new::<Self>(&dh);
        let output_power = OutputPowerState::new(&dh);
        let layer_shell_state =
            WlrLayerShellState::new_with_filter::<Self, _>(&dh, security::trusted);
        let security_context_state = SecurityContextState::new::<Self, _>(&dh, security::trusted);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        let mut config = Config::load();
        let typing = &mut config.keyboard;
        // Settings may name a layout this system does not have. A keyboard
        // with the default layout is better than no session.
        if seat
            .add_keyboard(xkb_config(typing), typing.repeat_delay, typing.repeat_rate)
            .is_err()
        {
            crate::say!(
                "no keymap for layout `{}`, using the default",
                typing.layout
            );
            // What is in use is the default, and that is what is kept as
            // the keyboard's state: the name that was refused is not.
            typing.layout.clear();
            typing.variant.clear();
            typing.options.clear();
            seat.add_keyboard(Default::default(), typing.repeat_delay, typing.repeat_rate)?;
        }
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
                    crate::say!("{}", state.config.keyboard.describe());
                    crate::say!("{}", state.output_power_status());
                    let inputs = state.describe_inputs.as_ref().map(|describe| describe());
                    for line in inputs.unwrap_or_default() {
                        crate::say!("{line}");
                    }
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
                // Settings has changed something, and says so the way
                // `labwc --reconfigure` does.
                Signal::SIGHUP => state.reload(),
                _ => state.loop_signal.stop(),
            })?;

        let socket = ListeningSocketSource::new_auto()?;
        let socket_name = socket.socket_name().to_os_string();
        let handle = event_loop.handle();
        handle.insert_source(socket, |stream, _, state| {
            let data = ClientState::of(false);
            if let Err(err) = state.display_handle.insert_client(stream, Arc::new(data)) {
                crate::say!("rejected a client: {err}");
            }
        })?;
        // Requests to the compositor itself come in beside the Wayland
        // socket, in the same directory, which only the user can enter.
        let control = ControlSocket::bind(&socket_name)?;
        handle.insert_source(
            Generic::new(control.listener.try_clone()?, Interest::READ, Mode::Level),
            |_, listener, state| {
                while let Ok((stream, _)) = listener.accept() {
                    state.control_connected(stream);
                }
                Ok(PostAction::Continue)
            },
        )?;
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
        if crate::text::Font::load(&theme.font_family, theme.font_weight, theme.font_px).is_none() {
            crate::say!(
                "{} is not installed, title bars will have no text",
                theme.font_family
            );
        }

        let dh2 = dh.clone();
        let output_management = OutputManagementState::new(&dh);
        let stored_layout = StoredLayout::load();

        let mut state = Self {
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
            config,
            configure_inputs: None,
            describe_inputs: None,
            output_power,
            login_screen: false,
            session_child: None,
            session_status: None,
            wake_buttons: Vec::new(),
            overlay_buttons: Vec::new(),
            capture: None,
            _screencopy_state: crate::screencopy::ScreencopyState::new(&dh2),
            primary_selection_state: PrimarySelectionState::new::<Self>(&dh2),
            xdg_activation_state: XdgActivationState::new::<Self>(&dh2),
            _viewporter_state: ViewporterState::new::<Self>(&dh2),
            _fractional_scale_state: FractionalScaleManagerState::new::<Self>(&dh2),
            _pointer_constraints_state: PointerConstraintsState::new::<Self>(&dh2),
            _relative_pointer_state: RelativePointerManagerState::new::<Self>(&dh2),
            output_scale: 1.0,
            keyboard_client: None,
            constrained: None,
            drag_icon: None,
            idle_inhibitors: Vec::new(),
            _idle_inhibit_state: idle_inhibit_state,
            locker_program: None,
            locker_gone: None,
            lock_askers: Vec::new(),
            control_waiting: VecDeque::new(),
            control_count: 0,
            lock_tries: 0,
            _control: control,
            ready_marker: None,
            switch_vt: None,
            page_flips: 0,
            cursor_plane_frames: 0,
            take_screenshot: None,
            dmabuf_state: DmabufState::new(),
            xwayland_shell_state: XWaylandShellState::new::<Self>(&dh2),
            x11: None,
            x11_display: None,
            x11_generation: 0,
            x11_gave_up: false,
            dmabuf_import: None,
            dmabuf_global: None,
            dmabufs_imported: 0,
            locker_command: keys::DEFAULT_LOCKER.to_string(),
            locker_pids: Vec::new(),
            relock_armed: false,
            tokens,
            theme,
            failed: false,
            space: Space::default(),
            popups: PopupManager::default(),
            pending_windows: Vec::new(),
            minimised: Vec::new(),
            focus_stack: FocusStack::default(),
            layer_focus: None,
            hovered_button: None,
            drag: None,
            titlebar_press: None,
            pointer_stale: false,
            swallowed_keys: Swallowed::default(),
            overlay: None,
            children: Vec::new(),
            painters: Vec::new(),
            draw_cursor: true,
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
            heads: Vec::new(),
            scales_dirty: false,
            output_backend: None,
            output_management,
            stored_layout,
            output_save: None,
            output_save_since: None,
            test_outputs: None,
            seat,
        };
        state.refresh_painters();
        Ok(state)
    }

    /// Runs until a backend stops the loop.
    pub fn run(mut self, mut event_loop: EventLoop<'static, Self>) -> Result<(), Box<dyn Error>> {
        event_loop.run(None, &mut self, |state| {
            state.space.refresh();
            state.popups.cleanup();
            state.sync_pointer_focus();
            state.refresh_foreign_toplevels();
            state.refresh_idle_inhibit();
            state.update_preferred_scales();
            state.reap_children();
            state.forget_gone_security_contexts();
            state.watch_locker();
            let _ = state.display_handle.flush_clients();
        })?;

        self.flush_output_layout();
        if self.failed {
            return Err("rendering failed".into());
        }
        // The compositor ends with the program it ran, so that whoever
        // started us can tell a session that failed from one that ended.
        match self.session_status {
            Some(status) if !status.success() => {
                Err(format!("the session program ended with {status}").into())
            }
            _ => Ok(()),
        }
    }

    pub fn set_redraw_ping(&mut self, ping: Ping) {
        self.redraw_ping = Some(ping);
    }

    /// Asks for a frame. Everything that changes what is on screen calls
    /// this; nothing is drawn otherwise.
    pub fn request_redraw(&mut self) {
        // Whatever moved a window may have moved it to an output of another
        // scale: only matters where there are two to choose from.
        if self.painters.len() > 1 {
            self.scales_dirty = true;
        }
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

    /// Applies a new size to `output`, for backends whose window can resize.
    #[cfg(feature = "winit")]
    pub fn resize_output(
        &mut self,
        output: &Output,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
    ) {
        let mode = smithay::output::Mode {
            size,
            refresh: REFRESH_MHZ,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);
        layer_map_for_output(output).arrange();
        self.configure_lock_surfaces();
        self.refit_fullscreen();
        self.usable_area_changed();
        self.output_management_changed();
        self.request_redraw();
    }

    /// Raises `window`, marks it active and gives it the keyboard.
    pub fn focus_window(&mut self, window: &Window) {
        // A menu of an X11 toolkit is never focused by the compositor.
        if window::is_override(window) {
            return;
        }
        if let Some(old) = self.focus_stack.current().cloned() {
            if old != *window {
                set_activated(&old, false);
            }
        }
        self.request_redraw();
        self.focus_stack.focus(window.clone());
        self.space.raise_element(window, true);
        // Menus stay over the window they belong to.
        let menus: Vec<Window> = self
            .space
            .elements()
            .filter(|other| window::is_override(other))
            .cloned()
            .collect();
        for menu in &menus {
            self.space.raise_element(menu, false);
        }
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
        self.forget_overlay_window(window);
        if !self.focus_stack.remove(window) {
            return;
        }
        match self.focus_stack.current().cloned() {
            Some(next) => self.focus_window(&next),
            None => self.update_keyboard_focus(),
        }
    }

    /// Runs `command` with the shell as a client of this compositor. It
    /// gets the DISPLAY of Xwayland at once, whether Xwayland is ready yet or
    /// not: an X11 client waits on the socket, nothing else waits for it.
    /// A program started after Xwayland has exited brings it back.
    pub fn spawn(&mut self, command: &str) -> Option<u32> {
        self.start_xwayland();
        self.spawn_plain(command)
    }

    /// Gives `command` the DISPLAY of Xwayland when it is running or starting.
    /// Without it the DISPLAY it inherited would name some other server, so
    /// it is removed.
    fn give_display(&self, command: &mut Command) {
        match self.display_name() {
            Some(display) => {
                command.env("DISPLAY", display);
            }
            None => {
                command.env_remove("DISPLAY");
            }
        }
    }

    pub(crate) fn spawn_plain(&mut self, command: &str) -> Option<u32> {
        let mut process = Command::new("sh");
        process
            .arg("-c")
            .arg(command)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            // For the session's restart loops, and for `--exit`.
            .env(PID_VARIABLE, std::process::id().to_string());
        self.give_display(&mut process);
        let child = process.spawn();
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

    /// Somebody connected to the control socket. What they ask is read
    /// when it has arrived, so one who connects and says nothing holds
    /// nothing up; and only so many may be silent at once, the oldest
    /// making room for the newest.
    fn control_connected(&mut self, stream: UnixStream) {
        if stream.set_nonblocking(true).is_err() {
            return;
        }
        while self.control_waiting.len() >= MAX_CONTROL_WAITING {
            if let Some((_, oldest)) = self.control_waiting.pop_front() {
                self.loop_handle.remove(oldest);
            }
        }
        self.control_count += 1;
        let id = self.control_count;
        let source = Generic::new(stream, Interest::READ, Mode::Level);
        let inserted = self
            .loop_handle
            .insert_source(source, move |_, stream, state| {
                state.control_waiting.retain(|(waiting, _)| *waiting != id);
                let mut word = [0u8; 96];
                let mut from: &UnixStream = stream;
                let read = from.read(&mut word).unwrap_or(0);
                let line = word[..read].trim_ascii();
                if line == b"lock" {
                    if let Ok(asker) = stream.try_clone() {
                        state.lock_requested(asker);
                    }
                } else if cfg!(feature = "test-hooks") {
                    // Plugs and unplugs screens and gives input, for the tests.
                    if let Ok(mut asker) = stream.try_clone() {
                        let answer = state.test_request(&String::from_utf8_lossy(line));
                        let _ = std::io::Write::write_all(&mut asker, answer.as_bytes());
                    }
                }
                Ok(PostAction::Remove)
            });
        if let Ok(token) = inserted {
            self.control_waiting.push_back((id, token));
        }
    }

    /// Starts `program` itself, without a shell in between. With `socket`
    /// the program gets that as its connection to us, the way
    /// `WAYLAND_SOCKET` hands one over.
    pub fn spawn_program(
        &mut self,
        program: &std::path::Path,
        arguments: &[String],
        socket: Option<UnixStream>,
    ) -> Option<u32> {
        let mut command = Command::new(program);
        command
            // Its own process group, so that ending it ends what it started.
            .process_group(0)
            .args(arguments)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env(PID_VARIABLE, std::process::id().to_string());
        self.give_display(&mut command);
        if let Some(socket) = &socket {
            let fd = socket.as_raw_fd();
            command.env("WAYLAND_SOCKET", fd.to_string());
            // SAFETY: runs in the child between fork and exec and makes one
            // system call, on a descriptor the child has a copy of.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = command.spawn();
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

    /// Ends a program we started, and the group it leads.
    pub fn end_child(&mut self, pid: u32) {
        // Only a child still in the list is signalled. One that has been
        // collected may have had its pid reused by now.
        if let Some(child) = self.children.iter_mut().find(|child| child.id() == pid) {
            // SAFETY: kill takes two integers. The child is not reaped, so
            // its pid is still its own and is the id of the group it leads,
            // which it made when it was started (the programs it started
            // are in that group too).
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
            let _ = child.kill();
        }
    }

    /// The programs we started that are still running.
    pub fn child_pids(&self) -> Vec<u32> {
        self.children.iter().map(Child::id).collect()
    }

    /// Collects the programs that have exited, so they do not stay around
    /// as zombies.
    fn reap_children(&mut self) {
        let session = self.session_child;
        let mut session_ended = None;
        self.children.retain_mut(|child| {
            let status = child.try_wait().ok().flatten();
            if Some(child.id()) == session {
                session_ended = session_ended.or(status);
            }
            status.is_none()
        });
        if let Some(status) = session_ended {
            self.session_child = None;
            self.session_status = Some(status);
            self.loop_signal.stop();
            self.loop_signal.wakeup();
        }
    }

    /// Says the compositor is up: a screen is lit and the session program or
    /// the login screen has been started. Not if that failed.
    pub fn mark_ready(&mut self) {
        if self.failed || self.ready_marker.is_some() {
            return;
        }
        match ReadyMarker::create() {
            Ok(marker) => self.ready_marker = Some(marker),
            Err(err) => crate::say!("cannot write the ready file: {err}"),
        }
    }

    /// `--session`: runs `command` and ends with it, the way a login
    /// screen's compositor has to for the login to go on.
    pub fn run_session(&mut self, command: &str) {
        self.start_xwayland();
        let pid = self.spawn_plain(command);
        self.session_started(pid);
    }

    /// The session program has been started, or could not be.
    pub fn session_started(&mut self, pid: Option<u32>) {
        self.session_child = pid;
        if pid.is_none() {
            self.failed = true;
            self.loop_signal.stop();
        }
    }

    /// `--greeter`: the compositor the login screen is shown in. Nobody is
    /// logged in, so there is nothing a key binding may start, nothing to
    /// lock, and nobody's settings to follow: the keyboard is the one the
    /// environment names, where the installer put the layout that was
    /// chosen. Terminals can still be switched.
    pub fn become_login_screen(&mut self) {
        self.login_screen = true;
        self.locker_program = None;
        let config = Config::from_environment();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        if keyboard.set_xkb_config(self, XkbConfig::default()).is_err() {
            crate::say!("no keymap for the keyboard the environment names");
        }
        keyboard.change_repeat_info(config.keyboard.repeat_rate, config.keyboard.repeat_delay);
        self.config = config;
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
        if self
            .titlebar_press
            .as_ref()
            .is_some_and(|(w, _, _)| w == window)
        {
            self.titlebar_press = None;
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
                let window = self.focus_stack.current()?;
                window.wl_surface().map(|surface| surface.into_owned())
            });

        if keyboard.current_focus() != target {
            keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
        }
    }

    /// Sets the scale that outputs with no scale of their own are drawn
    /// at. Frames, title text and the cursor are then painted in the
    /// screen's own pixels.
    pub fn set_output_scale(&mut self, scale: f64) {
        self.output_scale = scale;
        // No redraw here: this runs before any output exists, and a frame
        // drawn then would show nothing. The backends ask once they have one.
        self.refresh_painters();
    }

    /// Reads the settings again and acts on what changed: SIGHUP.
    pub fn reload(&mut self) {
        // The login screen follows nobody's settings.
        if self.login_screen {
            return;
        }
        let mut config = Config::load();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let (old, new) = (&self.config.keyboard, &config.keyboard);
        let same_keys =
            (&old.layout, &old.variant, &old.options) == (&new.layout, &new.variant, &new.options);
        if !same_keys && keyboard.set_xkb_config(self, xkb_config(new)).is_err() {
            crate::say!(
                "no keymap for layout `{}`, keeping the one in use",
                new.layout
            );
            let kept = self.config.keyboard.clone();
            config.keyboard.layout = kept.layout;
            config.keyboard.variant = kept.variant;
            config.keyboard.options = kept.options;
        }
        keyboard.change_repeat_info(config.keyboard.repeat_rate, config.keyboard.repeat_delay);
        if let Some(configure) = self.configure_inputs.as_mut() {
            configure(&config);
        }
        self.config = config;
        crate::say!("settings read again");
        self.reload_output_layout();
    }

    /// Tells idle watchers such as swayidle that the user did something,
    /// and lights whatever screen was switched off. Returns whether one
    /// was: the key or movement that brings a dark screen back was made
    /// without seeing anything, and is not passed on to any program.
    pub fn note_activity(&mut self) -> bool {
        let seat = self.seat.clone();
        self.idle_notifier_state.notify_activity(&seat);
        let woke = self.any_output_off();
        self.wake_outputs();
        // The cursor moved, a button lit up, a binding did something.
        self.request_redraw();
        woke
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

/// The keymap to ask XKB for. A named layout comes with its variants and
/// options as they are, empty ones too, so that nothing is filled in from
/// the `XKB_DEFAULT_*` variables. No layout leaves all of it to those.
fn xkb_config(keys: &crate::config::Keyboard) -> XkbConfig<'_> {
    if keys.layout.is_empty() {
        return XkbConfig::default();
    }
    XkbConfig {
        layout: &keys.layout,
        variant: &keys.variant,
        options: Some(keys.options.clone()),
        ..XkbConfig::default()
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// The client came in through a sandbox's security context.
    pub sandboxed: bool,
    /// Set for a locker we started on a connection we made for it, see
    /// `State::spawn_locker`. The flag inside says that connection has
    /// closed.
    pub locker: Option<Arc<AtomicBool>>,
}

impl ClientState {
    /// A client that connected by itself.
    pub fn of(sandboxed: bool) -> Self {
        Self {
            sandboxed,
            ..Self::default()
        }
    }
}

/// The socket `sengi-compositor --lock` talks to: the Wayland socket's
/// name with `.control` after it.
struct ControlSocket {
    listener: UnixListener,
    path: PathBuf,
}

impl ControlSocket {
    fn bind(socket_name: &std::ffi::OsStr) -> Result<Self, Box<dyn Error>> {
        let path = control_path(
            std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
            Some(socket_name),
        )
        .ok_or("XDG_RUNTIME_DIR is not set")?;
        // One left behind by a compositor that was killed.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        // The directory already keeps other users out. This says so on
        // the socket too, wherever it has been put.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self { listener, path })
    }
}

impl Drop for ControlSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What the file that says the compositor is up is called, in
/// `XDG_RUNTIME_DIR`. No Wayland socket is named like that.
pub const READY_FILE: &str = "sengi-compositor.ready";

/// The file `compositor.sh` waits for: it exists from the moment a screen is
/// lit and the session program runs until the compositor is gone.
struct ReadyMarker {
    path: PathBuf,
}

impl ReadyMarker {
    fn create() -> Result<Self, Box<dyn Error>> {
        use std::os::unix::fs::OpenOptionsExt;
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|dir| !dir.is_empty())
            .ok_or("XDG_RUNTIME_DIR is not set")?;
        let path = PathBuf::from(dir).join(READY_FILE);
        let _ = std::fs::remove_file(&path);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        writeln!(file, "{}", std::process::id())?;
        Ok(Self { path })
    }
}

impl Drop for ReadyMarker {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Where the control socket of the compositor at `display` is, given the
/// values of `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY`.
pub fn control_path(
    runtime_dir: Option<&std::ffi::OsStr>,
    display: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let display = display.filter(|name| !name.is_empty())?;
    let mut name = display.to_os_string();
    name.push(".control");
    let name = PathBuf::from(name);
    if name.is_absolute() {
        return Some(name);
    }
    let dir = runtime_dir.filter(|dir| !dir.is_empty())?;
    Some(PathBuf::from(dir).join(name))
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        if let Some(gone) = &self.locker {
            gone.store(true, Ordering::Relaxed);
        }
    }
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
        set_data_device_focus(&self.display_handle, seat, client.clone());
        set_primary_focus(&self.display_handle, seat, client.clone());
        // And so does the right to hold the pointer.
        self.keyboard_client = client.map(|client| client.id());
        self.settle_pointer_constraint();
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
