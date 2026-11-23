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
