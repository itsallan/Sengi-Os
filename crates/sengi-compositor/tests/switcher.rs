//! The window switcher and the menus, against the running compositor: the
//! order of the switcher, when focus moves, what Escape and the lock do, and
//! what the menus show and run, checked by the pixels of a picture.
//!
//! The keys of a script come a few seconds after the start, when the test
//! windows are up; a test that checks a state between keys waits for it.

mod common;
mod harness;

use std::time::Duration;

use common::Client;
use harness::{Compositor, SOON};

const GREEN: u32 = 0xff20_b020;
const BLUE: u32 = 0xff20_20b0;
const RED: u32 = 0xffb0_2020;
const DESKTOP: u32 = 0x0012_1316;
/// The accent, the raised surface, the card's surface and its edge, as the
/// tokens give them.
const ACCENT: u32 = 0x00d9_773a;
const RAISED: u32 = 0x002a_2d33;
const CARD: u32 = 0x001f_2126;
const EDGE: u32 = 0x003a_3d44;

fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

/// Three windows, the last one created in front. Their clients are returned
/// so that the compositor keeps them.
fn three_windows(compositor: &Compositor) -> Vec<Client> {
    let mut clients = Vec::new();
    for colour in [GREEN, BLUE, RED] {
        let mut app = compositor.connect();
        app.keyboard().unwrap();
        let _window = app.window(colour).unwrap();
        assert!(app.wait(SOON, |seen| seen.keyboard_enters >= 1));
        clients.push(app);
    }
    clients
}

/// Each client's enter and leave counts of the keyboard, after each has
/// taken in what the compositor has sent it.
fn focus_counts(clients: &mut [Client]) -> Vec<(u32, u32)> {
    clients
        .iter_mut()
        .map(|app| {
            app.roundtrip().unwrap();
            (app.seen.keyboard_enters, app.seen.keyboard_leaves)
        })
        .collect()
}

/// Not a test of its own: the compositor starts this test binary as the
/// screen locker when a test asks for one, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

#[test]
fn alt_tab_moves_to_the_next_window_when_alt_is_let_go() {
    let script = "3000 keydown alt\n3100 key tab\n3300 keyup alt\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut clients = three_windows(&compositor);
    // The window before the front one in focus order is the middle one.
    assert!(
        clients[1].wait(Duration::from_secs(6), |seen| seen.keyboard_enters >= 2),
        "the middle window was not given the keyboard: {:?}",
        focus_counts(&mut clients)
    );
    let counts = focus_counts(&mut clients);
    assert!(counts[2].1 >= 1, "the front window kept it: {counts:?}");
}

#[test]
fn the_keyboard_stays_put_until_alt_is_let_go() {
    let script = "3000 keydown alt\n3100 key tab\n7000 keyup alt\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut clients = three_windows(&compositor);
    // Past the Tab, and before Alt is let go: the switcher is open and no
    // focus has moved.
    let before = focus_counts(&mut clients);
    clients[0].wait(Duration::from_secs(5), |_| false);
    assert_eq!(
        focus_counts(&mut clients),
        before,
        "focus moved before Alt was let go"
    );
    assert!(
        clients[1].wait(Duration::from_secs(6), |seen| seen.keyboard_enters >= 2),
        "not moved on release"
    );
}

#[test]
fn shift_alt_tab_goes_the_other_way_round() {
    let script =
        "3000 keydown alt\n3050 keydown shift\n3100 key tab\n3150 keyup shift\n3300 keyup alt\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut clients = three_windows(&compositor);
    // Backwards from the front is the window that was created first.
    assert!(
        clients[0].wait(Duration::from_secs(6), |seen| seen.keyboard_enters >= 2),
        "the first window was not given the keyboard: {:?}",
        focus_counts(&mut clients)
    );
}

#[test]
fn escape_cancels_the_switcher_and_focus_stays() {
    let script = "3000 keydown alt\n3100 key tab\n3200 key esc\n3300 keyup alt\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut clients = three_windows(&compositor);
    let before = focus_counts(&mut clients);
    clients[0].wait(Duration::from_secs(5), |_| false);
    assert_eq!(
        focus_counts(&mut clients),
        before,
        "focus moved after Escape"
    );
}

#[test]
fn the_switcher_shows_each_window_with_the_selected_one_lit() {
    // Held open until the picture is taken, which comes at about 5 seconds.
    let script = "3000 keydown alt\n3100 key tab\n7000 keyup alt\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut clients = three_windows(&compositor);
    clients[0].wait(Duration::from_secs(5), |_| false);
    let picture = clients[0].capture().unwrap();
    // Three tiles of 320 pixels, 8 apart, in a card centred on 1280 by 720:
    // the middle tile spans x 480 to 800 and y 339 to 381, and the one to its
    // left x 152 to 472.
    assert_eq!(
        rgb(picture.at(796, 343)),
        ACCENT,
        "the middle tile is not lit"
    );
    assert_eq!(rgb(picture.at(460, 343)), RAISED, "the left tile is lit");
    assert_eq!(
        rgb(picture.at(1200, 343)),
        DESKTOP,
        "the card is not centred"
    );
}

#[test]
fn the_switcher_does_not_open_while_locked() {
    // The keys are scripted for five seconds in. The lock has to come before
    // them, or the test would pass without locking anything.
    let script = "5000 keydown alt\n5100 key tab\n5300 keyup alt\n";
    let started = std::time::Instant::now();
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut clients = three_windows(&compositor);
    let mut locker = compositor.locker();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, RED).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the lock came after the scripted keys"
    );
    let before = focus_counts(&mut clients);
    clients[0].wait(Duration::from_secs(5), |_| false);
    assert_eq!(
        focus_counts(&mut clients),
        before,
        "focus moved while locked"
    );
    lock.unlock_and_destroy();
    locker.roundtrip().unwrap();
}

#[test]
fn a_right_click_on_the_desktop_opens_the_root_menu_in_order() {
    let script = "1500 move 200 200\n1600 click right\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let _window = app.window(GREEN).unwrap();
    app.wait(Duration::from_secs(3), |_| false);
    let picture = app.capture().unwrap();
    // The card is 130 pixels tall from the pointer at (200, 200): five rows
    // of 22 between 6 of padding and an 8 pixel space for the line between
    // Settings and Lock, which is 76 down.
    assert_eq!(rgb(picture.at(204, 204)), CARD, "no card at the pointer");
    let line = (270..=282)
        .filter(|&y| rgb(picture.at(260, y)) == EDGE)
        .count();
    assert!(line >= 1, "no separator between Settings and Lock");
    assert_eq!(
        rgb(picture.at(260, 200 + 130 + 4)),
        DESKTOP,
        "the card runs on"
    );
}

#[test]
fn an_item_of_the_root_menu_runs_its_action() {
    let base = std::env::temp_dir().join(format!("sengi-test-root-menu-{}", std::process::id()));
    let (runs, script) = (base.with_extension("runs"), base.with_extension("sh"));
    let _ = std::fs::remove_file(&runs);
    std::fs::write(
        &script,
        format!("echo run >>'{}'\nsleep 30\n", runs.display()),
    )
    .unwrap();
    let locker = format!("/bin/sh {}", script.display());
    // Lock is the fourth row, from 80 to 102 down a card at (200, 200).
    let steps = "1500 move 200 200\n1600 click right\n1800 move 300 291\n1900 click left\n";
    let compositor = Compositor::start_in(Some(&locker), None, Some(steps), &[]);
    let mut app = compositor.connect();
    let _window = app.window(GREEN).unwrap();
    app.wait(Duration::from_secs(4), |_| false);
    let count = std::fs::read_to_string(&runs).map_or(0, |text| text.lines().count());
    let _ = std::fs::remove_file(&runs);
    let _ = std::fs::remove_file(&script);
    assert_eq!(count, 1, "the Lock item did not run the locker");
}

/// Pictures of the switcher and the menus, for looking at: written as PPM
/// files to the directory named by SENGI_SHOTS. Not run by a plain test run.
#[test]
#[ignore = "takes pictures for looking at; needs SENGI_SHOTS"]
fn pictures_of_the_switcher_and_the_menus() {
    let dir = std::path::PathBuf::from(std::env::var_os("SENGI_SHOTS").expect("SENGI_SHOTS"));
    std::fs::create_dir_all(&dir).unwrap();

    // The switcher, held with the middle window selected.
    let script = "3000 keydown alt\n3100 key tab\n9000 keyup alt\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut clients = three_windows(&compositor);
    clients[0].wait(Duration::from_secs(5), |_| false);
    clients[0]
        .capture()
        .unwrap()
        .write_ppm(&dir.join("switcher.ppm"))
        .unwrap();
    drop(clients);
    drop(compositor);

    // The root menu, opened on the empty desktop.
    let script = "1500 move 200 200\n1600 click right\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let _window = app.window(GREEN).unwrap();
    app.wait(Duration::from_secs(4), |_| false);
    app.capture()
        .unwrap()
        .write_ppm(&dir.join("desktop-menu.ppm"))
        .unwrap();
    drop(app);
    drop(compositor);

    // The window menu, opened on the title bar of a window in the middle.
    let script = "1500 move 700 258\n1600 click right\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let _window = app.decorated_window(GREEN).unwrap();
    app.wait(Duration::from_secs(4), |_| false);
    app.capture()
        .unwrap()
        .write_ppm(&dir.join("window-menu.ppm"))
        .unwrap();
}

/// A right-click on a window's title bar opens its menu with the pointer at
/// its top left corner: four rows, and no line between them.
#[test]
fn a_right_click_on_a_title_bar_opens_the_window_menu() {
    let script = "1500 move 700 258\n1600 click right\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let _window = app.decorated_window(GREEN).unwrap();
    app.wait(Duration::from_secs(4), |_| false);
    let picture = app.capture().unwrap();
    // Four rows of 22 pixels, with the padding around them: 100 tall.
    assert_eq!(
        rgb(picture.at(712, 258 + 100 - 4)),
        CARD,
        "the menu is too short"
    );
    assert_ne!(rgb(picture.at(712, 258 + 106)), CARD, "the menu runs on");
}

/// A right-click on the client area of a window opens nothing.
#[test]
fn a_right_click_on_a_window_opens_no_menu() {
    let script = "1500 move 700 400\n1600 click right\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let _window = app.window(GREEN).unwrap();
    app.wait(Duration::from_secs(4), |_| false);
    let picture = app.capture().unwrap();
    assert_eq!(
        rgb(picture.at(704, 404)),
        rgb(GREEN),
        "a menu opened on a window"
    );
}

/// A click on a window with no card open reaches the window: the control for
/// the test below, which proves the client sees a click at all.
#[test]
fn a_click_on_a_window_reaches_it() {
    let script = "1500 move 600 350\n1600 click left\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let _window = app.window(GREEN).unwrap();
    let _pointer = app.pointer().unwrap();
    assert!(app.wait(Duration::from_secs(4), |seen| seen
        .more
        .button_serial
        .is_some()));
}

/// The press that closes a menu is not a click on the window under it.
#[test]
fn a_click_that_closes_the_menu_does_not_reach_the_window_under_it() {
    let script = "1500 move 200 200\n1600 click right\n1800 move 600 350\n2000 click left\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let _window = app.window(GREEN).unwrap();
    let _pointer = app.pointer().unwrap();
    assert!(!app.wait(Duration::from_secs(4), |seen| seen
        .more
        .button_serial
        .is_some()));
}

/// A window menu does not outlive its window.
#[test]
fn a_window_menu_closes_when_its_window_goes() {
    let script = "1500 move 700 258\n1600 click right\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let (surface, toplevel) = app.decorated_window(GREEN).unwrap();
    app.wait(Duration::from_secs(4), |_| false);
    assert_eq!(
        rgb(app.capture().unwrap().at(712, 300)),
        CARD,
        "no window menu to close"
    );
    toplevel.destroy();
    surface.destroy();
    app.roundtrip().unwrap();
    app.wait(Duration::from_secs(1), |_| false);
    assert_eq!(
        rgb(app.capture().unwrap().at(712, 300)),
        DESKTOP,
        "the menu outlived its window"
    );
}
