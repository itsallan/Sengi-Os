//! X11 programs through Xwayland, against the running compositor: that
//! Xwayland waits for a program to ask for it, that an X11 window is listed
//! with its title and app id, takes the keyboard when it appears and when
//! it is chosen, is framed and maximised like the others, that a menu is
//! neither framed nor focused, and that a closed window leaves the focus
//! where it goes for the others.
//!
//! The X11 side is a client in this test process, written with x11rb. The
//! tests need the `Xwayland` binary. Without it they print that they were
//! skipped and do nothing; the report of a run says whether they ran.

mod common;
mod harness;

use std::{
    collections::HashMap,
    io::Write,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::Command,
    thread::sleep,
    time::{Duration, Instant},
};

use harness::{Compositor, SOON};
use wayland_client::{
    event_created_child,
    globals::{registry_queue_init, GlobalListContents},
    protocol::wl_registry::WlRegistry,
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use x11rb::{
    connection::Connection as _,
    protocol::{
        xproto::{
            AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
            EventMask, PropMode, Window, WindowClass,
        },
        Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
};

/// How long Xwayland may take to come up, and a program to take its key.
const XWAYLAND: Duration = Duration::from_secs(15);
const DESKTOP: u32 = 0x0012_1316;
const X_GREEN: u32 = 0x0020_b020;
const X_BLUE: u32 = 0x0020_20b0;
const MENU: u32 = 0x00ff_00ff;
const WAYLAND_GREEN: u32 = 0xff20_b020;
/// The evdev codes of A and B are 30 and 48; X keycodes are evdev codes plus eight.
const X_KEY_A: u8 = 38;
const X_KEY_B: u8 = 56;
/// Xwayland starts with the session; nothing has to start it.
const START: &str = "";
const RED: u32 = 0x00b0_2020;

static X11_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

thread_local! {
    // Held from the first call in a test until its thread ends.
    static X11_TURN: std::cell::RefCell<Option<std::sync::MutexGuard<'static, ()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Whether Xwayland is installed. When it is, the test also waits for its
/// turn: the compositors of these tests pick their display number from the
/// same /tmp/.X<n>-lock files, and one test killing or cleaning up an
/// Xwayland must not meet another that is starting, so they run one at a time.
fn xwayland_installed(test: &str) -> bool {
    let found = xwayland_found(test);
    if found {
        X11_TURN.with(|turn| {
            *turn.borrow_mut() = Some(X11_TESTS.lock().unwrap_or_else(|held| held.into_inner()));
        });
    }
    found
}

/// Whether the Xwayland binary is on the path. Without it the test says so.
fn xwayland_found(test: &str) -> bool {
    let found = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir: PathBuf| dir.join("Xwayland").is_file())
    });
    if !found && std::env::var_os("CI").is_some() {
        // A skip in CI would hide that the job has no Xwayland.
        panic!("{test}: the Xwayland binary is not installed, and in CI it must be");
    }
    if !found {
        // Written past the test harness's capture, so that a skip shows
        // even when the test passes.
        let _ = writeln!(
            std::io::stderr(),
            "SKIPPED {test}: the Xwayland binary is not installed"
        );
    }
    found
}

/// The display number Xwayland was given, once the compositor says it is up.
fn wait_for_display(compositor: &Compositor) -> u32 {
    let end = Instant::now() + XWAYLAND;
    while Instant::now() < end {
        let number = compositor
            .said("Xwayland is up on :")
            .and_then(|line| line.rsplit(':').next()?.trim().parse().ok());
        if let Some(number) = number {
            return number;
        }
        sleep(Duration::from_millis(20));
    }
    panic!(
        "Xwayland never came up; the compositor said: {:?}",
        compositor.said_all("")
    );
}

/// An X11 client in this process: its connection and what Xwayland has sent it.
struct XClient {
    conn: RustConnection,
    root: Window,
    delete: u32,
    heard: Vec<Event>,
}

impl XClient {
    fn connect(display: u32) -> Self {
        let (conn, screen) =
            x11rb::connect(Some(&format!(":{display}"))).expect("a connection to Xwayland");
        let root = conn.setup().roots[screen].root;
        let delete = conn
            .intern_atom(false, b"WM_DELETE_WINDOW")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        Self {
            conn,
            root,
            delete,
            heard: Vec::new(),
        }
    }

    fn make(&self, x: i16, y: i16, size: u16, colour: u32, menu: bool) -> Window {
        let id = self.conn.generate_id().unwrap();
        let mut aux = CreateWindowAux::new().background_pixel(colour).event_mask(
            EventMask::KEY_PRESS
                | EventMask::FOCUS_CHANGE
                | EventMask::STRUCTURE_NOTIFY
                | EventMask::ENTER_WINDOW
                | EventMask::POINTER_MOTION,
        );
        if menu {
            aux = aux.override_redirect(1);
        }
        self.conn
            .create_window(
                COPY_DEPTH_FROM_PARENT,
                id,
                self.root,
                x,
                y,
                size,
                size,
                0,
                WindowClass::INPUT_OUTPUT,
                COPY_FROM_PARENT,
                &aux,
            )
            .unwrap();
        id
    }

    /// A window of one colour with a title and a class, mapped.
    fn window(&self, x: i16, y: i16, size: u16, colour: u32, title: &str, class: &str) -> Window {
        let id = self.make(x, y, size, colour, false);
        self.conn
            .change_property8(
                PropMode::REPLACE,
                id,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                title.as_bytes(),
            )
            .unwrap();
        let class = format!("{class}\0{class}\0");
        self.conn
            .change_property8(
                PropMode::REPLACE,
                id,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                class.as_bytes(),
            )
            .unwrap();
        let protocols = self
            .conn
            .intern_atom(false, b"WM_PROTOCOLS")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        self.conn
            .change_property32(
                PropMode::REPLACE,
                id,
                protocols,
                AtomEnum::ATOM,
                &[self.delete],
            )
            .unwrap();
        self.conn.map_window(id).unwrap();
        self.conn.flush().unwrap();
        id
    }

    /// A menu as an X11 toolkit makes one: override-redirect, mapped where
    /// it is put.
    fn menu(&self, x: i16, y: i16, size: u16, colour: u32) -> Window {
        let id = self.make(x, y, size, colour, true);
        self.conn.map_window(id).unwrap();
        self.conn.flush().unwrap();
        id
    }

    /// Reads what Xwayland sends for `time`, into `heard`.
    fn pump(&mut self, time: Duration) {
        let end = Instant::now() + time;
        loop {
            self.conn.flush().unwrap();
            while let Some(event) = self.conn.poll_for_event().unwrap() {
                self.heard.push(event);
            }
            if Instant::now() >= end {
                return;
            }
            sleep(Duration::from_millis(10));
        }
    }

    /// Reads until `check` holds of this client, or time runs out.
    fn wait_for(&mut self, time: Duration, check: impl Fn(&XClient) -> bool) -> bool {
        let end = Instant::now() + time;
        loop {
            self.pump(Duration::ZERO);
            if check(self) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            sleep(Duration::from_millis(10));
        }
    }

    /// How many presses of the key with X keycode `code` reached `window`.
    fn keys_to(&self, window: Window, code: u8) -> usize {
        self.heard
            .iter()
            .filter(|event| {
                matches!(event, Event::KeyPress(e) if e.event == window && e.detail == code)
            })
            .count()
    }

    fn was_asked_to_close(&self, window: Window) -> bool {
        self.heard.iter().any(|event| match event {
            Event::ClientMessage(e) => e.window == window && e.data.as_data32()[0] == self.delete,
            _ => false,
        })
    }

    /// Where the window's top left corner is on the desktop, and its size.
    /// Xwayland puts a window inside a frame of its own, so its position
    /// within its parent is not the place on the desktop.
    fn size(&self, window: Window) -> (i16, i16, u16, u16) {
        let g = self.conn.get_geometry(window).unwrap().reply().unwrap();
        let at = self
            .conn
            .translate_coordinates(window, self.root, 0, 0)
            .unwrap()
            .reply()
            .unwrap();
        (at.dst_x, at.dst_y, g.width, g.height)
    }
}

/// Asks the window manager, as a client does, to change a window's state.
fn ask(x: &XClient, window: Window, kind: &str, data: [u32; 5]) {
    let atom = |name: &str| {
        x.conn
            .intern_atom(false, name.as_bytes())
            .unwrap()
            .reply()
            .unwrap()
            .atom
    };
    let event = ClientMessageEvent::new(32, window, atom(kind), data);
    x.conn
        .send_event(
            false,
            x.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .unwrap();
    x.conn.flush().unwrap();
}

fn atom(x: &XClient, name: &str) -> u32 {
    x.conn
        .intern_atom(false, name.as_bytes())
        .unwrap()
        .reply()
        .unwrap()
        .atom
}

/// Reads a screenshot the compositor saved: 8-bit RGB, deflate stored blocks,
/// as `src/png.rs` writes them. Gives the width, the height and the pixels
/// as 0xRRGGBB.
fn read_png(path: &std::path::Path) -> (u32, u32, Vec<u32>) {
    let data = std::fs::read(path).expect("a screenshot");
    let be = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
    let (width, height) = (be(16), be(20));
    let mut at = 8;
    let mut zlib = Vec::new();
    while at + 8 <= data.len() {
        let len = be(at) as usize;
        if &data[at + 4..at + 8] == b"IDAT" {
            zlib.extend_from_slice(&data[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    let mut raw = Vec::new();
    let mut at = 2;
    loop {
        let last = zlib[at] & 1 == 1;
        let len = u16::from_le_bytes([zlib[at + 1], zlib[at + 2]]) as usize;
        raw.extend_from_slice(&zlib[at + 5..at + 5 + len]);
        at += 5 + len;
        if last {
            break;
        }
    }
    let row = width as usize * 3 + 1;
    let pixels = (0..height as usize)
        .flat_map(|y| {
            let line = &raw[y * row + 1..(y + 1) * row];
            (0..width as usize)
                .map(|x| {
                    let p = &line[x * 3..x * 3 + 3];
                    u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])
                })
                .collect::<Vec<_>>()
        })
        .collect();
    (width, height, pixels)
}

/// A file name of this test run that nothing has written yet.
fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("sengi-x11-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// What `path` holds once something has written to it.
fn contents(path: &std::path::Path, time: Duration) -> Option<String> {
    let end = Instant::now() + time;
    while Instant::now() < end {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.ends_with('\n') {
                return Some(text);
            }
        }
        sleep(Duration::from_millis(20));
    }
    None
}

/// The ids of the processes called `name` whose parent is `parent`, read
/// from /proc.
fn children_named(parent: u32, name: &str) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let comm = stat.split_once('(')?.1.split_once(')')?.0;
            let rest = stat.rsplit_once(')')?.1;
            let ppid: u32 = rest.split_whitespace().nth(1)?.parse().ok()?;
            (ppid == parent && comm == name).then_some(pid)
        })
        .collect()
}

/// What the compositor lists as its windows, by title and app id, as the
/// dock sees them through wlr-foreign-toplevel-management.
#[derive(Default)]
struct Listed {
    windows: HashMap<u32, (String, String)>,
}

impl Dispatch<WlRegistry, GlobalListContents> for Listed {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Listed {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            state
                .windows
                .entry(toplevel.id().protocol_id())
                .or_default();
        }
    }

    event_created_child!(Listed, ZwlrForeignToplevelManagerV1, [
        0 => (ZwlrForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Listed {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id().protocol_id();
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                state.windows.entry(id).or_default().0 = title;
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state.windows.entry(id).or_default().1 = app_id;
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                state.windows.remove(&id);
            }
            _ => {}
        }
    }
}

/// A connection of the test to the compositor's window list.
struct WindowList {
    queue: EventQueue<Listed>,
    listed: Listed,
}

impl WindowList {
    fn connect(compositor: &Compositor) -> Self {
        let stream = UnixStream::connect(&compositor.socket).expect("the compositor's socket");
        let connection = Connection::from_socket(stream).expect("a Wayland connection");
        let (globals, mut queue) =
            registry_queue_init::<Listed>(&connection).expect("the registry");
        let qh = queue.handle();
        let _manager: ZwlrForeignToplevelManagerV1 = globals
            .bind(&qh, 1..=3, ())
            .expect("the compositor lists its windows");
        let mut listed = Listed::default();
        queue.roundtrip(&mut listed).expect("a roundtrip");
        Self { queue, listed }
    }

    /// Whether a window with this title and app id is listed within `time`.
    fn lists(&mut self, title: &str, app_id: &str, time: Duration) -> bool {
        let end = Instant::now() + time;
        loop {
            self.queue.roundtrip(&mut self.listed).expect("a roundtrip");
            if self
                .listed
                .windows
                .values()
                .any(|(t, a)| t == title && a == app_id)
            {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            sleep(Duration::from_millis(20));
        }
    }
}

/// Not a test of its own: the compositor starts this test program as the
/// screen locker when a test asks for the lock, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

#[test]
fn xwayland_starts_with_the_session_and_has_no_abstract_socket() {
    if !xwayland_installed("xwayland_starts_with_the_session_and_has_no_abstract_socket") {
        return;
    }
    let compositor = Compositor::start(None);
    let display = wait_for_display(&compositor);
    assert!(
        compositor.said("Xwayland took").is_some(),
        "the time to ready is not logged"
    );
    let sockets = std::fs::read_to_string("/proc/net/unix").unwrap();
    let path = format!("/tmp/.X11-unix/X{display}");
    assert!(
        sockets.lines().any(|line| line.ends_with(&path)),
        "the filesystem socket {path} is not listening"
    );
    assert!(
        !sockets.lines().any(|line| line.ends_with(&format!("@{path}"))),
        "Xwayland listens on the abstract socket @{path}, which any process in the network namespace can reach"
    );
}

#[test]
fn nothing_starts_xwayland_on_the_login_screen() {
    if !xwayland_installed("nothing_starts_xwayland_on_the_login_screen") {
        return;
    }
    // A program is started, which would start Xwayland anywhere else.
    let mut compositor =
        Compositor::start_login_screen(None, Some("300 spawn true\n3500 exit\n"), &[]);
    sleep(Duration::from_millis(1800));
    assert!(
        children_named(compositor.pid(), "Xwayland").is_empty(),
        "an Xwayland process runs on the login screen"
    );
    assert!(compositor.said_all("Xwayland").is_empty());
    assert!(compositor.ended());
}

#[test]
fn programs_get_the_display_whether_started_by_the_session_or_later() {
    if !xwayland_installed("programs_get_the_display_whether_started_by_the_session_or_later") {
        return;
    }
    // The startup program is not held for Xwayland, which takes a moment to
    // report ready; its DISPLAY is the one Xwayland comes up on.
    let (later, startup) = (scratch("display-later"), scratch("display-startup"));
    let script = format!("2500 spawn echo $DISPLAY > {}\n", later.display());
    let startup_command = format!("echo $DISPLAY > {}", startup.display());
    let compositor = Compositor::start_args(&["-s", &startup_command], Some(&script));
    let display = wait_for_display(&compositor);
    let want = format!(":{display}\n");
    assert_eq!(contents(&startup, XWAYLAND).as_deref(), Some(want.as_str()));
    assert_eq!(contents(&later, XWAYLAND).as_deref(), Some(want.as_str()));
}

#[test]
fn programs_are_not_held_for_a_slow_xwayland_and_get_its_display() {
    // A fake Xwayland that never reports ready, first on the path.
    let dir = std::env::temp_dir().join(format!("sengi-x11-fake-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("Xwayland");
    std::fs::write(&fake, "#!/bin/sh\nexec sleep 20\n").unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let (startup, early, late) = (
        scratch("display-slow-startup"),
        scratch("display-slow-early"),
        scratch("display-slow-late"),
    );
    let startup_command = format!("echo \"[$DISPLAY]\" > {}", startup.display());
    let script = format!(
        "500 spawn echo \"[$DISPLAY]\" > {}\n7500 spawn echo \"[$DISPLAY]\" > {}\n",
        early.display(),
        late.display()
    );
    let started = Instant::now();
    let compositor =
        Compositor::start_args_in(&["-s", &startup_command], Some(&script), &[("PATH", &path)]);
    // The display is known before Xwayland has said anything.
    let end = Instant::now() + Duration::from_secs(3);
    let mut announced = None;
    while announced.is_none() && Instant::now() < end {
        announced = compositor
            .said("Xwayland starting on :")
            .and_then(|line| line.rsplit(':').next()?.trim().parse::<u32>().ok());
        sleep(Duration::from_millis(20));
    }
    let number = announced.expect("the display number is not announced at the start");
    let want = format!("[:{number}]\n");

    // Neither the startup program nor one started a moment later waited for
    // the five seconds Xwayland is given: both ran, with its DISPLAY.
    assert_eq!(
        contents(&startup, Duration::from_secs(3)).as_deref(),
        Some(want.as_str())
    );
    assert_eq!(
        contents(&early, Duration::from_secs(3)).as_deref(),
        Some(want.as_str())
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "a program waited for Xwayland"
    );

    // Once Xwayland is given up, what is started after that has no display.
    assert_eq!(
        contents(&late, Duration::from_secs(12)).as_deref(),
        Some("[]\n")
    );
    assert!(compositor.said("did not report ready in time").is_some());
    assert!(compositor.status().contains("session"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_x11_window_is_listed_framed_and_takes_the_keyboard() {
    if !xwayland_installed("an_x11_window_is_listed_framed_and_takes_the_keyboard") {
        return;
    }
    // The key comes well after the window has appeared; it can only reach
    // the program if the program has the keyboard.
    let script = format!("{START}4000 key a\n");
    let compositor = Compositor::start_in(None, None, Some(&script), &[]);
    let display = wait_for_display(&compositor);
    let mut list = WindowList::connect(&compositor);

    let mut x = XClient::connect(display);
    let window = x.window(300, 200, 300, X_GREEN, "x11 test", "SengiXTest");
    assert!(
        list.lists("x11 test", "SengiXTest", SOON),
        "the X11 window is not listed with its title and app id"
    );

    // Framed like a Wayland window: the pixel above the client area is not
    // the desktop.
    let (x0, y0, _, _) = x.size(window);
    let mut app = compositor.connect();
    let picture = app.capture().expect("a picture of the screen");
    let above = picture.at(x0 as u32 + 100, (y0 - 4) as u32);
    assert_ne!(above & 0x00ff_ffff, DESKTOP, "no frame was drawn");

    assert!(
        x.wait_for(XWAYLAND, |x| x.keys_to(window, X_KEY_A) >= 1),
        "the key did not reach the X11 window: heard {:?}",
        x.heard
            .iter()
            .map(|e| format!("{e:?}").chars().take(40).collect::<String>())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_menu_of_an_x11_toolkit_is_not_framed_or_focused() {
    if !xwayland_installed("a_menu_of_an_x11_toolkit_is_not_framed_or_focused") {
        return;
    }
    let script = format!("{START}4000 key a\n");
    let compositor = Compositor::start_in(None, None, Some(&script), &[]);
    let display = wait_for_display(&compositor);
    let mut list = WindowList::connect(&compositor);
    let mut x = XClient::connect(display);
    let window = x.window(400, 300, 200, X_GREEN, "parent", "SengiXTest");
    // Placed once the compositor has listed it; until then X still has it
    // where the program first put it.
    assert!(list.lists("parent", "SengiXTest", SOON));
    let (wx, wy, _, _) = x.size(window);

    let menu = x.menu(wx + 20, wy + 40, 60, MENU);
    x.pump(Duration::from_millis(500));
    let mut app = compositor.connect();
    let picture = app.capture().expect("a picture of the screen");
    assert_eq!(
        picture.at((wx + 50) as u32, (wy + 70) as u32) & 0x00ff_ffff,
        MENU,
        "the menu is not drawn where it was put"
    );
    assert_eq!(
        picture.at((wx + 50) as u32, (wy + 36) as u32) & 0x00ff_ffff,
        X_GREEN,
        "the menu has a frame of its own"
    );

    // The key the script sends goes to the window, not to the menu.
    assert!(
        x.wait_for(XWAYLAND, |x| x.keys_to(window, X_KEY_A) >= 1),
        "the key did not reach the window that has the keyboard"
    );
    assert_eq!(x.keys_to(menu, X_KEY_A), 0, "the menu took the keyboard");
}

#[test]
fn an_x11_window_maximises_with_super_a() {
    if !xwayland_installed("an_x11_window_maximises_with_super_a") {
        return;
    }
    let script = format!("{START}4000 key super+a\n");
    let compositor = Compositor::start_in(None, None, Some(&script), &[]);
    let display = wait_for_display(&compositor);
    let mut x = XClient::connect(display);
    let window = x.window(300, 200, 300, X_BLUE, "maximise", "SengiXTest");
    x.pump(Duration::from_millis(500));
    let before = x.size(window).2;
    assert!(
        x.wait_for(XWAYLAND, |x| x.size(window).2 > before + 300),
        "super+a did not maximise the X11 window (width {before})"
    );
    drop(compositor);
}

#[test]
fn super_q_closes_an_x11_window_and_the_focus_goes_on() {
    if !xwayland_installed("super_q_closes_an_x11_window_and_the_focus_goes_on") {
        return;
    }
    // Super+Q closes the window that has the keyboard: the program is asked
    // with WM_DELETE_WINDOW, and the next window gets the keyboard once the
    // program has gone away. The key after that proves where the keyboard is.
    let script = format!("{START}4000 key super+q\n7000 key a\n");
    let compositor = Compositor::start_in(None, None, Some(&script), &[]);
    let display = wait_for_display(&compositor);
    let mut list = WindowList::connect(&compositor);
    let mut x = XClient::connect(display);
    let first = x.window(200, 200, 200, X_GREEN, "first", "SengiXTest");
    let second = x.window(500, 200, 200, X_BLUE, "second", "SengiXTest");
    assert!(list.lists("second", "SengiXTest", SOON));

    assert!(
        x.wait_for(XWAYLAND, |x| x.was_asked_to_close(second)),
        "the close key did not reach the window with the keyboard"
    );
    x.conn
        .destroy_window(second)
        .expect("the window is destroyed");
    x.conn.flush().unwrap();
    assert!(
        x.wait_for(XWAYLAND, |x| x.keys_to(first, X_KEY_A) >= 1),
        "the keyboard did not go to the window before the closed one"
    );
    assert_eq!(
        x.keys_to(second, X_KEY_A),
        0,
        "the closed window took a key"
    );
    assert!(
        !list.lists("second", "SengiXTest", Duration::from_millis(300)),
        "the closed window is still listed"
    );
    // The compositor is still there and still answers.
    assert!(compositor.status().contains("session"));
}

#[test]
fn alt_tab_moves_between_a_wayland_window_and_an_x11_window() {
    if !xwayland_installed("alt_tab_moves_between_a_wayland_window_and_an_x11_window") {
        return;
    }
    // Alt+Tab goes from the X11 window to the Wayland one, and the next
    // Alt+Tab back to the X11 window; a key after each shows who has it.
    let script = format!(
        "{START}3000 keydown alt\n3100 key tab\n3300 keyup alt\n3600 key b\n5000 keydown alt\n5100 key tab\n5300 keyup alt\n5600 key a\n"
    );
    let compositor = Compositor::start_in(None, None, Some(&script), &[]);
    let mut wayland = compositor.connect();
    wayland.keyboard().unwrap();
    let _window = wayland.window(WAYLAND_GREEN).unwrap();
    assert!(wayland.wait(SOON, |seen| seen.keyboard_enters >= 1));

    let display = wait_for_display(&compositor);
    let mut x = XClient::connect(display);
    let xwindow = x.window(600, 300, 200, X_GREEN, "x11 side", "SengiXTest");
    x.pump(Duration::from_millis(200));

    assert!(
        wayland.wait(Duration::from_secs(8), |seen| seen.keyboard_enters >= 2),
        "the first Alt+Tab did not move the focus to the Wayland window"
    );
    // The key after the second Alt+Tab is the X11 window's; the key sent
    // while the Wayland window had the keyboard went there, not here.
    assert!(
        x.wait_for(Duration::from_secs(8), |x| x.keys_to(xwindow, X_KEY_A) >= 1),
        "the second Alt+Tab did not move the keyboard back to the X11 window"
    );
    assert_eq!(
        x.keys_to(xwindow, X_KEY_B),
        0,
        "the key sent while the Wayland window had the keyboard reached the X11 window"
    );
}

#[test]
fn a_dead_xwayland_takes_its_windows_and_the_next_program_brings_it_back() {
    if !xwayland_installed("a_dead_xwayland_takes_its_windows_and_the_next_program_brings_it_back")
    {
        return;
    }
    // A second program starts at six seconds, after Xwayland has been killed:
    // it has to start Xwayland again.
    let script = format!("{START}6000 spawn true\n");
    let compositor = Compositor::start_in(None, None, Some(&script), &[]);
    let display = wait_for_display(&compositor);
    let mut list = WindowList::connect(&compositor);
    let x = XClient::connect(display);
    x.window(300, 200, 200, X_GREEN, "doomed", "SengiXTest");
    assert!(list.lists("doomed", "SengiXTest", SOON));

    let running = children_named(compositor.pid(), "Xwayland");
    assert_eq!(running.len(), 1, "expected one Xwayland, found {running:?}");
    let socket = std::path::PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
    let lock = std::path::PathBuf::from(format!("/tmp/.X{display}-lock"));
    Command::new("kill")
        .args(["-KILL", &running[0].to_string()])
        .status()
        .expect("kill runs");

    let end = Instant::now() + SOON;
    while Instant::now() < end && list.lists("doomed", "SengiXTest", Duration::ZERO) {
        sleep(Duration::from_millis(20));
    }
    assert!(
        !list.lists("doomed", "SengiXTest", Duration::ZERO),
        "the window of a dead Xwayland is still listed"
    );
    assert!(
        compositor.status().contains("session"),
        "the compositor did not survive"
    );

    // The dead process is collected, and what it left in /tmp is removed.
    let end = Instant::now() + SOON;
    while Instant::now() < end
        && (!children_named(compositor.pid(), "Xwayland").is_empty()
            || socket.exists()
            || lock.exists())
    {
        sleep(Duration::from_millis(20));
    }
    assert!(
        children_named(compositor.pid(), "Xwayland").is_empty(),
        "the dead Xwayland is a zombie"
    );
    assert!(!socket.exists(), "{} was left behind", socket.display());
    assert!(!lock.exists(), "{} was left behind", lock.display());

    let end = Instant::now() + XWAYLAND;
    while Instant::now() < end && compositor.said_all("Xwayland is up on").len() < 2 {
        sleep(Duration::from_millis(20));
    }
    assert_eq!(
        compositor.said_all("Xwayland is up on").len(),
        2,
        "the program started after the death did not bring Xwayland back"
    );
}

#[test]
fn a_window_asking_for_a_huge_size_is_held_to_the_screen() {
    if !xwayland_installed("a_window_asking_for_a_huge_size_is_held_to_the_screen") {
        return;
    }
    let compositor = Compositor::start(None);
    let display = wait_for_display(&compositor);
    let mut list = WindowList::connect(&compositor);
    let mut x = XClient::connect(display);
    let window = x.window(300, 200, 200, X_GREEN, "huge", "SengiXTest");
    assert!(list.lists("huge", "SengiXTest", SOON));
    x.conn
        .configure_window(window, &ConfigureWindowAux::new().width(5000).height(5000))
        .unwrap();
    x.conn.flush().unwrap();
    assert!(
        x.wait_for(SOON, |x| x.size(window).2 > 400),
        "it did not grow"
    );
    x.pump(Duration::from_millis(300));
    let (_, _, w, h) = x.size(window);
    assert!(
        w <= 1280 && h <= 720,
        "the window is {w} by {h}, past the screen"
    );
}

#[test]
fn an_x11_window_opens_on_the_output_of_the_pointer_and_is_held_to_it() {
    if !xwayland_installed("an_x11_window_opens_on_the_output_of_the_pointer_and_is_held_to_it") {
        return;
    }
    let compositor = Compositor::start_args(&["--outputs", "1280x720,800x600"], None);
    let display = wait_for_display(&compositor);
    compositor.input("move 1500 300");
    let mut list = WindowList::connect(&compositor);
    let mut x = XClient::connect(display);
    let window = x.window(300, 200, 200, X_GREEN, "two", "SengiXTest");
    assert!(list.lists("two", "SengiXTest", SOON));
    let mut app = compositor.connect();
    let green = |picture: &common::apps::Picture| {
        (0..picture.height)
            .flat_map(|y| (0..picture.width).map(move |x| (x, y)))
            .filter(|(x, y)| picture.at(*x, *y) == X_GREEN)
            .count()
    };
    let second = app.capture_named("HEADLESS-2").unwrap();
    let first = app.capture_named("HEADLESS-1").unwrap();
    assert!(green(&second) > 0, "the window is not on the second output");
    assert_eq!(green(&first), 0, "it is on the first");
    // Asking for more than there is, it gets what the second output has,
    // whichever output the pointer is on by then.
    compositor.input("move 100 100");
    x.conn
        .configure_window(window, &ConfigureWindowAux::new().width(5000).height(5000))
        .unwrap();
    x.conn.flush().unwrap();
    assert!(
        x.wait_for(SOON, |x| x.size(window).2 > 400),
        "it did not grow"
    );
    x.pump(Duration::from_millis(300));
    let (_, _, w, h) = x.size(window);
    assert!(
        w <= 800 && h <= 600,
        "the window is {w} by {h}, past its output"
    );
}

#[test]
fn an_x11_window_can_be_made_fullscreen() {
    if !xwayland_installed("an_x11_window_can_be_made_fullscreen") {
        return;
    }
    let compositor = Compositor::start(None);
    let display = wait_for_display(&compositor);
    let mut list = WindowList::connect(&compositor);
    let mut x = XClient::connect(display);
    let window = x.window(300, 200, 200, X_GREEN, "full", "SengiXTest");
    assert!(list.lists("full", "SengiXTest", SOON));
    let fullscreen = atom(&x, "_NET_WM_STATE_FULLSCREEN");
    ask(&x, window, "_NET_WM_STATE", [1, fullscreen, 0, 1, 0]);
    assert!(
        x.wait_for(SOON, |x| {
            let (_, _, w, h) = x.size(window);
            (w, h) == (1280, 720)
        }),
        "the window did not fill the screen: {:?}",
        x.size(window)
    );
}

#[test]
fn an_x11_window_can_be_minimised() {
    if !xwayland_installed("an_x11_window_can_be_minimised") {
        return;
    }
    let compositor = Compositor::start(None);
    let display = wait_for_display(&compositor);
    let mut list = WindowList::connect(&compositor);
    let x = XClient::connect(display);
    let window = x.window(300, 200, 200, X_GREEN, "mini", "SengiXTest");
    assert!(list.lists("mini", "SengiXTest", SOON));
    let (wx, wy, _, _) = x.size(window);
    let mut app = compositor.connect();
    let at = ((wx + 100) as u32, (wy + 100) as u32);
    assert_eq!(app.capture().unwrap().at(at.0, at.1), X_GREEN);
    ask(&x, window, "WM_CHANGE_STATE", [3, 0, 0, 0, 0]);
    let end = Instant::now() + SOON;
    while Instant::now() < end && app.capture().unwrap().at(at.0, at.1) != DESKTOP {
        sleep(Duration::from_millis(100));
    }
    assert_eq!(
        app.capture().unwrap().at(at.0, at.1),
        DESKTOP,
        "still drawn"
    );
}

#[test]
fn nothing_of_the_x11_session_shows_or_hears_while_the_session_is_locked() {
    if !xwayland_installed("nothing_of_the_x11_session_shows_or_hears_while_the_session_is_locked")
    {
        return;
    }
    // While locked, at five seconds on: a key, then the pointer across the
    // window. Ten seconds on, after the unlock: another key.
    let shot = scratch("locked.png");
    let script = format!(
        "4500 screenshot {}\n5000 key a\n5200 move 640 360\n5300 move 700 400\n5400 move 100 650\n9500 key b\n",
        shot.display()
    );
    let compositor = Compositor::start_with(None, None, Some(&script));
    let t0 = Instant::now();
    let display = wait_for_display(&compositor);
    let mut list = WindowList::connect(&compositor);
    let mut x = XClient::connect(display);
    let window = x.window(300, 200, 300, X_GREEN, "locked", "SengiXTest");
    assert!(list.lists("locked", "SengiXTest", SOON));
    let (wx, wy, _, _) = x.size(window);
    let _menu = x.menu(wx + 20, wy + 40, 60, MENU);
    x.pump(Duration::from_millis(500));
    let mut app = compositor.connect();
    let before = app.capture().unwrap();
    assert_eq!(before.at((wx + 50) as u32, (wy + 70) as u32), MENU);

    let mut locker = compositor.locker();
    locker.keyboard().unwrap();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, 0xffb0_2020).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    assert!(
        locker.wait(SOON, |seen| seen.keyboard_enters >= 1),
        "the lock surface has no keyboard"
    );

    // A window mapped while locked is not shown either.
    let second = x.window(100, 100, 200, X_BLUE, "second", "SengiXTest");
    x.pump(Duration::from_millis(800));
    // Mapping a window makes Xwayland say the pointer is in it; what counts
    // is what comes after this, from the script's key and pointer moves.
    let quiet = x.heard.len();
    assert!(
        t0.elapsed() < Duration::from_millis(4500),
        "the setup was too slow for the script"
    );

    // The script's key and pointer moves arrive while it is locked.
    while t0.elapsed() < Duration::from_millis(7000) {
        x.pump(Duration::from_millis(50));
        locker.roundtrip().unwrap();
    }
    assert!(
        locker.seen.keys >= 1,
        "the key did not reach the lock screen"
    );
    let (width, height, pixels) = read_png(&shot);
    for (n, pixel) in pixels.iter().enumerate() {
        assert!(
            *pixel != X_GREEN && *pixel != X_BLUE && *pixel != MENU,
            "{pixel:06x} of an X11 window is drawn at {},{} while locked",
            n as u32 % width,
            n as u32 / width
        );
    }
    assert_eq!(
        pixels[(height as usize / 2) * width as usize + width as usize / 2],
        RED
    );
    let heard: Vec<String> = x.heard[quiet..]
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::KeyPress(_) | Event::EnterNotify(_) | Event::MotionNotify(_)
            )
        })
        .map(|e| format!("{e:?}"))
        .collect();
    assert!(
        heard.is_empty(),
        "an X11 client heard input while locked: {heard:?}"
    );

    // After the unlock the window that was in front has the keyboard, and
    // the key typed while locked is not delivered late. The pointer was
    // moved off the windows: over the menu, Xwayland hands the keys to it.
    lock.unlock_and_destroy();
    locker.roundtrip().unwrap();
    assert!(
        x.wait_for(XWAYLAND, |x| x.keys_to(second, X_KEY_B) >= 1),
        "the window in front did not get the keyboard after the unlock"
    );
    assert_eq!(x.keys_to(window, X_KEY_A) + x.keys_to(second, X_KEY_A), 0);
}
