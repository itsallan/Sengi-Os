//! Starts the real compositor on the headless backend for a test.
#![allow(dead_code)]

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

use crate::common::Client;

pub const SOON: Duration = Duration::from_secs(5);

pub struct Compositor {
    child: Child,
    pub dir: PathBuf,
    pub socket: PathBuf,
    log: Arc<Mutex<Vec<String>>>,
}

impl Compositor {
    /// Starts the compositor. `locker` is the program it recognises as
    /// the screen locker; `None` names this test, so its clients count.
    pub fn start(locker: Option<&str>) -> Self {
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

    pub fn connect(&self) -> Client {
        Client::connect(Some(&self.socket)).expect("a connection")
    }

    /// What the compositor says about the session lock on SIGUSR1.
    pub fn status(&self) -> String {
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
