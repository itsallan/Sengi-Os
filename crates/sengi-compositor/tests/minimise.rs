//! Minimising with several windows open, by the title bar button, and menus
//! closed by a click elsewhere. Both hand the keyboard on while the pointer
//! is in the middle of a button event, which once froze the whole
//! compositor.

mod common;
mod harness;

use std::time::Duration;

use common::{apps::Picture, Client};
use harness::{Compositor, SOON};
use wayland_client::{protocol::wl_surface::WlSurface, Connection, Dispatch, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::XdgWmBase,
};

const COLOURS: [u32; 3] = [0xff20_b020, 0xffb0_2020, 0xff20_20b0];

fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

fn token(name: &str) -> u32 {
    let c = sengi_tokens::Tokens::builtin().color(name).unwrap();
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

/// The middle of the box around the pixels of one colour.
fn centre_of(picture: &Picture, colour: u32) -> Option<(u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..picture.height {
        for x in 0..picture.width {
            if picture.at(x, y) == colour {
                let b = found.get_or_insert((x, y, x, y));
                *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    found.map(|(x0, y0, x1, y1)| ((x0 + x1) / 2, (y0 + y1) / 2))
}

fn shows(picture: &Picture, argb: u32) -> bool {
    centre_of(picture, rgb(argb)).is_some()
}

struct App {
    client: Client,
    colour: u32,
    _surface: WlSurface,
    _toplevel: XdgToplevel,
}

/// Three programs with a window each, the last one in front.
fn three_apps(compositor: &Compositor) -> Vec<App> {
    COLOURS
        .iter()
        .map(|&colour| {
            let mut client = compositor.connect();
            client.keyboard().unwrap();
            let (surface, toplevel) = client.window(colour).unwrap();
            assert!(client.wait(SOON, |seen| seen.keyboard_enters == 1));
            App {
                client,
                colour,
                _surface: surface,
                _toplevel: toplevel,
            }
        })
        .collect()
}

/// Clicks the yellow button of the window in front, the only one that has
/// colours.
fn click_minimise(compositor: &Compositor, app: &mut Client) {
    let picture = app.capture().unwrap();
    let (x, y) =
        centre_of(&picture, token("color.window-minimise")).expect("a focused window's button");
    compositor.input(&format!("move {x} {y}"));
    compositor.input("click left");
}

/// The compositor still answers, and to Wayland clients too.
fn alive(compositor: &Compositor, app: &mut Client) {
    assert!(compositor.control("contexts").starts_with("ok"), "frozen");
    app.roundtrip().unwrap();
}

#[test]
fn minimising_by_the_button_hands_the_keyboard_to_the_next_window() {
    let compositor = Compositor::start(None);
    let mut apps = three_apps(&compositor);
    // Front to back: the third, the second, the first.
    for front in [2, 1, 0] {
        let enters: Vec<u32> = apps
            .iter()
            .map(|app| app.client.seen.keyboard_enters)
            .collect();
        click_minimise(&compositor, &mut apps[front].client);
        alive(&compositor, &mut apps[front].client);
        if front > 0 {
            let next = &mut apps[front - 1];
            let before = enters[front - 1];
            assert!(
                next.client.wait(SOON, |seen| seen.keyboard_enters > before),
                "the window behind did not get the keyboard"
            );
        }
        let colour = apps[front].colour;
        let picture = apps[front].client.capture().unwrap();
        assert!(!shows(&picture, colour), "still on the screen");
    }
    // The pointer still works afterwards.
    compositor.input("move 10 10");
    compositor.input("click left");
    alive(&compositor, &mut apps[0].client);
}

#[test]
fn a_window_in_the_middle_of_the_stack_minimises_after_being_chosen() {
    let compositor = Compositor::start(None);
    let mut apps = three_apps(&compositor);
    for chosen in [0, 1, 2] {
        // The dock or Alt+Tab brings it to the front.
        let picture = apps[chosen].client.capture().unwrap();
        if let Some((x, y)) = centre_of(&picture, rgb(apps[chosen].colour)) {
            compositor.input(&format!("move {x} {y}"));
            compositor.input("click left");
            click_minimise(&compositor, &mut apps[chosen].client);
            alive(&compositor, &mut apps[chosen].client);
            let colour = apps[chosen].colour;
            let picture = apps[chosen].client.capture().unwrap();
            assert!(!shows(&picture, colour), "{colour:08x} is still there");
        }
    }
}

#[test]
fn minimising_while_another_program_does_not_answer_or_has_gone() {
    let compositor = Compositor::start(None);
    let mut apps = three_apps(&compositor);
    // The first stops reading for good, the second dies.
    let silent = apps.remove(0);
    let gone = apps.remove(0);
    drop(gone);
    let mut front = apps.remove(0);
    front.client.wait(Duration::from_millis(200), |_| false);
    click_minimise(&compositor, &mut front.client);
    alive(&compositor, &mut front.client);
    drop(silent);
    alive(&compositor, &mut front.client);
}

impl Dispatch<XdgPositioner, ()> for common::Seen {
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

impl Dispatch<XdgPopup, ()> for common::Seen {
    fn event(
        _: &mut Self,
        _: &XdgPopup,
        _: xdg_popup::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// A window with a menu open that holds the keyboard and the pointer.
fn with_a_menu_open() -> (Compositor, Client, XdgPopup) {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    app.pointer().unwrap();
    let seat = app.seat().unwrap();
    let wm_base: XdgWmBase = app.globals.bind(&app.handle, 1..=2, ()).unwrap();

    let surface = app.compositor.create_surface(&app.handle, ());
    let parent: XdgSurface = wm_base.get_xdg_surface(&surface, &app.handle, ());
    let _toplevel = parent.get_toplevel(&app.handle, ());
    surface.commit();
    assert!(app.wait(SOON, |seen| seen.window_configures >= 1));
    app.paint(&surface, 300, 200, COLOURS[0]).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));

    let picture = app.capture().unwrap();
    let (x, y) = centre_of(&picture, rgb(COLOURS[0])).unwrap();
    compositor.input(&format!("move {x} {y}"));
    compositor.input("press left");
    assert!(app.wait(SOON, |seen| seen.more.button_serial.is_some()));
    let serial = app.seen.more.button_serial.unwrap();

    let positioner = wm_base.create_positioner(&app.handle, ());
    positioner.set_size(80, 60);
    positioner.set_anchor_rect(10, 10, 1, 1);
    let menu = app.compositor.create_surface(&app.handle, ());
    let shell = wm_base.get_xdg_surface(&menu, &app.handle, ());
    let popup = shell.get_popup(Some(&parent), &positioner, &app.handle, ());
    popup.grab(&seat, serial);
    app.roundtrip().unwrap();
    let before = app.seen.window_configures;
    menu.commit();
    assert!(app.wait(SOON, |seen| seen.window_configures > before));
    app.paint(&menu, 80, 60, COLOURS[1]).unwrap();
    // The menu has the keyboard now.
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 2));
    compositor.input("release left");
    (compositor, app, popup)
}

#[test]
fn a_menu_is_closed_by_a_click_outside_it() {
    let (compositor, mut app, _popup) = with_a_menu_open();
    // A click on the desktop.
    compositor.input("move 5 700");
    compositor.input("click left");
    alive(&compositor, &mut app);
    assert!(
        app.wait(SOON, |seen| seen.keyboard_enters == 3),
        "the window did not get the keyboard back"
    );
}

#[test]
fn the_screen_locks_while_a_menu_is_open() {
    let (compositor, mut app, _popup) = with_a_menu_open();
    let mut locker = compositor.locker();
    let _lock = locker.lock().unwrap();
    locker.roundtrip().unwrap();
    alive(&compositor, &mut app);
}

/// Not run by a plain `cargo test`: the compositor starts this test binary
/// as the screen locker, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}
