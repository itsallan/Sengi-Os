//! The Wayland side: layer surfaces, input and the loop that ties them to
//! Slint.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor};
use slint::platform::WindowEvent;
use slint::{LogicalPosition, PhysicalSize, SharedString};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, Region};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers, RepeatInfo,
};
use smithay_client_toolkit::seat::pointer::{
    CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor as WlrAnchor, KeyboardInteractivity, Layer as WlrLayer, LayerShell, LayerShellHandler,
    LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, delegate_shm, registry_handlers,
};
use wayland_client::protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

use crate::config::{Anchor, Config, KeyboardMode, Layer, Outputs};
use crate::convert::{
    is_modifier, key_repeats, key_text, modifier_fixes, pointer_button, repeats_due, scroll_delta,
    write_argb8888, ModifierState, RepeatTiming,
};
use crate::outputs::{plan, retry_delay};
use crate::platform::QuitGuard;
use crate::surface::{call_hook, Output, Rect, SurfaceHandle, SurfaceShared};
use crate::{Context, Error, Handle};

/// How often to look again while something animates without drawing, so
/// that no frame callback is on its way to wake the loop.
const FRAME: Duration = Duration::from_millis(16);

/// What the app has asked for through its [`Handle`]. Slint callbacks run
/// while the Wayland state is borrowed, so requests are left here and the
/// loop picks them up before it next goes to sleep.
pub(crate) struct Shared {
    pub config: Config,
    pub visible: bool,
    /// Counts changes to the size, margins, exclusive zone and keyboard
    /// mode, so each surface can tell whether it has sent the latest.
    pub revision: u64,
    /// Counts app-wide size changes, so that a surface's own size is
    /// dropped when a newer app-wide one arrives.
    pub size_stamp: u64,
    /// Some surface has asked for a size of its own.
    pub size_overridden: bool,
    /// Where the surfaces take pointer input. None is everywhere.
    pub input_region: Option<Vec<Rect>>,
    /// Counts app-wide input region changes, like `size_stamp`.
    pub input_stamp: u64,
    pub quit: bool,
}

/// A component the loop is showing, with its type forgotten.
pub(crate) trait Instance {
    /// Lets go of the component. Slint keeps a shown component alive from
    /// its window, so only hiding it frees it.
    fn retire(&self);
}

impl<C: slint::ComponentHandle> Instance for C {
    fn retire(&self) {
        let _ = self.hide();
    }
}

/// Hides a component for good without Slint taking the hiding of its last
/// window as a request to quit.
pub(crate) fn retire(component: Box<dyn Instance>, quits: &QuitGuard) {
    quits.hold(|| component.retire());
}

impl Shared {
    /// Sets the app-wide size, which takes over from any surface's own.
    pub(crate) fn set_size(&mut self, size: (u32, u32)) {
        if self.config.size != size || self.size_overridden {
            self.revision += 1;
        }
        self.config.size = size;
        self.size_stamp += 1;
        self.size_overridden = false;
    }
}

pub(crate) type Built = (Box<dyn Instance>, Rc<MinimalSoftwareWindow>);
pub(crate) type Builder = Box<dyn FnMut(&Context) -> Result<Built, Box<dyn std::error::Error>>>;

/// One instance of the app's component and the layer surface showing it.
struct Surface {
    component: Box<dyn Instance>,
    window: Rc<MinimalSoftwareWindow>,
    /// The output this instance belongs to. None when the compositor picks.
    output: Option<wl_output::WlOutput>,
    /// None while hidden, or while there is no output to be on.
    layer: Option<LayerSurface>,
    /// Logical size, known once the compositor configures the surface.
    width: u32,
    height: u32,
    scale: u32,
    configured: bool,
    /// The output the surface was last seen entering, for a surface whose
    /// output the compositor picks.
    entered: Option<wl_output::WlOutput>,
    /// What the instance has asked for through its `SurfaceHandle`.
    own: Rc<RefCell<SurfaceShared>>,
    /// The `Shared::revision` and `SurfaceShared::revision` the compositor
    /// was last told about.
    sent: (u64, u64),
    /// A frame was committed and the compositor has not yet said it is
    /// time for the next. Nothing is drawn meanwhile, which paces drawing
    /// to the display and stops it while the surface cannot be seen.
    frame_pending: bool,
    /// How many times in a row the compositor has closed the surface
    /// without its output going away.
    closes: u32,
    /// When to try mapping again after such a close, measured from
    /// `State::started`.
    retry: Option<Duration>,
}

struct Focus {
    surface: wl_surface::WlSurface,
    window: Rc<MinimalSoftwareWindow>,
    own: Rc<RefCell<SurfaceShared>>,
}

struct Repeat {
    raw_code: u32,
    text: SharedString,
    /// When the next repeat is due, measured from `State::started`.
    next: Duration,
}

pub(crate) struct State {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    queue: QueueHandle<State>,
    pointer: Option<ThemedPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,

    pub(crate) shared: Rc<RefCell<Shared>>,
    handle: Handle,
    quits: QuitGuard,
    builder: Option<Builder>,
    surfaces: Vec<Surface>,
    /// Whether the surfaces are currently meant to be mapped. Follows
    /// `Shared::visible` once the loop has acted on it.
    shown: bool,
    pixels: Vec<PremultipliedRgbaColor>,

    /// The surface with keyboard focus, the window it feeds and the
    /// instance to tell about it.
    focus: Option<Focus>,
    /// Modifier keys Slint has seen go down and not come up.
    held_modifiers: Vec<char>,
    /// The text sent for each key that is down, so its release matches.
    pressed: HashMap<u32, SharedString>,
    repeat_timing: Option<RepeatTiming>,
    repeat: Option<Repeat>,
    started: Instant,

    /// Set by a handler that cannot return an error itself; ends the loop.
    failure: Option<Error>,
}

impl From<Layer> for WlrLayer {
    fn from(layer: Layer) -> WlrLayer {
        match layer {
            Layer::Background => WlrLayer::Background,
            Layer::Bottom => WlrLayer::Bottom,
            Layer::Top => WlrLayer::Top,
            Layer::Overlay => WlrLayer::Overlay,
        }
    }
}

impl From<Anchor> for WlrAnchor {
    fn from(anchor: Anchor) -> WlrAnchor {
        let mut edges = WlrAnchor::empty();
        edges.set(WlrAnchor::TOP, anchor.contains(Anchor::TOP));
        edges.set(WlrAnchor::BOTTOM, anchor.contains(Anchor::BOTTOM));
        edges.set(WlrAnchor::LEFT, anchor.contains(Anchor::LEFT));
        edges.set(WlrAnchor::RIGHT, anchor.contains(Anchor::RIGHT));
        edges
    }
}

impl From<KeyboardMode> for KeyboardInteractivity {
    fn from(mode: KeyboardMode) -> KeyboardInteractivity {
        match mode {
            KeyboardMode::None => KeyboardInteractivity::None,
            KeyboardMode::OnDemand => KeyboardInteractivity::OnDemand,
            KeyboardMode::Exclusive => KeyboardInteractivity::Exclusive,
        }
    }
}

/// Sends everything about the surface that can change while it is mapped.
fn send_geometry(layer: &LayerSurface, config: &Config, size: (u32, u32)) {
    layer.set_layer(config.layer.into());
    layer.set_anchor(config.anchor.into());
    layer.set_size(size.0, size.1);
    layer.set_margin(
        config.margins.top,
        config.margins.right,
        config.margins.bottom,
        config.margins.left,
    );
    layer.set_exclusive_zone(config.exclusive_zone);
    layer.set_keyboard_interactivity(config.keyboard.into());
}

/// Says where the surface takes pointer input. Clicks elsewhere pass
/// through to whatever is below. It takes effect with the next commit.
fn send_input_region(compositor: &CompositorState, layer: &LayerSurface, region: Option<&[Rect]>) {
    let Some(rectangles) = region else {
        layer.wl_surface().set_input_region(None);
        return;
    };
    let Ok(region) = Region::new(compositor) else {
        return;
    };
    for rectangle in rectangles {
        region.add(rectangle.x, rectangle.y, rectangle.width, rectangle.height);
    }
    layer
        .wl_surface()
        .set_input_region(Some(region.wl_region()));
}

impl State {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        registry: RegistryState,
        seats: SeatState,
        outputs: OutputState,
        compositor: CompositorState,
        layer_shell: LayerShell,
        shm: Shm,
        pool: SlotPool,
        queue: QueueHandle<State>,
        shared: Rc<RefCell<Shared>>,
        handle: Handle,
        quits: QuitGuard,
        started: Instant,
    ) -> State {
        State {
            registry,
            seats,
            outputs,
            compositor,
            layer_shell,
            shm,
            pool,
            queue,
            pointer: None,
            keyboard: None,
            shared,
            handle,
            quits,
            builder: None,
            surfaces: Vec::new(),
            shown: false,
            pixels: Vec::new(),
            focus: None,
            held_modifiers: Vec::new(),
            pressed: HashMap::new(),
            repeat_timing: None,
            repeat: None,
            started,
            failure: None,
        }
    }

    fn mode(&self) -> Outputs {
        self.shared.borrow().config.outputs.clone()
    }

    fn index_of(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|candidate| {
            candidate.layer.as_ref().map(|layer| layer.wl_surface()) == Some(surface)
        })
    }

    /// Builds one more instance of the app's component.
    fn build(&mut self, output: Option<wl_output::WlOutput>) -> Result<usize, Error> {
        let output_name = output
            .as_ref()
            .and_then(|output| self.outputs.info(output))
            .and_then(|info| info.name);
        let own = Rc::new(RefCell::new(SurfaceShared::new()));
        own.borrow_mut().output = output.as_ref().and_then(|output| self.describe(output));
        let context = Context {
            handle: self.handle.clone(),
            surface: SurfaceHandle {
                app: self.shared.clone(),
                own: own.clone(),
            },
            output_name,
        };
        let builder = self
            .builder
            .as_mut()
            .expect("the builder is set before the loop runs");
        let (component, window) = builder(&context).map_err(Error::Build)?;
        self.surfaces.push(Surface {
            component,
            window,
            output,
            layer: None,
            width: 0,
            height: 0,
            scale: 1,
            configured: false,
            entered: None,
            own,
            sent: (0, 0),
            frame_pending: false,
            closes: 0,
            retry: None,
        });
        Ok(self.surfaces.len() - 1)
    }

    fn describe(&self, output: &wl_output::WlOutput) -> Option<Output> {
        let info = self.outputs.info(output)?;
        Some(Output {
            name: info.name,
            size: info
                .logical_size
                .map(|(width, height)| (width.max(0) as u32, height.max(0) as u32)),
            scale: info.scale_factor.max(1) as u32,
        })
    }

    /// Looks up the surface's output again and tells the app if what is
    /// known about it has changed.
    fn refresh_output(&mut self, index: usize) {
        let surface = &self.surfaces[index];
        let Some(output) = surface.output.as_ref().or(surface.entered.as_ref()) else {
            return;
        };
        let Some(described) = self.describe(output) else {
            return;
        };
        let own = surface.own.clone();
        if own.borrow().output.as_ref() == Some(&described) {
            return;
        }
        own.borrow_mut().output = Some(described.clone());
        call_hook(&own, |own| &mut own.on_output, described);
    }

    /// Gives the instance a layer surface. Nothing is drawn until the
    /// compositor answers with a configure.
    fn map(&mut self, index: usize) {
        let shared = self.shared.borrow();
        let config = &shared.config;
        let surface = &mut self.surfaces[index];
        let layer = self.layer_shell.create_layer_surface(
            &self.queue,
            self.compositor.create_surface(&self.queue),
            config.layer.into(),
            Some(config.namespace.clone()),
            surface.output.as_ref(),
        );
        let own = surface.own.borrow();
        send_geometry(&layer, config, own.wanted_size(&shared));
        send_input_region(
            &self.compositor,
            &layer,
            own.wanted_input_region(&shared).as_deref(),
        );
        layer.commit();
        surface.layer = Some(layer);
        surface.sent = (shared.revision, own.revision);
        // A new surface starts at scale 1 until the compositor says more.
        surface.scale = 1;
        surface.frame_pending = false;
        drop(own);
        surface.own.borrow_mut().scale = 1;
    }

    /// Takes the instance off the screen but keeps its component.
    fn unmap(&mut self, index: usize) {
        let surface = &mut self.surfaces[index];
        let Some(layer) = surface.layer.take() else {
            return;
        };
        surface.configured = false;
        surface.entered = None;
        // No leave events arrive for a surface that is gone.
        let window = surface.window.clone();
        window.dispatch_event(WindowEvent::PointerExited);
        if self.focus.as_ref().map(|focus| &focus.surface) == Some(layer.wl_surface()) {
            self.focus_lost();
        }
    }

    /// True if the instance is meant to be on screen and is not.
    fn wants_mapping(&self, index: usize) -> bool {
        let surface = &self.surfaces[index];
        self.shown
            && surface.own.borrow().visible
            && surface.layer.is_none()
            && surface.retry.is_none()
            && (surface.output.is_some() || self.outputs.outputs().next().is_some())
    }

    /// Brings the surfaces in line with the outputs that exist. `gone` is an
    /// output that is being removed but may still be listed.
    fn sync_outputs(&mut self, gone: Option<&wl_output::WlOutput>) {
        if self.builder.is_none() {
            return;
        }
        // An output counts once the compositor has described it, which is
        // also when its name is known.
        let ready: Vec<(wl_output::WlOutput, Option<String>)> = self
            .outputs
            .outputs()
            .filter(|output| Some(output) != gone)
            .filter_map(|output| {
                let info = self.outputs.info(&output)?;
                Some((output, info.name))
            })
            .collect();
        let instances: Vec<wl_output::WlOutput> = self
            .surfaces
            .iter()
            .filter_map(|surface| surface.output.clone())
            .collect();
        let plan = plan(&self.mode(), &ready, &instances);

        for output in plan.remove {
            let position = self
                .surfaces
                .iter()
                .position(|surface| surface.output.as_ref() == Some(&output));
            if let Some(index) = position {
                self.unmap(index);
                let surface = self.surfaces.remove(index);
                retire(surface.component, &self.quits);
            }
        }
        for output in plan.add {
            if let Err(error) = self.build(Some(output)) {
                self.failure = Some(error);
                return;
            }
        }
        for index in 0..self.surfaces.len() {
            if self.wants_mapping(index) {
                self.map(index);
            }
        }
    }

    fn resize(&mut self, index: usize) {
        let surface = &self.surfaces[index];
        surface
            .window
            .dispatch_event(WindowEvent::ScaleFactorChanged {
                scale_factor: surface.scale as f32,
            });
        surface.window.set_size(PhysicalSize::new(
            surface.width * surface.scale,
            surface.height * surface.scale,
        ));
        // The compositor is told the new scale in `draw`, together with
        // the first buffer rendered at it.
        surface.window.request_redraw();
    }

    /// Renders a frame if Slint has anything new to show.
    fn draw(&mut self, index: usize) {
        let surface = &mut self.surfaces[index];
        let Some(layer) = &surface.layer else {
            return;
        };
        if !surface.configured || surface.width == 0 || surface.height == 0 {
            return;
        }
        if surface.frame_pending {
            return;
        }
        let width = (surface.width * surface.scale) as usize;
        let height = (surface.height * surface.scale) as usize;

        let pixels = &mut self.pixels;
        let drawn = surface.window.draw_if_needed(|renderer| {
            pixels.clear();
            pixels.resize(width * height, PremultipliedRgbaColor::default());
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
            eprintln!("sengi-layer: could not allocate a frame buffer");
            return;
        };
        write_argb8888(&self.pixels, canvas);

        // The scale goes out in the same commit as the buffer it describes.
        // Sent on its own it could be committed against the previous
        // buffer, whose size need not be a multiple of the new scale, and
        // that is a protocol error.
        let wl_surface = layer.wl_surface();
        wl_surface.set_buffer_scale(surface.scale as i32);
        wl_surface.damage_buffer(0, 0, width as i32, height as i32);
        if buffer.attach_to(wl_surface).is_ok() {
            wl_surface.frame(&self.queue, wl_surface.clone());
            surface.frame_pending = true;
            layer.commit();
        }
    }

    /// Acts on what the app asked for through its handle since last time.
    fn apply_requests(&mut self) {
        // Before the commit that asks for keyboard focus: a surface focused
        // while there is no wl_keyboard gets no enter event.
        self.bind_keyboard();
        self.shown = self.shared.borrow().visible;
        for index in 0..self.surfaces.len() {
            if !(self.shown && self.surfaces[index].own.borrow().visible) {
                self.unmap(index);
            } else if self.wants_mapping(index) {
                self.map(index);
            }
        }
        let shared = self.shared.borrow();
        for surface in &mut self.surfaces {
            if let Some(layer) = &surface.layer {
                let own = surface.own.borrow();
                let latest = (shared.revision, own.revision);
                if surface.sent != latest {
                    send_geometry(layer, &shared.config, own.wanted_size(&shared));
                    send_input_region(
                        &self.compositor,
                        layer,
                        own.wanted_input_region(&shared).as_deref(),
                    );
                    layer.commit();
                    surface.sent = latest;
                }
            }
        }
    }

    /// Asks for the keyboard once the app wants key presses. A bar never
    /// does, and then no keymap is ever loaded.
    fn bind_keyboard(&mut self) {
        if self.keyboard.is_some() || self.shared.borrow().config.keyboard == KeyboardMode::None {
            return;
        }
        let seat = self
            .seats
            .seats()
            .find(|seat| self.seats.info(seat).is_some_and(|info| info.has_keyboard));
        if let Some(seat) = seat {
            self.keyboard = self.seats.get_keyboard(&self.queue, &seat, None).ok();
        }
    }

    fn focus_gained(&mut self, index: usize) {
        let surface = &self.surfaces[index];
        let Some(layer) = &surface.layer else {
            return;
        };
        let window = surface.window.clone();
        let own = surface.own.clone();
        self.focus = Some(Focus {
            surface: layer.wl_surface().clone(),
            window: window.clone(),
            own: own.clone(),
        });
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        own.borrow_mut().focused = true;
        call_hook(&own, |own| &mut own.on_focus, true);
    }

    /// Keys only reach the client that has the keyboard, so a key press
    /// with no focus on record means an enter event was never sent.
    /// wlroots does this when the first keyboard is plugged in while the
    /// surface is already focused. With one surface on screen there is no
    /// doubt which one is meant.
    fn assume_focus(&mut self) {
        let mut mapped =
            (0..self.surfaces.len()).filter(|&index| self.surfaces[index].layer.is_some());
        if let (Some(index), None) = (mapped.next(), mapped.next()) {
            self.focus_gained(index);
        }
    }

    fn focus_lost(&mut self) {
        let Some(Focus { window, own, .. }) = self.focus.take() else {
            return;
        };
        self.repeat = None;
        self.pressed.clear();
        for key in std::mem::take(&mut self.held_modifiers) {
            window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        }
        window.dispatch_event(WindowEvent::WindowActiveChanged(false));
        own.borrow_mut().focused = false;
        call_hook(&own, |own| &mut own.on_focus, false);
    }

    /// Maps the surfaces whose wait after a close is over.
    fn fire_retries(&mut self) {
        let now = self.started.elapsed();
        for index in 0..self.surfaces.len() {
            if self.surfaces[index].retry.is_some_and(|due| due <= now) {
                self.surfaces[index].retry = None;
                if self.wants_mapping(index) {
                    self.map(index);
                }
            }
        }
    }

    /// Sends the repeats a held key has built up.
    fn fire_repeats(&mut self) {
        let (Some(timing), Some(repeat), Some(Focus { window, .. })) =
            (self.repeat_timing, &mut self.repeat, &self.focus)
        else {
            self.repeat = None;
            return;
        };
        let (count, next) = repeats_due(repeat.next, timing.interval, self.started.elapsed());
        repeat.next = next;
        let text = repeat.text.clone();
        let window = window.clone();
        for _ in 0..count {
            window.dispatch_event(WindowEvent::KeyPressRepeated { text: text.clone() });
        }
    }

    /// How long the loop may sleep before something needs doing.
    fn timeout(&self) -> Option<Duration> {
        // An animating surface that has just drawn is woken by its frame
        // callback. One that has not, because the animation changed nothing
        // visible yet, has to be looked at again.
        let polling = self.surfaces.iter().any(|surface| {
            surface.layer.is_some()
                && !surface.frame_pending
                && surface.window.has_active_animations()
        });
        let timers = if polling {
            Some(FRAME)
        } else {
            slint::platform::duration_until_next_timer_update()
        };
        let now = self.started.elapsed();
        let repeat = self.repeat.as_ref().map(|repeat| repeat.next);
        let retries = self.surfaces.iter().filter_map(|surface| surface.retry);
        let deadline = repeat
            .into_iter()
            .chain(retries)
            .min()
            .map(|due| due.saturating_sub(now));
        match (timers, deadline) {
            (Some(timers), Some(deadline)) => Some(timers.min(deadline)),
            (timers, deadline) => timers.or(deadline),
        }
    }
}

/// Runs until the app quits or the connection to the compositor is lost.
pub(crate) fn run(
    mut event_loop: EventLoop<'static, State>,
    mut state: State,
    builder: Builder,
) -> Result<(), Error> {
    state.builder = Some(builder);
    state.shown = state.shared.borrow().visible;
    if state.mode() == Outputs::Auto {
        // Built before any output is known, so a build error is reported
        // at once. The surface follows when the first output is announced.
        state.build(None)?;
    }
    state.sync_outputs(None);

    loop {
        slint::platform::update_timers_and_animations();
        state.fire_repeats();
        state.fire_retries();
        state.apply_requests();
        if let Some(error) = state.failure.take() {
            return Err(error);
        }
        if state.shared.borrow().quit {
            return Ok(());
        }
        for index in 0..state.surfaces.len() {
            state.draw(index);
        }
        event_loop
            .dispatch(state.timeout(), &mut state)
            .map_err(|error| Error::Wayland(error.to_string()))?;
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        let Some(index) = self.index_of(surface) else {
            return;
        };
        let factor = factor.max(1) as u32;
        if factor != self.surfaces[index].scale {
            self.surfaces[index].scale = factor;
            self.surfaces[index].own.borrow_mut().scale = factor;
            self.resize(index);
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

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(index) = self.index_of(surface) {
            self.surfaces[index].frame_pending = false;
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if let Some(index) = self.index_of(surface) {
            self.surfaces[index].entered = Some(output.clone());
            self.refresh_output(index);
        }
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if let Some(index) = self.index_of(surface) {
            if self.surfaces[index].entered.as_ref() == Some(output) {
                self.surfaces[index].entered = None;
            }
        }
    }
}

impl LayerShellHandler for State {
    // The compositor closes a surface when its output goes away, for
    // example when an external monitor is unplugged. A surface that follows
    // the compositor's choice moves to an output that is left; with none
    // left a new surface would be closed straight away, so it waits for
    // `new_output`.
    //
    // A surface tied to one output cannot tell yet whether the output is
    // going. It waits a moment: if the output is removed the instance goes
    // with it, and if it is still there the surface is put back, with a
    // longer wait each time the compositor closes it again.
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        let Some(index) = self.index_of(layer.wl_surface()) else {
            return;
        };
        self.unmap(index);
        if self.surfaces[index].output.is_some() {
            let surface = &mut self.surfaces[index];
            surface.closes += 1;
            surface.retry = Some(self.started.elapsed() + retry_delay(surface.closes));
        } else if self.wants_mapping(index) {
            self.map(index);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(index) = self.index_of(layer.wl_surface()) else {
            return;
        };
        // Zero from the compositor means the choice is ours.
        let surface = &mut self.surfaces[index];
        let requested = surface.own.borrow().wanted_size(&self.shared.borrow());
        let (width, height) = configure.new_size;
        surface.width = if width != 0 { width } else { requested.0 };
        surface.height = if height != 0 { height } else { requested.1 };
        surface.own.borrow_mut().configured = (surface.width, surface.height);
        surface.configured = true;
        surface.closes = 0;
        // A new size is drawn at once rather than after the next callback.
        surface.frame_pending = false;
        self.resize(index);
        self.surfaces[index].window.request_redraw();
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        connection: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(index) = self.index_of(&event.surface) else {
                continue;
            };
            let position = LogicalPosition::new(event.position.0 as f32, event.position.1 as f32);
            let translated = match event.kind {
                PointerEventKind::Enter { .. } => {
                    // A client has to say which cursor it wants each time
                    // the pointer comes in, or the last one stays.
                    if let Some(pointer) = &self.pointer {
                        let _ = pointer.set_cursor(connection, CursorIcon::Default);
                    }
                    Some(WindowEvent::PointerMoved { position })
                }
                PointerEventKind::Motion { .. } => Some(WindowEvent::PointerMoved { position }),
                PointerEventKind::Leave { .. } => Some(WindowEvent::PointerExited),
                PointerEventKind::Press { button, .. } => pointer_button(button)
                    .map(|button| WindowEvent::PointerPressed { position, button }),
                PointerEventKind::Release { button, .. } => pointer_button(button)
                    .map(|button| WindowEvent::PointerReleased { position, button }),
                PointerEventKind::Axis {
                    horizontal,
                    vertical,
                    ..
                } => Some(WindowEvent::PointerScrolled {
                    position,
                    delta_x: scroll_delta(horizontal.absolute, horizontal.discrete),
                    delta_y: scroll_delta(vertical.absolute, vertical.discrete),
                }),
            };
            if let Some(translated) = translated {
                let window = self.surfaces[index].window.clone();
                window.dispatch_event(translated);
            }
        }
    }
}

impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.focus_lost();
        if let Some(index) = self.index_of(surface) {
            self.focus_gained(index);
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self.focus.as_ref().map(|focus| &focus.surface) == Some(surface) {
            self.focus_lost();
        }
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if self.focus.is_none() {
            self.assume_focus();
        }
        let Some(window) = self.focus.as_ref().map(|focus| focus.window.clone()) else {
            return;
        };
        let Some(text) = key_text(event.keysym, event.utf8.as_deref()) else {
            return;
        };
        self.pressed.insert(event.raw_code, text.clone());
        if is_modifier(&text) {
            let key = text.chars().next().unwrap_or_default();
            if !self.held_modifiers.contains(&key) {
                self.held_modifiers.push(key);
            }
        } else if let (Some(timing), true) = (self.repeat_timing, key_repeats(&text)) {
            self.repeat = Some(Repeat {
                raw_code: event.raw_code,
                text: text.clone(),
                next: self.started.elapsed() + timing.delay,
            });
        }
        window.dispatch_event(WindowEvent::KeyPressed { text });
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if self
            .repeat
            .as_ref()
            .is_some_and(|repeat| repeat.raw_code == event.raw_code)
        {
            self.repeat = None;
        }
        let Some(window) = self.focus.as_ref().map(|focus| focus.window.clone()) else {
            return;
        };
        // A release carries no text; it repeats what the press sent. A
        // press that typed nothing has nothing to release.
        let Some(text) = self.pressed.remove(&event.raw_code) else {
            return;
        };
        if is_modifier(&text) {
            let key = text.chars().next();
            self.held_modifiers.retain(|held| Some(*held) != key);
        }
        window.dispatch_event(WindowEvent::KeyReleased { text });
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: u32,
    ) {
        let Some(window) = self.focus.as_ref().map(|focus| focus.window.clone()) else {
            return;
        };
        let actual = ModifierState {
            shift: modifiers.shift,
            control: modifiers.ctrl,
            alt: modifiers.alt,
            meta: modifiers.logo,
        };
        for (key, pressed) in modifier_fixes(&self.held_modifiers, actual) {
            if pressed {
                self.held_modifiers.push(key);
                window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            } else {
                self.held_modifiers.retain(|held| *held != key);
                window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
            }
        }
    }

    fn update_repeat_info(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        info: RepeatInfo,
    ) {
        self.repeat_timing = match info {
            RepeatInfo::Repeat { rate, delay } => RepeatTiming::new(rate.get(), delay),
            RepeatInfo::Disable => None,
        };
    }
}

impl SeatHandler for State {
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
            let cursor_surface = self.compositor.create_surface(queue);
            self.pointer = self
                .seats
                .get_pointer_with_theme(
                    queue,
                    &seat,
                    self.shm.wl_shm(),
                    cursor_surface,
                    ThemeSpec::default(),
                )
                .ok();
        }
        if capability == Capability::Keyboard {
            self.bind_keyboard();
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
            self.pointer = None;
            // The pointer went without leaving; do not let hover stick.
            for surface in &self.surfaces {
                surface.window.dispatch_event(WindowEvent::PointerExited);
            }
        }
        if capability == Capability::Keyboard {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
            self.focus_lost();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.sync_outputs(None);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if self.mode() != Outputs::Auto {
            self.sync_outputs(None);
        }
        for index in 0..self.surfaces.len() {
            self.refresh_output(index);
        }
    }

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.mode() != Outputs::Auto {
            self.sync_outputs(Some(&output));
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_pointer!(State);
delegate_keyboard!(State);
delegate_layer!(State);
delegate_registry!(State);
