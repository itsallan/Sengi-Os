//! A maximised window has its screen to itself: the bar and the dock slide
//! out of its way, come back while the pointer rests at their edge or while
//! something opened from them is up, and are there again as soon as the
//! window in front is not maximised. Checked against the real compositor,
//! by what it reports and by pictures of the screen.

mod common;
mod harness;

use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use common::Client;
use harness::{Compositor, SOON};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::Layer, zwlr_layer_surface_v1::KeyboardInteractivity,
    zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
};

const GREEN: u32 = 0xff20_b020;
const RED: u32 = 0xffb0_2020;
const BLUE: u32 = 0xff20_20b0;
const GREY: u32 = 0xff60_6060;

const BAR: u32 = 30;
const DOCK: u32 = 60;

/// Longer than the pointer rests, the panel waits and the panel slides,
/// put together.
const WHILE: Duration = Duration::from_millis(700);

fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

/// Not run by a plain `cargo test`: the compositor starts this test binary
/// as the screen locker, with `--include-ignored`, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

/// What the compositor says now, and that nothing in it is broken.
fn report(compositor: &Compositor) -> String {
    let report = compositor.control("inspect");
    assert!(!report.contains("\nbroken "), "{report}");
    report
}

/// The line about the screen `name`.
fn screen(compositor: &Compositor, name: &str) -> String {
    let report = report(compositor);
    let start = format!("screen name={name} ");
    report
        .lines()
        .find(|line| line.starts_with(&start))
        .unwrap_or_else(|| panic!("no {name} in {report}"))
        .to_string()
}

const FIRST: &str = "HEADLESS-1";

/// Waits for the first screen's line to say `what`.
fn settles(compositor: &Compositor, what: &str) -> String {
    settles_on(compositor, FIRST, what)
}

fn settles_on(compositor: &Compositor, name: &str, what: &str) -> String {
    let end = Instant::now() + SOON;
    loop {
        let line = screen(compositor, name);
        if line.contains(what) {
            return line;
        }
        assert!(Instant::now() < end, "never `{what}`: {line}");
        sleep(Duration::from_millis(20));
    }
}

/// That the screen still says `what` after a while.
fn stays(compositor: &Compositor, what: &str) {
    sleep(WHILE);
    let line = screen(compositor, FIRST);
    assert!(line.contains(what), "not still `{what}`: {line}");
}

const HIDDEN: &str = "top=hidden bottom=hidden";
const SHOWN: &str = "top=shown bottom=shown";

/// A screen with a bar, a dock and one window.
struct Desk {
    compositor: Compositor,
    shell: Client,
    bar: (WlSurface, ZwlrLayerSurfaceV1),
    dock: (WlSurface, ZwlrLayerSurfaceV1),
    app: Client,
    surface: WlSurface,
    toplevel: XdgToplevel,
}

fn desk() -> Desk {
    let compositor = Compositor::start(None);
    let mut shell = compositor.connect();
    shell.pointer().unwrap();
    let bar = shell.bar(BAR, RED).unwrap();
    let dock = shell.dock(DOCK, BLUE).unwrap();
    // The pointer is well clear of both edges.
    compositor.input("move 640 360");
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    Desk {
        compositor,
        shell,
        bar,
        dock,
        app,
        surface,
        toplevel,
    }
}

impl Desk {
    /// Waits to be told to be `w` by `h`, and draws that.
    fn draws(&mut self, w: i32, h: i32) {
        assert!(
            self.app.wait(SOON, |seen| seen
                .more
                .toplevel
                .is_some_and(|(told_w, told_h, _)| (told_w, told_h) == (w, h))),
            "told {:?}, not {w}x{h}",
            self.app.seen.more.toplevel
        );
        self.app.paint(&self.surface, w, h, GREEN).unwrap();
    }

    /// Maximises the window, which takes the whole screen but its frame.
    fn maximise(&mut self) {
        self.compositor.input("key super+a");
        self.draws(1278, 689);
        settles(&self.compositor, HIDDEN);
    }

    fn at(&mut self, x: u32, y: u32) -> u32 {
        self.app.capture().unwrap().at(x, y)
    }

    fn go(&self, x: i32, y: i32) {
        self.compositor.input(&format!("move {x} {y}"));
    }

    fn click(&self, x: i32, y: i32) {
        self.go(x, y);
        self.compositor.input("click left");
    }

    /// Where the middle of a title bar button is while nothing has moved.
    fn button(&self, name: &str) -> (i32, i32) {
        let report = report(&self.compositor);
        let at = report
            .lines()
            .find(|line| line.starts_with("window title=test_window"))
            .and_then(|line| {
                line.split(' ')
                    .find_map(|part| part.strip_prefix(&format!("{name}=")))
            })
            .unwrap_or_else(|| panic!("no {name} button in {report}"));
        let (x, y) = at.split_once(',').expect("a place");
        (x.parse().unwrap(), y.parse().unwrap())
    }

    fn reveal_top(&self) {
        self.go(640, 0);
        settles(&self.compositor, "top=shown bottom=hidden drop=30");
    }

    fn reveal_bottom(&self) {
        self.go(640, 719);
        settles(&self.compositor, "top=hidden bottom=shown drop=0");
    }
}

#[test]
fn a_maximised_window_has_the_whole_screen_and_gives_it_back() {
    let mut desk = desk();
    assert!(screen(&desk.compositor, FIRST).contains("cover=- top=shown bottom=shown"));
    assert_eq!(desk.at(5, 5), rgb(RED));
    assert_eq!(desk.at(5, 715), rgb(BLUE));

    desk.maximise();
    assert!(screen(&desk.compositor, FIRST).contains("cover=test_window"));
    assert_ne!(desk.at(5, 5), rgb(RED), "the bar is still there");
    assert_eq!(
        desk.at(5, 35),
        rgb(GREEN),
        "the window is not under the bar"
    );
    assert_eq!(desk.at(640, 715), rgb(GREEN), "the dock is still there");

    desk.compositor.input("key super+a");
    desk.draws(300, 200);
    settles(&desk.compositor, SHOWN);
    assert_eq!(desk.at(5, 5), rgb(RED), "the bar did not come back");
    assert_eq!(desk.at(5, 715), rgb(BLUE), "the dock did not come back");
}

#[test]
fn resting_at_the_top_brings_the_bar_and_the_title_bar_moves_below_it() {
    let mut desk = desk();
    desk.maximise();
    desk.go(640, 0);
    // Not at once: the pointer has to rest there.
    assert!(screen(&desk.compositor, FIRST).contains("top=hidden"));
    desk.reveal_top();
    assert_eq!(desk.at(640, 5), rgb(RED), "no bar");
    // The title bar is under the bar, over what the program draws.
    assert_ne!(desk.at(640, 33), rgb(GREEN), "no title bar under the bar");
    assert_ne!(desk.at(640, 57), rgb(GREEN), "no title bar under the bar");
    assert_eq!(desk.at(640, 65), rgb(GREEN));
    // The dock stays away.
    assert_eq!(desk.at(640, 715), rgb(GREEN));

    // On the title bar the bar stays.
    desk.go(300, 45);
    stays(&desk.compositor, "top=shown");
    // Below it the bar goes, and the title bar back up.
    desk.go(640, 200);
    settles(&desk.compositor, "top=hidden bottom=hidden drop=0");
    assert_ne!(desk.at(640, 5), rgb(RED));
    assert_eq!(desk.at(640, 45), rgb(GREEN));
}

#[test]
fn brushing_an_edge_brings_nothing() {
    let mut desk = desk();
    desk.maximise();
    for (x, y) in [(640, 0), (640, 719), (0, 0), (1279, 719)] {
        desk.go(x, y);
        desk.go(640, 360);
        stays(&desk.compositor, HIDDEN);
    }
}

#[test]
fn resting_at_the_bottom_brings_the_dock_until_the_pointer_leaves_it() {
    let mut desk = desk();
    desk.maximise();
    // Anywhere along the edge, and the dock stays while the pointer is on it.
    desk.go(20, 719);
    settles(&desk.compositor, "top=hidden bottom=shown");
    assert_eq!(desk.at(640, 700), rgb(BLUE), "no dock");
    assert_ne!(desk.at(640, 5), rgb(RED), "the bar came too");
    desk.go(640, 670);
    stays(&desk.compositor, "bottom=shown");
    desk.go(640, 640);
    settles(&desk.compositor, HIDDEN);
    assert_eq!(desk.at(640, 700), rgb(GREEN));
}

#[test]
fn the_dock_takes_clicks_only_while_it_shows() {
    let mut desk = desk();
    desk.maximise();
    // Where the dock was, the window has the pointer.
    desk.app.pointer().unwrap();
    let before = desk.app.seen.more.button_serial;
    desk.click(640, 690);
    assert!(
        desk.app
            .wait(SOON, |seen| seen.more.button_serial != before),
        "the window was not clicked where the dock hides"
    );
    desk.reveal_bottom();
    let before = desk.shell.seen.more.button_serial;
    desk.click(640, 690);
    assert!(
        desk.shell
            .wait(SOON, |seen| seen.more.button_serial != before),
        "the dock was not clicked"
    );
}

#[test]
fn a_clicked_panel_waits_for_what_the_click_asked_for() {
    let mut desk = desk();
    desk.maximise();
    desk.reveal_top();
    desk.click(640, 10);
    desk.go(640, 360);
    sleep(Duration::from_millis(350));
    assert!(screen(&desk.compositor, FIRST).contains("top=shown"));
    settles(&desk.compositor, HIDDEN);
}

#[test]
fn a_button_held_on_the_dock_keeps_it() {
    let mut desk = desk();
    desk.maximise();
    desk.reveal_bottom();
    desk.go(640, 690);
    desk.compositor.input("press left");
    desk.go(640, 300);
    stays(&desk.compositor, "bottom=shown");
    desk.compositor.input("release left");
    settles(&desk.compositor, HIDDEN);
}

#[test]
fn a_button_held_in_the_window_brings_nothing_at_the_edge() {
    let mut desk = desk();
    desk.maximise();
    desk.go(640, 360);
    desk.compositor.input("press left");
    desk.go(640, 719);
    stays(&desk.compositor, HIDDEN);
    desk.compositor.input("release left");
    settles(&desk.compositor, "bottom=shown");
}

#[test]
fn a_pointer_already_at_the_edge_has_to_leave_it_first() {
    let mut desk = desk();
    desk.go(640, 0);
    desk.maximise();
    stays(&desk.compositor, HIDDEN);
    desk.go(660, 0);
    stays(&desk.compositor, HIDDEN);
    desk.go(660, 100);
    desk.reveal_top();
}

#[test]
fn the_buttons_of_a_title_bar_that_moved_down_work_there() {
    let mut desk = desk();
    desk.maximise();
    let (x, y) = desk.button("minimise");
    desk.reveal_top();
    desk.click(x, y + BAR as i32);
    settles(&desk.compositor, "cover=- top=shown bottom=shown");
    assert!(report(&desk.compositor).contains("place=minimised"));
}

#[test]
fn the_maximise_button_of_a_title_bar_that_moved_down_gives_the_size_back() {
    let mut desk = desk();
    desk.maximise();
    let (x, y) = desk.button("maximise");
    desk.reveal_top();
    desk.click(x, y + BAR as i32);
    desk.draws(300, 200);
    settles(&desk.compositor, "cover=- top=shown bottom=shown drop=0");
}

#[test]
fn a_double_click_on_a_title_bar_that_moved_down_unmaximises() {
    let mut desk = desk();
    desk.maximise();
    desk.reveal_top();
    desk.click(500, 45);
    desk.compositor.input("click left");
    desk.draws(300, 200);
    settles(&desk.compositor, "cover=-");
}

#[test]
fn a_window_that_closes_or_leaves_gives_the_panels_back() {
    let mut desk = desk();
    desk.maximise();
    desk.toplevel.destroy();
    desk.app.roundtrip().unwrap();
    settles(&desk.compositor, "cover=- top=shown bottom=shown");
}

#[test]
fn another_window_in_front_brings_the_panels_back_and_going_back_hides_them() {
    let mut desk = desk();
    desk.maximise();
    let mut other = desk.compositor.connect();
    other.keyboard().unwrap();
    let _window = other.window(GREY).unwrap();
    assert!(other.wait(SOON, |seen| seen.keyboard_enters == 1));
    settles(&desk.compositor, "cover=- top=shown bottom=shown");
    assert_eq!(desk.at(5, 5), rgb(RED));
    // The maximised window is still as large as the screen behind.
    assert_eq!(desk.at(5, 300), rgb(GREEN));
    desk.click(5, 300);
    settles(&desk.compositor, HIDDEN);
}

#[test]
fn a_dialog_over_a_maximised_window_keeps_the_panels_away() {
    let mut desk = desk();
    desk.maximise();
    let (_surface, dialog) = desk.app.window(GREY).unwrap();
    // A window of its own brings them back, until it says what it is part
    // of.
    settles(&desk.compositor, SHOWN);
    dialog.set_parent(Some(&desk.toplevel));
    desk.app.roundtrip().unwrap();
    settles(&desk.compositor, HIDDEN);
    assert!(screen(&desk.compositor, FIRST).contains("cover=test_window"));
}

#[test]
fn locking_puts_a_revealed_panel_away_and_unlocking_finds_it_hidden() {
    let mut desk = desk();
    desk.maximise();
    desk.reveal_top();
    let mut locker = desk.compositor.locker();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, GREY).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    assert!(screen(&desk.compositor, FIRST).contains("drop=0"));
    // The edges say nothing to a locked screen.
    desk.go(640, 360);
    desk.go(640, 719);
    sleep(WHILE);
    report(&desk.compositor);
    desk.go(640, 360);
    lock.unlock_and_destroy();
    locker.roundtrip().unwrap();
    settles(&desk.compositor, HIDDEN);
    assert_eq!(desk.at(640, 5), desk.at(300, 5));
    assert_ne!(desk.at(640, 5), rgb(RED));
}

/// Something opened over the whole screen with the keyboard, as quick
/// settings, the session menu and the calendar are.
fn panel_opens(compositor: &Compositor) -> (Client, WlSurface, ZwlrLayerSurfaceV1) {
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    let (surface, layer) = client.desktop(0x2000_0000).unwrap();
    layer.set_layer(Layer::Overlay);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();
    assert!(client.wait(SOON, |seen| seen.keyboard_enters == 1));
    (client, surface, layer)
}

#[test]
fn a_panel_opened_from_the_bar_keeps_it_until_it_closes() {
    let mut desk = desk();
    desk.maximise();
    desk.reveal_top();
    desk.click(640, 10);
    let (mut panel, surface, layer) = panel_opens(&desk.compositor);
    desk.go(640, 360);
    sleep(Duration::from_millis(900));
    stays(&desk.compositor, "top=shown bottom=hidden drop=30");
    layer.destroy();
    surface.destroy();
    panel.roundtrip().unwrap();
    settles(&desk.compositor, HIDDEN);
}

#[test]
fn a_panel_opened_while_nothing_shows_brings_nothing() {
    let mut desk = desk();
    desk.maximise();
    let _panel = panel_opens(&desk.compositor);
    stays(&desk.compositor, HIDDEN);
}

#[test]
fn the_dock_stays_while_its_own_menu_has_the_keyboard() {
    let mut desk = desk();
    desk.maximise();
    desk.reveal_bottom();
    let (surface, layer) = &desk.dock;
    // As sengi-dock does for its menu.
    layer.set_layer(Layer::Overlay);
    layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
    surface.commit();
    desk.shell.roundtrip().unwrap();
    desk.go(640, 360);
    stays(&desk.compositor, "bottom=shown");
    let (surface, layer) = &desk.dock;
    layer.set_layer(Layer::Top);
    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    surface.commit();
    desk.shell.roundtrip().unwrap();
    settles(&desk.compositor, HIDDEN);
}

#[test]
fn a_bar_that_goes_away_takes_nothing_with_it() {
    let mut desk = desk();
    desk.maximise();
    desk.reveal_top();
    let (surface, layer) = &desk.bar;
    layer.destroy();
    surface.destroy();
    desk.shell.roundtrip().unwrap();
    settles(&desk.compositor, "top=none bottom=hidden drop=0");
    // Nothing to bring: resting at the top does nothing.
    desk.go(640, 360);
    desk.go(640, 0);
    stays(&desk.compositor, "top=none");
}

#[test]
fn reduced_motion_moves_the_panels_at_once_and_they_still_wait() {
    let compositor =
        Compositor::start_with(None, Some("[appearance]\nreduce-motion = true\n"), None);
    let mut shell = compositor.connect();
    let _bar = shell.bar(BAR, RED).unwrap();
    compositor.input("move 640 360");
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    compositor.input("key super+a");
    assert!(screen(&compositor, FIRST).contains("top=hidden"));
    compositor.input("move 640 0");
    assert!(screen(&compositor, FIRST).contains("top=hidden"));
    settles(&compositor, "top=shown");
}

#[test]
fn each_screen_hides_and_shows_its_own() {
    let compositor = Compositor::start_args(&["--outputs", "1280x720,1280x720"], None);
    let mut shell = compositor.connect();
    let first = shell.output_named("HEADLESS-1").unwrap();
    let second = shell.output_named("HEADLESS-2").unwrap();
    let _one = shell.bar_on(Some(&first), BAR, RED).unwrap();
    let _two = shell.bar_on(Some(&second), BAR, RED).unwrap();
    compositor.input("move 1900 360");
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, _toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    compositor.input("key super+a");
    assert!(app.wait(SOON, |seen| seen.more.toplevel == Some((1278, 689, false))));
    app.paint(&surface, 1278, 689, GREEN).unwrap();
    settles_on(&compositor, "HEADLESS-2", "top=hidden bottom=none");
    settles_on(&compositor, FIRST, "cover=- top=shown");
    assert_eq!(shell.capture_named(FIRST).unwrap().at(5, 5), rgb(RED));
    assert_ne!(
        shell.capture_named("HEADLESS-2").unwrap().at(5, 5),
        rgb(RED)
    );

    // The other screen's top edge says nothing to this one.
    compositor.input("move 600 0");
    sleep(WHILE);
    assert!(screen(&compositor, "HEADLESS-2").contains("top=hidden"));
    compositor.input("move 1900 0");
    settles_on(&compositor, "HEADLESS-2", "top=shown");
    assert_eq!(
        shell.capture_named("HEADLESS-2").unwrap().at(5, 5),
        rgb(RED)
    );
    // Gone to the other screen, the pointer has left this one's bar.
    compositor.input("move 600 200");
    settles_on(&compositor, "HEADLESS-2", "top=hidden");

    // A screen pulled out takes its state with it, and the window that
    // moves to the other one has that one to itself.
    compositor.unplug("HEADLESS-2");
    settles_on(&compositor, FIRST, "cover=test_window top=hidden");
}
