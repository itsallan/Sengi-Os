//! What the compositor does when the computer is left alone and when it is
//! put to sleep, against the real compositor: the screen dims, goes off and
//! locks at the times in the settings, and input brings it back; the lock
//! before sleep; the lid. The times are a second or two here, so the tests
//! take that long.
//!
//! The programs the compositor starts for this (`sengi-brightness`,
//! `systemd-inhibit`) are stand-ins that write down what they were asked.

mod common;
mod harness;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

use harness::{Compositor, SOON};

/// Not a test of its own: the compositor starts this test binary as the
/// screen locker when a test asks for one, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

/// A directory of programs that write down their arguments, to be put
/// first in the compositor's `PATH`.
struct Stubs {
    dir: PathBuf,
}

impl Stubs {
    fn new() -> Stubs {
        static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sengi-stubs-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let stubs = Stubs { dir };
        // What sengi-brightness was asked to do, one word a line.
        stubs.program(
            "sengi-brightness",
            "echo \"$1\" >> \"$(dirname \"$0\")/brightness.log\"",
        );
        // systemd-inhibit holds until its input closes, as with `cat`.
        stubs.program(
            "systemd-inhibit",
            "echo \"$@\" >> \"$(dirname \"$0\")/inhibit.log\"\ncat >/dev/null\n\
             echo ended >> \"$(dirname \"$0\")/inhibit.log\"",
        );
        stubs
    }

    fn program(&self, name: &str, body: &str) {
        let path = self.dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The `PATH` to start the compositor with.
    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn log(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).unwrap_or_default()
    }

    /// Waits until the log named has `what` in it.
    fn wait_for(&self, name: &str, what: &str, time: Duration) -> bool {
        let end = Instant::now() + time;
        while Instant::now() < end {
            if self.log(name).contains(what) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Stubs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn start(stubs: &Stubs, settings: &str) -> Compositor {
    Compositor::start_in(None, Some(settings), None, &[("PATH", &stubs.path())])
}

/// Waits until the compositor says that `what` has happened.
fn wait_said(compositor: &Compositor, what: &str, time: Duration) -> bool {
    let end = Instant::now() + time;
    while Instant::now() < end {
        if compositor.said(what).is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// How many times the compositor's threads have been scheduled out: the
/// wake-ups it has had.
fn switches(pid: u32) -> u64 {
    let mut total = 0;
    for task in fs::read_dir(format!("/proc/{pid}/task")).unwrap().flatten() {
        let status = fs::read_to_string(task.path().join("status")).unwrap_or_default();
        for line in status.lines() {
            if line.starts_with("voluntary_ctxt_switches")
                || line.starts_with("nonvoluntary_ctxt_switches")
            {
                total += line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|number| number.parse::<u64>().ok())
                    .unwrap_or(0);
            }
        }
    }
    total
}

#[test]
fn the_screen_dims_then_switches_off_and_input_brings_it_back() {
    let stubs = Stubs::new();
    let compositor = start(&stubs, "[power]\ndim-after = 1\nblank-after = 3\n");
    let mut client = compositor.connect();
    let _controls = client.output_power().unwrap();
    assert_eq!(client.seen.power, [true], "a screen starts lit");

    // Dimmed first, still lit.
    assert!(
        stubs.wait_for("brightness.log", "dim", SOON),
        "the screen was never dimmed; the compositor said {:?}",
        compositor.said("idle:")
    );
    client.roundtrip().unwrap();
    assert_eq!(client.seen.power, [true], "dimming is not switching off");
    assert!(compositor.status_line("idle:").contains("idle: dimmed"));

    // Then off.
    assert!(
        client.wait(Duration::from_secs(5), |seen| seen.power == [false]),
        "the screen was never switched off"
    );
    assert!(compositor.status_line("idle:").contains("idle: off"));

    // The first input only lights it, and the brightness comes back.
    compositor.input("move 100 100");
    assert!(
        client.wait(SOON, |seen| seen.power == [true]),
        "input did not light the screen"
    );
    assert!(
        stubs.wait_for("brightness.log", "restore", SOON),
        "the brightness was not restored"
    );
    assert_eq!(stubs.log("brightness.log"), "dim\nrestore\n");
    assert!(compositor.status_line("idle:").contains("idle: awake"));
}

#[test]
fn input_keeps_the_screen_as_it_is_and_starts_the_wait_again() {
    let stubs = Stubs::new();
    let compositor = start(&stubs, "[power]\ndim-after = 2\nblank-after = 4\n");
    // Something every half second, for longer than the time to dim.
    for step in 0..9 {
        compositor.input(&format!("move {} 200", 100 + step * 10));
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(stubs.log("brightness.log"), "", "dimmed in spite of input");
    assert!(compositor.status_line("idle:").contains("idle: awake"));
    // And then, left alone, it does dim.
    assert!(stubs.wait_for("brightness.log", "dim", SOON));
}

#[test]
fn nothing_happens_by_default_for_two_minutes_and_the_compositor_does_not_wake_for_it() {
    let stubs = Stubs::new();
    let compositor = start(&stubs, "");
    let said = compositor.status_line("idle:");
    assert!(
        said.contains("dim 120s, off 300s, lock never"),
        "the defaults are dim after 2 minutes, off after 5, no lock: {said}"
    );
    // Let the start-up settle, then count: a timer set minutes ahead
    // costs no wake-ups in between.
    std::thread::sleep(Duration::from_secs(2));
    let before = switches(compositor.pid());
    std::thread::sleep(Duration::from_secs(4));
    let wakeups = switches(compositor.pid()) - before;
    assert!(wakeups <= 6, "{wakeups} wake-ups in four idle seconds");
}

#[test]
fn the_session_locks_after_the_time_set_and_not_without_one() {
    let stubs = Stubs::new();
    let compositor = start(
        &stubs,
        "[power]\nlock-after = 1\ndim-after = 0\nblank-after = 0\n",
    );
    let _locker = compositor.next_locker();
    assert!(wait_said(&compositor, "idle: locking", SOON));

    // The default asks for no lock.
    let stubs = Stubs::new();
    let compositor = start(&stubs, "[power]\ndim-after = 1\n");
    assert!(stubs.wait_for("brightness.log", "dim", SOON));
    std::thread::sleep(Duration::from_secs(1));
    assert!(compositor.said("idle: locking").is_none());
    assert!(compositor.status().contains("session unlocked"));
}

#[test]
fn new_times_are_taken_up_on_a_hangup() {
    let stubs = Stubs::new();
    let compositor = start(&stubs, "");
    assert!(compositor
        .status_line("idle:")
        .contains("dim 120s, off 300s"));
    compositor.write_settings("[power]\ndim-after = 1\nblank-after = 0\n");
    compositor.signal("HUP");
    assert!(
        stubs.wait_for("brightness.log", "dim", SOON),
        "the new time was not used"
    );
    let said = compositor.status_line("idle:");
    assert!(said.contains("dim 1s, off never"), "{said}");

    // And a screen that was dimmed for a time that is no more comes back.
    compositor.write_settings("[power]\ndim-after = 0\nblank-after = 0\n");
    compositor.signal("HUP");
    assert!(stubs.wait_for("brightness.log", "restore", SOON));
    assert!(compositor.status_line("idle:").contains("idle: awake"));
}

#[test]
fn a_video_holds_the_screen_up() {
    let stubs = Stubs::new();
    let compositor = start(&stubs, "[power]\ndim-after = 1\nblank-after = 2\n");
    let mut client = compositor.connect();
    let (surface, _toplevel) = client.window(0xff20_b020).unwrap();
    let inhibitor = client.inhibit_idle(&surface).unwrap();
    let _controls = client.output_power().unwrap();
    std::thread::sleep(Duration::from_millis(3500));
    assert_eq!(stubs.log("brightness.log"), "", "dimmed under a video");
    client.roundtrip().unwrap();
    assert_eq!(client.seen.power, [true]);
    inhibitor.destroy();
    client.roundtrip().unwrap();
    assert!(
        client.wait(Duration::from_secs(6), |seen| seen.power == [false]),
        "the screen never went off once the video stopped"
    );
}

#[test]
fn the_lock_before_sleep_is_asked_for_and_answered_once_the_screen_is_covered() {
    let compositor = Compositor::start(None);
    let mut asker = compositor.ask_with("--lock-for-sleep");
    let locker = compositor.next_locker();
    let (_locker, _held) = hold(locker);
    let status = asker.wait().unwrap();
    assert!(status.success(), "the request was not answered: {status}");
    assert!(compositor.status().contains("session locked"));
}

/// Locks the session on `locker`'s connection.
fn hold(mut locker: common::Client) -> (common::Client, impl Sized) {
    locker.keyboard().unwrap();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let surface = locker.lock_surface(&lock, &output, 0xffb0_2020).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    (locker, (lock, surface))
}

#[test]
fn settings_can_ask_for_no_lock_before_sleep() {
    let compositor = Compositor::start_with(None, Some("[power]\nlock-on-sleep = false\n"), None);
    let mut asker = compositor.ask_with("--lock-for-sleep");
    let status = asker.wait().unwrap();
    assert!(status.success(), "{status}");
    assert!(
        compositor.status().contains("session unlocked"),
        "locked in spite of the setting"
    );
}

#[test]
fn the_lid_does_nothing_only_while_settings_says_so() {
    let stubs = Stubs::new();
    let compositor = start(&stubs, "[power]\nlid = \"nothing\"\n");
    assert!(
        stubs.wait_for("inhibit.log", "--what=handle-lid-switch", SOON),
        "logind was not asked to leave the lid alone"
    );
    assert!(stubs.log("inhibit.log").contains("--mode=block"));
    assert!(compositor
        .status_line("lid:")
        .contains("closing it does nothing"));
    // Said again, it is not asked for twice.
    compositor.signal("HUP");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(stubs.log("inhibit.log").matches("--what").count(), 1);

    compositor.write_settings("[power]\nlid = \"sleep\"\n");
    compositor.signal("HUP");
    assert!(
        stubs.wait_for("inhibit.log", "ended", SOON),
        "the hold on the lid was not let go"
    );
    assert!(compositor
        .status_line("lid:")
        .contains("left to the system"));
}

#[test]
fn the_lid_is_left_to_the_system_by_default() {
    let stubs = Stubs::new();
    let compositor = start(&stubs, "");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(stubs.log("inhibit.log"), "");
    assert!(compositor
        .status_line("lid:")
        .contains("left to the system"));
}
