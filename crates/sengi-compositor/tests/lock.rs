//! The session lock, checked against the real compositor: it is started on
//! the headless backend and test clients talk to it over its socket.

mod common;
mod harness;

use std::time::Duration;

use common::LOCK_MANAGER;
use harness::{Compositor, SOON};

const RED: u32 = 0xffb0_2020;
const GREEN: u32 = 0xff20_b020;

/// A locker that holds the session with a red surface on the first output.
fn locked(compositor: &Compositor) -> (common::Client, impl Sized) {
    let mut locker = compositor.connect();
    locker.keyboard().unwrap();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let surface = locker.lock_surface(&lock, &output, RED).unwrap();
    assert!(
        locker.wait(SOON, |seen| seen.locked == 1),
        "the lock is granted"
    );
    assert!(locker.wait(SOON, |seen| seen.keyboard_enters == 1));
    (locker, (lock, surface))
}

#[test]
fn the_locker_is_told_once_a_locked_frame_is_out() {
    let compositor = Compositor::start(None);
    assert!(compositor.status().contains("session unlocked"));
    let (locker, _held) = locked(&compositor);
    assert_eq!(locker.seen.finished, 0);
    let status = compositor.status();
    assert!(status.contains("session locked"), "{status}");
    assert!(status.contains("1 lock surface"), "{status}");
    assert!(status.contains("locker told yes"), "{status}");
}

#[test]
fn another_client_cannot_unlock() {
    let compositor = Compositor::start(None);
    let (mut locker, _held) = locked(&compositor);

    let mut other = compositor.connect();
    let refused = other.lock().unwrap();
    assert!(
        other.wait(SOON, |seen| seen.finished == 1),
        "a second lock is refused"
    );
    assert_eq!(other.seen.locked, 0);
    refused.unlock_and_destroy();
    // The protocol calls that an error; either way it must not unlock.
    let _ = other.roundtrip();

    locker.roundtrip().unwrap();
    let status = compositor.status();
    assert!(status.contains("session locked"), "{status}");
    assert!(status.contains("locker alive"), "{status}");
}

#[test]
fn another_client_cannot_supply_the_lock_surface() {
    let compositor = Compositor::start(None);
    let (mut locker, _held) = locked(&compositor);

    let mut other = compositor.connect();
    other.keyboard().unwrap();
    let refused = other.lock().unwrap();
    assert!(other.wait(SOON, |seen| seen.finished == 1));
    let output = other.outputs().remove(0);
    // It may be told off or ignored, but must not end up on screen.
    let _fake = other.lock_surface(&refused, &output, GREEN);
    other.wait(Duration::from_millis(300), |_| false);
    assert_eq!(
        other.seen.keyboard_enters, 0,
        "the fake surface got the keyboard"
    );

    locker.wait(Duration::from_millis(100), |_| false);
    assert_eq!(
        locker.seen.keyboard_leaves, 0,
        "the locker lost the keyboard"
    );
    let status = compositor.status();
    assert!(status.contains("1 lock surface"), "{status}");
}

#[test]
fn only_the_locker_has_the_keyboard_while_locked() {
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.window(GREEN).unwrap();
    assert!(
        app.wait(SOON, |seen| seen.keyboard_enters == 1),
        "the window is focused"
    );

    let (_locker, held) = locked(&compositor);
    assert!(
        app.wait(SOON, |seen| seen.keyboard_leaves == 1),
        "the window keeps the keyboard"
    );

    // A window mapped under the lock gets nothing either.
    let mut late = compositor.connect();
    late.keyboard().unwrap();
    let _late_window = late.window(GREEN).unwrap();
    late.wait(Duration::from_millis(300), |_| false);
    assert_eq!(late.seen.keyboard_enters, 0);
    app.roundtrip().unwrap();
    assert_eq!(app.seen.keyboard_enters, 1);
    drop(held);
}

#[test]
fn a_dead_lockers_session_stays_locked() {
    // No locker is known, so nothing is started to take over.
    let compositor = Compositor::start(None);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.window(GREEN).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));

    let (locker, held) = locked(&compositor);
    drop(held);
    drop(locker);
    app.wait(Duration::from_millis(500), |_| false);

    let status = compositor.status();
    assert!(status.contains("session locked"), "{status}");
    assert!(status.contains("locker gone"), "{status}");
    assert_eq!(
        app.seen.keyboard_enters, 1,
        "the desktop got the keyboard back"
    );
}

#[test]
fn a_new_locker_takes_over_from_a_dead_one() {
    let compositor = Compositor::start(None);
    let (first, held) = locked(&compositor);
    drop(held);
    drop(first);
    // Let the compositor notice before the next one asks.
    let mut app = compositor.connect();
    app.wait(Duration::from_millis(300), |_| false);
    assert!(compositor.status().contains("locker gone"));

    let (_second, _held) = locked(&compositor);
    let status = compositor.status();
    assert!(status.contains("locker alive"), "{status}");
    assert!(status.contains("1 lock surface"), "{status}");
    assert!(status.contains("locker told yes"), "{status}");
}

#[test]
fn a_program_that_is_not_the_locker_cannot_ask_for_the_lock() {
    let compositor = Compositor::start(Some("/bin/true"));
    let client = compositor.connect();
    assert!(!client.has_global(LOCK_MANAGER));
}

/// Not a test of its own: `a_file_of_the_same_name_elsewhere_is_not_the_locker`
/// runs this test binary again, in a mount namespace, with the variable
/// set, to be the program that pretends.
#[test]
fn pretend_locker() {
    let Some(socket) = std::env::var_os("SENGI_TEST_PRETEND_SOCKET") else {
        return;
    };
    let client = common::Client::connect(Some(std::path::Path::new(&socket))).unwrap();
    println!(
        "PRETENDER sees the lock: {}",
        client.has_global(LOCK_MANAGER)
    );
}

#[test]
fn a_file_of_the_same_name_elsewhere_is_not_the_locker() {
    // The real locker is some file; in a mount namespace of its own, a
    // program can make that very path name itself instead.
    let scratch = std::env::temp_dir().join(format!("sengi-test-pretend-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let locker = scratch.join("locker");
    std::fs::copy("/bin/true", &locker).unwrap();
    let compositor = Compositor::start(Some(locker.to_str().unwrap()));

    let me = std::env::current_exe().unwrap();
    let script = format!(
        "mount --bind '{}' '{}' && exec '{}' --exact pretend_locker --nocapture",
        me.display(),
        locker.display(),
        locker.display()
    );
    let run = std::process::Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "sh", "-c", &script])
        .env("SENGI_TEST_PRETEND_SOCKET", &compositor.socket)
        .env_remove("SENGI_COMPOSITOR_PID")
        .output();
    let _ = std::fs::remove_dir_all(&scratch);
    let said = match run {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(err) => {
            eprintln!("skipped, cannot run unshare: {err}");
            return;
        }
    };
    if !said.contains("PRETENDER sees the lock") {
        eprintln!("skipped, no mount namespace to be had here: {said}");
        return;
    }
    assert!(said.contains("sees the lock: false"), "{said}");
}
