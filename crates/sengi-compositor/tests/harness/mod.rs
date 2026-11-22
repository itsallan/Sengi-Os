//! Starts the real compositor on the headless backend for a test.
#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::net::{UnixListener, UnixStream},
    },
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
    /// Where the lockers the compositor starts leave their connections.
    handover: UnixListener,
    /// The `--lock` requests made through `locker`, to end with the test.
    askers: Mutex<Vec<Child>>,
}

/// Names the socket a started locker hands its connection over on.
const HANDOVER: &str = "SENGI_TEST_HANDOVER";

impl Compositor {
    /// Starts the compositor. `locker` is the command it starts as the
    /// screen locker. `None` makes that this test program's
    /// `locker_stand_in`, which hands the connection it was started on
    /// over to the test, so that the test can be the locker.
    pub fn start(locker: Option<&str>) -> Self {
        Self::start_with(locker, None, None)
    }

    /// As `start_with`, with `variables` in the compositor's environment.
    pub fn start_in(
        locker: Option<&str>,
        settings: Option<&str>,
        script: Option<&str>,
        variables: &[(&str, &str)],
    ) -> Self {
        Self::launch(locker, settings, script, variables)
    }

    /// As `start`, with the settings file `settings` in a configuration
    /// directory of its own (see `write_settings`), and with `script`
    /// played as input.
    pub fn start_with(locker: Option<&str>, settings: Option<&str>, script: Option<&str>) -> Self {
        Self::launch(locker, settings, script, &[])
    }

    fn launch(
        locker: Option<&str>,
        settings: Option<&str>,
        script: Option<&str>,
        variables: &[(&str, &str)],
    ) -> Self {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sengi-test-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a runtime directory");
        let me = std::env::current_exe().expect("the test's own path");
        let stand_in = format!("{} --exact locker_stand_in --nocapture", me.display());
        let locker = locker.map_or(stand_in, str::to_string);
        let handover_path = dir.join("handover");
        let handover = UnixListener::bind(&handover_path).expect("a socket for lockers");
        handover
            .set_nonblocking(true)
            .expect("a socket for lockers");
        let config = dir.join("config");
        std::fs::create_dir_all(config.join("sengi-os")).expect("a configuration directory");
        if let Some(settings) = settings {
            std::fs::write(config.join("sengi-os/settings.toml"), settings).expect("settings");
        }
        let mut command = Command::new(env!("CARGO_BIN_EXE_sengi-compositor"));
        command.args(["--backend", "headless", "--locker", &locker]);
        if let Some(script) = script {
            let path = dir.join("script");
            std::fs::write(&path, script).expect("a script");
            command.arg("--script").arg(path);
        }
        let mut child = command
            .env("XDG_RUNTIME_DIR", &dir)
            // The settings are the test's, not those of whoever runs it.
            .env("XDG_CONFIG_HOME", &config)
            .env("XDG_CONFIG_DIRS", dir.join("nowhere"))
            .env_remove("XKB_DEFAULT_LAYOUT")
            .env_remove("XKB_DEFAULT_VARIANT")
            .env_remove("XKB_DEFAULT_OPTIONS")
            .envs(variables.iter().copied())
            .env(HANDOVER, &handover_path)
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the compositor starts");

        let mut name = String::new();
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
        stdout.read_line(&mut name).expect("the socket name");
        // The lockers it starts write to the same pipe, and must not find
        // it closed.
        std::thread::spawn(move || for _ in stdout.lines().map_while(Result::ok) {});
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
            handover,
            askers: Mutex::new(Vec::new()),
        }
    }

    /// Runs `sengi-compositor --lock` against this compositor, the way the
    /// session's idle command does. It ends when the compositor answers.
    pub fn ask_for_lock(&self) -> Child {
        Command::new(env!("CARGO_BIN_EXE_sengi-compositor"))
            .arg("--lock")
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env("WAYLAND_DISPLAY", self.socket.file_name().expect("a name"))
            .stderr(Stdio::null())
            .spawn()
            .expect("the request starts")
    }

    /// Asks for the lock and returns the connection of the locker the
    /// compositor starts for it.
    pub fn locker(&self) -> Client {
        let asker = self.ask_for_lock();
        self.askers.lock().unwrap().push(asker);
        self.next_locker()
    }

    /// The connection of the next locker the compositor starts, whether
    /// because it was asked or because the last one died.
    pub fn next_locker(&self) -> Client {
        let end = Instant::now() + SOON;
        while Instant::now() < end {
            if let Ok((from_locker, _)) = self.handover.accept() {
                from_locker
                    .set_nonblocking(false)
                    .expect("a blocking socket");
                let stream = receive(&from_locker).expect("the locker's connection");
                return Client::from_stream(stream).expect("a client on it");
            }
            sleep(Duration::from_millis(10));
        }
        panic!(
            "the compositor started no locker; it said: {:?}",
            self.log.lock().unwrap()
        );
    }

    /// Replaces the settings file, the way Settings does when something
    /// is changed in it.
    pub fn write_settings(&self, settings: &str) {
        std::fs::write(self.dir.join("config/sengi-os/settings.toml"), settings).expect("settings");
    }

    /// The last line the compositor wrote that contains `what`.
    pub fn said(&self, what: &str) -> Option<String> {
        let log = self.log.lock().unwrap();
        log.iter().rev().find(|line| line.contains(what)).cloned()
    }

    pub fn signal(&self, name: &str) {
        Command::new("kill")
            .args([&format!("-{name}"), &self.child.id().to_string()])
            .status()
            .expect("kill runs");
    }

    pub fn connect(&self) -> Client {
        Client::connect(Some(&self.socket)).expect("a connection")
    }

    /// What the compositor says about the session lock on SIGUSR1.
    pub fn status(&self) -> String {
        self.status_line("session ")
    }

    /// The line of the SIGUSR1 report that contains `what`.
    pub fn status_line(&self, what: &str) -> String {
        let count = |log: &Vec<String>| log.iter().filter(|line| line.contains(what)).count();
        let before = count(&self.log.lock().unwrap());
        Command::new("kill")
            .args(["-USR1", &self.child.id().to_string()])
            .status()
            .expect("kill runs");
        let end = Instant::now() + SOON;
        while Instant::now() < end {
            let log = self.log.lock().unwrap();
            if count(&log) > before {
                let line = log.iter().rev().find(|line| line.contains(what));
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
        for asker in self.askers.lock().unwrap().iter_mut() {
            let _ = asker.kill();
            let _ = asker.wait();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What `locker_stand_in` does when the compositor has started it as the
/// locker: pass the connection it was given on to the test that waits
/// for it, and leave. Does nothing in an ordinary test run.
pub fn hand_over() {
    let (Some(path), Ok(socket)) = (std::env::var_os(HANDOVER), std::env::var("WAYLAND_SOCKET"))
    else {
        return;
    };
    let fd: RawFd = socket.parse().expect("a descriptor number");
    let to_test = UnixStream::connect(path).expect("the test is listening");
    send(&to_test, fd).expect("the connection is passed on");
}

fn message(byte: &mut [u8; 1], iov: &mut libc::iovec, control: &mut [u64; 4]) -> libc::msghdr {
    iov.iov_base = byte.as_mut_ptr().cast();
    iov.iov_len = 1;
    // SAFETY: a msghdr of zeroes is an empty one.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(control) as _;
    msg
}

const FD_BYTES: u32 = std::mem::size_of::<RawFd>() as u32;

/// Sends the descriptor `fd` across `socket`.
fn send(socket: &UnixStream, fd: RawFd) -> std::io::Result<()> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: std::ptr::null_mut(),
        iov_len: 0,
    };
    let mut control = [0u64; 4];
    let mut msg = message(&mut byte, &mut iov, &mut control);
    // SAFETY: the control buffer is aligned and large enough for one
    // header with one descriptor, and everything outlives the call.
    let sent = unsafe {
        msg.msg_controllen = libc::CMSG_SPACE(FD_BYTES) as _;
        let header = libc::CMSG_FIRSTHDR(&msg);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(FD_BYTES) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd);
        libc::sendmsg(socket.as_raw_fd(), &msg, 0)
    };
    if sent < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Receives the descriptor that `send` sent, as the socket it is.
fn receive(socket: &UnixStream) -> std::io::Result<UnixStream> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: std::ptr::null_mut(),
        iov_len: 0,
    };
    let mut control = [0u64; 4];
    let mut msg = message(&mut byte, &mut iov, &mut control);
    // SAFETY: as in `send`; the header is only read if the kernel put a
    // whole one there, and the descriptor in it is new and ours.
    unsafe {
        if libc::recvmsg(socket.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let header = libc::CMSG_FIRSTHDR(&msg);
        if header.is_null() || (*header).cmsg_type != libc::SCM_RIGHTS {
            return Err(std::io::Error::other("no descriptor came"));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>());
        Ok(UnixStream::from(OwnedFd::from_raw_fd(fd)))
    }
}
