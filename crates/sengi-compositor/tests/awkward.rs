//! Things done to windows at awkward moments: during a drag, with a menu
//! open, while screens come and go, and to windows that are just closing.
//! Each of these once left the desktop stuck or a window out of reach.

mod common;
mod harness;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use common::{Client, Seen};
use harness::{Compositor, SOON};
use wayland_client::{
    event_created_child, protocol::wl_surface::WlSurface, Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::XdgWmBase,
};
use wayland_protocols_wlr::{
    foreign_toplevel::v1::client::{
        zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
        zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
    },
    layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1::KeyboardInteractivity},
};

const GREEN: u32 = 0xff20_b020;
const RED: u32 = 0xffb0_2020;
const GREY: u32 = 0xff80_8080;

/// Not run by a plain `cargo test`: the compositor starts this test binary
/// as the screen locker, with `--include-ignored`, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

/// The line the compositor reports for the window called `title`.
fn window_line(compositor: &Compositor, title: &str) -> String {
    let report = compositor.control("inspect");
    let wanted = format!("window title={title} ");
    report
        .lines()
        .find(|line| line.starts_with(&wanted))
        .unwrap_or_else(|| panic!("no window {title} in {report}"))
        .to_string()
}

/// A `name=a,b,..` fact of a line.
fn numbers(line: &str, name: &str) -> Vec<i32> {
    line.split(' ')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("no {name} in {line}"))
        .split(',')
        .map(|n| n.parse().expect("a number"))
        .collect()
}

/// The compositor answers, and nothing it remembers disagrees.
fn sound(compositor: &Compositor) -> String {
    let report = compositor.control("inspect");
    assert!(report.starts_with("ok"), "no answer: `{report}`");
    assert!(
        !report.lines().any(|line| line.starts_with("broken")),
        "{report}"
    );
    report
}

/// A window with a title, made by hand so that menus can hang off it.
struct Window {
    surface: WlSurface,
    shell: XdgSurface,
    toplevel: XdgToplevel,
}

fn window(app: &mut Client, title: &str, argb: u32) -> Window {
    let wm_base: XdgWmBase = app.globals.bind(&app.handle, 1..=2, ()).unwrap();
    let surface = app.compositor.create_surface(&app.handle, ());
    let shell = wm_base.get_xdg_surface(&surface, &app.handle, ());
    let toplevel = shell.get_toplevel(&app.handle, ());
    toplevel.set_title(title.into());
    let before = app.seen.window_configures;
    surface.commit();
    assert!(app.wait(SOON, |seen| seen.window_configures > before));
    app.paint(&surface, 300, 200, argb).unwrap();
    Window {
        surface,
        shell,
        toplevel,
    }
}

type Dismissed = Arc<AtomicBool>;

struct Menu {
    popup: XdgPopup,
    shell: XdgSurface,
    surface: WlSurface,
    dismissed: Dismissed,
}

impl Menu {
    fn destroy(self) {
        self.popup.destroy();
        self.shell.destroy();
        self.surface.destroy();
    }
}

/// Presses the left button in the middle of `window` and returns the serial
/// the program was given for it. The button stays down.
fn press_on(compositor: &Compositor, app: &mut Client, title: &str) -> u32 {
    let client = numbers(&window_line(compositor, title), "client");
    let (x, y) = (client[0] + client[2] / 2, client[1] + client[3] / 2);
    compositor.input(&format!("move {x} {y}"));
    let before = app.seen.more.button_serial;
    compositor.input("press left");
    assert!(app.wait(SOON, |seen| seen.more.button_serial != before));
    app.seen.more.button_serial.unwrap()
}

/// A menu of `parent` that holds the keyboard and the pointer.
fn menu(app: &mut Client, parent: &Window, serial: u32) -> Menu {
    let seat = app.seat().unwrap();
    let wm_base: XdgWmBase = app.globals.bind(&app.handle, 1..=2, ()).unwrap();
    let positioner = wm_base.create_positioner(&app.handle, ());
    positioner.set_size(80, 60);
    positioner.set_anchor_rect(10, 10, 1, 1);
    let surface = app.compositor.create_surface(&app.handle, ());
    let shell = wm_base.get_xdg_surface(&surface, &app.handle, ());
    let dismissed = Dismissed::default();
    let popup = shell.get_popup(
        Some(&parent.shell),
        &positioner,
        &app.handle,
        dismissed.clone(),
    );
    popup.grab(&seat, serial);
    let before = app.seen.window_configures;
    surface.commit();
    assert!(app.wait(SOON, |seen| seen.window_configures > before));
    app.paint(&surface, 80, 60, GREY).unwrap();
    Menu {
        popup,
        shell,
        surface,
        dismissed,
    }
}

fn a_program(compositor: &Compositor) -> Client {
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    app.pointer().unwrap();
    app
}

/// Whether a key pressed now reaches `app`.
fn types_into(compositor: &Compositor, app: &mut Client) -> bool {
    app.roundtrip().unwrap();
    let before = app.seen.keys;
    compositor.input("key a");
    app.wait(SOON, |seen| seen.keys > before)
}

fn pull_the_only_screen_during(drag_from: fn(&[i32]) -> (i32, i32)) {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let _window = window(&mut app, "dragged", GREEN);
    let frame = numbers(&window_line(&compositor, "dragged"), "frame");
    let (x, y) = drag_from(&frame);
    compositor.input(&format!("move {x} {y}"));
    compositor.input("press left");
    compositor.input(&format!("move {} {}", x + 20, y + 20));
    compositor.unplug("HEADLESS-1");
    // The mouse goes on moving with nothing to show it on.
    compositor.input(&format!("move {} {}", x + 60, y + 40));
    sound(&compositor);
    compositor.input("release left");
    compositor.plug("1280x720");
    sound(&compositor);
    app.roundtrip().unwrap();
}

#[test]
fn a_move_goes_on_while_the_only_screen_is_pulled_out() {
    // By the title bar.
    pull_the_only_screen_during(|frame| (frame[0] + frame[2] / 2, frame[1] + 12));
}

#[test]
fn a_resize_goes_on_while_the_only_screen_is_pulled_out() {
    // By the right edge.
    pull_the_only_screen_during(|frame| (frame[0] + frame[2] - 1, frame[1] + frame[3] / 2));
}

#[test]
fn a_move_asked_for_with_the_press_a_menu_holds_does_not_stick() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let seat = app.seat().unwrap();
    let parent = window(&mut app, "parent", GREEN);
    let serial = press_on(&compositor, &mut app, "parent");
    let held = menu(&mut app, &parent, serial);
    parent.toplevel._move(&seat, serial);
    app.roundtrip().unwrap();
    compositor.input("release left");
    held.destroy();
    app.roundtrip().unwrap();
    compositor.input("move 5 700");
    let report = sound(&compositor);
    assert!(report.contains("drag=none"), "{report}");
    assert!(report.contains("pointer_grabbed=0"), "{report}");
}

/// The dock's side of the window list: the handles, each with its title.
type Handles = Arc<Mutex<Vec<ZwlrForeignToplevelHandleV1>>>;
type Title = Arc<Mutex<String>>;

fn dock(compositor: &Compositor) -> (Client, Handles) {
    let mut dock = compositor.connect();
    let handles = Handles::default();
    let _manager: ZwlrForeignToplevelManagerV1 = dock
        .globals
        .bind(&dock.handle, 1..=3, handles.clone())
        .unwrap();
    dock.roundtrip().unwrap();
    (dock, handles)
}

fn handle(dock: &mut Client, handles: &Handles, title: &str) -> ZwlrForeignToplevelHandleV1 {
    dock.roundtrip().unwrap();
    dock.roundtrip().unwrap();
    handles
        .lock()
        .unwrap()
        .iter()
        .find(|handle| {
            handle
                .data::<Title>()
                .is_some_and(|known| *known.lock().unwrap() == title)
        })
        .cloned()
        .unwrap_or_else(|| panic!("{title} is not listed"))
}

/// Sends the compositor a window that closes and, in the same turn of its
/// loop, the dock's click on it.
fn click_the_dock_on_a_window_as_it_closes(minimised: bool) {
    let compositor = Compositor::start(None);
    let mut other = a_program(&compositor);
    let _other = window(&mut other, "other", RED);
    let mut app = a_program(&compositor);
    let closing = window(&mut app, "closing", GREEN);
    let (mut dock, handles) = dock(&compositor);
    let seat = dock.seat().unwrap();
    let wanted = handle(&mut dock, &handles, "closing");
    if minimised {
        wanted.set_minimized();
        dock.roundtrip().unwrap();
    }
    compositor.signal("STOP");
    closing.toplevel.destroy();
    closing.shell.destroy();
    closing.surface.destroy();
    app.queue.flush().unwrap();
    wanted.activate(&seat);
    dock.queue.flush().unwrap();
    compositor.signal("CONT");
    app.roundtrip().unwrap();
    dock.roundtrip().unwrap();
    sound(&compositor);
    assert!(
        types_into(&compositor, &mut other),
        "the keyboard went with the window that closed"
    );
}

#[test]
fn a_click_in_the_dock_on_a_window_that_is_closing_changes_nothing() {
    click_the_dock_on_a_window_as_it_closes(false);
}

#[test]
fn a_click_in_the_dock_on_a_minimised_window_that_is_closing_changes_nothing() {
    click_the_dock_on_a_window_as_it_closes(true);
}

impl Dispatch<XdgPositioner, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &XdgPositioner,
        _: xdg_positioner::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgPopup, Dismissed> for Seen {
    fn event(
        _: &mut Self,
        _: &XdgPopup,
        event: xdg_popup::Event,
        dismissed: &Dismissed,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = event {
            dismissed.store(true, Ordering::Relaxed);
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, Handles> for Seen {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        handles: &Handles,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            handles.lock().unwrap().push(toplevel);
        }
    }

    event_created_child!(Seen, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, Title::default())
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, Title> for Seen {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        title: &Title,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_handle_v1::Event::Title { title: said } = event {
            *title.lock().unwrap() = said;
        }
    }
}
