//! What Settings drives, checked against the real compositor: the
//! keyboard's layouts and repeat, read from the settings file at start and
//! again on SIGHUP; switching the screen off and on; and holding idleness
//! off.

mod common;
mod harness;

use std::time::Duration;

use harness::{Compositor, SOON};

const GREEN: u32 = 0xff20_b020;

const TWO_LAYOUTS: &str = "[keyboard]\nlayouts = [\"us\", \"de\"]\n\
    switch = \"grp:alt_shift_toggle\"\nrepeat-delay = 250\nrepeat-rate = 40\n";

/// Not a test of its own: the compositor starts this as the screen locker,
/// see `harness::hand_over`.
#[test]
fn locker_stand_in() {
    harness::hand_over();
}

/// The keymap a compositor started with these settings and variables
/// gives its clients.
fn keymap_of(settings: Option<&str>, variables: &[(&str, &str)]) -> String {
    let compositor = Compositor::start_in(None, settings, None, variables);
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| !seen.keymaps.is_empty()));
    client.seen.keymaps.remove(0)
}

#[test]
fn the_keyboard_starts_as_settings_has_it() {
    let compositor = Compositor::start_with(None, Some(TWO_LAYOUTS), None);
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| !seen.keymaps.is_empty()));
    let keymap = client.seen.keymaps.last().unwrap();
    assert!(keymap.contains("German"), "the second layout is German");
    let without_switch = keymap_of(Some("[keyboard]\nlayouts = [\"us\", \"de\"]\n"), &[]);
    assert_ne!(keymap, &without_switch, "the switch is in it");
    assert_eq!(client.seen.repeat, Some((40, 250)));
}

#[test]
fn with_no_settings_the_keyboard_is_plain_and_repeats_as_settings_would_default() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| !seen.keymaps.is_empty()));
    assert!(!client.seen.keymaps[0].contains("German"));
    assert_eq!(client.seen.repeat, Some((25, 600)));
}

#[test]
fn a_change_in_settings_reaches_the_keyboard_on_a_hangup() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| seen.keymaps.len() == 1));

    compositor.write_settings(TWO_LAYOUTS);
    compositor.signal("HUP");
    assert!(
        client.wait(SOON, |seen| seen.keymaps.len() == 2),
        "no new keymap came"
    );
    assert!(client.seen.keymaps[1].contains("German"));
    assert!(client.wait(SOON, |seen| seen.repeat == Some((40, 250))));
    let said = compositor.status_line("keyboard:");
    assert!(said.contains("layout us,de"), "{said}");

    // And back again.
    compositor.write_settings("");
    compositor.signal("HUP");
    assert!(client.wait(SOON, |seen| seen.keymaps.len() == 3));
    assert!(!client.seen.keymaps[2].contains("German"));
    assert!(client.wait(SOON, |seen| seen.repeat == Some((25, 600))));
}

#[test]
fn going_back_to_the_plain_layout_does_not_fall_back_on_the_environment() {
    // As after a login with German set: Settings wrote the variables for
    // the session, and they stay in the compositor's environment.
    let german = "[keyboard]\nlayouts = [\"de\"]\nswitch = \"grp:alt_shift_toggle\"\n";
    let compositor = Compositor::start_in(
        None,
        Some(german),
        None,
        &[
            ("XKB_DEFAULT_LAYOUT", "de"),
            ("XKB_DEFAULT_OPTIONS", "grp:alt_shift_toggle"),
        ],
    );
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| seen.keymaps.len() == 1));
    assert!(client.seen.keymaps[0].contains("German"));

    compositor.write_settings("");
    compositor.signal("HUP");
    assert!(
        client.wait(SOON, |seen| seen.keymaps.len() == 2),
        "no new keymap came"
    );
    assert!(!client.seen.keymaps[1].contains("German"));
    assert_eq!(
        client.seen.keymaps[1],
        keymap_of(None, &[]),
        "not the keymap of a session with nothing set"
    );
}

#[test]
fn settings_decide_the_keyboard_not_the_environment() {
    let variables = [
        ("XKB_DEFAULT_LAYOUT", "de"),
        ("XKB_DEFAULT_OPTIONS", "grp:alt_shift_toggle"),
    ];
    assert_eq!(keymap_of(None, &variables), keymap_of(None, &[]));
}

#[test]
fn a_layout_that_does_not_exist_leaves_the_keyboard_as_it_was() {
    let compositor = Compositor::start_with(None, Some(TWO_LAYOUTS), None);
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| seen.keymaps.len() == 1));

    compositor.write_settings("[keyboard]\nlayouts = [\"nosuchlayout\"]\nrepeat-rate = 30\n");
    compositor.signal("HUP");
    // The repeat rate is taken, the layout is not.
    assert!(client.wait(SOON, |seen| seen.repeat == Some((30, 600))));
    assert_eq!(client.seen.keymaps.len(), 1);
    let said = compositor.status_line("keyboard:");
    assert!(said.contains("layout us,de"), "{said}");
}

#[test]
fn a_started_session_survives_settings_it_cannot_use() {
    let compositor = Compositor::start_with(
        None,
        Some("[keyboard]\nlayouts = [\"nosuchlayout\"]\n"),
        None,
    );
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| !seen.keymaps.is_empty()));
    // What it says it uses is what it uses, not the name it was given.
    let said = compositor.status_line("keyboard:");
    assert!(said.contains("layout (default)"), "{said}");
}

#[test]
fn a_screen_is_switched_off_and_on_by_whoever_asks() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    let controls = client.output_power().unwrap();
    assert_eq!(client.seen.power, [true], "a screen starts lit");

    controls[0].set_mode(zwlr_mode::Off);
    assert!(client.wait(SOON, |seen| seen.power == [false]));
    let said = compositor.status_line("outputs:");
    assert!(said.contains(" off"), "{said}");

    // Somebody who asks later is told how things are.
    let mut other = compositor.connect();
    let _watching = other.output_power().unwrap();
    assert_eq!(other.seen.power, [false]);

    controls[0].set_mode(zwlr_mode::On);
    assert!(client.wait(SOON, |seen| seen.power == [true]));
    assert!(other.wait(SOON, |seen| seen.power == [true]));
    let said = compositor.status_line("outputs:");
    assert!(!said.contains(" off"), "{said}");
}

#[test]
fn a_key_lights_a_dark_screen_also_under_the_lock() {
    // A key two seconds in, when everything below has been set up, and
    // another when the screen is lit again.
    let script = "2000 key enter\n3500 key enter\n";
    let compositor = Compositor::start_with(None, None, Some(script));
    let mut locker = compositor.locker();
    locker.keyboard().unwrap();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, 0xffb0_2020).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    assert!(locker.wait(SOON, |seen| seen.keyboard_enters == 1));

    let mut client = compositor.connect();
    let controls = client.output_power().unwrap();
    controls[0].set_mode(zwlr_mode::Off);
    assert!(client.wait(Duration::from_millis(500), |seen| seen.power == [false]));
    assert!(
        client.wait(SOON, |seen| seen.power == [true]),
        "the key did not light the screen"
    );
    assert!(compositor.status().contains("session locked"));

    // The key that lit the screen was typed blind and went nowhere, down
    // or up. The next one is the lock screen's.
    locker.wait(Duration::from_millis(300), |_| false);
    assert_eq!(locker.seen.keys, 0, "the waking key reached the locker");
    assert!(locker.wait(SOON, |seen| seen.keys == 2));
}

#[test]
fn a_surface_on_screen_can_hold_idleness_off() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    let (surface, _window) = client.window(GREEN).unwrap();
    let inhibitor = client.inhibit_idle(&surface).unwrap();
    let _notification = client
        .idle_notification(Duration::from_millis(300))
        .unwrap();

    client.wait(Duration::from_millis(900), |_| false);
    assert_eq!(
        client.seen.idled, 0,
        "idle although a window asked not to be"
    );

    inhibitor.destroy();
    assert!(
        client.wait(SOON, |seen| seen.idled == 1),
        "not idle after the window stopped asking"
    );
}

#[test]
fn a_minimised_window_does_not_hold_idleness_off() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    let (surface, _window) = client.window(GREEN).unwrap();
    let _inhibitor = client.inhibit_idle(&surface).unwrap();
    let _notification = client
        .idle_notification(Duration::from_millis(300))
        .unwrap();
    client.wait(Duration::from_millis(900), |_| false);
    assert_eq!(client.seen.idled, 0, "idle although a window asked not to be");

    assert_eq!(compositor.control("minimise"), "ok 1");
    assert!(
        client.wait(SOON, |seen| seen.idled == 1),
        "a minimised window still holds idleness off"
    );
}

#[test]
fn a_surface_that_is_on_no_screen_does_not_hold_idleness_off() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    // A surface with a picture and no window to show it in.
    let surface = client.compositor.create_surface(&client.handle, ());
    let buffer = client.buffer(64, 64, GREEN).unwrap();
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    let _inhibitor = client.inhibit_idle(&surface).unwrap();
    let _notification = client
        .idle_notification(Duration::from_millis(300))
        .unwrap();
    assert!(
        client.wait(SOON, |seen| seen.idled == 1),
        "a surface that is not shown holds idleness off"
    );
}

#[test]
fn without_anything_holding_it_off_the_session_goes_idle() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    let _notification = client
        .idle_notification(Duration::from_millis(300))
        .unwrap();
    assert!(client.wait(SOON, |seen| seen.idled == 1));
}

use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::Mode as zwlr_mode;
