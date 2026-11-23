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
            AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Window, WindowClass,
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
/// Starts Xwayland, as a user's launch does, at the start of a run.
const START: &str = "100 spawn true\n";

/// Whether the Xwayland binary is on the path. Without it the test says so.
fn xwayland_installed(test: &str) -> bool {
    let found = std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir: PathBuf| dir.join("Xwayland").is_file())
    });
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
            EventMask::KEY_PRESS | EventMask::FOCUS_CHANGE | EventMask::STRUCTURE_NOTIFY,
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

#[test]
fn xwayland_is_not_started_until_a_program_asks_for_it() {
    if !xwayland_installed("xwayland_is_not_started_until_a_program_asks_for_it") {
        return;
    }
    // Nothing is started; the session ends on its own after two seconds.
    let mut compositor = Compositor::start_in(None, None, Some("2000 exit\n"), &[]);
    assert!(
        compositor.ended(),
        "the compositor did not end on its script"
    );
    assert!(
        compositor.said_all("Xwayland").is_empty(),
        "Xwayland was started with no program asking for it: {:?}",
        compositor.said_all("Xwayland")
    );
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
