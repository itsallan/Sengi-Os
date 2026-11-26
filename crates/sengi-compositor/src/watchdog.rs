//! Leaves a line in the journal when the main loop stops answering.
//!
//! A compositor that is stuck takes the pointer, the keyboard and terminal
//! switching with it, and looks like a frozen machine. A small thread wakes
//! the loop every few seconds and, if the loop has not answered after
//! `STALL_AFTER`, says so and what the main thread was doing, which tells a
//! deadlock from a loop that spins or waits on a disk. It only reports.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use calloop::{
    ping::{make_ping, Ping},
    LoopHandle,
};

use crate::state::State;

/// How long between two looks at the loop.
const CHECK_EVERY: Duration = Duration::from_secs(3);

/// How long the loop may take to answer.
const STALL_AFTER: Duration = Duration::from_secs(2);

/// The thread watching the loop. It ends when this is dropped.
pub struct Watchdog {
    _alive: mpsc::Sender<()>,
}

/// Watches the loop of `handle`, which has to run on the calling thread.
/// None, and said so, if that cannot be set up.
pub fn start(handle: &LoopHandle<'static, State>) -> Option<Watchdog> {
    match set_up(handle) {
        Ok(watchdog) => Some(watchdog),
        Err(err) => {
            crate::say!("no watchdog for the main loop: {err}");
            None
        }
    }
}

fn set_up(handle: &LoopHandle<'static, State>) -> Result<Watchdog, String> {
    let (ping, source) = make_ping().map_err(|err| err.to_string())?;
    let answers = Arc::new(AtomicU64::new(0));
    let answered = answers.clone();
    handle
        .insert_source(source, move |_, _, _| {
            answered.fetch_add(1, Ordering::Relaxed);
        })
        .map_err(|err| err.to_string())?;
    // SAFETY: gettid takes nothing and cannot fail.
    let main_thread = unsafe { libc::gettid() };
    let (alive, gone) = mpsc::channel();
    thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || watch(&ping, &answers, main_thread, &gone))
        .map_err(|err| err.to_string())?;
    Ok(Watchdog { _alive: alive })
}

fn watch(ping: &Ping, answers: &AtomicU64, main_thread: libc::pid_t, gone: &mpsc::Receiver<()>) {
    // Waits for `time`; true if the watchdog was dropped meanwhile.
    let ended = |time: Duration| gone.recv_timeout(time) != Err(RecvTimeoutError::Timeout);
    loop {
        if ended(CHECK_EVERY) {
            return;
        }
        let before = answers.load(Ordering::Relaxed);
        let asked = Instant::now();
        ping.ping();
        if ended(STALL_AFTER) {
            return;
        }
        if answers.load(Ordering::Relaxed) != before {
            continue;
        }
        // Said now, and again each time the stall has doubled in length.
        let mut next = Duration::ZERO;
        while answers.load(Ordering::Relaxed) == before {
            let stalled = asked.elapsed();
            if stalled >= next {
                crate::say!(
                    "watchdog: the main loop has not answered for {} s ({})",
                    stalled.as_secs(),
                    describe(main_thread)
                );
                next = stalled * 2;
            }
            if ended(Duration::from_millis(500)) {
                return;
            }
        }
        crate::say!(
            "watchdog: the main loop answered again after {} s",
            asked.elapsed().as_secs()
        );
    }
}

/// The state of a thread of ours and the kernel function it waits in.
fn describe(thread: libc::pid_t) -> String {
    let read = |what: &str| {
        std::fs::read_to_string(format!("/proc/self/task/{thread}/{what}")).unwrap_or_default()
    };
    let stat = read("stat");
    let wait = read("wchan");
    format!(
        "main thread state {}, waiting in {}",
        state_letter(&stat).unwrap_or('?'),
        match wait.trim() {
            "" | "0" => "nothing",
            function => function,
        }
    )
}

/// The state letter of a `/proc/.../stat` line: what follows the name,
/// which is in parentheses and may itself contain them.
fn state_letter(stat: &str) -> Option<char> {
    stat.rsplit_once(") ")?.1.chars().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_follows_the_last_parenthesis() {
        assert_eq!(state_letter("12 (sengi) S 1 12"), Some('S'));
        assert_eq!(state_letter("12 (a) b) D 1 12"), Some('D'));
        assert_eq!(state_letter(""), None);
    }
}
