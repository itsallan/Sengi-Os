//! Keeping a locker alive for as long as the session is locked.
//!
//! A compositor keeps a session locked when its locker dies; that is the
//! point of the protocol. labwc then shows black on every output, and the
//! only thing that can unlock is a new locker taking the lock over. So
//! the program that locks the screen is two processes. The one that was
//! started stays small: it forks the worker, which connects to the
//! compositor, draws and talks to PAM, and waits for it. If the worker is
//! killed or crashes, a new one is started, takes over the lock that was
//! left behind, and asks for the password as before.
//!
//! The supervisor never touches the compositor and cannot unlock. All it
//! decides is whether there should be a worker.
//!
//! With `detach` there is a third process for a moment: the one that was
//! started forks the supervisor and exits once the first worker says the
//! session is locked and covered. That is what `swayidle -w` waits for
//! before it lets the machine sleep.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

/// The session was unlocked with the right password.
pub const UNLOCKED: u8 = 0;
/// Something went wrong that is none of the below.
pub const FAILED: u8 = 1;
/// The compositor turned the lock down, which is what it does when
/// another locker already holds the session.
pub const REFUSED: u8 = 2;
/// There is no compositor to reach, or none that can lock.
pub const NO_COMPOSITOR: u8 = 3;

/// How often a worker may die without the session ever having been
/// locked before giving up. After a lock there is no giving up.
const TRIES_BEFORE_LOCK: u32 = 3;

/// A worker that lasted this long was working; the next death starts the
/// waits over.
const SETTLED: Duration = Duration::from_secs(30);

/// The worker's way of saying the session is locked and every output is
/// covered. Dropped unused, it says nothing.
pub struct Ready(Option<File>);

impl Ready {
    pub fn locked(mut self) {
        if let Some(mut pipe) = self.0.take() {
            let _ = pipe.write_all(b"L");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Exited(u8),
    /// Killed by a signal, an abort after a panic included.
    Killed,
    /// The kernel would not say how it ended.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Exit(u8),
    Restart,
}

/// What to do about a worker that has ended. `locked` is whether any
/// worker so far has reported the session locked; `tries` is how many
/// have ended without that.
fn decide(ended: Ended, locked: bool, tries: u32) -> Decision {
    match ended {
        // The three ends that leave nothing to do: unlocked by its owner,
        // in another locker's hands, or no compositor left to lock.
        Ended::Exited(code @ (UNLOCKED | REFUSED | NO_COMPOSITOR)) => Decision::Exit(code),
        // It may have unlocked, and a new worker would lock the session
        // again under the user's hands. If it died instead, the session
        // is still locked, by the compositor; what is lost is only the
        // restart. So no new worker without knowing that one is needed.
        Ended::Unknown => Decision::Exit(FAILED),
        // The session is locked and nothing is there to unlock it.
        _ if locked => Decision::Restart,
        // It may have died between locking and saying so, and locking is
        // what was asked for either way.
        _ if tries < TRIES_BEFORE_LOCK => Decision::Restart,
        Ended::Exited(code) => Decision::Exit(code),
        Ended::Killed => Decision::Exit(FAILED),
    }
}

/// How long to wait before starting a worker again after `deaths` quick
/// ones in a row.
fn restart_delay(deaths: u32) -> Duration {
    (Duration::from_millis(250) * 2u32.pow(deaths.saturating_sub(1).min(8)))
        .min(Duration::from_secs(5))
}

fn pipe() -> Option<(File, File)> {
    let mut ends = [0; 2];
    // SAFETY: pipe2 fills in the two descriptors.
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    // SAFETY: both were just made and belong to nobody else.
    unsafe {
        Some((
            File::from(OwnedFd::from_raw_fd(ends[0])),
            File::from(OwnedFd::from_raw_fd(ends[1])),
        ))
    }
}

/// True if the other end wrote its one byte before closing.
fn told(mut pipe: File) -> bool {
    let mut byte = [0u8; 1];
    // read_exact retries when a signal interrupts it.
    pipe.read_exact(&mut byte).is_ok()
}

fn wait_for(child: libc::pid_t) -> Ended {
    let mut status = 0;
    loop {
        // SAFETY: waitpid writes one integer.
        let waited = unsafe { libc::waitpid(child, &mut status, 0) };
        if waited == child {
            break;
        }
        if waited < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            // ECHILD: the child was reaped by someone else, or was never
            // ours to wait for. Not the same as knowing that it died.
            return Ended::Unknown;
        }
    }
    if libc::WIFEXITED(status) {
        Ended::Exited(libc::WEXITSTATUS(status) as u8)
    } else {
        Ended::Killed
    }
}

/// Ends a forked child at once. Nothing it inherited is flushed or run a
/// second time.
fn leave(code: u8) -> ! {
    // SAFETY: _exit takes an integer and does not return.
    unsafe { libc::_exit(code.into()) }
}

/// Runs `work` in a child process until it ends in a way that needs no
/// successor, and returns the exit code for this process. `locked` is
/// told the first time a worker reports the session locked.
fn supervise(work: fn(Ready) -> u8, mut locked: Option<File>) -> u8 {
    let mut was_locked = false;
    let mut tries = 0;
    let mut deaths = 0;
    loop {
        let Some((from_worker, to_supervisor)) = pipe() else {
            return FAILED;
        };
        let started = Instant::now();
        // SAFETY: this process has one thread, so the child may go on to
        // do anything a fresh process may.
        let child = unsafe { libc::fork() };
        if child < 0 {
            return FAILED;
        }
        if child == 0 {
            drop(from_worker);
            drop(locked);
            let code = work(Ready(Some(to_supervisor)));
            leave(code);
        }
        drop(to_supervisor);
        if told(from_worker) {
            was_locked = true;
            if let Some(mut pipe) = locked.take() {
                let _ = pipe.write_all(b"L");
            }
        } else {
            tries += 1;
        }
        match decide(wait_for(child), was_locked, tries) {
            Decision::Exit(code) => return code,
            Decision::Restart => {
                deaths = if started.elapsed() >= SETTLED {
                    1
                } else {
                    deaths + 1
                };
                eprintln!("sengi-lock: the lock screen stopped; starting it again");
                std::thread::sleep(restart_delay(deaths));
            }
        }
    }
}

/// Locks the session by running `work` under supervision. Returns this
/// process's exit code.
///
/// Without `detach`, returns when the session has been unlocked, or could
/// not be locked. With it, returns as soon as the session is locked and
/// covered, leaving the supervisor behind, or with the failure if that
/// never happens.
///
/// Must be called before the process has started any thread.
pub fn run(detach: bool, work: fn(Ready) -> u8) -> u8 {
    // Whoever started this may have set SIGCHLD to be ignored, and that
    // is inherited. The kernel then reaps children itself and waitpid
    // has nothing to report, so the supervisor could not tell an unlock
    // from a crash.
    // SAFETY: setting a signal's disposition to its default.
    unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) };
    if !detach {
        return supervise(work, None);
    }
    let Some((from_supervisor, to_starter)) = pipe() else {
        return FAILED;
    };
    // SAFETY: one thread, as above.
    let supervisor = unsafe { libc::fork() };
    if supervisor < 0 {
        return FAILED;
    }
    if supervisor == 0 {
        drop(from_supervisor);
        // Out of the starter's session and process group, so that what
        // is aimed at it does not hit the lock screen. Nothing to read.
        // SAFETY: setsid takes nothing; dup2 is given two open descriptors.
        unsafe {
            libc::setsid();
            if let Ok(null) = File::open("/dev/null") {
                libc::dup2(null.as_raw_fd(), 0);
            }
        }
        let code = supervise(work, Some(to_starter));
        leave(code);
    }
    drop(to_starter);
    if told(from_supervisor) {
        // Locked, which is what was asked for.
        return 0;
    }
    // It ended without ever locking, and its exit code says why.
    match wait_for(supervisor) {
        Ended::Exited(UNLOCKED) | Ended::Killed | Ended::Unknown => FAILED,
        Ended::Exited(code) => code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worker_that_unlocked_or_was_turned_away_is_not_replaced() {
        for locked in [false, true] {
            for code in [UNLOCKED, REFUSED, NO_COMPOSITOR] {
                assert_eq!(decide(Ended::Exited(code), locked, 0), Decision::Exit(code));
            }
        }
    }

    #[test]
    fn a_locked_session_always_gets_its_locker_back() {
        for ended in [Ended::Killed, Ended::Exited(FAILED), Ended::Exited(134)] {
            for tries in [0, 1, 1000] {
                assert_eq!(decide(ended, true, tries), Decision::Restart);
            }
        }
    }

    #[test]
    fn a_locker_that_never_locks_is_given_up_on() {
        assert_eq!(decide(Ended::Killed, false, 1), Decision::Restart);
        assert_eq!(decide(Ended::Killed, false, 2), Decision::Restart);
        assert_eq!(decide(Ended::Killed, false, 3), Decision::Exit(FAILED));
        assert_eq!(decide(Ended::Exited(7), false, 3), Decision::Exit(7));
    }

    #[test]
    fn restarts_slow_down_and_level_out() {
        let millis: Vec<u128> = (1..=7).map(|n| restart_delay(n).as_millis()).collect();
        assert_eq!(millis, [250, 500, 1000, 2000, 4000, 5000, 5000]);
        assert_eq!(restart_delay(0).as_millis(), 250);
        assert_eq!(restart_delay(u32::MAX).as_millis(), 5000);
    }

    #[test]
    fn a_worker_whose_end_is_unknown_is_not_replaced() {
        // Not even after a lock: it may have been unlocked.
        for locked in [false, true] {
            for tries in [0, 3] {
                assert_eq!(
                    decide(Ended::Unknown, locked, tries),
                    Decision::Exit(FAILED)
                );
            }
        }
    }

    // The tests below fork. The children do nothing but exit, which is
    // safe in a test binary with other threads running. They take turns,
    // because one of them changes what SIGCHLD does for the whole process.
    static FORKING: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        FORKING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn an_ignored_sigchld_does_not_hide_an_unlock() {
        let _turn = turn();
        // SAFETY: as a shell or a session manager may leave it for us.
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
        // Without the reset in `run`, waitpid fails with ECHILD here.
        assert_eq!(run(false, unlocks), UNLOCKED);
        assert_eq!(run(true, unlocks), 0);
        // And it is the reset that does it.
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_IGN) };
        // SAFETY: the child exits at once.
        let child = unsafe { libc::fork() };
        if child == 0 {
            leave(UNLOCKED);
        }
        assert_eq!(wait_for(child), Ended::Unknown);
        unsafe { libc::signal(libc::SIGCHLD, libc::SIG_DFL) };
    }

    fn unlocks(ready: Ready) -> u8 {
        ready.locked();
        UNLOCKED
    }

    fn refused(_: Ready) -> u8 {
        REFUSED
    }

    fn always_dies(_: Ready) -> u8 {
        FAILED
    }

    #[test]
    fn the_exit_code_is_the_workers() {
        let _turn = turn();
        assert_eq!(run(false, unlocks), UNLOCKED);
        assert_eq!(run(false, refused), REFUSED);
    }

    #[test]
    fn detached_it_returns_once_locked_or_with_the_reason_it_never_was() {
        let _turn = turn();
        assert_eq!(run(true, unlocks), 0);
        assert_eq!(run(true, refused), REFUSED);
    }

    #[test]
    fn a_worker_that_keeps_dying_before_locking_ends_it() {
        let _turn = turn();
        let started = Instant::now();
        assert_eq!(run(false, always_dies), FAILED);
        // Three tries with two waits between them.
        assert!(started.elapsed() >= Duration::from_millis(750));
    }
}
