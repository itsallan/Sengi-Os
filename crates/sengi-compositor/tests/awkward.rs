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

#[test]
fn alt_tab_closes_a_menu_and_the_keyboard_goes_where_the_user_went() {
    let compositor = Compositor::start(None);
    let mut other = a_program(&compositor);
    let _other = window(&mut other, "other", RED);
    let mut app = a_program(&compositor);
    let parent = window(&mut app, "parent", GREEN);
    let serial = press_on(&compositor, &mut app, "parent");
    let held = menu(&mut app, &parent, serial);
    compositor.input("release left");

    compositor.input("keydown alt");
    compositor.input("key tab");
    compositor.input("keyup alt");
    assert!(window_line(&compositor, "other").contains("active=1"));
    app.roundtrip().unwrap();
    assert!(
        held.dismissed.load(Ordering::Relaxed),
        "the menu was not told to close"
    );
    held.destroy();
    app.roundtrip().unwrap();
    sound(&compositor);
    assert!(types_into(&compositor, &mut other), "typing went elsewhere");
}

#[test]
fn a_window_that_appears_takes_the_keyboard_from_a_menu() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let parent = window(&mut app, "parent", GREEN);
    let serial = press_on(&compositor, &mut app, "parent");
    let held = menu(&mut app, &parent, serial);
    compositor.input("release left");

    let mut late = a_program(&compositor);
    let _late = window(&mut late, "late", RED);
    app.roundtrip().unwrap();
    assert!(held.dismissed.load(Ordering::Relaxed));
    sound(&compositor);
    assert!(types_into(&compositor, &mut late));
}

#[test]
fn a_minimised_window_takes_its_menu_with_it() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let parent = window(&mut app, "parent", GREEN);
    let serial = press_on(&compositor, &mut app, "parent");
    let held = menu(&mut app, &parent, serial);
    compositor.input("release left");
    parent.toplevel.set_minimized();
    app.roundtrip().unwrap();
    assert!(held.dismissed.load(Ordering::Relaxed));
    let report = sound(&compositor);
    assert!(report.contains("keyboard_grabbed=0"), "{report}");
    assert!(report.contains("pointer_grabbed=0"), "{report}");
}

#[test]
fn a_menu_its_program_closes_lets_go_of_the_keyboard_at_once() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let parent = window(&mut app, "parent", GREEN);
    let serial = press_on(&compositor, &mut app, "parent");
    let held = menu(&mut app, &parent, serial);
    compositor.input("release left");
    held.destroy();
    app.roundtrip().unwrap();
    let report = sound(&compositor);
    assert!(report.contains("keyboard_grabbed=0"), "{report}");
    assert!(report.contains("pointer_grabbed=0"), "{report}");
    assert!(types_into(&compositor, &mut app));
}

#[test]
fn the_launcher_gets_the_keyboard_over_an_open_menu() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let parent = window(&mut app, "parent", GREEN);
    let serial = press_on(&compositor, &mut app, "parent");
    let held = menu(&mut app, &parent, serial);
    compositor.input("release left");

    let mut launcher = a_program(&compositor);
    let (surface, layer) = launcher.desktop(RED).unwrap();
    layer.set_layer(zwlr_layer_shell_v1::Layer::Overlay);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();
    launcher.roundtrip().unwrap();
    app.roundtrip().unwrap();
    assert!(held.dismissed.load(Ordering::Relaxed));
    sound(&compositor);
    assert!(types_into(&compositor, &mut launcher));
}

#[test]
fn the_lock_closes_an_open_menu() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let parent = window(&mut app, "parent", GREEN);
    let serial = press_on(&compositor, &mut app, "parent");
    let held = menu(&mut app, &parent, serial);
    compositor.input("release left");
    let mut locker = compositor.locker();
    let lock = locker.lock().unwrap();
    app.roundtrip().unwrap();
    assert!(held.dismissed.load(Ordering::Relaxed));
    lock.unlock_and_destroy();
    locker.roundtrip().unwrap();
    held.destroy();
    app.roundtrip().unwrap();
    sound(&compositor);
    assert!(types_into(&compositor, &mut app));
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

#[test]
fn a_window_the_dock_restores_comes_back_in_front() {
    let compositor = Compositor::start(None);
    let mut other = a_program(&compositor);
    let _other = window(&mut other, "other", RED);
    let mut app = a_program(&compositor);
    let _window = window(&mut app, "hidden", GREEN);
    let (mut dock, handles) = dock(&compositor);
    let wanted = handle(&mut dock, &handles, "hidden");
    wanted.set_minimized();
    dock.roundtrip().unwrap();
    wanted.unset_minimized();
    dock.roundtrip().unwrap();
    sound(&compositor);
    assert!(window_line(&compositor, "hidden").contains("active=1"));
    assert!(types_into(&compositor, &mut app));
}

#[test]
fn a_minimised_window_comes_back_onto_a_screen_that_is_there() {
    let compositor = Compositor::start_args(&["--outputs", "1280x720,800x600"], None);
    let mut app = a_program(&compositor);
    let _window = window(&mut app, "wanderer", GREEN);
    // To the second screen by its title bar, then out of sight.
    let frame = numbers(&window_line(&compositor, "wanderer"), "frame");
    let (x, y) = (frame[0] + frame[2] / 2, frame[1] + 12);
    compositor.input(&format!("move {x} {y}"));
    compositor.input("press left");
    compositor.input("move 1700 300");
    compositor.input("release left");
    assert!(numbers(&window_line(&compositor, "wanderer"), "frame")[0] >= 1280);
    let (mut dock, handles) = dock(&compositor);
    let seat = dock.seat().unwrap();
    let wanted = handle(&mut dock, &handles, "wanderer");
    wanted.set_minimized();
    dock.roundtrip().unwrap();
    compositor.unplug("HEADLESS-2");
    wanted.activate(&seat);
    dock.roundtrip().unwrap();
    let frame = numbers(&window_line(&compositor, "wanderer"), "frame");
    assert!(frame[0] + frame[2] <= 1280, "it came back at {frame:?}");
    sound(&compositor);
}

#[test]
fn a_window_leaving_fullscreen_after_its_screen_went_is_within_reach() {
    let compositor = Compositor::start_args(&["--outputs", "1280x720,800x600"], None);
    let mut app = a_program(&compositor);
    let shown = window(&mut app, "film", GREEN);
    let frame = numbers(&window_line(&compositor, "film"), "frame");
    let (x, y) = (frame[0] + frame[2] / 2, frame[1] + 12);
    compositor.input(&format!("move {x} {y}"));
    compositor.input("press left");
    compositor.input("move 1700 300");
    compositor.input("release left");
    shown.toplevel.set_fullscreen(None);
    app.roundtrip().unwrap();
    compositor.unplug("HEADLESS-2");
    shown.toplevel.unset_fullscreen();
    app.roundtrip().unwrap();
    let frame = numbers(&window_line(&compositor, "film"), "frame");
    assert!(frame[0] + frame[2] <= 1280, "it is at {frame:?}");
    sound(&compositor);
}

#[test]
fn a_hidden_window_asked_to_fill_the_screen_gives_its_place_back() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let hidden = window(&mut app, "hidden", GREEN);
    let before = numbers(&window_line(&compositor, "hidden"), "client");
    let (mut dock, handles) = dock(&compositor);
    let seat = dock.seat().unwrap();
    let wanted = handle(&mut dock, &handles, "hidden");
    wanted.set_minimized();
    dock.roundtrip().unwrap();
    hidden.toplevel.set_fullscreen(None);
    app.roundtrip().unwrap();
    wanted.activate(&seat);
    dock.roundtrip().unwrap();
    hidden.toplevel.unset_fullscreen();
    app.roundtrip().unwrap();
    assert_eq!(
        numbers(&window_line(&compositor, "hidden"), "client"),
        before
    );
    sound(&compositor);
}

#[test]
fn a_window_that_shrinks_while_over_the_edge_stays_within_reach() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let shrinking = window(&mut app, "shrinking", GREEN);
    app.paint(&shrinking.surface, 600, 200, GREEN).unwrap();
    // Far out to the left by its title bar, then much narrower.
    let frame = numbers(&window_line(&compositor, "shrinking"), "frame");
    let (x, y) = (frame[0] + frame[2] - 40, frame[1] + 12);
    compositor.input(&format!("move {x} {y}"));
    compositor.input("press left");
    compositor.input("move 60 300");
    compositor.input("release left");
    assert!(numbers(&window_line(&compositor, "shrinking"), "frame")[0] < -300);
    app.paint(&shrinking.surface, 100, 80, GREEN).unwrap();
    let frame = numbers(&window_line(&compositor, "shrinking"), "frame");
    assert!(frame[0] + frame[2] > 0, "it is at {frame:?}");
    sound(&compositor);
}

#[test]
fn a_window_that_fills_the_screen_is_sized_for_the_next_one_plugged_in() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let shown = window(&mut app, "film", GREEN);
    shown.toplevel.set_fullscreen(None);
    app.roundtrip().unwrap();
    compositor.unplug("HEADLESS-1");
    compositor.plug("800x600");
    assert!(
        app.wait(SOON, |seen| seen.more.toplevel == Some((800, 600, true))),
        "it was told {:?}",
        app.seen.more.toplevel
    );
    sound(&compositor);
}

#[test]
fn a_hidden_window_asked_to_be_maximised_comes_back_so() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let hidden = window(&mut app, "hidden", GREEN);
    let (mut dock, handles) = dock(&compositor);
    let seat = dock.seat().unwrap();
    let wanted = handle(&mut dock, &handles, "hidden");
    // While it fills the screen as well, which wins.
    for fullscreen in [false, true] {
        wanted.set_minimized();
        dock.roundtrip().unwrap();
        if fullscreen {
            hidden.toplevel.set_fullscreen(None);
        }
        hidden.toplevel.set_maximized();
        app.roundtrip().unwrap();
        wanted.activate(&seat);
        dock.roundtrip().unwrap();
        sound(&compositor);
        let line = window_line(&compositor, "hidden");
        assert!(line.contains("maximised=1"), "{line}");
    }
}

#[test]
fn the_snap_outline_goes_with_a_window_that_closes_while_dragged() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let closing = window(&mut app, "closing", GREEN);
    let frame = numbers(&window_line(&compositor, "closing"), "frame");
    let (x, y) = (frame[0] + frame[2] / 2, frame[1] + 12);
    compositor.input(&format!("move {x} {y}"));
    compositor.input("press left");
    compositor.input("move 640 0");
    closing.toplevel.destroy();
    closing.shell.destroy();
    closing.surface.destroy();
    app.roundtrip().unwrap();
    sound(&compositor);
    compositor.input("release left");
}

#[test]
fn a_window_that_shrinks_right_after_a_resize_stays_within_reach() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let shrinking = window(&mut app, "shrinking", GREEN);
    app.paint(&shrinking.surface, 600, 200, GREEN).unwrap();
    // Far out to the right, then a little wider by its left edge.
    let frame = numbers(&window_line(&compositor, "shrinking"), "frame");
    let (x, y) = (frame[0] + 90, frame[1] + 12);
    compositor.input(&format!("move {x} {y}"));
    compositor.input("press left");
    // Short of the edge itself, which would snap it.
    compositor.input(&format!("move 1260 {y}"));
    compositor.input("release left");
    let frame = numbers(&window_line(&compositor, "shrinking"), "frame");
    assert!(frame[0] + frame[2] > 1500, "it is at {frame:?}");
    let (left, middle) = (frame[0], frame[1] + frame[3] / 2);
    compositor.input(&format!("move {left} {middle}"));
    compositor.input("press left");
    compositor.input(&format!("move {} {middle}", left - 10));
    compositor.input("release left");
    app.paint(&shrinking.surface, 100, 200, GREEN).unwrap();
    let frame = numbers(&window_line(&compositor, "shrinking"), "frame");
    assert!(frame[0] < 1280, "it is at {frame:?}");
    sound(&compositor);
}

#[test]
fn a_window_that_fills_the_screen_stays_there_when_a_bar_appears() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let shown = window(&mut app, "film", GREEN);
    shown.toplevel.set_fullscreen(None);
    app.roundtrip().unwrap();
    app.paint(&shown.surface, 1280, 720, GREEN).unwrap();
    let mut bar = compositor.connect();
    let _bar = bar.bar(28, RED).unwrap();
    let client = numbers(&window_line(&compositor, "film"), "client");
    assert_eq!(client, [0, 0, 1280, 720]);
    // The same when the screens change.
    compositor.plug("800x600");
    let client = numbers(&window_line(&compositor, "film"), "client");
    assert_eq!(client, [0, 0, 1280, 720]);
}

#[test]
fn a_window_minimised_with_its_menu_open_closes_the_menu() {
    let compositor = Compositor::start(None);
    let mut app = a_program(&compositor);
    let _window = window(&mut app, "framed", GREEN);
    let (mut dock, handles) = dock(&compositor);
    let frame = numbers(&window_line(&compositor, "framed"), "frame");
    compositor.input(&format!(
        "move {} {}",
        frame[0] + frame[2] / 2,
        frame[1] + 12
    ));
    compositor.input("click right");
    assert!(sound(&compositor).contains("overlay=menu"));
    handle(&mut dock, &handles, "framed").set_minimized();
    dock.roundtrip().unwrap();
    assert!(sound(&compositor).contains("overlay=none"));
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
