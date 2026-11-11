//! Runs Slint components on wlr-layer-shell surfaces.
//!
//! Slint has no layer-shell backend, so the Sengi OS shell programs (bar,
//! dock, launcher) share this one. It owns the Wayland connection, puts a
//! Slint component on a layer surface, feeds it pointer and keyboard input,
//! and copies software-rendered frames into shared-memory buffers.
//!
//! # A launcher-like client
//!
//! ```no_run
//! use sengi_layer::{Config, KeyboardMode, Layer};
//! use slint::ComponentHandle;
//!
//! slint::slint! {
//!     export component Prompt inherits Window {
//!         callback dismissed();
//!         in-out property <string> query <=> input.text;
//!         forward-focus: input;
//!         FocusScope {
//!             key-pressed(event) => {
//!                 if event.text == Key.Escape {
//!                     root.dismissed();
//!                     return accept;
//!                 }
//!                 reject
//!             }
//!             input := TextInput { }
//!         }
//!     }
//! }
//!
//! fn main() -> Result<(), sengi_layer::Error> {
//!     let config = Config::new("sengi-launcher")
//!         .layer(Layer::Overlay)
//!         .size(640, 48)
//!         .keyboard(KeyboardMode::Exclusive);
//!
//!     sengi_layer::run(config, |context| {
//!         let prompt = Prompt::new()?;
//!         let handle = context.handle();
//!         prompt.on_dismissed(move || handle.quit());
//!         Ok(prompt)
//!     })
//! }
//! ```
//!
//! # How it fits together
//!
//! * [`Config`] says where the surface goes: layer, anchored edges, size,
//!   margins, exclusive zone, keyboard mode and whether there is one surface
//!   or one per output.
//! * [`run`] connects, calls your closure to build the component, shows it
//!   and runs the loop until [`Handle::quit`]. [`App`] is the same in two
//!   steps, for programs that need the [`Handle`] before the loop starts,
//!   for example to open a channel for a worker thread.
//! * [`Handle`] changes the surface while it runs: resize, hide and show,
//!   keyboard mode, quit. It also opens channels into the loop and hands
//!   out the Wayland [`Connection`](wayland_client::Connection) for
//!   protocols this crate does not speak.
//! * [`Instances`] keeps track of every live copy of the component, which
//!   matters once there is one per output.
//!
//! Slint timers and animations work as usual. The loop sleeps until the next
//! timer or Wayland event and only ticks at frame rate while something is
//! animating. `slint::invoke_from_event_loop`, `Weak::upgrade_in_event_loop`
//! and `slint::quit_event_loop` work from any thread.
//!
//! Only one [`App`] can exist in a process, because Slint takes one platform
//! per process.

mod backend;
mod config;
mod convert;
mod outputs;
mod platform;
mod surface;

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use std::time::Instant;

use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::platform::WindowAdapter;
use slint::{ComponentHandle, PlatformError};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::output::OutputState;
use smithay_client_toolkit::reexports::calloop::channel::{self, Event as ChannelEvent};
use smithay_client_toolkit::reexports::calloop::{EventLoop, LoopHandle};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::RegistryState;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::Shm;
use wayland_client::globals::registry_queue_init;
use wayland_client::Connection;

use backend::{Builder, Instance, Shared, State};
use platform::{LayerPlatform, ProxyEvent, QuitGuard, Unclaimed};

pub use config::{Anchor, Config, ConfigError, KeyboardMode, Layer, Margins, Outputs};
pub use surface::{Output, SurfaceHandle};
/// The version of wayland-client the connection from [`Handle::connection`]
/// belongs to. Use this instead of a dependency of your own so the types
/// match.
pub use wayland_client;

/// Room for a few frames of a bar; the pool grows when a frame needs more.
const INITIAL_POOL: usize = 256 * 1024;

/// Why a layer-shell program could not start or had to stop.
#[derive(Debug)]
pub enum Error {
    /// The [`Config`] asks for something the compositor would reject.
    Config(ConfigError),
    /// No compositor, a compositor without wlr-layer-shell, or a connection
    /// that broke while running.
    Wayland(String),
    /// Slint refused the platform, which happens when one is already set.
    Platform(PlatformError),
    /// The build closure failed.
    Build(Box<dyn std::error::Error>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Config(error) => write!(f, "{error}"),
            Error::Wayland(error) => write!(f, "{error}"),
            Error::Platform(error) => write!(f, "{error}"),
            Error::Build(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Config(error) => Some(error),
            Error::Wayland(_) => None,
            Error::Platform(error) => Some(error),
            Error::Build(error) => Some(error.as_ref()),
        }
    }
}

impl From<ConfigError> for Error {
    fn from(error: ConfigError) -> Error {
        Error::Config(error)
    }
}

fn wayland(what: &str, error: impl fmt::Display) -> Error {
    Error::Wayland(format!("{what}: {error}"))
}

/// Connects to the compositor, builds a component with `build` and runs it
/// on a layer surface until [`Handle::quit`].
///
/// `build` is called after the Slint platform is installed, so it is the
/// place to create the component and wire up its callbacks. With
/// [`Outputs::All`] it is called once for every output. The component it
/// returns is shown and kept alive by the loop.
///
/// This is [`App::new`] followed by [`App::run`].
pub fn run<C, F>(config: Config, build: F) -> Result<(), Error>
where
    C: ComponentHandle + 'static,
    F: FnMut(&Context) -> Result<C, Box<dyn std::error::Error>> + 'static,
{
    App::new()?.run(config, build)
}

/// A connection to the compositor with the Slint platform installed, not
/// yet running.
///
/// Between [`App::new`] and [`App::run`] a program can take the [`Handle`],
/// open channels, start worker threads and create Slint timers:
///
/// ```no_run
/// # use sengi_layer::{App, Config};
/// # slint::slint! { export component Dock inherits Window { in property <int> windows; } }
/// # fn main() -> Result<(), sengi_layer::Error> {
/// use slint::ComponentHandle;
///
/// let app = App::new()?;
/// let docks = sengi_layer::Instances::new();
///
/// let update = {
///     let docks = docks.clone();
///     app.handle().channel(move |count: i32| {
///         docks.for_each(|dock: &Dock| dock.set_windows(count));
///     })?
/// };
/// std::thread::spawn(move || {
///     let _ = update.send(3);
/// });
///
/// app.run(Config::new("sengi-dock").size(200, 52), move |_| {
///     let dock = Dock::new()?;
///     docks.add(&dock);
///     Ok(dock)
/// })
/// # }
/// ```
pub struct App {
    event_loop: EventLoop<'static, State>,
    state: State,
    unclaimed: Unclaimed,
    handle: Handle,
}

impl App {
    /// Connects to the compositor named by `WAYLAND_DISPLAY` and installs
    /// the Slint platform. Fails if there is no compositor, if it lacks
    /// wlr-layer-shell, or if this process already has a Slint platform.
    pub fn new() -> Result<App, Error> {
        let connection = Connection::connect_to_env()
            .map_err(|error| wayland("could not connect to the compositor", error))?;
        let (globals, queue) = registry_queue_init::<State>(&connection)
            .map_err(|error| wayland("could not list the compositor's globals", error))?;
        let queue_handle = queue.handle();

        let compositor = CompositorState::bind(&globals, &queue_handle)
            .map_err(|error| wayland("the compositor has no wl_compositor", error))?;
        let layer_shell = LayerShell::bind(&globals, &queue_handle)
            .map_err(|error| wayland("the compositor does not support wlr-layer-shell", error))?;
        let shm = Shm::bind(&globals, &queue_handle)
            .map_err(|error| wayland("the compositor has no wl_shm", error))?;
        let pool = SlotPool::new(INITIAL_POOL, &shm)
            .map_err(|error| wayland("could not create a buffer pool", error))?;

        let event_loop: EventLoop<'static, State> = EventLoop::try_new()
            .map_err(|error| wayland("could not create the event loop", error))?;
        WaylandSource::new(connection.clone(), queue)
            .insert(event_loop.handle())
            .map_err(|error| wayland("could not watch the compositor connection", error))?;

        let (proxy, proxy_events) = channel::channel();
        event_loop
            .handle()
            .insert_source(proxy_events, |event, _, state: &mut State| match event {
                ChannelEvent::Msg(ProxyEvent::Invoke(call)) => call(),
                ChannelEvent::Msg(ProxyEvent::Quit) => state.shared.borrow_mut().quit = true,
                ChannelEvent::Closed => {}
            })
            .map_err(|error| wayland("could not set up the event loop", error))?;

        let started = Instant::now();
        let unclaimed = Unclaimed::default();
        let quits = QuitGuard::default();
        slint::platform::set_platform(Box::new(LayerPlatform {
            unclaimed: unclaimed.clone(),
            started,
            proxy,
            quits: quits.clone(),
            clipboard: RefCell::new(None),
        }))
        .map_err(|error| Error::Platform(PlatformError::Other(error.to_string())))?;

        let shared = Rc::new(RefCell::new(Shared {
            config: Config::new(""),
            visible: true,
            revision: 1,
            size_stamp: 0,
            size_overridden: false,
            quit: false,
        }));
        let handle = Handle {
            shared: shared.clone(),
            loop_handle: event_loop.handle(),
            connection,
        };
        let state = State::new(
            RegistryState::new(&globals),
            SeatState::new(&globals, &queue_handle),
            OutputState::new(&globals, &queue_handle),
            compositor,
            layer_shell,
            shm,
            pool,
            queue_handle,
            shared,
            handle.clone(),
            quits,
            started,
        );

        Ok(App {
            event_loop,
            state,
            unclaimed,
            handle,
        })
    }

    /// The handle for changing the surface and reaching the loop. The same
    /// handle is given to the build closure through [`Context::handle`].
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// Builds the component with `build`, shows it on a layer surface
    /// described by `config`, and runs until [`Handle::quit`] or
    /// `slint::quit_event_loop`.
    ///
    /// Returns `Ok` after a quit and an error if the configuration is
    /// invalid, `build` fails, or the compositor connection is lost.
    ///
    /// `config` replaces anything set through the handle before this call,
    /// except [`Handle::hide`]: an app hidden before `run` starts hidden.
    pub fn run<C, F>(self, config: Config, mut build: F) -> Result<(), Error>
    where
        C: ComponentHandle + 'static,
        F: FnMut(&Context) -> Result<C, Box<dyn std::error::Error>> + 'static,
    {
        config.validate()?;
        self.state.shared.borrow_mut().config = config;

        let unclaimed = self.unclaimed;
        let builder: Builder = Box::new(move |context| {
            let component = build(context)?;
            let window = claim(&unclaimed, component.window())
                .ok_or("the component returned by the build closure has no window here")?;
            component.show()?;
            Ok((Box::new(component) as Box<dyn Instance>, window))
        });
        backend::run(self.event_loop, self.state, builder)
    }
}

/// Finds the software window behind a component among those Slint has
/// created, and forgets windows whose component is already gone.
fn claim(unclaimed: &Unclaimed, wanted: &slint::Window) -> Option<Rc<MinimalSoftwareWindow>> {
    let mut windows = unclaimed.borrow_mut();
    let position = windows
        .iter()
        .position(|candidate| std::ptr::eq(WindowAdapter::window(&**candidate), wanted))?;
    let window = windows.swap_remove(position);
    windows.retain(|candidate| Rc::strong_count(candidate) > 1);
    Some(window)
}

/// What the build closure is told about the instance it is building.
pub struct Context {
    pub(crate) handle: Handle,
    pub(crate) surface: SurfaceHandle,
    pub(crate) output_name: Option<String>,
}

impl Context {
    /// The app's handle; clone it into callbacks. What it changes applies
    /// to every surface of the app.
    pub fn handle(&self) -> Handle {
        self.handle.clone()
    }

    /// The handle for the surface of the instance being built. Use it to
    /// size the surface from the component, and for anything that should
    /// affect this instance only.
    pub fn surface(&self) -> SurfaceHandle {
        self.surface.clone()
    }

    /// The output this instance is for: its name, logical size and scale.
    /// `None` with [`Outputs::Auto`], where the compositor has not picked
    /// yet; use [`SurfaceHandle::on_output_changed`] to hear when it has,
    /// and when the output later changes.
    pub fn output(&self) -> Option<Output> {
        self.surface.output()
    }

    /// The name of the output this instance is for, such as `eDP-1`, when
    /// running with [`Outputs::All`] or [`Outputs::Named`] and the compositor
    /// names its outputs.
    /// Always `None` with [`Outputs::Auto`].
    pub fn output_name(&self) -> Option<&str> {
        self.output_name.as_deref()
    }
}

/// Changes the running surface and reaches the loop.
///
/// Cheap to clone; move clones into Slint callbacks. It belongs to the UI
/// thread: to reach the loop from another thread use [`Handle::channel`] or
/// `slint::invoke_from_event_loop`.
///
/// Changes apply to every surface of the app, take effect before the loop
/// next sleeps, and are remembered for surfaces created later.
#[derive(Clone)]
pub struct Handle {
    shared: Rc<RefCell<Shared>>,
    loop_handle: LoopHandle<'static, State>,
    connection: Connection,
}

impl Handle {
    /// Asks for a new size in logical pixels; 0 stretches along that axis
    /// and needs both edges of the axis anchored. The component is resized
    /// when the compositor confirms, usually within a frame.
    ///
    /// This replaces any size a surface was given on its own through
    /// [`SurfaceHandle::set_size`].
    pub fn set_size(&self, width: u32, height: u32) -> Result<(), ConfigError> {
        let mut shared = self.shared.borrow_mut();
        config::validate_size(shared.config.anchor, (width, height))?;
        shared.set_size((width, height));
        Ok(())
    }

    /// The app-wide size last asked for, as passed to [`Config::size`] or
    /// [`Handle::set_size`]. For the size a surface really has, see
    /// [`SurfaceHandle::size`].
    pub fn size(&self) -> (u32, u32) {
        self.shared.borrow().config.size
    }

    /// Changes the edges the surface is attached to. Fails if the current
    /// size stretches along an axis the new anchor does not span; use
    /// [`Handle::set_anchor_and_size`] to change both at once.
    pub fn set_anchor(&self, anchor: Anchor) -> Result<(), ConfigError> {
        let size = self.size();
        self.set_anchor_and_size(anchor, size.0, size.1)
    }

    /// Changes the anchored edges and the size together, for moves where
    /// neither is valid with the other's old value, such as turning a
    /// centred box into a strip along an edge.
    pub fn set_anchor_and_size(
        &self,
        anchor: Anchor,
        width: u32,
        height: u32,
    ) -> Result<(), ConfigError> {
        config::validate_size(anchor, (width, height))?;
        let mut shared = self.shared.borrow_mut();
        if shared.config.anchor != anchor {
            shared.config.anchor = anchor;
            shared.revision += 1;
        }
        shared.set_size((width, height));
        Ok(())
    }

    /// Moves the surface to another layer.
    pub fn set_layer(&self, layer: Layer) {
        let mut shared = self.shared.borrow_mut();
        if shared.config.layer != layer {
            shared.config.layer = layer;
            shared.revision += 1;
        }
    }

    /// Changes how much of the screen edge is reserved. See
    /// [`Config::exclusive_zone`].
    pub fn set_exclusive_zone(&self, zone: i32) {
        let mut shared = self.shared.borrow_mut();
        if shared.config.exclusive_zone != zone {
            shared.config.exclusive_zone = zone;
            shared.revision += 1;
        }
    }

    /// Changes the margins to the anchored edges.
    pub fn set_margins(&self, margins: Margins) {
        let mut shared = self.shared.borrow_mut();
        if shared.config.margins != margins {
            shared.config.margins = margins;
            shared.revision += 1;
        }
    }

    /// Changes whether the surface receives key presses.
    pub fn set_keyboard(&self, keyboard: KeyboardMode) {
        let mut shared = self.shared.borrow_mut();
        if shared.config.keyboard != keyboard {
            shared.config.keyboard = keyboard;
            shared.revision += 1;
        }
    }

    /// Takes the surface off the screen. The component, its state and its
    /// timers stay alive, and an exclusive zone or keyboard grab is given
    /// up until [`Handle::show`].
    pub fn hide(&self) {
        self.shared.borrow_mut().visible = false;
    }

    /// Puts a hidden surface back on screen.
    pub fn show(&self) {
        self.shared.borrow_mut().visible = true;
    }

    /// False between [`Handle::hide`] and [`Handle::show`].
    pub fn is_visible(&self) -> bool {
        self.shared.borrow().visible
    }

    /// Ends the loop; [`run`] returns `Ok`.
    pub fn quit(&self) {
        self.shared.borrow_mut().quit = true;
    }

    /// Opens a channel into the loop. Messages sent from any thread wake
    /// the loop and are passed to `receive` on the UI thread, in order.
    ///
    /// The channel lives as long as a [`Sender`] does.
    pub fn channel<T: 'static>(
        &self,
        mut receive: impl FnMut(T) + 'static,
    ) -> Result<Sender<T>, Error> {
        let (sender, messages) = channel::channel();
        self.loop_handle
            .insert_source(messages, move |event, _, _| {
                if let ChannelEvent::Msg(message) = event {
                    receive(message);
                }
            })
            .map_err(|error| wayland("could not add a channel to the event loop", error))?;
        Ok(Sender(sender))
    }

    /// The Wayland connection, for protocols this crate does not handle.
    ///
    /// Create your own event queue on it (`connection.new_event_queue()`),
    /// bind what you need from your own registry, and dispatch that queue
    /// on a thread of your own with `blocking_dispatch`. Pass what you
    /// learn to the UI through [`Handle::channel`]. Do not dispatch on the
    /// UI thread: the loop would not see the events.
    pub fn connection(&self) -> Connection {
        self.connection.clone()
    }
}

/// The sending half of [`Handle::channel`]. Clone it freely; it can be sent
/// to other threads when the message type can.
pub struct Sender<T>(channel::Sender<T>);

impl<T> Sender<T> {
    /// Queues a message and wakes the loop. Fails only when the loop has
    /// ended, which is the cue for a worker thread to stop.
    pub fn send(&self, message: T) -> Result<(), LoopEnded> {
        self.0.send(message).map_err(|_| LoopEnded)
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Sender<T> {
        Sender(self.0.clone())
    }
}

/// A message was sent after the loop ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopEnded;

impl fmt::Display for LoopEnded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the event loop has ended")
    }
}

impl std::error::Error for LoopEnded {}

/// Every live instance of a component.
///
/// With [`Outputs::All`] there is one instance per output and they come and
/// go with the outputs. Add each one in the build closure, then update them
/// all from timers and channels without caring how many there are. It
/// works the same with a single instance.
///
/// Instances are held weakly: one that the loop has dropped is skipped and
/// forgotten.
pub struct Instances<C: ComponentHandle>(Rc<RefCell<Vec<slint::Weak<C>>>>);

impl<C: ComponentHandle> Instances<C> {
    /// An empty list.
    pub fn new() -> Instances<C> {
        Instances(Rc::new(RefCell::new(Vec::new())))
    }

    /// Remembers an instance.
    pub fn add(&self, instance: &C) {
        self.0.borrow_mut().push(instance.as_weak());
    }

    /// Calls `visit` with each live instance, oldest first.
    pub fn for_each(&self, mut visit: impl FnMut(&C)) {
        let alive: Vec<C> = {
            let mut instances = self.0.borrow_mut();
            instances.retain(|instance| instance.upgrade().is_some());
            instances.iter().filter_map(slint::Weak::upgrade).collect()
        };
        for instance in &alive {
            visit(instance);
        }
    }
}

impl<C: ComponentHandle> Clone for Instances<C> {
    fn clone(&self) -> Instances<C> {
        Instances(self.0.clone())
    }
}

impl<C: ComponentHandle> Default for Instances<C> {
    fn default() -> Instances<C> {
        Instances::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    slint::slint! {
        export component Probe inherits Window { }
    }

    // Slint keeps one platform per thread of tests, so everything that
    // needs one lives in this single test.
    #[test]
    fn a_retired_instance_is_freed_and_does_not_quit_the_loop() {
        let (proxy, events) = channel::channel();
        let unclaimed = Unclaimed::default();
        let quits = QuitGuard::default();
        slint::platform::set_platform(Box::new(LayerPlatform {
            unclaimed: unclaimed.clone(),
            started: Instant::now(),
            proxy,
            quits: quits.clone(),
            clipboard: RefCell::new(None),
        }))
        .expect("no platform is set yet");

        let instances = Instances::new();
        let probe = Probe::new().expect("the component builds");
        let weak = probe.as_weak();
        instances.add(&probe);
        let window = claim(&unclaimed, probe.window()).expect("the window is claimed");
        probe.show().expect("the component shows");

        // What the loop holds for an instance: the component and its window.
        let held: Box<dyn Instance> = Box::new(probe);
        assert!(weak.upgrade().is_some());

        backend::retire(held, &quits);
        drop(window);
        assert!(weak.upgrade().is_none(), "the component is still alive");
        let mut visited = 0;
        instances.for_each(|_| visited += 1);
        assert_eq!(visited, 0);
        assert!(events.try_recv().is_err(), "hiding it asked the loop to quit");

        // A quit the app asks for still gets through.
        slint::quit_event_loop().expect("the proxy takes the request");
        assert!(matches!(events.try_recv(), Ok(ProxyEvent::Quit)));
    }
}
