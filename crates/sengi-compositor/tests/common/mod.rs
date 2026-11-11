//! A small Wayland client for driving the compositor from tests and from
//! `examples/misbehave.rs`: enough to map a window, to act as a screen
//! locker, and to act as a client trying to get past one.
#![allow(dead_code)]

use std::{
    fs::{self, File},
    io::Write,
    os::{
        fd::AsFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::atomic::{AtomicU32, Ordering},
    thread::sleep,
    time::{Duration, Instant},
};

use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalList, GlobalListContents},
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_output::WlOutput,
        wl_registry::WlRegistry,
        wl_seat::WlSeat,
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::{
    ext::session_lock::v1::client::{
        ext_session_lock_manager_v1::ExtSessionLockManagerV1,
        ext_session_lock_surface_v1::{self, ExtSessionLockSurfaceV1},
        ext_session_lock_v1::{self, ExtSessionLockV1},
    },
    wp::security_context::v1::client::{
        wp_security_context_manager_v1::WpSecurityContextManagerV1,
        wp_security_context_v1::WpSecurityContextV1,
    },
    xdg::shell::client::{
        xdg_surface::{self, XdgSurface},
        xdg_toplevel::XdgToplevel,
        xdg_wm_base::{self, XdgWmBase},
    },
};

pub const LOCK_MANAGER: &str = "ext_session_lock_manager_v1";

type Failure = Box<dyn std::error::Error>;

/// What the compositor has told this client so far.
#[derive(Default)]
pub struct Seen {
    pub locked: u32,
    pub finished: u32,
    /// The latest lock surface configure: serial, width, height.
    pub lock_configure: Option<(u32, u32, u32)>,
    pub window_configures: u32,
    pub keyboard_enters: u32,
    pub keyboard_leaves: u32,
    pub keys: u32,
    pub frames: u32,
}

pub struct Client {
    pub queue: EventQueue<Seen>,
    pub handle: QueueHandle<Seen>,
    pub seen: Seen,
    pub globals: GlobalList,
    pub compositor: WlCompositor,
    pub shm: WlShm,
}

impl Client {
    /// Connects to the socket at `path`, or to `WAYLAND_DISPLAY`.
    pub fn connect(path: Option<&Path>) -> Result<Self, Failure> {
        let connection = match path {
            Some(path) => Connection::from_socket(UnixStream::connect(path)?)?,
            None => Connection::connect_to_env()?,
        };
        let (globals, queue) = registry_queue_init::<Seen>(&connection)?;
        let handle = queue.handle();
        let compositor = globals.bind(&handle, 1..=4, ())?;
        let shm = globals.bind(&handle, 1..=1, ())?;
        Ok(Self {
            queue,
            handle,
            seen: Seen::default(),
            globals,
            compositor,
            shm,
        })
    }

    /// Sends what is queued and reads the answers. A protocol error the
    /// compositor raised against this client comes back as the error.
    pub fn roundtrip(&mut self) -> Result<(), Failure> {
        self.queue.roundtrip(&mut self.seen)?;
        Ok(())
    }

    /// Keeps talking until `done` holds; false if it never did.
    pub fn wait(&mut self, time: Duration, done: impl Fn(&Seen) -> bool) -> bool {
        let end = Instant::now() + time;
        loop {
            if self.roundtrip().is_err() {
                return false;
            }
            if done(&self.seen) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            sleep(Duration::from_millis(10));
        }
    }

    pub fn has_global(&self, interface: &str) -> bool {
        self.globals
            .contents()
            .with_list(|list| list.iter().any(|global| global.interface == interface))
    }

    pub fn outputs(&self) -> Vec<WlOutput> {
        let names: Vec<u32> = self.globals.contents().with_list(|list| {
            list.iter()
                .filter(|global| global.interface == "wl_output")
                .map(|global| global.name)
                .collect()
        });
        names
            .into_iter()
            .map(|name| self.globals.registry().bind(name, 1, &self.handle, ()))
            .collect()
    }

    /// Listens for keyboard focus and keys.
    pub fn keyboard(&mut self) -> Result<WlKeyboard, Failure> {
        let seat: WlSeat = self.globals.bind(&self.handle, 1..=5, ())?;
        let keyboard = seat.get_keyboard(&self.handle, ());
        self.roundtrip()?;
        Ok(keyboard)
    }

    /// A buffer of one colour, given as 0xAARRGGBB.
    pub fn buffer(&self, width: i32, height: i32, argb: u32) -> Result<WlBuffer, Failure> {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "sengi-test-buffer-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        fs::remove_file(&path)?;
        file.write_all(&argb.to_le_bytes().repeat((width * height) as usize))?;
        let pool = self
            .shm
            .create_pool(file.as_fd(), width * height * 4, &self.handle, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Argb8888,
            &self.handle,
            (),
        );
        pool.destroy();
        Ok(buffer)
    }

    /// Maps an ordinary window of one colour.
    pub fn window(&mut self, argb: u32) -> Result<(WlSurface, XdgToplevel), Failure> {
        let wm_base: XdgWmBase = self.globals.bind(&self.handle, 1..=2, ())?;
        let surface = self.compositor.create_surface(&self.handle, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &self.handle, ());
        let toplevel = xdg_surface.get_toplevel(&self.handle, ());
        toplevel.set_title("test window".into());
        surface.commit();
        if !self.wait(Duration::from_secs(5), |seen| seen.window_configures > 0) {
            return Err("the window was never configured".into());
        }
        let buffer = self.buffer(300, 200, argb)?;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, 300, 200);
        surface.commit();
        self.roundtrip()?;
        Ok((surface, toplevel))
    }

    /// Opens a socket at `path` whose clients the compositor treats as
    /// sandboxed, the way a sandbox's portal does.
    pub fn sandbox(&mut self, path: &Path) -> Result<(), Failure> {
        let manager: WpSecurityContextManagerV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let listener = UnixListener::bind(path)?;
        // The compositor listens for as long as the other end stays open.
        let (keep, close) = UnixStream::pair()?;
        let context = manager.create_listener(listener.as_fd(), close.as_fd(), &self.handle, ());
        context.set_sandbox_engine("org.sengi.test".into());
        context.set_app_id("org.sengi.test.App".into());
        context.set_instance_id("1".into());
        context.commit();
        self.roundtrip()?;
        std::mem::forget(keep);
        Ok(())
    }

    /// Asks for the session lock. Whether it was granted shows in
    /// `seen.locked` and `seen.finished` later.
    pub fn lock(&mut self) -> Result<ExtSessionLockV1, Failure> {
        let manager: ExtSessionLockManagerV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let lock = manager.lock(&self.handle, ());
        self.roundtrip()?;
        Ok(lock)
    }

    /// Puts a lock surface of one colour on `output`, the way a locker
    /// does: wait for the size, acknowledge it, attach a buffer that fits.
    pub fn lock_surface(
        &mut self,
        lock: &ExtSessionLockV1,
        output: &WlOutput,
        argb: u32,
    ) -> Result<(WlSurface, ExtSessionLockSurfaceV1), Failure> {
        let surface = self.compositor.create_surface(&self.handle, ());
        self.seen.lock_configure = None;
        let lock_surface = lock.get_lock_surface(&surface, output, &self.handle, ());
        if !self.wait(Duration::from_secs(5), |seen| seen.lock_configure.is_some()) {
            return Err("the lock surface was never configured".into());
        }
        let (serial, width, height) = self.seen.lock_configure.take().unwrap_or_default();
        lock_surface.ack_configure(serial);
        let buffer = self.buffer(width as i32, height as i32, argb)?;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width as i32, height as i32);
        surface.commit();
        self.roundtrip()?;
        Ok((surface, lock_surface))
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Seen {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            seen.window_configures += 1;
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => seen.locked += 1,
            ext_session_lock_v1::Event::Finished => seen.finished += 1,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockSurfaceV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtSessionLockSurfaceV1,
        event: ext_session_lock_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_session_lock_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            seen.lock_configure = Some((serial, width, height));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => seen.keyboard_enters += 1,
            wl_keyboard::Event::Leave { .. } => seen.keyboard_leaves += 1,
            wl_keyboard::Event::Key { .. } => seen.keys += 1,
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            seen.frames += 1;
        }
    }
}

delegate_noop!(Seen: WlCompositor);
delegate_noop!(Seen: WlShmPool);
delegate_noop!(Seen: ExtSessionLockManagerV1);
delegate_noop!(Seen: WpSecurityContextManagerV1);
delegate_noop!(Seen: WpSecurityContextV1);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
delegate_noop!(Seen: ignore WlSeat);
delegate_noop!(Seen: ignore WlOutput);
delegate_noop!(Seen: ignore XdgToplevel);
