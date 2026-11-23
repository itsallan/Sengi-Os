//! Several outputs, and the management of them through
//! wlr-output-management, as Settings does it, against the real compositor
//! on the headless backend.

mod common;
mod harness;

use std::time::Duration;

use common::heads::{Op, Outcome};
use harness::{Compositor, SOON};
use wayland_client::protocol::wl_output::Transform;

const GREEN: u32 = 0xff20_b020;

/// Not run by a plain `cargo test`: the compositor starts this test binary
/// as the screen locker, with `--include-ignored`, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

fn two() -> Compositor {
    Compositor::start_args(&["--outputs", "1280x720,1280x720"], None)
}

#[test]
fn every_output_is_a_head_with_what_a_client_needs() {
    let compositor = two();
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    assert_eq!(app.head_names(), ["HEADLESS-1", "HEADLESS-2"]);
    let (one, two) = (app.head("HEADLESS-1"), app.head("HEADLESS-2"));
    assert!(one.enabled && two.enabled);
    assert_eq!(one.position, (0, 0));
    assert_eq!(two.position, (1280, 0), "the second goes to the right");
    assert_eq!(one.scale, 1.0);
    assert_eq!(one.transform, Some(Transform::Normal));
    assert_eq!(one.make, "Sengi OS");
    assert_eq!(one.model, "Headless");
    // A virtual screen takes any size, so it lists no modes.
    assert!(one.modes.is_empty());
    assert!(app.seen.heads.serial.is_some());
    assert_eq!(app.seen.heads.dones, 1);

    let plain = app.plain_outputs().unwrap();
    let sizes: Vec<_> = plain.iter().map(|p| (p.name.clone(), p.size)).collect();
    assert_eq!(
        sizes,
        [
            ("HEADLESS-1".to_string(), Some((1280, 720))),
            ("HEADLESS-2".to_string(), Some((1280, 720)))
        ]
    );
}

#[test]
fn a_panel_lists_its_modes_and_what_it_is() {
    let compositor = Compositor::start_args(&["--outputs", "1920x1080:panel"], None);
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    let head = app.head("HEADLESS-1");
    assert_eq!(head.modes.len(), 3);
    assert_eq!(head.current_size(), Some((1920, 1080, 60_000)));
    assert_eq!(head.modes.iter().filter(|m| m.preferred).count(), 1);
    assert!(head.modes.iter().any(|m| m.size == (1280, 720)));
    assert_eq!(head.serial, "1920-1080");
    assert!(head.physical.is_some_and(|(w, h)| w > 0 && h > 0));
    assert_eq!(head.adaptive_sync, Some(0));
}

#[test]
fn a_scale_changes_the_output_and_its_neighbour_follows() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let serial = app.seen.heads.serial;
    let ops = [Op::keep("HEADLESS-1").scale(2.0), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_ne!(app.seen.heads.serial, serial, "a new serial goes out");
    assert_eq!(app.head("HEADLESS-1").scale, 2.0);
    // Half as wide on the desktop now: the other one moved up to it.
    assert_eq!(app.head("HEADLESS-2").position, (640, 0));
    let plain = app.plain_outputs().unwrap();
    assert_eq!(plain[0].scale, 2);
    assert_eq!(plain[1].scale, 1);
    assert_eq!(plain[1].position, (640, 0));
}

#[test]
fn an_output_is_moved_by_naming_where() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2").at(0, 720)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(app.head("HEADLESS-2").position, (0, 720));
    // Two at one place are refused, and nothing changes.
    let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2").at(100, 100)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Failed
    );
    assert_eq!(app.head("HEADLESS-2").position, (0, 720));
}

#[test]
fn an_output_is_switched_off_and_on() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1"), Op::off("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert!(app.head("HEADLESS-1").enabled);
    assert!(!app.head("HEADLESS-2").enabled);
    // A switched off head is still a head, and its wl_output is gone.
    assert_eq!(app.head_names().len(), 2);
    let plain = app.plain_outputs().unwrap();
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].name, "HEADLESS-1");

    let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert!(app.head("HEADLESS-2").enabled);
    assert_eq!(app.head("HEADLESS-2").position, (1280, 0));
    assert_eq!(app.plain_outputs().unwrap().len(), 2);
}

#[test]
fn every_output_off_is_refused() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::off("HEADLESS-1"), Op::off("HEADLESS-2")];
    for apply in [false, true] {
        assert_eq!(
            app.configure(&manager, None, &ops, apply).unwrap(),
            Outcome::Failed
        );
    }
    assert!(app.head("HEADLESS-1").enabled && app.head("HEADLESS-2").enabled);
    assert_eq!(app.plain_outputs().unwrap().len(), 2);
}

#[test]
fn a_test_changes_nothing() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let serial = app.seen.heads.serial;
    let ops = [Op::keep("HEADLESS-1").scale(2.0), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, false).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(app.head("HEADLESS-1").scale, 1.0);
    assert_eq!(app.seen.heads.serial, serial);
    // A test that cannot work says so as well.
    let ops = [Op::keep("HEADLESS-1").scale(40.0), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, false).unwrap(),
        Outcome::Failed
    );
}

#[test]
fn a_stale_serial_is_cancelled() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let old = app.seen.heads.serial.unwrap();
    let ops = [Op::keep("HEADLESS-1").scale(1.5), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, Some(old), &ops, true).unwrap(),
        Outcome::Succeeded
    );
    // Asked again against what it knew before: that is no longer so.
    let ops = [Op::keep("HEADLESS-1").scale(2.0), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, Some(old), &ops, true).unwrap(),
        Outcome::Cancelled
    );
    assert_eq!(app.head("HEADLESS-1").scale, 1.5);
    // With the serial it has now it goes through.
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(app.head("HEADLESS-1").scale, 2.0);
}

#[test]
fn an_output_plugged_in_while_a_configuration_is_made_cancels_it() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1").scale(2.0), Op::keep("HEADLESS-2")];
    let before = app.seen.heads.outcomes.len();
    let configuration = app.half_configuration(&manager, &ops).unwrap();
    let name = compositor.plug("800x600");
    configuration.apply();
    assert!(app.wait(SOON, |seen| seen.heads.outcomes.len() > before));
    assert_eq!(app.seen.heads.outcomes.last(), Some(&Outcome::Cancelled));
    assert_eq!(app.head("HEADLESS-1").scale, 1.0);
    // The new head came through as news, and goes to the right.
    assert!(app.head_names().contains(&name));
    assert_eq!(app.head(&name).position, (2560, 0));
    assert!(app.head(&name).enabled);
    // Made again against the news, it is applied, the new head included.
    let ops = [
        Op::keep("HEADLESS-1").scale(2.0),
        Op::keep("HEADLESS-2"),
        Op::keep(&name),
    ];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(app.head("HEADLESS-1").scale, 2.0);
}

#[test]
fn an_output_pulled_out_is_finished_and_the_others_are_told() {
    let compositor = two();
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    let dones = app.seen.heads.dones;
    compositor.unplug("HEADLESS-2");
    assert!(app.wait(SOON, |seen| seen.heads.dones > dones));
    assert_eq!(app.head_names(), ["HEADLESS-1"]);
    assert_eq!(app.plain_outputs().unwrap().len(), 1);
}

#[test]
fn a_client_that_dies_in_the_middle_leaves_nothing() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let serial = app.seen.heads.serial;
    {
        let mut other = compositor.connect();
        let other_manager = other.manager().unwrap();
        let ops = [Op::keep("HEADLESS-1").scale(2.0), Op::off("HEADLESS-2")];
        let _half = other.half_configuration(&other_manager, &ops).unwrap();
        // It goes away with the configuration made and never applied.
    }
    {
        // Another applies and goes at once, before it hears back.
        let mut other = compositor.connect();
        let other_manager = other.manager().unwrap();
        let ops = [Op::keep("HEADLESS-1").scale(1.5), Op::keep("HEADLESS-2")];
        let configuration = other.half_configuration(&other_manager, &ops).unwrap();
        configuration.apply();
        other.queue.flush().ok();
    }
    std::thread::sleep(Duration::from_millis(300));
    app.settle().unwrap();
    // Only the one that was applied is in force, whole.
    assert_eq!(app.head("HEADLESS-1").scale, 1.5);
    assert!(app.head("HEADLESS-2").enabled);
    assert_ne!(app.seen.heads.serial, serial);
    assert!(compositor.said("panicked").is_none());
    // And the compositor goes on taking configurations.
    let ops = [Op::keep("HEADLESS-1").scale(1.0), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
}

#[test]
fn leaving_a_head_out_is_the_clients_error_and_nobody_elses() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let configuration = app
        .half_configuration(&manager, &[Op::keep("HEADLESS-1")])
        .unwrap();
    configuration.apply();
    assert!(app.roundtrip().is_err(), "the protocol error is reported");
    // Other clients are not touched by it.
    let mut other = compositor.connect();
    let _manager = other.manager().unwrap();
    assert_eq!(other.head_names().len(), 2);
}

#[test]
fn a_custom_size_resizes_a_virtual_screen() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [
        Op::keep("HEADLESS-1").custom(800, 600, 0),
        Op::keep("HEADLESS-2"),
    ];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    let plain = app.plain_outputs().unwrap();
    assert_eq!(plain[0].size, Some((800, 600)));
    // The next one is where the first ends.
    assert_eq!(plain[1].position, (800, 0));
    let picture = app.capture_named("HEADLESS-1").unwrap();
    assert_eq!((picture.width, picture.height), (800, 600));
    // Not a size anybody has.
    let ops = [
        Op::keep("HEADLESS-1").custom(0, 600, 0),
        Op::keep("HEADLESS-2"),
    ];
    assert!(
        app.half_configuration(&manager, &ops).is_err(),
        "a mode of no width is a protocol error"
    );
}

#[test]
fn a_panel_changes_to_one_of_its_modes_and_to_no_other() {
    let compositor = Compositor::start_args(&["--outputs", "1920x1080:panel"], None);
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1").mode(1920, 1080, 30_000)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(
        app.head("HEADLESS-1").current_size(),
        Some((1920, 1080, 30_000))
    );
    let ops = [Op::keep("HEADLESS-1").mode(1280, 720, 60_000)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(
        app.head("HEADLESS-1").current_size(),
        Some((1280, 720, 60_000))
    );
    let plain = app.plain_outputs().unwrap();
    assert_eq!(plain[0].size, Some((1280, 720)));
    // A size it does not have: refused, still as it was.
    let ops = [Op::keep("HEADLESS-1").custom(1000, 700, 60_000)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Failed
    );
    assert_eq!(
        app.head("HEADLESS-1").current_size(),
        Some((1280, 720, 60_000))
    );
}

#[test]
fn a_rotated_output_swaps_its_width_and_height() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [
        Op::keep("HEADLESS-1").transform(Transform::_90),
        Op::keep("HEADLESS-2"),
    ];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(app.head("HEADLESS-1").transform, Some(Transform::_90));
    // 720 wide on the desktop now, and the neighbour came up to it.
    assert_eq!(app.head("HEADLESS-2").position, (720, 0));
    let picture = app.capture_named("HEADLESS-1").unwrap();
    assert_eq!((picture.width, picture.height), (1280, 720));
}

#[test]
fn adaptive_sync_is_not_there_to_turn_on() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let mut on = Op::keep("HEADLESS-1");
    on.sync = Some(true);
    let ops = [on, Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Failed
    );
    let _ = GREEN;
}

// What the compositor does with several outputs, as opposed to what it
// says about them.

const RED: u32 = 0xffb0_2020;
const BLUE: u32 = 0xff20_20b0;
const DESKTOP: u32 = 0x0012_1316;
const CARD: u32 = 0x001f_2126;

fn rgb(argb: u32) -> u32 {
    argb & 0x00ff_ffff
}

/// The saved layout that puts the second of two outputs at twice the scale.
const SECOND_AT_TWICE: &str = "[HEADLESS-2]\nenabled = true\nx = 1280\ny = 0\nscale = 2000\n";

fn twice_as_large() -> Compositor {
    Compositor::start_with_layout(
        &["--outputs", "1280x720,1280x720"],
        Some(SECOND_AT_TWICE),
        "60000",
    )
}

/// How many pixels of `colour` there are along row `y`.
fn across(picture: &common::apps::Picture, y: u32, colour: u32) -> usize {
    (0..picture.width)
        .filter(|x| picture.at(*x, y) == rgb(colour))
        .count()
}

fn first_x(picture: &common::apps::Picture, y: u32, colour: u32) -> Option<u32> {
    (0..picture.width).find(|x| picture.at(*x, y) == rgb(colour))
}

fn last_x(picture: &common::apps::Picture, y: u32, colour: u32) -> Option<u32> {
    (0..picture.width)
        .rev()
        .find(|x| picture.at(*x, y) == rgb(colour))
}

fn count(picture: &common::apps::Picture, colour: u32) -> usize {
    (0..picture.height)
        .map(|y| across(picture, y, colour))
        .sum()
}

#[test]
fn a_window_is_drawn_at_the_scale_of_the_output_it_is_on() {
    let compositor = twice_as_large();
    let mut app = compositor.connect();
    let plain = app.plain_outputs().unwrap();
    assert_eq!((plain[0].scale, plain[1].scale), (1, 2));
    assert_eq!(plain[1].position, (1280, 0));

    compositor.input("move 100 100");
    let (_one, _t1) = app.window(GREEN).unwrap();
    compositor.input("move 1500 100");
    let (_two, _t2) = app.window(RED).unwrap();

    let first = app.capture_named("HEADLESS-1").unwrap();
    let second = app.capture_named("HEADLESS-2").unwrap();
    assert_eq!((first.width, first.height), (1280, 720));
    assert_eq!((second.width, second.height), (1280, 720));
    // 300 logical pixels: 300 on the first, 600 on the second.
    assert_eq!(across(&first, 360, GREEN), 300);
    let wide = across(&second, 360, RED);
    assert!((596..=600).contains(&wide), "{wide} wide on the second");
    // And each only on its own output.
    assert_eq!(count(&first, RED), 0);
    assert_eq!(count(&second, GREEN), 0);
}

#[test]
fn a_window_over_two_outputs_is_drawn_on_each_at_its_scale() {
    let compositor = twice_as_large();
    let mut app = compositor.connect();
    compositor.input("move 100 100");
    let (surface, _toplevel) = app.decorated_window(GREEN).unwrap();
    assert_eq!(app.preferred_scale(&surface).unwrap(), Some(120));
    let before = app.capture_named("HEADLESS-1").unwrap();
    let left = first_x(&before, 360, GREEN).expect("the window is on the first output");
    let right = last_x(&before, 360, GREEN).unwrap();
    assert_eq!(right - left + 1, 300);
    // The frame around it: one border, in the first output's pixels.
    let line = before.at(left - 1, 360);
    let border = (0..20)
        .take_while(|back| before.at(left - 1 - back, 360) == line)
        .count() as u32;
    assert!(border >= 1 && line != rgb(DESKTOP), "no border");
    // The title bar is above the client area; find where it is.
    let top = (0..360u32)
        .rev()
        .find(|y| before.at(left + 150, *y) == rgb(DESKTOP))
        .unwrap()
        + 1;
    let client_top = (0..360u32)
        .find(|y| before.at(left + 150, *y) == rgb(GREEN))
        .unwrap();
    let bar = client_top - top;
    assert!(bar >= 10, "a title bar of {bar}");

    // Drag it by the title bar over the edge between the outputs.
    let (grab_x, grab_y) = (left + 200, top + bar / 2);
    compositor.input(&format!("move {grab_x} {grab_y}"));
    compositor.input("press left");
    compositor.input(&format!("move {} {grab_y}", grab_x + 690));
    compositor.input("release left");
    app.roundtrip().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    // Most of it is on the second output now, and it is told so.
    assert!(
        app.wait(SOON, |seen| seen.more.preferred_scale == Some(240)),
        "told {:?}",
        app.seen.more.preferred_scale
    );

    let one = app.capture_named("HEADLESS-1").unwrap();
    let two = app.capture_named("HEADLESS-2").unwrap();
    // The client area went from 1180 to 1480: a hundred on the first
    // output, two hundred, drawn as four hundred, on the second.
    let on_one = across(&one, 360, GREEN);
    // Row 300 on the desktop is 600 in the pixels of the second output.
    let on_two = across(&two, 600, GREEN);
    assert_eq!(on_one, 100, "on the first output");
    assert!((396..=400).contains(&on_two), "{on_two} on the second");
    // The frame on the second output is drawn at its scale too: the
    // right border is twice as thick as the left one was.
    let end = last_x(&two, 600, GREEN).unwrap();
    let thick = (1..40)
        .map(|more| two.at(end + more, 600))
        .skip_while(|colour| *colour != line)
        .take_while(|colour| *colour == line)
        .count() as u32;
    assert_eq!(
        thick,
        border * 2,
        "border {border} px on one, {thick} on two"
    );
    // And the title bar: as tall again on the second as on the first.
    let bar_one = bar_height(&one, 1250, 360);
    let bar_two = bar_height(&two, 360, 600);
    // Give or take the row where the client's scaled edge is blended.
    assert!(
        bar_two.abs_diff(bar_one * 2) <= 1,
        "title bar {bar_one} px and {bar_two}"
    );
}

/// How tall the title bar is above the client area in column `x`, the
/// client area being green on row `row`.
fn bar_height(picture: &common::apps::Picture, x: u32, row: u32) -> u32 {
    let client = (0..row)
        .find(|y| picture.at(x, *y) == rgb(GREEN))
        .expect("the client area");
    let above = (0..client)
        .rev()
        .find(|y| picture.at(x, *y) == rgb(DESKTOP))
        .map_or(0, |y| y + 1);
    client - above
}

#[test]
fn each_output_is_told_its_own_preferred_scale() {
    let compositor = twice_as_large();
    let mut one = compositor.connect();
    let mut two = compositor.connect();
    compositor.input("move 100 100");
    let (a, _ta) = one.window(GREEN).unwrap();
    compositor.input("move 1500 100");
    let (b, _tb) = two.window(RED).unwrap();
    assert_eq!(one.preferred_scale(&a).unwrap(), Some(120));
    assert_eq!(two.preferred_scale(&b).unwrap(), Some(240));
    // Moved to the other output, a window is told the other scale.
    let mut mover = compositor.connect();
    compositor.input("move 100 100");
    let (c, _tc) = mover.decorated_window(BLUE).unwrap();
    assert_eq!(mover.preferred_scale(&c).unwrap(), Some(120));
}

#[test]
fn a_new_window_opens_where_the_pointer_is() {
    let compositor = two();
    let mut app = compositor.connect();
    compositor.input("move 1500 300");
    let (_surface, _toplevel) = app.window(GREEN).unwrap();
    let one = app.capture_named("HEADLESS-1").unwrap();
    let two = app.capture_named("HEADLESS-2").unwrap();
    assert_eq!(count(&one, GREEN), 0, "on the first output");
    assert_eq!(count(&two, GREEN), 300 * 200);
    // Centred on the second, as the first would have had it.
    assert_eq!(two.at(640, 360), rgb(GREEN));
}

#[test]
fn the_pointer_crosses_from_one_output_to_the_next_and_takes_its_focus_along() {
    let compositor = two();
    let mut left = compositor.connect();
    let mut right = compositor.connect();
    let _left_pointer = left.pointer().unwrap();
    let _right_pointer = right.pointer().unwrap();
    left.keyboard().unwrap();
    right.keyboard().unwrap();
    compositor.input("move 100 100");
    let (_a, _ta) = left.window(GREEN).unwrap();
    compositor.input("move 1500 100");
    let (_b, _tb) = right.window(RED).unwrap();

    compositor.input("move 640 360");
    assert!(left.wait(SOON, |seen| seen.more.pointer_enters == 1));
    assert_eq!(right.seen.more.pointer_enters, 0);
    compositor.input("move 1920 360");
    assert!(right.wait(SOON, |seen| seen.more.pointer_enters == 1));
    // A click there gives that window the keyboard, and takes it from the
    // other.
    compositor.input("click left");
    assert!(right.wait(SOON, |seen| seen.keyboard_enters >= 1));
    assert!(left.wait(SOON, |seen| seen.keyboard_leaves >= 1));

    // The mouse itself carries the pointer over the edge, and does not stop
    // it there.
    compositor.input("move 1275 400");
    compositor.input("nudge 10 0");
    let report = compositor.status_line("pointer at");
    assert!(report.contains("pointer at 1285,400"), "{report}");
    // But not off the desktop.
    compositor.input("nudge 5000 0");
    let report = compositor.status_line("pointer at");
    assert!(report.contains("pointer at 2559,400"), "{report}");
}

#[test]
fn maximising_fills_the_output_the_window_is_on() {
    let compositor = twice_as_large();
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    compositor.input("move 1500 100");
    let (_surface, _toplevel) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    compositor.input("key super+a");
    // The second output is 640 by 360 on the desktop.
    assert!(
        app.wait(SOON, |seen| seen.more.toplevel == Some((640, 360, false))),
        "told {:?}",
        app.seen.more.toplevel
    );
    let mut other = compositor.connect();
    other.keyboard().unwrap();
    compositor.input("move 100 100");
    let (_s, _t) = other.window(RED).unwrap();
    assert!(other.wait(SOON, |seen| seen.keyboard_enters == 1));
    compositor.input("key super+a");
    assert!(
        other.wait(SOON, |seen| seen.more.toplevel == Some((1280, 720, false))),
        "told {:?}",
        other.seen.more.toplevel
    );
}

#[test]
fn a_bar_takes_room_from_its_own_output_only() {
    let compositor = twice_as_large();
    let mut shell = compositor.connect();
    let second = shell.output_named("HEADLESS-2").unwrap();
    let (_surface, _bar) = shell.bar_on(Some(&second), 30, RED).unwrap();
    // The bar is as wide as the output is on the desktop.
    assert_eq!(shell.seen.more.layer_configure.map(|c| c.1), None);
    let picture = shell.capture_named("HEADLESS-2").unwrap();
    assert_eq!(picture.at(5, 5), rgb(RED), "the bar is on the second");
    assert_eq!(picture.at(5, 56), rgb(RED), "thirty pixels, drawn twice");
    assert_ne!(picture.at(5, 62), rgb(RED));
    let first = shell.capture_named("HEADLESS-1").unwrap();
    assert_ne!(first.at(5, 5), rgb(RED), "not on the first");

    // A window maximised on the first output is not shorter for it.
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    compositor.input("move 100 100");
    let (_s, _t) = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    compositor.input("key super+a");
    assert!(app.wait(SOON, |seen| seen.more.toplevel == Some((1280, 720, false))));
    // And on the second it is.
    let mut other = compositor.connect();
    other.keyboard().unwrap();
    compositor.input("move 1500 300");
    let (_s2, _t2) = other.window(BLUE).unwrap();
    assert!(other.wait(SOON, |seen| seen.keyboard_enters == 1));
    compositor.input("key super+a");
    assert!(other.wait(SOON, |seen| seen.more.toplevel == Some((640, 330, false))));
}

#[test]
fn a_fullscreen_window_fills_only_its_output() {
    let compositor = twice_as_large();
    let mut app = compositor.connect();
    compositor.input("move 1500 100");
    let (_surface, toplevel) = app.window(GREEN).unwrap();
    toplevel.set_fullscreen(None);
    assert!(
        app.wait(SOON, |seen| seen.more.toplevel == Some((640, 360, true))),
        "told {:?}",
        app.seen.more.toplevel
    );
}

#[test]
fn a_window_on_an_output_that_is_pulled_out_moves_to_the_one_that_is_left() {
    let compositor = two();
    let mut app = compositor.connect();
    compositor.input("move 1500 300");
    let (_surface, _toplevel) = app.window(GREEN).unwrap();
    assert_eq!(count(&app.capture_named("HEADLESS-1").unwrap(), GREEN), 0);
    compositor.unplug("HEADLESS-2");
    let picture = app.capture_named("HEADLESS-1").unwrap();
    assert_eq!(
        count(&picture, GREEN),
        300 * 200,
        "the window is not on the first"
    );
    // Its size is whole and it is not under the edge.
    let left = first_x(&picture, 300, GREEN).unwrap();
    assert!(left + 300 <= 1280);
}

#[test]
fn a_fullscreen_window_on_an_output_that_is_pulled_out_fills_the_one_left() {
    let compositor = two();
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    compositor.input("move 1500 300");
    let (surface, toplevel) = app.window(GREEN).unwrap();
    toplevel.set_fullscreen(None);
    assert!(app.wait(SOON, |seen| seen.more.toplevel == Some((1280, 720, true))));
    app.paint(&surface, 1280, 720, GREEN).unwrap();
    compositor.plug("800x600 SMALL-1");
    compositor.unplug("HEADLESS-2");
    // Told to fill what is left, which is the first.
    assert!(app.wait(SOON, |seen| seen.more.toplevel == Some((1280, 720, true))));
    compositor.unplug("HEADLESS-1");
    assert!(
        app.wait(SOON, |seen| seen.more.toplevel == Some((800, 600, true))),
        "told {:?}",
        app.seen.more.toplevel
    );
}

#[test]
fn a_picture_is_taken_of_each_output_on_its_own() {
    let compositor = Compositor::start_args(&["--outputs", "1280x720,800x600"], None);
    let mut app = compositor.connect();
    compositor.input("move 100 100");
    let (_a, _ta) = app.window(GREEN).unwrap();
    compositor.input("move 1500 100");
    let (_b, _tb) = app.window(RED).unwrap();
    let one = app.capture_named("HEADLESS-1").unwrap();
    let two = app.capture_named("HEADLESS-2").unwrap();
    assert_eq!((one.width, one.height), (1280, 720));
    assert_eq!((two.width, two.height), (800, 600));
    assert_eq!(one.at(640, 360), rgb(GREEN));
    assert_eq!(two.at(400, 300), rgb(RED));
    assert_eq!(count(&one, RED), 0);
    assert_eq!(count(&two, GREEN), 0);
}

#[test]
fn the_switcher_opens_on_the_output_the_pointer_is_on() {
    let compositor = two();
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    compositor.input("move 100 100");
    let (_a, _ta) = app.window(GREEN).unwrap();
    let mut other = compositor.connect();
    let (_b, _tb) = other.window(RED).unwrap();
    // The pointer goes to the second output, where there is no window.
    compositor.input("move 1500 600");
    compositor.input("keydown alt");
    compositor.input("key tab");
    std::thread::sleep(Duration::from_millis(100));
    let second = app.capture_named("HEADLESS-2").unwrap();
    let first = app.capture_named("HEADLESS-1").unwrap();
    // The card is in the middle of the second and not on the first.
    assert_eq!(second.at(640, 360), rgb(CARD), "no card on the second");
    assert_ne!(first.at(640, 360), rgb(CARD));
    assert_eq!(count(&first, CARD), 0);
    compositor.input("keyup alt");
    std::thread::sleep(Duration::from_millis(100));
    let second = app.capture_named("HEADLESS-2").unwrap();
    assert_eq!(count(&second, CARD), 0, "the card stays");

    // With the pointer on the first it opens there.
    compositor.input("move 100 600");
    compositor.input("keydown alt");
    compositor.input("key tab");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        app.capture_named("HEADLESS-1").unwrap().at(640, 360),
        rgb(CARD)
    );
    assert_eq!(count(&app.capture_named("HEADLESS-2").unwrap(), CARD), 0);
    compositor.input("keyup alt");
}

fn scratch(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("sengi-outputs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

/// Waits for a screenshot the script saves, and reads it.
fn shot(path: &std::path::Path) -> (u32, u32, Vec<u32>) {
    let end = std::time::Instant::now() + Duration::from_secs(12);
    while std::time::Instant::now() < end {
        if path.exists() {
            std::thread::sleep(Duration::from_millis(200));
            return common::heads::read_png(path);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no screenshot at {}", path.display());
}

#[test]
fn the_lock_covers_every_output_also_one_plugged_in_under_it() {
    let path = scratch("lock.png");
    let script = format!("5000 screenshot {}\n", path.display());
    let compositor = Compositor::start_args(&["--outputs", "1280x720,1280x720"], Some(&script));
    let mut app = compositor.connect();
    compositor.input("move 1500 300");
    let (_surface, _toplevel) = app.window(GREEN).unwrap();

    let mut locker = compositor.locker();
    let lock = locker.lock().unwrap();
    // The locker has a screen for the first output and none for the second.
    let first = locker.output_named("HEADLESS-1").unwrap();
    let _held = locker.lock_surface(&lock, &first, RED).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1), "not locked");
    // A screen that is plugged in while locked is covered at once.
    compositor.plug("800x600");

    let (width, height, pixels) = shot(&path);
    assert_eq!((width, height), (1280 + 1280 + 800, 720));
    let at = |x: u32, y: u32| pixels[(y * width + x) as usize];
    for y in 0..720 {
        for x in 0..1280 {
            assert_eq!(at(x, y), rgb(RED), "the lock screen has a hole at {x},{y}");
        }
    }
    let cover = at(1280, 0);
    assert_ne!(cover, rgb(DESKTOP));
    assert_ne!(cover, rgb(GREEN));
    for (from, to, rows) in [(1280, 2560, 720), (2560, 3360, 600)] {
        for y in 0..rows {
            for x in from..to {
                // The pointer is drawn over the lock screen, at 1500,300.
                if (1490..1540).contains(&x) && (290..340).contains(&y) {
                    continue;
                }
                assert_eq!(at(x, y), cover, "something shows at {x},{y}");
            }
        }
    }
}

#[test]
fn a_lock_screen_on_each_output_shows_on_that_output() {
    let path = scratch("lock-both.png");
    let script = format!("5000 screenshot {}\n", path.display());
    let compositor = Compositor::start_args(&["--outputs", "1280x720,1280x720"], Some(&script));
    let mut locker = compositor.locker();
    let lock = locker.lock().unwrap();
    let first = locker.output_named("HEADLESS-1").unwrap();
    let second = locker.output_named("HEADLESS-2").unwrap();
    let _a = locker.lock_surface(&lock, &first, RED).unwrap();
    let _b = locker.lock_surface(&lock, &second, BLUE).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    let (width, _, pixels) = shot(&path);
    let at = |x: u32, y: u32| pixels[(y * width + x) as usize];
    assert_eq!(at(10, 10), rgb(RED));
    assert_eq!(at(1270, 710), rgb(RED));
    assert_eq!(at(1290, 10), rgb(BLUE));
    assert_eq!(at(2550, 710), rgb(BLUE));
    // A locked screen cannot be copied, either.
    let mut spy = compositor.connect();
    assert!(spy.capture_named("HEADLESS-2").is_err());
}

#[test]
fn a_layout_that_was_kept_is_what_a_new_session_starts_with() {
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720,1280x720"], None, "300");
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [
        Op::keep("HEADLESS-1").scale(2.0),
        Op::keep("HEADLESS-2").transform(Transform::_180),
    ];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2").at(0, 360)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    // Not at once: only what stays is kept.
    assert!(compositor.saved_layout().is_none());
    let end = std::time::Instant::now() + Duration::from_secs(5);
    let saved = loop {
        if let Some(saved) = compositor.saved_layout() {
            break saved;
        }
        assert!(std::time::Instant::now() < end, "never saved");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(saved.contains("scale = 2000"), "{saved}");
    assert!(saved.contains("transform = \"180\""), "{saved}");

    // A new session, with that file.
    drop(app);
    let again =
        Compositor::start_with_layout(&["--outputs", "1280x720,1280x720"], Some(&saved), "60000");
    let mut app = again.connect();
    let _manager = app.manager().unwrap();
    assert_eq!(app.head("HEADLESS-1").scale, 2.0);
    assert_eq!(app.head("HEADLESS-2").transform, Some(Transform::_180));
    assert_eq!(app.head("HEADLESS-2").position, (0, 360));
    let plain = app.plain_outputs().unwrap();
    assert_eq!(plain[0].scale, 2);
}

#[test]
fn a_change_that_is_taken_back_is_not_what_is_kept() {
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720,1280x720"], None, "1500");
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1").scale(2.0), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    // The way back, as Settings takes a change that nobody keeps.
    let ops = [Op::keep("HEADLESS-1").scale(1.0), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    let end = std::time::Instant::now() + Duration::from_secs(6);
    let saved = loop {
        if let Some(saved) = compositor.saved_layout() {
            break saved;
        }
        assert!(std::time::Instant::now() < end, "never saved");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(!saved.contains("scale = 2000"), "{saved}");
    assert!(saved.contains("scale = 1000"), "{saved}");
}

#[test]
fn a_session_that_ends_with_a_change_on_trial_keeps_nothing() {
    let compositor = Compositor::start_with_layout(&["--outputs", "1280x720"], None, "60000");
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1").scale(2.0)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(compositor.saved_layout().is_none());
}

#[test]
fn what_the_file_says_of_screens_that_are_not_there_is_ignored() {
    let layout = "[DP-7]\nenabled = false\nscale = 2000\n\n[HEADLESS-2]\nenabled = false\n\n[HEADLESS-1]\nscale = 1500\nmode = \"640x480@60000\"\n";
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720,1280x720"], Some(layout), "60000");
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    // The one that is there takes what was kept for it, the size too.
    assert_eq!(app.head("HEADLESS-1").scale, 1.5);
    assert!(!app.head("HEADLESS-2").enabled);
    assert_eq!(app.head_names().len(), 2, "DP-7 is not a head");
    assert_eq!(app.plain_outputs().unwrap().len(), 1);
    let plain = app.plain_outputs().unwrap();
    assert_eq!(plain[0].size, Some((640, 480)));
}

#[test]
fn a_screen_that_was_off_when_unplugged_comes_back_off_and_a_new_one_does_not() {
    let layout = "[EXTRA-1]\nenabled = false\n";
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720"], Some(layout), "60000");
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    compositor.plug("800x600 EXTRA-1");
    assert!(app.wait(SOON, |seen| seen.heads.heads.len() == 2));
    assert!(!app.head("EXTRA-1").enabled);
    compositor.plug("800x600 EXTRA-2");
    assert!(app.wait(SOON, |seen| seen.heads.heads.len() == 3));
    assert!(app.head("EXTRA-2").enabled);
    assert_eq!(app.head("EXTRA-2").position, (1280, 0));
}

#[test]
fn a_saved_off_is_not_kept_when_it_would_leave_nothing() {
    let layout = "[HEADLESS-1]\nenabled = false\n";
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720"], Some(layout), "60000");
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    assert!(app.head("HEADLESS-1").enabled);
}

#[test]
fn the_saved_layout_is_read_again_on_sighup_but_not_over_a_change_waiting_to_be_saved() {
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720,1280x720"], None, "60000");
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    compositor.write_layout("[HEADLESS-1]\nscale = 1500\n");
    compositor.signal("HUP");
    assert!(app.wait(SOON, |seen| seen
        .heads
        .heads
        .iter()
        .any(|h| h.name == "HEADLESS-1" && h.scale == 1.5)));

    // A change is waiting to be saved: the file does not know of it, and
    // does not undo it.
    let ops = [Op::keep("HEADLESS-1").scale(2.0), Op::keep("HEADLESS-2")];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    compositor.write_layout("[HEADLESS-1]\nscale = 1250\n");
    compositor.signal("HUP");
    app.wait(Duration::from_millis(500), |_| false);
    assert_eq!(app.head("HEADLESS-1").scale, 2.0);
}

// A client that asks for what cannot be, or cannot be trusted with it.

#[test]
fn a_place_at_the_end_of_the_number_range_is_refused_and_nothing_breaks() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    for (x, y) in [
        (i32::MAX, 0),
        (0, i32::MAX),
        (i32::MIN, i32::MIN),
        (i32::MAX - 1279, 0),
        (1 << 21, 0),
    ] {
        let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2").at(x, y)];
        let outcome = app.configure(&manager, None, &ops, true);
        assert_eq!(
            outcome.as_ref().ok(),
            Some(&Outcome::Failed),
            "{x},{y}: {:?}",
            compositor.said_all("panicked")
        );
    }
    assert_eq!(app.head("HEADLESS-2").position, (1280, 0));
    // The pointer still moves.
    compositor.input("move 5 5");
    compositor.input("nudge 10 0");
    assert!(compositor
        .status_line("pointer at")
        .contains("pointer at 15,5"));
}

#[test]
fn a_saved_place_at_the_end_of_the_number_range_does_not_stop_a_session_starting() {
    let layout = "[HEADLESS-1]\nx = 2147483647\ny = -2147483648\n\n[HEADLESS-2]\nx = 2147483000\ny = 2147483000\n";
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720,1280x720"], Some(layout), "60000");
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    assert_eq!(app.head("HEADLESS-1").position, (0, 0));
    assert_eq!(app.head("HEADLESS-2").position, (1280, 0));
    compositor.input("move 5 5");
    compositor.input("nudge 10 0");
    assert!(compositor
        .status_line("pointer at")
        .contains("pointer at 15,5"));
}

#[test]
fn a_configuration_used_twice_is_the_clients_error() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2")];
    let configuration = app.half_configuration(&manager, &ops).unwrap();
    let before = app.seen.heads.outcomes.len();
    configuration.test();
    assert!(app.wait(SOON, |seen| seen.heads.outcomes.len() > before));
    configuration.apply();
    assert!(app.roundtrip().is_err());
    // And the compositor lives for the next client.
    let mut other = compositor.connect();
    let _m = other.manager().unwrap();
    assert_eq!(other.head_names().len(), 2);
}

#[test]
fn a_head_enabled_twice_or_after_the_configuration_was_used_is_the_clients_error() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-1")];
    assert!(app.half_configuration(&manager, &ops).is_err());

    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2")];
    let configuration = app.half_configuration(&manager, &ops).unwrap();
    let before = app.seen.heads.outcomes.len();
    configuration.test();
    assert!(app.wait(SOON, |seen| seen.heads.outcomes.len() > before));
    let head = app.head("HEADLESS-1").proxy.clone();
    let _late = configuration.enable_head(&head, &app.handle, ());
    assert!(app.roundtrip().is_err());
    let mut other = compositor.connect();
    let _m = other.manager().unwrap();
    assert_eq!(other.head_names().len(), 2);
}

#[test]
fn a_setting_given_twice_a_scale_of_nothing_and_a_mode_of_another_head_are_errors() {
    let compositor = Compositor::start_args(&["--outputs", "1280x720:panel,1280x720:panel"], None);
    let ask = |what: &dyn Fn(&mut common::Client, &common::heads::HeadInfoRef)| {
        let mut app = compositor.connect();
        let manager = app.manager().unwrap();
        let configuration =
            manager.create_configuration(app.seen.heads.serial.unwrap(), &app.handle, ());
        let head = app.head("HEADLESS-1").proxy.clone();
        let settings = configuration.enable_head(&head, &app.handle, ());
        let _ = &settings;
        what(&mut app, &common::heads::HeadInfoRef { settings });
        app.roundtrip().is_err()
    };
    assert!(ask(&|_, h| {
        h.settings.set_scale(1.5);
        h.settings.set_scale(2.0);
    }));
    assert!(ask(&|_, h| h.settings.set_scale(0.0)));
    assert!(ask(&|_, h| h.settings.set_scale(-1.0)));
    assert!(ask(&|app, h| {
        let other = app.head("HEADLESS-2").modes[0].proxy.clone();
        h.settings.set_mode(&other);
    }));
    assert!(ask(&|_, h| {
        h.settings.set_custom_mode(800, 600, 0);
        h.settings.set_position(0, 0);
        h.settings.set_position(5, 5);
    }));
    // One that is in order is not.
    assert!(!ask(&|app, h| {
        let own = app.head("HEADLESS-1").modes[0].proxy.clone();
        h.settings.set_mode(&own);
        h.settings.set_scale(1.5);
    }));
}

#[test]
fn outputs_that_do_not_touch_along_an_edge_are_refused() {
    let compositor = two();
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    // A gap, and a corner only: the pointer could not cross either.
    for (x, y) in [(1300, 0), (1280, 720), (0, 721)] {
        let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2").at(x, y)];
        assert_eq!(
            app.configure(&manager, None, &ops, true).unwrap(),
            Outcome::Failed,
            "{x},{y}"
        );
    }
    // Sliding along the edge is fine.
    let ops = [Op::keep("HEADLESS-1"), Op::keep("HEADLESS-2").at(1280, 500)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
}

#[test]
fn a_saved_place_that_leaves_a_gap_is_not_used() {
    let layout = "[HEADLESS-2]\nx = 5000\ny = 0\n";
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720,1280x720"], Some(layout), "60000");
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    assert_eq!(app.head("HEADLESS-2").position, (1280, 0));
}

#[test]
fn a_saved_off_for_the_first_screen_is_kept_with_the_second_on() {
    // The lid closed and a monitor on: the first is off.
    let layout = "[HEADLESS-1]\nenabled = false\n\n[HEADLESS-2]\nenabled = true\n";
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720,800x600"], Some(layout), "60000");
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    assert!(!app.head("HEADLESS-1").enabled);
    assert!(app.head("HEADLESS-2").enabled);
    let plain = app.plain_outputs().unwrap();
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].name, "HEADLESS-2");
    // New windows open on it, and the pointer is on it.
    let (_s, _t) = app.window(GREEN).unwrap();
    assert_eq!(
        app.capture_named("HEADLESS-2").unwrap().at(400, 300),
        rgb(GREEN)
    );
}

#[test]
fn a_layout_that_would_switch_everything_off_is_ignored() {
    let layout = "[HEADLESS-1]\nenabled = false\n\n[HEADLESS-2]\nenabled = false\n";
    let compositor =
        Compositor::start_with_layout(&["--outputs", "1280x720,1280x720"], Some(layout), "60000");
    let mut app = compositor.connect();
    let _manager = app.manager().unwrap();
    assert!(app.head("HEADLESS-1").enabled && app.head("HEADLESS-2").enabled);
}
