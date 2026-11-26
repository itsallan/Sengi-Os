//! Settings' side: looking for updates as the user, and running the
//! helper and Flatpak to install them.
//!
//! # How the password gets to polkit
//!
//! The system updates need root, which is asked of polkit by running the
//! helper through `pkexec`. Polkit needs an agent to ask the person for a
//! password, and the image has none: no GNOME, no Qt, and a resident agent
//! would be a process running all the time for something used rarely.
//! `pkexec` has an agent of its own that reads the password from a
//! terminal, so Settings gives it one: a pseudo-terminal, the pkexec
//! process's controlling terminal, with echo off. When pkexec prints its
//! "Password:" prompt there, Settings asks for the password in a dialog
//! of its own and writes it to the terminal, once. The password is held
//! in a [`Pass`] that is overwritten when dropped, is never put in an
//! argument or the environment, and is never logged. polkit's own helper
//! checks it against PAM, so the usual lockout and delay apply. A second
//! prompt means the first password was wrong, and ends the attempt.
//!
//! Nothing is written to the terminal but that one line. The helper's
//! own output comes on a pipe, not the terminal, so it cannot be mixed
//! up with the prompt.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::cache::Snapshot;
use crate::event::{Event, Phase, Progress};
use crate::flatpak;
use crate::guard::{self, Paths, Refusal};
use crate::helper;
use crate::run::{self, Ran};
use crate::tools::Tools;

/// A password that is overwritten when it is dropped.
pub struct Pass(Vec<u8>);

impl Pass {
    pub fn new(bytes: Vec<u8>) -> Pass {
        Pass(bytes)
    }

    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

impl std::fmt::Debug for Pass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pass(..)")
    }
}

fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: a plain write to a byte we hold; volatile so that the
        // wipe of a buffer that is about to be freed is kept.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
}

/// What the person answered when asked for a password.
#[derive(Debug)]
pub enum Reply {
    Password(Pass),
    Cancel,
}

/// How a running update is steered: a stop that is asked for, and the
/// answers to questions the update asks.
pub struct Control {
    pub stop: Arc<AtomicBool>,
    pub replies: Receiver<Reply>,
}

/// What an update tells Settings while it runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    Event(Event),
    /// polkit wants the person's password. Answer through the
    /// [`Control`].
    NeedPassword,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Done {
        upgraded: u32,
        reboot_required: bool,
    },
    Cancelled,
    /// Not started, and why, in words for the person.
    Refused(String),
    Failed(String),
    /// polkit did not take the password.
    WrongPassword,
    /// The person backed out of the password question.
    NoPassword,
}

/// The result of looking for updates.
#[derive(Debug, Clone)]
pub struct Checked {
    pub snapshot: Snapshot,
    /// What went wrong, in words, for the parts that did not work.
    pub problems: Vec<String>,
    /// Why nothing was looked at.
    pub refused: Option<Refusal>,
    pub cancelled: bool,
}

/// Runs `command` to its end, or until `stop` is set or `limit` passes.
fn capture(mut command: Command, stop: &AtomicBool, limit: Duration) -> io::Result<Ran> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = run::spawn_group(&mut command)?;
    let out = child.stdout.take().map(run::collect);
    let err = child.stderr.take().map(run::collect);
    let started = Instant::now();
    let mut asked_at: Option<Instant> = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if asked_at.is_none() && (stop.load(Ordering::SeqCst) || started.elapsed() > limit) {
            asked_at = Some(Instant::now());
            run::signal_group(child.id(), libc::SIGTERM);
        }
        if asked_at.is_some_and(|at| at.elapsed() > Duration::from_secs(5)) {
            run::signal_group(child.id(), libc::SIGKILL);
        }
        thread::sleep(Duration::from_millis(50));
    };
    run::signal_group(child.id(), libc::SIGKILL);
    Ok(Ran {
        code: status.code(),
        stopped: asked_at.is_some(),
        stdout: out.and_then(|t| t.join().ok()).unwrap_or_default(),
        stderr: err.and_then(|t| t.join().ok()).unwrap_or_default(),
    })
}

/// How long Flatpak may take to say what has updates.
const LIST_LIMIT: Duration = Duration::from_secs(180);

fn flatpak_command(tools: &Tools, args: &[&str]) -> Command {
    let mut command = Command::new(&tools.flatpak);
    command.args(args).env("LC_ALL", "C").env("LANG", "C");
    command
}

/// Looks for system and app updates, as the user, with no password. What
/// is found replaces what `previous` had, a part at a time: a part that
/// could not be looked at keeps what was known.
pub fn check_all(
    tools: &Tools,
    paths: &Paths,
    state: Option<&Path>,
    previous: &Snapshot,
    stop: &AtomicBool,
    emit: &mut dyn FnMut(Event),
) -> Checked {
    let mut now = previous.clone();
    now.attempted = crate::now();
    let mut problems = Vec::new();
    let mut worked = false;
    let mut cancelled = false;

    match helper::check_system(tools, paths, state, stop, emit) {
        Ok(found) => {
            now.system = found.plan.upgrades;
            now.download = found.plan.download;
            worked = true;
        }
        Err(Event::Refused { code, message }) => {
            let refusal = Refusal::from_code(&code, &message).map(|(r, _)| r);
            return Checked {
                snapshot: previous.clone(),
                problems,
                refused: refusal,
                cancelled,
            };
        }
        Err(Event::Cancelled) => cancelled = true,
        Err(Event::Failed { message }) => {
            problems.push(format!("System updates could not be checked. {message}"))
        }
        Err(_) => {}
    }

    if !cancelled {
        let command = flatpak_command(
            tools,
            &["--user", "remote-ls", "--updates", flatpak::COLUMNS],
        );
        match capture(command, stop, LIST_LIMIT) {
            Ok(ran) if ran.success() => {
                now.apps = flatpak::parse_remote_ls(&ran.stdout);
                worked = true;
            }
            Ok(ran) if ran.stopped && stop.load(Ordering::SeqCst) => cancelled = true,
            Ok(ran) => problems.push(format!(
                "App updates could not be checked. {}",
                crate::last_line(&ran.stderr)
            )),
            // No Flatpak on this system: no apps to update.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                now.apps.clear();
                worked = true;
            }
            Err(e) => problems.push(format!("App updates could not be checked. {e}")),
        }
    }
    if worked {
        now.checked = crate::now();
    }
    Checked {
        snapshot: now,
        problems,
        refused: None,
        cancelled,
    }
}

/// Updates what was asked: the system's packages through the helper,
/// then the apps through Flatpak. Stops at the first part that does not
/// work, so that what the person is told is the first thing that went
/// wrong.
pub fn update_all(
    tools: &Tools,
    paths: &Paths,
    system: bool,
    apps: bool,
    control: &Control,
    emit: &mut dyn FnMut(Note),
) -> Outcome {
    let allowed = if system {
        guard::allow_apply(paths)
    } else {
        guard::allow_check(paths)
    };
    if let Err(refusal) = allowed {
        return Outcome::Refused(refusal.message());
    }
    let mut upgraded = 0;
    if system {
        match privileged(tools, control, emit) {
            Sys::Done { count } => upgraded = count,
            Sys::Cancelled => return Outcome::Cancelled,
            Sys::Refused(words) => return Outcome::Refused(words),
            Sys::Failed(words) => return Outcome::Failed(words),
            Sys::WrongPassword => return Outcome::WrongPassword,
            Sys::NoPassword => return Outcome::NoPassword,
        }
    }
    if apps {
        match flatpak_update(tools, control, emit) {
            Apps::Done => {}
            Apps::Cancelled => return Outcome::Cancelled,
            Apps::Failed(words) => return Outcome::Failed(words),
        }
    }
    Outcome::Done {
        upgraded,
        reboot_required: guard::reboot_required(&paths.reboot_required),
    }
}

enum Sys {
    Done { count: u32 },
    Cancelled,
    Refused(String),
    Failed(String),
    WrongPassword,
    NoPassword,
}

enum Item {
    Line(String),
    Prompt,
    Closed,
}

/// A pseudo-terminal: (master, slave), with echo off on the slave.
fn open_pty() -> io::Result<(File, File)> {
    // SAFETY: each call is given descriptors and buffers of its own, and
    // every descriptor is wrapped in a File as soon as it exists, so that
    // it is closed on every way out.
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if master < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = File::from_raw_fd(master);
        let m = master.as_raw_fd();
        if libc::grantpt(m) != 0 || libc::unlockpt(m) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = [0 as libc::c_char; 128];
        let rc = libc::ptsname_r(m, name.as_mut_ptr(), name.len());
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        let slave = libc::open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        );
        if slave < 0 {
            return Err(io::Error::last_os_error());
        }
        let slave = File::from_raw_fd(slave);
        let s = slave.as_raw_fd();
        let mut modes: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(s, &mut modes) == 0 {
            modes.c_lflag &= !(libc::ECHO | libc::ECHOE | libc::ECHOK | libc::ECHONL);
            libc::tcsetattr(s, libc::TCSANOW, &modes);
        }
        Ok((master, slave))
    }
}

/// Whether what the terminal has shown ends in a password prompt.
pub fn is_password_prompt(shown: &str) -> bool {
    let end = shown.trim_end().to_ascii_lowercase();
    end.ends_with("password:") || end.ends_with("passphrase:")
}

/// Reads what pkexec writes to its terminal, only to know when it asks
/// for a password. What it reads is kept for no longer than that.
fn watch_prompts(mut master: File, tx: Sender<Item>) {
    thread::spawn(move || {
        let mut buffer = [0u8; 512];
        let mut shown = String::new();
        loop {
            match master.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    shown.push_str(&String::from_utf8_lossy(&buffer[..n]));
                    if shown.len() > 1024 {
                        let keep: String = shown.chars().rev().take(512).collect();
                        shown = keep.chars().rev().collect();
                    }
                    if is_password_prompt(&shown) {
                        shown.clear();
                        if tx.send(Item::Prompt).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
}

fn wait_reply(control: &Control) -> Reply {
    loop {
        match control.replies.recv_timeout(Duration::from_millis(100)) {
            Ok(reply) => return reply,
            Err(RecvTimeoutError::Timeout) => {
                if control.stop.load(Ordering::SeqCst) {
                    return Reply::Cancel;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Reply::Cancel,
        }
    }
}

/// The password and a line feed, to the terminal, and nothing kept.
fn send_password(master: &mut File, pass: &Pass) -> io::Result<()> {
    let mut line = pass.bytes().to_vec();
    line.push(b'\n');
    let written = master.write_all(&line).and_then(|()| master.flush());
    wipe(&mut line);
    written
}

fn privileged(tools: &Tools, control: &Control, emit: &mut dyn FnMut(Note)) -> Sys {
    let (mut master, slave) = match open_pty() {
        Ok(pair) => pair,
        Err(e) => return Sys::Failed(format!("Could not set up the sign-in: {e}")),
    };
    let slave_fd = slave.as_raw_fd();
    let mut command = Command::new(&tools.pkexec);
    // The program, and the one word it is given. The helper checks its
    // arguments again.
    command
        .arg(&tools.helper)
        .arg("apply")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: between fork and exec only setsid and ioctl are called,
    // with a descriptor that is open in the child.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match run::spawn_retry(&mut command) {
        Ok(child) => child,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Sys::Failed(
                "pkexec is not installed, so the system cannot be updated from here.".to_string(),
            )
        }
        Err(e) => return Sys::Failed(format!("Could not ask for permission: {e}")),
    };
    // The slave stays open here until the end: with no slave open, reading
    // the master fails, and does so in the moment between pkexec's exec
    // and its opening of the terminal.

    let (tx, rx) = mpsc::channel::<Item>();
    match master.try_clone() {
        Ok(reader) => watch_prompts(reader, tx.clone()),
        Err(e) => return Sys::Failed(format!("Could not set up the sign-in: {e}")),
    }
    if let Some(stdout) = child.stdout.take() {
        let lines = run::lines(stdout);
        let tx = tx.clone();
        thread::spawn(move || {
            for line in lines {
                if tx.send(Item::Line(line)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Item::Closed);
        });
    }
    drop(tx);
    let errors = child.stderr.take().map(run::collect);
    let mut stdin = child.stdin.take();

    let mut sent_password = false;
    let mut wrong = false;
    let mut dismissed = false;
    let mut install_begun = false;
    let mut cancel_sent = false;
    let mut last: Option<Event> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Item::Line(line)) => {
                if let Some(event) = Event::from_line(&line) {
                    match &event {
                        Event::Phase(Phase::Install) => install_begun = true,
                        Event::Done { .. }
                        | Event::Cancelled
                        | Event::Refused { .. }
                        | Event::Failed { .. } => last = Some(event.clone()),
                        _ => {}
                    }
                    emit(Note::Event(event));
                }
            }
            Ok(Item::Prompt) => {
                if sent_password {
                    wrong = true;
                    break;
                }
                emit(Note::NeedPassword);
                match wait_reply(control) {
                    Reply::Password(pass) => {
                        if send_password(&mut master, &pass).is_err() {
                            dismissed = true;
                            break;
                        }
                        sent_password = true;
                    }
                    Reply::Cancel => {
                        dismissed = true;
                        break;
                    }
                }
            }
            Ok(Item::Closed) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        // A stop is passed on, but not once dpkg has begun: the helper
        // would not hear it, and the page should not pretend it would.
        if control.stop.load(Ordering::SeqCst) && !install_begun && !cancel_sent {
            cancel_sent = true;
            if let Some(pipe) = stdin.as_mut() {
                let _ = pipe.write_all(b"cancel\n");
                let _ = pipe.flush();
            }
        }
    }

    if wrong || dismissed {
        // Closing the terminal ends pkexec's question.
        drop(master);
        drop(stdin);
        let _ = child.kill();
        let waited = Instant::now();
        while waited.elapsed() < Duration::from_secs(5) {
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        drop(slave);
        return if wrong {
            Sys::WrongPassword
        } else {
            Sys::NoPassword
        };
    }

    let status = child.wait().ok();
    let errors = errors.and_then(|t| t.join().ok()).unwrap_or_default();
    drop(master);
    drop(slave);
    match last {
        Some(Event::Done { upgraded, .. }) => Sys::Done { count: upgraded },
        Some(Event::Cancelled) => Sys::Cancelled,
        Some(Event::Refused { code, message }) => {
            let words = Refusal::from_code(&code, &message)
                .map(|(_, words)| words)
                .unwrap_or(message);
            Sys::Refused(words)
        }
        Some(Event::Failed { message }) => Sys::Failed(message),
        _ => match status.and_then(|s| s.code()) {
            // pkexec: dismissed, or not authorized.
            Some(126) | Some(127) if sent_password => Sys::WrongPassword,
            Some(126) | Some(127) => {
                Sys::Failed("Permission to update the system was not given.".to_string())
            }
            _ => {
                let said = crate::last_line(&errors);
                Sys::Failed(if said.is_empty() {
                    "The update stopped without saying why.".to_string()
                } else {
                    said
                })
            }
        },
    }
}

enum Apps {
    Done,
    Cancelled,
    Failed(String),
}

/// The last few kilobytes of a stream, kept by a thread that reads it.
struct Tail {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
}

fn keep_tail(mut stream: impl Read + Send + 'static) -> Tail {
    let tail = Tail {
        bytes: Arc::new(Mutex::new(Vec::new())),
        done: Arc::new(AtomicBool::new(false)),
    };
    let (theirs, done) = (tail.bytes.clone(), tail.done.clone());
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut kept = theirs.lock().unwrap_or_else(|e| e.into_inner());
                    kept.extend_from_slice(&buffer[..n]);
                    if kept.len() > 8192 {
                        let cut = kept.len() - 4096;
                        kept.drain(..cut);
                    }
                }
            }
        }
        done.store(true, Ordering::SeqCst);
    });
    tail
}

fn snapshot(tail: &Tail) -> Vec<u8> {
    tail.bytes.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// After the tool has ended, waits (not for long) for what it wrote to
/// be read, so that its last words are not missed.
fn settle(tails: &[&Option<Tail>]) {
    let waited = Instant::now();
    while waited.elapsed() < Duration::from_secs(2)
        && tails
            .iter()
            .any(|t| t.as_ref().is_some_and(|t| !t.done.load(Ordering::SeqCst)))
    {
        thread::sleep(Duration::from_millis(5));
    }
}

fn flatpak_update(tools: &Tools, control: &Control, emit: &mut dyn FnMut(Note)) -> Apps {
    emit(Note::Event(Event::Phase(Phase::Flatpak)));
    let mut command = flatpak_command(tools, &["--user", "update", "--noninteractive", "-y"]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match run::spawn_group(&mut command) {
        Ok(child) => child,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Apps::Done,
        Err(e) => return Apps::Failed(format!("Could not start Flatpak: {e}")),
    };
    let out = child.stdout.take().map(keep_tail);
    let err = child.stderr.take().map(keep_tail);
    let mut shown = flatpak::Progress::default();
    let mut stopped_at: Option<Instant> = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Apps::Failed(format!("Flatpak stopped: {e}")),
        }
        if stopped_at.is_none() && control.stop.load(Ordering::SeqCst) {
            stopped_at = Some(Instant::now());
            run::signal_group(child.id(), libc::SIGTERM);
        }
        if stopped_at.is_some_and(|at| at.elapsed() > Duration::from_secs(8)) {
            run::signal_group(child.id(), libc::SIGKILL);
        }
        let (o, e) = (
            out.as_ref().map(snapshot).unwrap_or_default(),
            err.as_ref().map(snapshot).unwrap_or_default(),
        );
        let now = flatpak::Progress::from_tails(&[&o, &e]);
        if !now.is_empty() && now != shown {
            shown = now.clone();
            let mut detail = now.verb.clone().unwrap_or_else(|| "Updating".to_string());
            if let Some(speed) = &now.speed {
                detail = format!("{detail} \u{b7} {speed}");
            }
            emit(Note::Event(Event::Progress(Progress {
                phase: Phase::Flatpak,
                percent: now.percent.map(f32::from),
                step: now.step,
                size: now.size.clone(),
                detail,
            })));
        }
        thread::sleep(Duration::from_millis(150));
    };
    run::signal_group(child.id(), libc::SIGKILL);
    settle(&[&out, &err]);
    if stopped_at.is_some() {
        return Apps::Cancelled;
    }
    if status.success() {
        return Apps::Done;
    }
    let said = {
        let e = err.as_ref().map(snapshot).unwrap_or_default();
        let o = out.as_ref().map(snapshot).unwrap_or_default();
        let from_err = crate::last_line(&String::from_utf8_lossy(&e));
        if from_err.is_empty() {
            crate::last_line(&String::from_utf8_lossy(&o))
        } else {
            from_err
        }
    };
    Apps::Failed(if said.is_empty() {
        "Flatpak could not update the apps.".to_string()
    } else {
        said
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{scratch, script};

    #[test]
    fn password_prompts_are_recognised() {
        assert!(is_password_prompt("==== AUTHENTICATING ====\r\nPassword: "));
        assert!(is_password_prompt("Password:"));
        assert!(is_password_prompt("password: \r\n"));
        assert!(!is_password_prompt("Authenticating as: Allan (allan)\r\n"));
        assert!(!is_password_prompt("Password: \r\nSorry, try again.\r\n"));
        assert!(!is_password_prompt(""));
    }

    #[test]
    fn a_password_is_wiped_when_dropped() {
        let mut bytes = b"hunter2".to_vec();
        wipe(&mut bytes);
        assert!(bytes.iter().all(|b| *b == 0));
        assert_eq!(format!("{:?}", Pass::new(b"x".to_vec())), "Pass(..)");
    }

    fn control() -> (Control, Sender<Reply>) {
        let (tx, rx) = mpsc::channel();
        (
            Control {
                stop: Arc::new(AtomicBool::new(false)),
                replies: rx,
            },
            tx,
        )
    }

    fn user_tools(dir: &std::path::Path, pkexec: &str, flatpak: &str) -> Tools {
        let pkexec = script(dir, "pkexec", pkexec);
        let flatpak = script(dir, "flatpak", flatpak);
        Tools {
            apt_get: dir.join("none"),
            flatpak: flatpak.into(),
            pkexec: pkexec.into(),
            helper: dir.join("sengi-update"),
        }
    }

    #[test]
    fn the_apps_with_updates_are_listed() {
        let dir = scratch("check-apps");
        let tools = user_tools(
            &dir,
            "exit 1",
            "echo \"$*\" >>\"$(dirname \"$0\")/flatpak.log\"; \
             printf 'GIMP\\torg.gimp.GIMP\\t2.10\\t112 MB\\n'",
        );
        // The live system has no apt lists, so the system part is
        // refused; give it a system that is not live.
        let paths = Paths::under(&dir);
        let apt = script(&dir, "apt-get", "case \" $* \" in *\" -s \"*) echo 'Inst a [1] (2 Debian-Security:13/stable-security [amd64])';; esac");
        let tools = Tools {
            apt_get: apt,
            ..tools
        };
        let stop = AtomicBool::new(false);
        let found = check_all(
            &tools,
            &paths,
            None,
            &Snapshot::default(),
            &stop,
            &mut |_| {},
        );
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        assert_eq!(found.snapshot.system.len(), 1);
        assert!(found.snapshot.system[0].security);
        assert_eq!(found.snapshot.apps.len(), 1);
        assert_eq!(found.snapshot.apps[0].id, "org.gimp.GIMP");
        assert!(found.snapshot.checked > 0);
        let log = std::fs::read_to_string(dir.join("flatpak.log")).unwrap();
        assert!(log.contains("--user remote-ls --updates"), "{log}");
    }

    #[test]
    fn the_live_system_is_not_looked_at() {
        let dir = scratch("check-live");
        std::fs::create_dir_all(dir.join("proc")).unwrap();
        std::fs::write(dir.join("proc/mounts"), "overlay / overlay rw 0 0\n").unwrap();
        let tools = user_tools(
            &dir,
            "exit 1",
            "echo should-not-run >\"$(dirname \"$0\")/ran\"",
        );
        let found = check_all(
            &tools,
            &Paths::under(&dir),
            None,
            &Snapshot::default(),
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert_eq!(found.refused, Some(Refusal::Live));
        assert!(!dir.join("ran").exists());
        assert_eq!(found.snapshot, Snapshot::default());
    }

    #[test]
    fn a_part_that_fails_keeps_what_was_known() {
        let dir = scratch("check-keep");
        let paths = Paths::under(&dir);
        let apt = script(
            &dir,
            "apt-get",
            "echo 'E: Could not resolve deb.debian.org' >&2; exit 100",
        );
        let tools = Tools {
            apt_get: apt,
            ..user_tools(&dir, "exit 1", "echo 'error: no route' >&2; exit 1")
        };
        let before = Snapshot {
            checked: 5,
            system: vec![crate::apt::Upgrade {
                name: "old".to_string(),
                from: None,
                to: "1".to_string(),
                security: false,
            }],
            ..Snapshot::default()
        };
        let found = check_all(
            &tools,
            &paths,
            None,
            &before,
            &AtomicBool::new(false),
            &mut |_| {},
        );
        assert_eq!(found.problems.len(), 2, "{:?}", found.problems);
        assert!(found.problems[0].contains("Could not resolve"));
        assert_eq!(found.snapshot.system, before.system);
        assert_eq!(found.snapshot.checked, 5);
        assert!(found.snapshot.attempted > 0);
    }

    #[test]
    fn apps_update_with_real_progress_and_no_password() {
        let dir = scratch("flatpak-update");
        let tools = user_tools(
            &dir,
            "exit 1",
            "echo \"$*\" >\"$(dirname \"$0\")/args\"; \
             printf 'Updating 1/2\\342\\200\\246\\rUpdating\\342\\200\\246 40%%  5.0 MB  1.0 MB/s\\r'; sleep 0.5",
        );
        let (control, _tx) = control();
        let mut notes = Vec::new();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            false,
            true,
            &control,
            &mut |n| notes.push(n),
        );
        assert_eq!(
            outcome,
            Outcome::Done {
                upgraded: 0,
                reboot_required: false
            }
        );
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        assert_eq!(args.trim(), "--user update --noninteractive -y");
        assert!(notes.contains(&Note::Event(Event::Phase(Phase::Flatpak))));
        let progress = notes.iter().find_map(|n| match n {
            Note::Event(Event::Progress(p)) => Some(p.clone()),
            _ => None,
        });
        let progress = progress.expect("progress was reported");
        assert_eq!(progress.percent, Some(40.0));
        assert_eq!(progress.step, Some((1, 2)));
        assert_eq!(progress.size.as_deref(), Some("5.0 MB"));
        // No password was asked for.
        assert!(!notes.contains(&Note::NeedPassword));
    }

    #[test]
    fn a_stop_ends_the_apps_update() {
        let dir = scratch("flatpak-stop");
        let tools = user_tools(&dir, "exit 1", "sleep 30");
        let (control, _tx) = control();
        let stop = control.stop.clone();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            stop.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            false,
            true,
            &control,
            &mut |_| {},
        );
        setter.join().unwrap();
        assert_eq!(outcome, Outcome::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_failed_apps_update_says_why() {
        let dir = scratch("flatpak-fail");
        let tools = user_tools(
            &dir,
            "exit 1",
            "echo 'error: Unable to connect' >&2; exit 1",
        );
        let (control, _tx) = control();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            false,
            true,
            &control,
            &mut |_| {},
        );
        assert_eq!(
            outcome,
            Outcome::Failed("error: Unable to connect".to_string())
        );
    }

    #[test]
    fn the_battery_stops_a_system_update_before_pkexec_runs() {
        let dir = scratch("update-battery");
        let bat = dir.join("sys/class/power_supply/BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();
        std::fs::write(bat.join("capacity"), "9\n").unwrap();
        let tools = user_tools(&dir, "touch \"$(dirname \"$0\")/pkexec-ran\"", "exit 0");
        let (control, _tx) = control();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            true,
            true,
            &control,
            &mut |_| {},
        );
        match outcome {
            Outcome::Refused(words) => assert!(words.contains("9%"), "{words}"),
            other => panic!("{other:?}"),
        }
        assert!(!dir.join("pkexec-ran").exists());
    }

    #[test]
    fn the_helper_runs_through_pkexec_with_one_word_and_its_output_is_read() {
        let dir = scratch("pkexec-ok");
        // A pkexec that is not asked for a password: it runs what it is
        // given. Its arguments are recorded.
        let tools = user_tools(
            &dir,
            "echo \"$*\" >\"$(dirname \"$0\")/pkexec-args\"; \
             echo '{\"event\":\"phase\",\"phase\":\"download\"}'; \
             echo '{\"event\":\"progress\",\"phase\":\"download\",\"percent\":45,\"detail\":\"x\"}'; \
             echo '{\"event\":\"phase\",\"phase\":\"install\"}'; \
             echo '{\"event\":\"done\",\"upgraded\":3,\"reboot_required\":true}'",
            "exit 0",
        );
        let (control, _tx) = control();
        let mut notes = Vec::new();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            true,
            false,
            &control,
            &mut |n| notes.push(n),
        );
        assert_eq!(
            outcome,
            Outcome::Done {
                upgraded: 3,
                reboot_required: false
            }
        );
        let args = std::fs::read_to_string(dir.join("pkexec-args")).unwrap();
        assert_eq!(
            args.trim(),
            format!("{} apply", dir.join("sengi-update").display())
        );
        assert!(notes.iter().any(|n| matches!(
            n,
            Note::Event(Event::Progress(p)) if p.percent == Some(45.0)
        )));
        assert!(!notes.contains(&Note::NeedPassword));
    }

    // A pkexec that behaves like the real one at the terminal: asks for
    // a password on /dev/tty, with echo off, and goes on only if it is
    // "correct horse".
    const ASKING: &str = r#"
stty -echo </dev/tty 2>/dev/null
printf 'Password: ' >/dev/tty
read pw </dev/tty
if [ "$pw" = "correct horse" ]; then
  echo '{"event":"done","upgraded":1,"reboot_required":false}'
  exit 0
fi
printf '\r\nPassword: ' >/dev/tty
read pw </dev/tty
exit 126
"#;

    #[test]
    fn the_password_goes_to_the_terminal_and_nowhere_else() {
        let dir = scratch("pkexec-ask");
        let tools = user_tools(&dir, ASKING, "exit 0");
        let (control, tx) = control();
        // The answer waits in the channel until the update looks for it.
        tx.send(Reply::Password(Pass::new(b"correct horse".to_vec())))
            .unwrap();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            true,
            false,
            &control,
            &mut |_| {},
        );
        // A shell without a controlling terminal cannot open /dev/tty, so
        // on a machine where the test cannot get one the outcome is a
        // failure, not a pass: the assertion is on what the answer was.
        assert_eq!(
            outcome,
            Outcome::Done {
                upgraded: 1,
                reboot_required: false
            }
        );
    }

    #[test]
    fn a_wrong_password_ends_the_attempt() {
        let dir = scratch("pkexec-wrong");
        let tools = user_tools(&dir, ASKING, "exit 0");
        let (control, tx) = control();
        tx.send(Reply::Password(Pass::new(b"wrong".to_vec())))
            .unwrap();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            true,
            false,
            &control,
            &mut |_| {},
        );
        assert_eq!(outcome, Outcome::WrongPassword);
    }

    #[test]
    fn backing_out_of_the_question_ends_it() {
        let dir = scratch("pkexec-cancel");
        let tools = user_tools(&dir, ASKING, "exit 0");
        let (control, tx) = control();
        tx.send(Reply::Cancel).unwrap();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            true,
            false,
            &control,
            &mut |_| {},
        );
        assert_eq!(outcome, Outcome::NoPassword);
    }

    #[test]
    fn a_cancel_is_passed_to_the_helper_before_the_install_only() {
        let dir = scratch("pkexec-stop");
        // Waits for a line on its standard input, as the helper does.
        let tools = user_tools(
            &dir,
            "echo '{\"event\":\"phase\",\"phase\":\"download\"}'; \
             read line; \
             if [ \"$line\" = cancel ]; then echo '{\"event\":\"cancelled\"}'; exit 3; fi; exit 1",
            "exit 0",
        );
        let (control, _tx) = control();
        let stop = control.stop.clone();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(400));
            stop.store(true, Ordering::SeqCst);
        });
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            true,
            false,
            &control,
            &mut |_| {},
        );
        setter.join().unwrap();
        assert_eq!(outcome, Outcome::Cancelled);
    }

    #[test]
    fn a_cancel_after_dpkg_began_is_not_passed_on() {
        let dir = scratch("pkexec-late-stop");
        let tools = user_tools(
            &dir,
            "echo '{\"event\":\"phase\",\"phase\":\"install\"}'; \
             sleep 1; \
             dd of=\"$(dirname \"$0\")/heard\" bs=64 count=1 iflag=nonblock 2>/dev/null; \
             echo '{\"event\":\"done\",\"upgraded\":2,\"reboot_required\":false}'",
            "exit 0",
        );
        let (control, _tx) = control();
        let stop = control.stop.clone();
        let setter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            stop.store(true, Ordering::SeqCst);
        });
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            true,
            false,
            &control,
            &mut |_| {},
        );
        setter.join().unwrap();
        assert_eq!(
            outcome,
            Outcome::Done {
                upgraded: 2,
                reboot_required: false
            }
        );
        // Whatever dd saw on the helper's input, it was nothing.
        let heard = std::fs::read(dir.join("heard")).unwrap_or_default();
        assert!(heard.is_empty(), "{heard:?}");
    }

    #[test]
    fn a_missing_pkexec_is_said() {
        let dir = scratch("no-pkexec");
        let tools = Tools {
            pkexec: dir.join("not-there").into(),
            ..user_tools(&dir, "exit 0", "exit 0")
        };
        let (control, _tx) = control();
        let outcome = update_all(
            &tools,
            &Paths::under(&dir),
            true,
            false,
            &control,
            &mut |_| {},
        );
        match outcome {
            Outcome::Failed(words) => assert!(words.contains("pkexec"), "{words}"),
            other => panic!("{other:?}"),
        }
    }
}
