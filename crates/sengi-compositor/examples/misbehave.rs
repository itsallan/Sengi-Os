//! A client that breaks the rules on purpose, to check the compositor
//! holds up. Run it against a compositor with one of:
//!
//!     misbehave forged-move   map a window, then keep asking for an
//!                             interactive move with a serial it was never
//!                             given; the window must stay put
//!     misbehave unmap         map a window, detach its buffer, then map it
//!                             again; it must vanish cleanly and come back

use std::{
    error::Error,
    fs::{self, File},
    io::Write,
    os::fd::AsFd,
    thread::sleep,
    time::{Duration, Instant},
};

use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{
        wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry, wl_seat::WlSeat,
        wl_shm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
    },
    Connection, Dispatch, EventQueue, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

const WIDTH: i32 = 300;
const HEIGHT: i32 = 200;
/// A serial no compositor handed to this client.
const FORGED_SERIAL: u32 = 0x00ba_d5e7;

#[derive(Default)]
struct App {
    /// Configures seen so far; mapping waits for the next one.
    configures: u32,
}

impl Dispatch<WlRegistry, GlobalListContents> for App {
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

impl Dispatch<XdgWmBase, ()> for App {
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

impl Dispatch<XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            app.configures += 1;
        }
    }
}

delegate_noop!(App: WlCompositor);
delegate_noop!(App: WlShmPool);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore WlBuffer);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlSeat);
delegate_noop!(App: ignore XdgToplevel);

/// A buffer of one flat colour, backed by an unlinked file.
fn buffer(shm: &wl_shm::WlShm, queue: &QueueHandle<App>) -> Result<WlBuffer, Box<dyn Error>> {
    let dir = std::env::var("XDG_RUNTIME_DIR")?;
    let path = format!("{dir}/misbehave-{}", std::process::id());
    let mut file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    fs::remove_file(&path)?;
    // XRGB8888, little-endian: a warm orange that is easy to spot.
    let pixel = [0x3a, 0x77, 0xd9, 0xff];
    file.write_all(&pixel.repeat((WIDTH * HEIGHT) as usize))?;

    let pool = shm.create_pool(file.as_fd(), WIDTH * HEIGHT * 4, queue, ());
    let buffer = pool.create_buffer(
        0,
        WIDTH,
        HEIGHT,
        WIDTH * 4,
        wl_shm::Format::Xrgb8888,
        queue,
        (),
    );
    pool.destroy();
    Ok(buffer)
}

/// Keeps the connection serviced for `time`, calling `tick` now and then.
fn run_for(
    queue: &mut EventQueue<App>,
    app: &mut App,
    time: Duration,
    mut tick: impl FnMut(),
) -> Result<(), Box<dyn Error>> {
    let end = Instant::now() + time;
    while Instant::now() < end {
        tick();
        queue.roundtrip(app)?;
        sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode != "forged-move" && mode != "unmap" {
        return Err("usage: misbehave forged-move|unmap".into());
    }

    let connection = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<App>(&connection)?;
    let handle = queue.handle();
    let mut app = App::default();

    let compositor: WlCompositor = globals.bind(&handle, 1..=4, ())?;
    let shm: wl_shm::WlShm = globals.bind(&handle, 1..=1, ())?;
    let wm_base: XdgWmBase = globals.bind(&handle, 1..=2, ())?;
    let seat: WlSeat = globals.bind(&handle, 1..=5, ())?;

    let surface = compositor.create_surface(&handle, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &handle, ());
    let toplevel = xdg_surface.get_toplevel(&handle, ());
    toplevel.set_title(format!("misbehave {mode}"));
    toplevel.set_app_id("misbehave".into());
    let buffer = buffer(&shm, &handle)?;

    // The usual start: an empty commit, a configure, then the first buffer.
    let map = |queue: &mut EventQueue<App>, app: &mut App| -> Result<(), Box<dyn Error>> {
        let seen = app.configures;
        surface.commit();
        while app.configures == seen {
            queue.blocking_dispatch(app)?;
        }
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, WIDTH, HEIGHT);
        surface.commit();
        queue.roundtrip(app)?;
        Ok(())
    };
    map(&mut queue, &mut app)?;

    match mode.as_str() {
        "forged-move" => {
            // Ask again and again, so some requests land while the user is
            // dragging in another client.
            run_for(&mut queue, &mut app, Duration::from_secs(8), || {
                toplevel._move(&seat, FORGED_SERIAL);
            })?;
        }
        _ => {
            run_for(&mut queue, &mut app, Duration::from_millis(2500), || {})?;
            surface.attach(None, 0, 0);
            surface.commit();
            run_for(&mut queue, &mut app, Duration::from_millis(2500), || {})?;
            map(&mut queue, &mut app)?;
            run_for(&mut queue, &mut app, Duration::from_secs(4), || {})?;
        }
    }
    Ok(())
}
