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

#[test]
fn the_keyboard_starts_as_settings_has_it() {
    let compositor = Compositor::start_with(None, Some(TWO_LAYOUTS), None);
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| !seen.keymaps.is_empty()));
    let keymap = client.seen.keymaps.last().unwrap();
    assert!(keymap.contains("German"), "the second layout is German");
    assert!(keymap.contains("ISO_Next_Group"), "the switch is in it");
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
    // A key two seconds in, when everything below has been set up.
    let compositor = Compositor::start_with(None, None, Some("2000 key enter\n"));
    let mut locker = compositor.locker();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, 0xffb0_2020).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));

    let mut client = compositor.connect();
    let controls = client.output_power().unwrap();
    controls[0].set_mode(zwlr_mode::Off);
    assert!(client.wait(Duration::from_millis(500), |seen| seen.power == [false]));
    assert!(
        client.wait(SOON, |seen| seen.power == [true]),
        "the key did not light the screen"
    );
    assert!(compositor.status().contains("session locked"));
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
fn without_anything_holding_it_off_the_session_goes_idle() {
    let compositor = Compositor::start(None);
    let mut client = compositor.connect();
    let _notification = client
        .idle_notification(Duration::from_millis(300))
        .unwrap();
    assert!(client.wait(SOON, |seen| seen.idled == 1));
}

use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::Mode as zwlr_mode;
