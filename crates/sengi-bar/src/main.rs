//! sengi-bar, the Sengi OS top bar.
//!
//! The bar is a Slint window drawn with the software renderer into a
//! wlr-layer-shell surface anchored to the top of the screen. Slint has no
//! layer-shell backend of its own, so this file is that backend: it owns the
//! Wayland connection, feeds pointer events to Slint and copies rendered
//! frames into shared-memory buffers.

mod status;

use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, PlatformError, Timer, TimerMode};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, registry_handlers,
};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

slint::include_modules!();

// Linux input event codes for the mouse buttons.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// How often the network, volume and battery readings are refreshed.
const STATUS_INTERVAL: Duration = Duration::from_secs(5);

struct BarPlatform {
    window: Rc<MinimalSoftwareWindow>,
    started: Instant,
}

impl Platform for BarPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.started.elapsed()
    }
}

struct App {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    layer: LayerSurface,
    pointer: Option<wl_pointer::WlPointer>,

    window: Rc<MinimalSoftwareWindow>,
    pixels: Vec<PremultipliedRgbaColor>,
    /// Logical height from the design tokens.
    height: u32,
    /// Logical width, known once the compositor configures the surface.
    width: u32,
    scale: u32,
    configured: bool,
    closed: bool,
}

impl App {
    fn resize(&mut self) {
        self.window.dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: self.scale as f32,
        });
        self.window.set_size(PhysicalSize::new(
            self.width * self.scale,
            self.height * self.scale,
        ));
        self.layer.wl_surface().set_buffer_scale(self.scale as i32);
    }

    /// Renders a frame if Slint has anything new to show.
    fn draw(&mut self) {
        if !self.configured || self.width == 0 {
            return;
        }
        let width = (self.width * self.scale) as usize;
        let height = (self.height * self.scale) as usize;
        self.pixels.clear();
        self.pixels
            .resize(width * height, PremultipliedRgbaColor::default());

        let pixels = &mut self.pixels;
        let drawn = self.window.draw_if_needed(|renderer| {
            renderer.render(pixels, width);
        });
        if !drawn {
            return;
        }

        let stride = width as i32 * 4;
        let Ok((buffer, canvas)) = self.pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) else {
            eprintln!("sengi-bar: could not allocate a frame buffer");
            return;
        };
        // Argb8888 is stored little-endian, so the bytes run B, G, R, A.
        for (out, pixel) in canvas.chunks_exact_mut(4).zip(self.pixels.iter()) {
            out[0] = pixel.blue;
            out[1] = pixel.green;
            out[2] = pixel.red;
            out[3] = pixel.alpha;
        }

        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, width as i32, height as i32);
        if buffer.attach_to(surface).is_ok() {
            self.layer.commit();
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        let factor = factor.max(1) as u32;
        if factor != self.scale {
            self.scale = factor;
            self.resize();
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.closed = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (width, _) = configure.new_size;
        if width != 0 {
            self.width = width;
        }
        self.configured = true;
        self.resize();
        self.window.request_redraw();
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            let position = LogicalPosition::new(event.position.0 as f32, event.position.1 as f32);
            let button = |code: u32| match code {
                BTN_LEFT => Some(PointerEventButton::Left),
                BTN_RIGHT => Some(PointerEventButton::Right),
                BTN_MIDDLE => Some(PointerEventButton::Middle),
                _ => None,
            };
            let translated = match event.kind {
                PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                    Some(WindowEvent::PointerMoved { position })
                }
                PointerEventKind::Leave { .. } => Some(WindowEvent::PointerExited),
                PointerEventKind::Press { button: code, .. } => {
                    button(code).map(|button| WindowEvent::PointerPressed { position, button })
                }
                PointerEventKind::Release { button: code, .. } => {
                    button(code).map(|button| WindowEvent::PointerReleased { position, button })
                }
                PointerEventKind::Axis { .. } => None,
            };
            if let Some(translated) = translated {
                self.window.dispatch_event(translated);
            }
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        queue: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seats.get_pointer(queue, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_pointer!(App);
delegate_layer!(App);
delegate_registry!(App);

/// Runs a shell command without waiting for it. The extra shell exits at
/// once, so the command is adopted by the session and never left a zombie.
fn spawn(command: &str) {
    let result = Command::new("sh")
        .arg("-c")
        .arg(format!("{command} >/dev/null 2>&1 &"))
        .stdin(Stdio::null())
        .status();
    if let Err(error) = result {
        eprintln!("sengi-bar: could not run {command}: {error}");
    }
}

fn refresh_clock(bar: &Bar) {
    let now = chrono::Local::now();
    bar.set_clock(now.format("%a %e %b  %H:%M").to_string().into());
}

fn refresh_status(bar: &Bar) {
    let network = status::network(&status::read_interfaces(Path::new("/sys/class/net")));
    bar.set_network(network.label().into());
    bar.set_online(network != status::Network::Offline);

    match status::read_volume() {
        Some(volume) => {
            bar.set_volume(volume.label().into());
            bar.set_muted(volume.muted);
        }
        None => bar.set_volume("".into()),
    }

    match status::read_battery(Path::new("/sys/class/power_supply")) {
        Some(battery) => {
            bar.set_battery(battery.label().into());
            bar.set_battery_low(battery.low());
        }
        None => bar.set_battery("".into()),
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init(&connection)?;
    let handle = queue.handle();

    let compositor = CompositorState::bind(&globals, &handle)?;
    let layer_shell = LayerShell::bind(&globals, &handle)?;
    let shm = Shm::bind(&globals, &handle)?;

    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(BarPlatform {
        window: window.clone(),
        started: Instant::now(),
    }))
    .map_err(|error| error.to_string())?;

    let bar = Bar::new()?;
    let height = (bar.get_bar_height().round() as u32).max(1);

    let surface = compositor.create_surface(&handle);
    let layer =
        layer_shell.create_layer_surface(&handle, surface, Layer::Top, Some("sengi-bar"), None);
    layer.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    layer.set_size(0, height);
    layer.set_exclusive_zone(height as i32);
    layer.commit();

    // Big enough for one 4K-wide frame; the pool grows if it has to.
    let pool = SlotPool::new(3840 * height as usize * 4, &shm)?;

    let mut app = App {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &handle),
        outputs: OutputState::new(&globals, &handle),
        shm,
        pool,
        layer,
        pointer: None,
        window,
        pixels: Vec::new(),
        height,
        width: 0,
        scale: 1,
        configured: false,
        closed: false,
    };

    bar.on_open_launcher(|| spawn("fuzzel"));
    bar.on_open_network(|| spawn("sengi-run foot -e iwctl"));
    bar.on_show_notifications(|| spawn("makoctl restore"));
    bar.on_dismiss_notifications(|| spawn("makoctl dismiss --all"));
    bar.on_open_session(|| spawn("sengi-power"));
    {
        let weak = bar.as_weak();
        bar.on_toggle_mute(move || {
            let _ = Command::new("wpctl")
                .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
                .stdin(Stdio::null())
                .status();
            if let Some(bar) = weak.upgrade() {
                refresh_status(&bar);
            }
        });
    }

    refresh_clock(&bar);
    refresh_status(&bar);

    let clock_timer = Timer::default();
    {
        let weak = bar.as_weak();
        clock_timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            if let Some(bar) = weak.upgrade() {
                refresh_clock(&bar);
            }
        });
    }
    let status_timer = Timer::default();
    {
        let weak = bar.as_weak();
        status_timer.start(TimerMode::Repeated, STATUS_INTERVAL, move || {
            if let Some(bar) = weak.upgrade() {
                refresh_status(&bar);
            }
        });
    }

    bar.show()?;

    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    WaylandSource::new(connection, queue).insert(event_loop.handle())?;

    while !app.closed {
        slint::platform::update_timers_and_animations();
        app.draw();

        let timeout = if app.window.has_active_animations() {
            Some(Duration::from_millis(16))
        } else {
            slint::platform::duration_until_next_timer_update()
        };
        event_loop.dispatch(timeout, &mut app)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-bar: {error}");
            ExitCode::FAILURE
        }
    }
}
