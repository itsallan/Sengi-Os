//! Compositor state shared by every backend.

use std::{
    error::Error,
    ffi::OsString,
    process::{Child, Command},
    sync::Arc,
    time::Instant,
};

use sengi_tokens::{Rgba, Tokens};
use smithay::{
    backend::renderer::{utils::with_renderer_surface_state, Color32F},
    delegate_cursor_shape, delegate_data_device, delegate_idle_notify, delegate_output,
    delegate_seat, delegate_shm,
    desktop::{layer_map_for_output, PopupManager, Space, Window, WindowSurfaceType},
    input::{
        pointer::{CursorIcon, CursorImageStatus},
        Seat, SeatHandler, SeatState,
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{generic::Generic, EventLoop, Interest, LoopSignal, Mode, PostAction},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
            Display, DisplayHandle, Resource,
        },
    },
    utils::{Physical, Size, Transform, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorState},
        cursor_shape::CursorShapeManagerState,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
        output::{OutputHandler, OutputManagerState},
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
    keys::Swallowed,
    layout::FocusStack,
    lock::Lock,
    text::Font,
};

/// Refresh rate reported for outputs that have no real one, in mHz.
pub const REFRESH_MHZ: i32 = 60_000;

pub struct State {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
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
    /// The button whose release ends `drag`.
    pub drag_button: u32,
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
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    // Held so the xdg-output global stays alive.
    _output_manager_state: OutputManagerState,

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
        let session_lock_state = SessionLockManagerState::new::<Self, _>(&dh, |_| true);
        let idle_notifier_state = IdleNotifierState::<Self>::new(&dh, event_loop.handle());
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);
        let data_device_state = DataDeviceState::new::<Self>(&dh);

        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 300, 30)?;
        seat.add_pointer();

        let socket = ListeningSocketSource::new_auto()?;
        let socket_name = socket.socket_name().to_os_string();
        let handle = event_loop.handle();
        handle.insert_source(socket, |stream, _, state| {
            if let Err(err) = state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                eprintln!("sengi-compositor: rejected a client: {err}");
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
            eprintln!(
                "sengi-compositor: {} is not installed, title bars will have no text",
                theme.font_family
            );
        }

        let cursors = Cursors::new(&theme);

        Ok(Self {
            start_time: Instant::now(),
            socket_name,
            display_handle: dh,
            loop_signal: event_loop.get_signal(),
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
            drag_button: 0,
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
            shm_state,
            seat_state,
            data_device_state,
            _output_manager_state: output_manager_state,
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
            let _ = state.display_handle.flush_clients();
        })?;

        if self.failed {
            return Err("rendering failed".into());
        }
        Ok(())
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

    /// Announces an output of `size` pixels to clients and puts it at the
    /// origin of the desktop.
    pub fn add_output(
        &mut self,
        name: &str,
        model: &str,
        size: Size<i32, Physical>,
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
        // The global lives as long as the compositor does.
        let _global = output.create_global::<Self>(&self.display_handle);
        let mode = OutputMode {
            size,
            refresh: REFRESH_MHZ,
        };
        output.change_current_state(Some(mode), Some(transform), None, Some((0, 0).into()));
        output.set_preferred(mode);
        self.space.map_output(&output, (0, 0));
        output
    }

    /// Applies a new size to `output`, for backends whose window can resize.
    #[cfg_attr(not(feature = "winit"), allow(dead_code))]
    pub fn resize_output(&mut self, output: &Output, size: Size<i32, Physical>) {
        let mode = OutputMode {
            size,
            refresh: REFRESH_MHZ,
        };
        output.change_current_state(Some(mode), None, None, None);
        output.set_preferred(mode);
        layer_map_for_output(output).arrange();
        self.usable_area_changed();
    }

    /// Raises `window`, marks it active and gives it the keyboard.
    pub fn focus_window(&mut self, window: &Window) {
        if let Some(old) = self.focus_stack.current().cloned() {
            if old != *window {
                set_activated(&old, false);
            }
        }
        self.focus_stack.focus(window.clone());
        self.space.raise_element(window, true);
        set_activated(window, true);

        // Choosing a window takes the keyboard back from an on-demand layer.
        self.layer_focus = None;
        self.update_keyboard_focus();
    }

    /// Forgets `window` and hands focus to the one used before it.
    pub fn remove_window(&mut self, window: &Window) {
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
    pub fn spawn(&mut self, command: &str) {
        let child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .spawn();
        match child {
            Ok(child) => self.children.push(child),
            Err(err) => eprintln!("sengi-compositor: cannot start `{command}`: {err}"),
        }
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
            let target = self.lock_surface();
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
                && self.output().is_some_and(|output| {
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
    }

    /// The front-most layer surface asking for exclusive keyboard focus.
    /// Only the layers above windows may do that.
    fn exclusive_layer(&self) -> Option<WlSurface> {
        let output = self.output()?;
        let map = layer_map_for_output(&output);
        [Layer::Overlay, Layer::Top].into_iter().find_map(|kind| {
            map.layers_on(kind)
                .rev()
                .find(|layer| {
                    layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                        && has_buffer(layer.wl_surface())
                })
                .map(|layer| layer.wl_surface().clone())
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
delegate_idle_notify!(State);
delegate_cursor_shape!(State);
delegate_data_device!(State);
delegate_output!(State);
