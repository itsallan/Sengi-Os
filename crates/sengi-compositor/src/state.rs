//! Compositor state shared by every backend.

use std::{error::Error, ffi::OsString, sync::Arc, time::Instant};

use sengi_tokens::Tokens;
use smithay::{
    backend::renderer::Color32F,
    delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    desktop::{layer_map_for_output, PopupManager, Space, Window},
    input::{Seat, SeatHandler, SeatState},
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
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            SelectionHandler,
        },
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState},
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};

use crate::layout::FocusStack;

/// Refresh rate reported for outputs that have no real one, in mHz.
pub const REFRESH_MHZ: i32 = 60_000;

pub struct State {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub loop_signal: LoopSignal,
    pub tokens: Tokens,
    /// Set when the backend gave up, so the process exits non-zero.
    pub failed: bool,

    pub space: Space<Window>,
    pub popups: PopupManager,
    /// Toplevels that exist but have not committed a buffer yet. They are
    /// placed and mapped once their size is known.
    pub pending_windows: Vec<Window>,
    pub focus_stack: FocusStack<Window>,
    /// Counts mapped windows so new ones cascade.
    pub placed: usize,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
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

        Ok(Self {
            start_time: Instant::now(),
            socket_name,
            display_handle: dh,
            loop_signal: event_loop.get_signal(),
            tokens: Tokens::builtin(),
            failed: false,
            space: Space::default(),
            popups: PopupManager::default(),
            pending_windows: Vec::new(),
            focus_stack: FocusStack::default(),
            placed: 0,
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
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
            let _ = state.display_handle.flush_clients();
        })?;

        if self.failed {
            return Err("rendering failed".into());
        }
        Ok(())
    }

    /// The colour behind all windows.
    pub fn desktop_color(&self) -> Color32F {
        let c = self
            .tokens
            .color("color.desktop")
            .expect("tokens define color.desktop");
        Color32F::new(
            f32::from(c.r) / 255.0,
            f32::from(c.g) / 255.0,
            f32::from(c.b) / 255.0,
            1.0,
        )
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

        let surface = window.toplevel().map(|t| t.wl_surface().clone());
        self.set_keyboard_focus(surface);
    }

    /// Forgets `window` and hands focus to the one used before it.
    pub fn remove_window(&mut self, window: &Window) {
        self.pending_windows.retain(|w| w != window);
        self.space.unmap_elem(window);
        if !self.focus_stack.remove(window) {
            return;
        }
        match self.focus_stack.current().cloned() {
            Some(next) => self.focus_window(&next),
            None => self.set_keyboard_focus(None),
        }
    }

    fn set_keyboard_focus(&mut self, surface: Option<WlSurface>) {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
    }
}

fn set_activated(window: &Window, active: bool) {
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
delegate_data_device!(State);
delegate_output!(State);
