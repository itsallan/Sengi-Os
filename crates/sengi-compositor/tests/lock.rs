//! The session lock, checked against the real compositor: it is started on
//! the headless backend and test clients talk to it over its socket.

mod common;

use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    thread::sleep,
    time::{Duration, Instant},
};

use common::{Client, LOCK_MANAGER};

const RED: u32 = 0xffb0_2020;
const GREEN: u32 = 0xff20_b020;
const SOON: Duration = Duration::from_secs(5);

struct Compositor {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
    log: Arc<Mutex<Vec<String>>>,
}

impl Compositor {
    /// Starts the compositor. `locker` is the program it recognises as
    /// the screen locker; `None` names this test, so its clients count.
    fn start(locker: Option<&str>) -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sengi-test-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a runtime directory");
        let me = std::env::current_exe().expect("the test's own path");
        let locker = locker.map_or_else(|| me.display().to_string(), str::to_string);
        let mut child = Command::new(env!("CARGO_BIN_EXE_sengi-compositor"))
            .args(["--backend", "headless", "--locker", &locker])
            .env("XDG_RUNTIME_DIR", &dir)
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the compositor starts");

        let mut name = String::new();
        BufReader::new(child.stdout.take().expect("stdout"))
            .read_line(&mut name)
            .expect("the socket name");
        let socket = dir.join(name.trim());

        let log = Arc::new(Mutex::new(Vec::new()));
        let lines = log.clone();
        let stderr = BufReader::new(child.stderr.take().expect("stderr"));
        std::thread::spawn(move || {
            for line in stderr.lines().map_while(Result::ok) {
                lines.lock().unwrap().push(line);
            }
        });
        Self {
            child,
            dir,
            socket,
            log,
        }
    }

    fn connect(&self) -> Client {
        Client::connect(Some(&self.socket)).expect("a connection")
    }

    /// What the compositor says about the session lock on SIGUSR1.
    fn status(&self) -> String {
        let count = |log: &Vec<String>| log.iter().filter(|line| line.contains("session ")).count();
        let before = count(&self.log.lock().unwrap());
        Command::new("kill")
            .args(["-USR1", &self.child.id().to_string()])
            .status()
            .expect("kill runs");
        let end = Instant::now() + SOON;
        while Instant::now() < end {
            let log = self.log.lock().unwrap();
            if count(&log) > before {
                let line = log.iter().rev().find(|line| line.contains("session "));
                return line.cloned().unwrap_or_default();
            }
            drop(log);
            sleep(Duration::from_millis(10));
        }
        panic!("the compositor did not report its state");
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A locker that holds the session with a red surface on the first output.
fn locked(compositor: &Compositor) -> (Client, impl Sized) {
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
fn a_program_that_is_not_the_locker_cannot_ask_for_the_lock() {
    let compositor = Compositor::start(Some("/bin/true"));
    let client = compositor.connect();
    assert!(!client.has_global(LOCK_MANAGER));
}
