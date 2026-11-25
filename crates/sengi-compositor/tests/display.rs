//! What Settings' Display and Mouse & Touchpad pages ask of the compositor,
//! on the headless backend: the night light's tint and its timer, the
//! primary output, mirroring, and the three-finger swipes.
//!
//! The headless backend has no display to hold a colour ramp, so it applies
//! the ramp to the pictures a script saves, as a panel would, and leaves
//! wlr-screencopy alone: that is how these tests see both sides.

mod common;
mod harness;

use std::time::Duration;

use common::{
    heads::{Op, Outcome},
    Client,
};
use harness::{Compositor, SOON};

const LIGHT: u32 = 0xffc0_c0c0;
const GREEN: u32 = 0xff20_b020;
const BLUE: u32 = 0xff20_20b0;
const RED: u32 = 0xffb0_2020;
const BAR: u32 = 0xff30_70d0;

#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

fn scratch(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("sengi-display-{name}-{}", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

fn shot(path: &std::path::Path) -> (u32, u32, Vec<u32>) {
    const IEND: [u8; 8] = [0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82];
    let end = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < end {
        if std::fs::read(path).is_ok_and(|data| data.ends_with(&IEND)) {
            return common::heads::read_png(path);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no complete screenshot at {}", path.display());
}

fn channels(pixel: u32) -> (u32, u32, u32) {
    (pixel >> 16 & 0xff, pixel >> 8 & 0xff, pixel & 0xff)
}

/// The settings of a night light that is on from 21:00 to 07:00.
fn night(strength: u32) -> String {
    format!("[display]\nnight-light = true\nnight-strength = {strength}\n")
}

/// Starts with a light window and a script picture, the clock set to `now`.
fn lit(settings: &str, now: &str, name: &str) -> (Compositor, Client, std::path::PathBuf) {
    let path = scratch(name);
    let script = format!("4000 screenshot {}\n", path.display());
    let compositor = Compositor::start_in(
        None,
        Some(settings),
        Some(&script),
        &[("SENGI_COMPOSITOR_NIGHT_NOW", now)],
    );
    let mut app = compositor.connect();
    let _window = app.window(LIGHT).unwrap();
    (compositor, app, path)
}

/// How many pixels of the picture have exactly this colour.
fn count(pixels: &[u32], colour: u32) -> usize {
    pixels.iter().filter(|p| **p == colour).count()
}

#[test]
fn at_night_the_display_is_tinted_and_a_screen_copy_is_not() {
    let (_compositor, mut app, path) = lit(&night(100), "22:00:00", "tint");
    let (_, _, pixels) = shot(&path);
    // The window's grey is no more: red stays, green and blue are cut.
    assert_eq!(count(&pixels, 0xc0c0c0), 0, "something shows untinted");
    let warm = pixels
        .iter()
        .filter(|p| {
            let (r, g, b) = channels(**p);
            r == 0xc0 && (125..=145).contains(&g) && (75..=95).contains(&b)
        })
        .count();
    assert!(warm > 5000, "only {warm} pixels have the tinted grey");
    // What a client copies from the screen is the picture before the ramp.
    let picture = app.capture().unwrap();
    let mut grey = 0;
    for y in 0..picture.height {
        for x in 0..picture.width {
            if picture.at(x, y) == 0xc0c0c0 {
                grey += 1;
            }
        }
    }
    assert!(grey > 5000, "the screen copy is tinted: {grey}");
}

#[test]
fn by_day_or_with_the_switch_off_there_is_no_tint() {
    let (_compositor, _app, path) = lit(&night(100), "12:00:00", "day");
    let (_, _, pixels) = shot(&path);
    assert!(count(&pixels, 0xc0c0c0) > 5000);

    let (_compositor, _app, path) = lit("[display]\nnight-light = false\n", "22:00:00", "off");
    let (_, _, pixels) = shot(&path);
    assert!(count(&pixels, 0xc0c0c0) > 5000);
}

#[test]
fn a_weaker_strength_is_a_milder_tint() {
    let green_at = |strength, name| {
        let (_compositor, _app, path) = lit(&night(strength), "23:00:00", name);
        let (_, _, pixels) = shot(&path);
        let (r, g, b) = pixels
            .iter()
            .map(|p| channels(*p))
            .find(|(r, ..)| *r == 0xc0)
            .expect("a grey pixel");
        assert_eq!(r, 0xc0);
        (g, b)
    };
    let (g25, b25) = green_at(25, "weak");
    let (g100, b100) = green_at(100, "strong");
    assert!(g25 > g100 && b25 > b100, "{g25},{b25} against {g100},{b100}");
    assert!(g25 < 0xc0 && b25 < 0xc0);
}

#[test]
fn the_tint_comes_on_at_the_start_of_the_night_and_goes_with_a_reload() {
    // Three seconds before nine in the evening; the timer is set for the
    // edge and nothing else.
    let compositor = Compositor::start_in(
        None,
        Some(&night(60)),
        None,
        &[("SENGI_COMPOSITOR_NIGHT_NOW", "20:59:58")],
    );
    assert!(compositor.said("night light: on").is_none());
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while compositor.said("night light: on").is_none() && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(compositor.said("night light: on").is_some(), "never came on");
    let line = compositor.status_line("night light:");
    assert!(line.contains("21:00 to 07:00") && line.contains("strength 60"), "{line}");

    // Switched off in Settings: SIGHUP reads it and the tint goes.
    compositor.write_settings("[display]\nnight-light = false\n");
    compositor.signal("HUP");
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while compositor.said("night light: off").is_none() && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(compositor.said("night light: off").is_some());
    assert!(compositor.status_line("night light:").ends_with("night light: off"));
}

#[test]
fn the_bar_goes_to_the_primary_output_and_follows_it_on_a_reload() {
    let compositor = Compositor::start_args_in(
        &["--outputs", "1280x720,800x600"],
        None,
        &[],
    );
    compositor.write_settings("[display]\nprimary = \"HEADLESS-2\"\n");
    compositor.signal("HUP");
    std::thread::sleep(Duration::from_millis(500));
    let mut app = compositor.connect();
    let _bar = app.bar(24, BAR).unwrap();
    let on = |app: &mut Client, name: &str| app.capture_named(name).unwrap().at(10, 4) == BAR & 0xff_ffff;
    assert!(on(&mut app, "HEADLESS-2"), "not on the primary output");
    assert!(!on(&mut app, "HEADLESS-1"));

    // The primary changes: the bar moves to the other one.
    compositor.write_settings("[display]\nprimary = \"HEADLESS-1\"\n");
    compositor.signal("HUP");
    let end = std::time::Instant::now() + SOON;
    let mut moved = false;
    while std::time::Instant::now() < end && !moved {
        app.roundtrip().unwrap();
        moved = on(&mut app, "HEADLESS-1");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(moved, "the bar stayed on the old primary");
}

#[test]
fn outputs_of_one_size_in_one_place_are_a_mirror_and_others_still_may_not_overlap() {
    let compositor = Compositor::start_args(&["--outputs", "1280x720,1280x720"], None);
    let mut app = compositor.connect();
    let manager = app.manager().unwrap();
    let ops = [Op::keep("HEADLESS-1").at(0, 0), Op::keep("HEADLESS-2").at(0, 0)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(app.head("HEADLESS-2").position, (0, 0));
    // A window shows on both.
    let _window = app.window(GREEN).unwrap();
    let first = app.capture_named("HEADLESS-1").unwrap();
    let second = app.capture_named("HEADLESS-2").unwrap();
    let green = 0x20b020;
    let has = |p: &common::apps::Picture| {
        (0..p.height).any(|y| (0..p.width).any(|x| p.at(x, y) == green))
    };
    assert!(has(&first) && has(&second));
    // Then pulled apart again.
    let ops = [Op::keep("HEADLESS-1").at(0, 0), Op::keep("HEADLESS-2").at(1280, 0)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Succeeded
    );
    assert_eq!(app.head("HEADLESS-2").position, (1280, 0));
    // A scale on one of them makes them different sizes: not a mirror.
    let ops = [Op::keep("HEADLESS-1").at(0, 0).scale(2.0), Op::keep("HEADLESS-2").at(0, 0)];
    assert_eq!(
        app.configure(&manager, None, &ops, true).unwrap(),
        Outcome::Failed
    );
}

/// Three windows, the last created in front. The clients are returned so
/// that the compositor keeps them.
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

fn focus_counts(clients: &mut [Client]) -> Vec<(u32, u32)> {
    clients
        .iter_mut()
        .map(|app| {
            app.roundtrip().unwrap();
            (app.seen.keyboard_enters, app.seen.keyboard_leaves)
        })
        .collect()
}

#[test]
fn a_three_finger_swipe_left_walks_back_through_the_windows() {
    // 150 to the left is two steps: past the middle window to the first.
    let compositor =
        Compositor::start_with(None, None, Some("3000 swipe 3 -150 0\n"));
    let mut clients = three_windows(&compositor);
    assert!(
        clients[0].wait(Duration::from_secs(6), |seen| seen.keyboard_enters >= 2),
        "the first window was not given the keyboard: {:?}",
        focus_counts(&mut clients)
    );
}

#[test]
fn one_step_of_a_swipe_goes_to_the_window_before_this_one() {
    let compositor = Compositor::start_with(None, None, Some("3000 swipe 3 -80 0\n"));
    let mut clients = three_windows(&compositor);
    assert!(
        clients[1].wait(Duration::from_secs(6), |seen| seen.keyboard_enters >= 2),
        "{:?}",
        focus_counts(&mut clients)
    );
    // The first one never had it again.
    assert_eq!(focus_counts(&mut clients)[0].0, 1);
}

#[test]
fn other_swipes_and_a_switched_off_setting_do_nothing() {
    // Four fingers, and a swipe too short to count.
    let compositor = Compositor::start_with(
        None,
        None,
        Some("3000 swipe 4 -300 0\n3100 swipe 3 -10 5\n"),
    );
    let mut clients = three_windows(&compositor);
    clients[0].wait(Duration::from_secs(5), |_| false);
    let counts = focus_counts(&mut clients);
    assert_eq!(counts.iter().map(|c| c.0).collect::<Vec<_>>(), vec![1, 1, 1], "{counts:?}");

    let compositor = Compositor::start_with(
        None,
        Some("[touchpad]\ngestures = false\n"),
        Some("3000 swipe 3 -150 0\n"),
    );
    let mut clients = three_windows(&compositor);
    clients[0].wait(Duration::from_secs(5), |_| false);
    assert_eq!(focus_counts(&mut clients)[0].0, 1);
}

#[test]
fn a_swipe_up_opens_the_switcher_and_a_swipe_down_picks() {
    let compositor = Compositor::start_with(
        None,
        None,
        Some("3000 swipe 3 0 -100\n5500 swipe 3 -80 0\n8000 swipe 3 0 100\n"),
    );
    let mut clients = three_windows(&compositor);
    let end = std::time::Instant::now() + Duration::from_secs(8);
    // After the swipe up the switcher stays open, with no focus moved.
    std::thread::sleep(Duration::from_millis(1500));
    clients[0].wait(Duration::from_millis(1500), |_| false);
    let during = compositor.status_line("overlay:");
    assert!(
        during.contains("switcher") || compositor.status_line("overlay:").contains("switcher"),
        "{during}"
    );
    // The swipe down picks the window it has selected.
    while std::time::Instant::now() < end {
        clients[0].roundtrip().unwrap();
        if clients.iter_mut().any(|app| {
            app.roundtrip().unwrap();
            app.seen.keyboard_enters >= 2
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let counts = focus_counts(&mut clients);
    assert!(counts.iter().any(|c| c.0 >= 2), "{counts:?}");
    assert!(compositor.status_line("overlay:").ends_with("overlay: none"));
}
