//! The session lock, checked against the real compositor: it is started on
//! the headless backend and test clients talk to it over its socket.

mod common;
mod harness;

use std::time::Duration;

use common::LOCK_MANAGER;
use harness::{Compositor, SOON};

const RED: u32 = 0xffb0_2020;
const GREEN: u32 = 0xff20_b020;

/// Not a test of its own: the compositor starts this test program, with
/// only this test in it, as the screen locker. It passes the connection it
/// was started on to the test that is waiting to be the locker.
#[test]
fn locker_stand_in() {
    harness::hand_over();
}

/// A locker that holds the session with a red surface on the first output.
fn locked(compositor: &Compositor) -> (common::Client, impl Sized) {
    hold(compositor.locker())
}

/// Locks the session on `locker`'s connection.
fn hold(mut locker: common::Client) -> (common::Client, impl Sized) {
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
fn whoever_asked_for_the_lock_is_answered_once_it_holds() {
    let compositor = Compositor::start(None);
    let mut asker = compositor.ask_for_lock();
    let locker = compositor.next_locker();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        asker.try_wait().unwrap().is_none(),
        "answered before anything was locked"
    );
    let (_locker, _held) = hold(locker);
    assert!(asker.wait().unwrap().success());

    // Asked again while it holds: yes at once, and no second locker.
    let again = compositor.ask_for_lock().wait().unwrap();
    assert!(again.success());
    let status = compositor.status();
    assert!(status.contains("locker alive"), "{status}");
    assert!(status.contains("1 lock surface"), "{status}");
}

#[test]
fn a_lock_that_does_not_come_is_reported() {
    // A locker that leaves without locking anything.
    let compositor = Compositor::start(Some("/bin/true"));
    let asked = compositor.ask_for_lock().wait().unwrap();
    assert!(!asked.success());
    assert!(compositor.status().contains("session unlocked"));
}

#[test]
fn a_second_lock_object_cannot_unlock() {
    let compositor = Compositor::start(None);
    let (mut locker, _held) = locked(&compositor);

    let refused = locker.lock().unwrap();
    assert!(
        locker.wait(SOON, |seen| seen.finished == 1),
        "a second lock is refused"
    );
    assert_eq!(locker.seen.locked, 1);
    refused.unlock_and_destroy();
    // The protocol calls that an error; either way it must not unlock.
    let _ = locker.roundtrip();

    let status = compositor.status();
    assert!(status.contains("session locked"), "{status}");
}

#[test]
fn a_second_lock_object_cannot_supply_the_lock_surface() {
    let compositor = Compositor::start(None);
    let (mut locker, _held) = locked(&compositor);

    let refused = locker.lock().unwrap();
    assert!(locker.wait(SOON, |seen| seen.finished == 1));
    let output = locker.outputs().remove(0);
    // It may be told off or ignored, but must not end up on screen.
    let _fake = locker.lock_surface(&refused, &output, GREEN);
    locker.wait(Duration::from_millis(300), |_| false);
    assert_eq!(
        locker.seen.keyboard_enters, 1,
        "the fake surface got the keyboard"
    );
    let status = compositor.status();
    assert!(status.contains("locker alive"), "{status}");
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

    // Nobody asks: the compositor starts the next locker by itself.
    let (_second, _held) = hold(compositor.next_locker());
    let status = compositor.status();
    assert!(status.contains("locker alive"), "{status}");
    assert!(status.contains("1 lock surface"), "{status}");
    assert!(status.contains("locker told yes"), "{status}");
}

#[test]
fn a_client_the_compositor_did_not_start_cannot_ask_for_the_lock() {
    // The locker is this very program. Connecting as it is not enough.
    let compositor = Compositor::start(None);
    assert!(!compositor.connect().has_global(LOCK_MANAGER));
    // Not while the real one holds the lock either.
    let (_locker, _held) = locked(&compositor);
    assert!(!compositor.connect().has_global(LOCK_MANAGER));
}

/// Not a test of its own: the next one has the compositor start this as
/// the locker. It closes itself to other processes the way sengi-lock
/// does, after which nobody can look up what program it is, and locks.
#[test]
fn undumpable_locker() {
    // Only a program started as the locker is handed a connection.
    if std::env::var_os("WAYLAND_SOCKET").is_none() {
        return;
    }
    // SAFETY: prctl with PR_SET_DUMPABLE takes two integers.
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) };
    let (mut locker, _held) = hold(common::Client::connect(None).unwrap());
    // Until the compositor goes away.
    while locker.roundtrip().is_ok() {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_locker_closed_to_other_processes_can_lock() {
    let me = std::env::current_exe().unwrap();
    let locker = format!("{} --exact undumpable_locker --nocapture", me.display());
    let compositor = Compositor::start(Some(&locker));
    let asked = compositor.ask_for_lock().wait().unwrap();
    assert!(asked.success(), "the screen was not locked");
    let status = compositor.status();
    assert!(status.contains("locker alive"), "{status}");
    assert!(status.contains("locker told yes"), "{status}");
}

#[test]
fn a_hangup_does_not_end_the_session() {
    let compositor = Compositor::start(None);
    compositor.signal("HUP");
    assert!(compositor.status().contains("session unlocked"));
    assert!(compositor.connect().roundtrip().is_ok());
}
