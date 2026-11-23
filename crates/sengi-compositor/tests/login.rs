//! The compositor as the login screen's: `--greeter` and `--session`,
//! checked against the real thing on the headless backend.

mod common;
mod harness;

use std::time::Duration;

use harness::{Compositor, SOON};

const GREEN: u32 = 0xff20_b020;

/// How many key events a window in front is sent when Super+Q is pressed
/// and let go a second and a half in.
fn keys_from_super_q(compositor: &Compositor) -> u32 {
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    let _window = client.window(GREEN).unwrap();
    assert!(client.wait(SOON, |seen| seen.keyboard_enters == 1));
    client.wait(Duration::from_millis(2500), |_| false);
    client.seen.keys
}

const SUPER_Q: &str = "1500 key super+q\n";

#[test]
fn it_ends_when_its_program_does() {
    let mut compositor = Compositor::start_login_screen(Some("sleep 0.5"), None, &[]);
    assert!(compositor.ended(), "still running, or ended with an error");
}

#[test]
fn a_session_compositor_binds_super_q_and_the_login_screens_does_not() {
    // In a session the Q never reaches the window: Super down and up.
    let session = Compositor::start_with(None, None, Some(SUPER_Q));
    assert_eq!(keys_from_super_q(&session), 2);

    // On the login screen it is a key like any other: both, down and up.
    let login = Compositor::start_login_screen(Some("sleep 30"), Some(SUPER_Q), &[]);
    assert_eq!(keys_from_super_q(&login), 4);
}

#[test]
fn nothing_can_lock_the_login_screen() {
    let compositor = Compositor::start_login_screen(Some("sleep 30"), None, &[]);
    assert!(!compositor.ask_for_lock().wait().unwrap().success());
    assert!(compositor.status().contains("session unlocked"));
}

#[test]
fn its_keyboard_is_the_environments_and_stays_so() {
    let german = [("XKB_DEFAULT_LAYOUT", "de")];
    let compositor = Compositor::start_login_screen(Some("sleep 30"), None, &german);
    let mut client = compositor.connect();
    client.keyboard().unwrap();
    assert!(client.wait(SOON, |seen| !seen.keymaps.is_empty()));
    assert!(client.seen.keymaps[0].contains("German"));

    // Settings of the user it runs as, were there any, change nothing.
    compositor.write_settings("[keyboard]\nlayouts = [\"fr\"]\nrepeat-rate = 50\n");
    compositor.signal("HUP");
    client.wait(Duration::from_millis(400), |_| false);
    assert_eq!(client.seen.keymaps.len(), 1);
    assert_eq!(client.seen.repeat, Some((25, 600)));
}
