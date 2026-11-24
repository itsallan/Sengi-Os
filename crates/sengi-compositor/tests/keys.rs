//! The keys that start a program: each is a press in a script and the
//! program is a stand-in found first on PATH that writes down how it was
//! run. The real ones need a bus, PipeWire and a screen to copy.

mod common;
mod harness;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use harness::{Compositor, SOON};

const RED: u32 = 0xffb0_2020;

/// Not a test of its own: the compositor starts this test program as the
/// screen locker, see tests/lock.rs.
#[test]
fn locker_stand_in() {
    harness::hand_over();
}

/// A directory with stand-ins for the programs the keys run. Each notes
/// its name and arguments in `runs`, a line a run.
struct Stand {
    dir: PathBuf,
    runs: PathBuf,
}

impl Stand {
    fn new(name: &str, programs: &[&str]) -> Stand {
        let dir = std::env::temp_dir().join(format!("sengi-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let runs = dir.join("runs");
        for program in programs {
            let path = dir.join(program);
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho \"{program} $*\" >>'{}'\n", runs.display()),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Stand { dir, runs }
    }

    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.runs)
            .map(|text| text.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Waits for `count` runs, and a moment longer for any that should not be.
    fn wait_for(&self, count: usize) -> Vec<String> {
        let end = Instant::now() + SOON * 2;
        while self.lines().len() < count && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(800));
        self.lines()
    }
}

impl Drop for Stand {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn the_brightness_and_print_keys_start_their_programs() {
    let stand = Stand::new(
        "keys",
        &["sengi-brightness", "sengi-shot", "sengi-osd", "wpctl"],
    );
    let script = "2000 key brightnessup\n2300 key brightnessdown\n2600 key kbdup\n\
                  2900 key kbddown\n3200 key kbdtoggle\n3500 key print\n";
    let compositor = Compositor::start_in(None, None, Some(script), &[("PATH", &stand.path())]);
    let _app = compositor.connect();
    let mut lines = stand.wait_for(6);
    lines.sort();
    assert_eq!(
        lines,
        [
            "sengi-brightness down",
            "sengi-brightness kbd-down",
            "sengi-brightness kbd-toggle",
            "sengi-brightness kbd-up",
            "sengi-brightness up",
            "sengi-shot ",
        ]
    );
}

#[test]
fn the_volume_keys_ask_the_indicator_after_wpctl() {
    let stand = Stand::new("volume", &["sengi-osd", "wpctl"]);
    let script = "2000 key volumeup\n";
    let compositor = Compositor::start_in(None, None, Some(script), &[("PATH", &stand.path())]);
    let _app = compositor.connect();
    let lines = stand.wait_for(3);
    // Two wpctl runs for the change, then the indicator, in that order.
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("wpctl set-mute"), "{lines:?}");
    assert!(lines[1].starts_with("wpctl set-volume"), "{lines:?}");
    assert_eq!(lines[2], "sengi-osd volume");
}

#[test]
fn alt_print_asks_for_the_frame_of_the_window_in_front() {
    let stand = Stand::new("window", &["sengi-shot"]);
    let script = "3000 keydown alt\n3100 key print\n3300 keyup alt\n";
    let compositor = Compositor::start_in(None, None, Some(script), &[("PATH", &stand.path())]);
    let mut app = compositor.connect();
    let _window = app.window(RED).unwrap();
    let lines = stand.wait_for(1);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert!(line.starts_with("sengi-shot --output "), "{line}");
    let region = line.split("--region ").nth(1).expect("a region");
    let numbers: Vec<i32> = region
        .split(',')
        .map(|n| n.trim().parse().unwrap())
        .collect();
    assert_eq!(numbers.len(), 4, "{line}");
    assert!(numbers[2] > 0 && numbers[3] > 0, "{line}");
}

#[test]
fn the_brightness_keys_work_on_the_lock_screen_and_print_does_not() {
    let stand = Stand::new("locked", &["sengi-brightness", "sengi-shot"]);
    // The lock has to come before the keys, which are scripted for six
    // seconds in.
    let script = "6000 key brightnessup\n6300 key print\n";
    let started = Instant::now();
    let compositor = Compositor::start_in(None, None, Some(script), &[("PATH", &stand.path())]);
    let mut locker = compositor.locker();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, RED).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "the lock came late"
    );
    let lines = stand.wait_for(1);
    assert_eq!(lines, ["sengi-brightness up"]);
    lock.unlock_and_destroy();
    locker.roundtrip().unwrap();
}
