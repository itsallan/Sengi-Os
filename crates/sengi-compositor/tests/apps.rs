//! The protocols ordinary programs expect, checked against the real
//! compositor on the headless backend. Where it matters what was drawn,
//! the test asks for a picture of the screen and looks at the pixels.

mod common;
mod harness;

use std::time::Duration;

use common::apps::SCREENCOPY;
use harness::{Compositor, SOON};

const RED: u32 = 0xffb0_2020;
const GREEN: u32 = 0xff20_b020;
const BLUE: u32 = 0xff20_20b0;

/// A colour as a picture has it.
fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

/// Not a test of its own: the compositor starts this as the screen locker,
/// see `harness::hand_over`.
#[test]
fn locker_stand_in() {
    harness::hand_over();
}

#[test]
fn what_is_selected_can_be_pasted_by_whoever_has_the_keyboard() {
    let compositor = Compositor::start(None);
    let mut first = compositor.connect();
    first.keyboard().unwrap();
    let _window = first.window(GREEN).unwrap();
    assert!(first.wait(SOON, |seen| seen.keyboard_enters == 1));
    let _selection = first
        .select_primary(first.seen.enter_serial.unwrap())
        .unwrap();
    assert!(first.wait(SOON, |seen| seen.more.primary_selections == 1));
    assert_eq!(first.seen.more.primary_types, ["text/plain"]);

    // Another program is told of it when the keyboard comes to it.
    let mut second = compositor.connect();
    let _device = second.primary_device().unwrap();
    assert_eq!(second.seen.more.primary_selections, 0);
    second.keyboard().unwrap();
    let _window = second.window(BLUE).unwrap();
    assert!(second.wait(SOON, |seen| seen.more.primary_selections == 1));
    assert_eq!(second.seen.more.primary_types, ["text/plain"]);
}

#[test]
fn only_the_program_in_front_can_bring_a_window_forward() {
    let compositor = Compositor::start(None);
    let mut behind = compositor.connect();
    behind.keyboard().unwrap();
    let (behind_surface, _toplevel) = behind.window(GREEN).unwrap();
    assert!(behind.wait(SOON, |seen| seen.keyboard_enters == 1));
    let mut front = compositor.connect();
    front.keyboard().unwrap();
    let (front_surface, _toplevel) = front.window(BLUE).unwrap();
    assert!(front.wait(SOON, |seen| seen.keyboard_enters == 1));
    assert!(behind.wait(SOON, |seen| seen.keyboard_leaves == 1));

    // The one behind cannot put itself in front: not with the serial of
    // the focus it once had, and not with none.
    for serial in [behind.seen.enter_serial, None] {
        let token = behind.activation_token(&behind_surface, serial).unwrap();
        behind.activate(&token, &behind_surface).unwrap();
        behind.wait(Duration::from_millis(300), |_| false);
        assert_eq!(behind.seen.keyboard_enters, 1, "it took the keyboard");
    }
    // Nor with a token it made up.
    behind.activate("not-a-token", &behind_surface).unwrap();
    behind.wait(Duration::from_millis(300), |_| false);
    assert_eq!(behind.seen.keyboard_enters, 1);

    // The one in front can hand the keyboard on, as a launcher does.
    let token = front
        .activation_token(&front_surface, front.seen.enter_serial)
        .unwrap();
    behind.activate(&token, &behind_surface).unwrap();
    assert!(
        behind.wait(SOON, |seen| seen.keyboard_enters == 2),
        "the window was not brought forward"
    );
}

#[test]
fn a_fullscreen_window_covers_the_bar_and_gives_it_back() {
    let compositor = Compositor::start(None);
    let mut shell = compositor.connect();
    let _bar = shell.bar(30, RED).unwrap();
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let (surface, toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    assert_eq!(
        app.capture().unwrap().at(5, 5),
        rgb(RED),
        "the bar is there"
    );

    toplevel.set_fullscreen(None);
    assert!(
        app.wait(SOON, |seen| seen.more.toplevel == Some((1280, 720, true))),
        "not told to fill the screen: {:?}",
        app.seen.more.toplevel
    );
    app.paint(&surface, 1280, 720, GREEN).unwrap();
    let picture = app.capture().unwrap();
    assert_eq!(picture.at(5, 5), rgb(GREEN), "the bar still shows");
    assert_eq!(picture.at(1275, 715), rgb(GREEN));

    toplevel.unset_fullscreen();
    assert!(app.wait(SOON, |seen| matches!(
        seen.more.toplevel,
        Some((300, 200, false))
    )));
    app.paint(&surface, 300, 200, GREEN).unwrap();
    let picture = app.capture().unwrap();
    assert_eq!(picture.at(5, 5), rgb(RED), "the bar did not come back");
    assert_eq!(
        picture.at(640, 360),
        rgb(GREEN),
        "the window is not back in place"
    );
}

/// How many pixels of `colour` the row across the middle of the screen has.
fn across_the_middle(client: &mut common::Client, colour: u32) -> usize {
    let picture = client.capture().unwrap();
    assert_eq!((picture.width, picture.height), (1280, 720));
    (0..picture.width)
        .filter(|x| picture.at(*x, 360) == rgb(colour))
        .count()
}

#[test]
fn a_screen_drawn_twice_as_large_says_so_and_draws_so() {
    let compositor = Compositor::start_args(&["--scale", "2"], None);
    let mut client = compositor.connect();
    assert_eq!(client.output_scale().unwrap(), Some(2));
    assert!(client.has_global("wp_viewporter"));
    let (surface, _toplevel) = client.window(GREEN).unwrap();
    assert_eq!(client.preferred_scale(&surface).unwrap(), Some(240));
    // A window 300 wide takes 600 pixels; the outermost are blended with
    // what is behind when a buffer is drawn larger than it is.
    let wide = across_the_middle(&mut client, GREEN);
    assert!((596..=600).contains(&wide), "{wide} pixels wide");
}

#[test]
fn a_screen_scaled_by_a_fraction_gives_the_fraction_to_those_who_ask() {
    let compositor = Compositor::start_args(&["--scale", "1.5"], None);
    let mut client = compositor.connect();
    // Those who cannot take a fraction draw for the next whole number.
    assert_eq!(client.output_scale().unwrap(), Some(2));
    let (surface, _toplevel) = client.window(GREEN).unwrap();
    assert_eq!(client.preferred_scale(&surface).unwrap(), Some(180));
    let wide = across_the_middle(&mut client, GREEN);
    assert!((446..=450).contains(&wide), "{wide} pixels wide");
}

#[test]
fn an_unscaled_screen_is_as_it_was() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    assert_eq!(client.output_scale().unwrap(), Some(1));
    let (surface, _toplevel) = client.window(GREEN).unwrap();
    assert_eq!(client.preferred_scale(&surface).unwrap(), Some(120));
    assert_eq!(across_the_middle(&mut client, GREEN), 300);
}

#[test]
fn a_locked_pointer_stays_put_and_its_movement_is_still_told() {
    let script = "1500 move 640 360\n2600 nudge 50 0\n3200 nudge 0 30\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let pointer = app.pointer().unwrap();
    let (surface, _toplevel) = app.window(GREEN).unwrap();
    let _relative = app.relative_pointer(&pointer).unwrap();
    assert!(app.wait(SOON, |seen| seen.more.pointer_enters == 1));
    let _lock = app.lock_pointer(&surface, &pointer).unwrap();
    assert!(app.wait(SOON, |seen| seen.more.pointer_locked == 1));
    let moved = app.seen.more.pointer_motions;

    assert!(app.wait(SOON, |seen| seen.more.relative.len() == 2));
    assert_eq!(app.seen.more.relative, [(50.0, 0.0), (0.0, 30.0)]);
    assert_eq!(app.seen.more.pointer_motions, moved, "the pointer moved");
    let said = compositor.status_line("pointer at");
    assert!(said.contains("pointer at 640,360"), "{said}");
}

#[test]
fn a_confined_pointer_does_not_leave_its_window() {
    // The window is 300 wide around the middle: 500 to the right is out
    // of it, 20 is not.
    let script = "1500 move 640 360\n2600 nudge 500 0\n3200 nudge 20 0\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let pointer = app.pointer().unwrap();
    let (surface, _toplevel) = app.window(GREEN).unwrap();
    let _relative = app.relative_pointer(&pointer).unwrap();
    assert!(app.wait(SOON, |seen| seen.more.pointer_enters == 1));
    let _confine = app.confine_pointer(&surface, &pointer).unwrap();
    assert!(app.wait(SOON, |seen| seen.more.pointer_confined == 1));

    assert!(app.wait(SOON, |seen| seen.more.relative.len() == 2));
    let said = compositor.status_line("pointer at");
    assert!(said.contains("pointer at 660,360"), "{said}");
}

#[test]
fn an_unheld_pointer_moves_by_what_the_mouse_reports() {
    let script = "1500 move 640 360\n2000 nudge 500 0\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let pointer = app.pointer().unwrap();
    let _window = app.window(GREEN).unwrap();
    let _relative = app.relative_pointer(&pointer).unwrap();
    assert!(app.wait(SOON, |seen| seen.more.relative.len() == 1));
    let said = compositor.status_line("pointer at");
    assert!(said.contains("pointer at 1140,360"), "{said}");
}

#[test]
fn a_picture_of_the_screen_shows_what_is_on_it() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    assert!(client.has_global(SCREENCOPY));
    let _window = client.window(GREEN).unwrap();
    let picture = client.capture().unwrap();
    assert_eq!((picture.width, picture.height), (1280, 720));
    assert_eq!(picture.at(640, 360), rgb(GREEN));
    assert_ne!(picture.at(5, 5), rgb(GREEN));
}

#[test]
fn no_picture_is_given_of_a_locked_screen() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    let _window = client.window(GREEN).unwrap();

    let mut locker = compositor.locker();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, RED).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    assert!(client.capture().is_err(), "a locked screen was copied");

    lock.unlock_and_destroy();
    locker.roundtrip().unwrap();
    assert_eq!(client.capture().unwrap().at(640, 360), rgb(GREEN));
}

#[test]
fn what_is_dragged_is_drawn_under_the_pointer() {
    let script = "1500 move 640 360\n1700 press left\n2600 move 900 500\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut app = compositor.connect();
    let _pointer = app.pointer().unwrap();
    let (surface, _toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.more.button_serial.is_some()));
    let serial = app.seen.more.button_serial.unwrap();
    let _drag = app.drag(&surface, serial, BLUE).unwrap();

    // Until the pointer has been taken away from the window.
    app.wait(Duration::from_millis(1500), |_| false);
    let picture = app.capture().unwrap();
    assert_eq!(
        picture.at(940, 540),
        rgb(BLUE),
        "no drag icon at the pointer"
    );
    assert_ne!(picture.at(640, 360), rgb(BLUE));
}
