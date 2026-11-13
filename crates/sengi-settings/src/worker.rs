//! The thread a page's service runs on.
//!
//! Every call to a service may block: on a bus, a child process, a file.
//! None is made on the UI thread. A page sends its worker commands; the
//! service tells it when something changed outside; after either, the
//! worker reads the state again and posts it to the window. A worker that
//! has nothing to do is asleep in `recv`, not polling.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Services send changes in bursts: one scan is dozens of signals. A
/// worker waits for this much quiet before reading again.
pub const QUIET: Duration = Duration::from_millis(120);
/// And for no longer than this in all, however long the burst goes on.
pub const PATIENCE: Duration = Duration::from_millis(600);

/// Called by a service, from any thread, when a later read could differ.
pub type Notify = Box<dyn Fn() + Send + Sync>;

enum Signal<C> {
    Command(C),
    Changed,
    Stop,
}

/// What a worker should do next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next<C> {
    /// Carry this out, then read again.
    Command(C),
    /// Something changed outside: read again.
    Refresh,
}

/// What [`Inbox::wait_until`] came back with.
#[derive(Debug, PartialEq, Eq)]
pub enum Waited<C> {
    Command(C),
    Refresh,
    /// The deadline passed with nothing to do.
    TimedOut,
    Stopped,
}

/// Set when a worker's thread has run to its end.
#[derive(Default)]
struct Done {
    over: Mutex<bool>,
    told: Condvar,
}

/// The page's end.
pub struct Handle<C> {
    tx: Sender<Signal<C>>,
    done: Arc<Done>,
}

impl<C> Clone for Handle<C> {
    fn clone(&self) -> Handle<C> {
        Handle {
            tx: self.tx.clone(),
            done: self.done.clone(),
        }
    }
}

impl<C: Send + 'static> Handle<C> {
    pub fn send(&self, command: C) {
        let _ = self.tx.send(Signal::Command(command));
    }

    /// Have the worker read again.
    pub fn refresh(&self) {
        let _ = self.tx.send(Signal::Changed);
    }

    /// End the worker's loop. Its thread finishes what it is doing first.
    pub fn stop(&self) {
        let _ = self.tx.send(Signal::Stop);
    }

    /// End the worker's loop and wait, for at most `limit`, until its
    /// thread has finished whatever it does on the way out. False if it
    /// had not by then.
    pub fn stop_and_wait(&self, limit: Duration) -> bool {
        self.stop();
        let over = self.done.over.lock().unwrap_or_else(|p| p.into_inner());
        let (over, _) = self
            .done
            .told
            .wait_timeout_while(over, limit, |over| !*over)
            .unwrap_or_else(|p| p.into_inner());
        *over
    }

    /// Something for a service to call when its state changes.
    pub fn notifier(&self) -> Notify {
        let tx = Mutex::new(self.tx.clone());
        Box::new(move || {
            let tx = tx.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = tx.send(Signal::Changed);
        })
    }
}

/// The worker's end.
pub struct Inbox<C> {
    rx: Receiver<Signal<C>>,
    /// A command that arrived while a burst of changes was being waited
    /// out. It is next.
    held: Option<C>,
    stopped: bool,
}

impl<C> Inbox<C> {
    /// Blocks until there is something to do. `None` when the worker
    /// should end.
    pub fn wait(&mut self) -> Option<Next<C>> {
        match self.wait_until(None) {
            Waited::Command(command) => Some(Next::Command(command)),
            Waited::Refresh => Some(Next::Refresh),
            Waited::TimedOut | Waited::Stopped => None,
        }
    }

    /// As [`Inbox::wait`], but gives up at `deadline` if there is one.
    pub fn wait_until(&mut self, deadline: Option<Instant>) -> Waited<C> {
        if self.stopped {
            return Waited::Stopped;
        }
        if let Some(command) = self.held.take() {
            return Waited::Command(command);
        }
        let first = match deadline {
            None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(deadline) => self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now())),
        };
        match first {
            Ok(Signal::Command(command)) => Waited::Command(command),
            Err(RecvTimeoutError::Timeout) => Waited::TimedOut,
            Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => {
                self.stopped = true;
                Waited::Stopped
            }
            Ok(Signal::Changed) => {
                let started = Instant::now();
                while started.elapsed() < PATIENCE {
                    match self.rx.recv_timeout(QUIET) {
                        Ok(Signal::Changed) => {}
                        Ok(Signal::Command(command)) => {
                            self.held = Some(command);
                            break;
                        }
                        Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => {
                            self.stopped = true;
                            return Waited::Stopped;
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                Waited::Refresh
            }
        }
    }
}

pub fn channel<C>() -> (Handle<C>, Inbox<C>) {
    let (tx, rx) = mpsc::channel();
    (
        Handle {
            tx,
            done: Arc::default(),
        },
        Inbox {
            rx,
            held: None,
            stopped: false,
        },
    )
}

/// Start a worker thread running `body`.
pub fn spawn<C: Send + 'static>(
    name: &str,
    body: impl FnOnce(Handle<C>, Inbox<C>) + Send + 'static,
) -> Handle<C> {
    let (handle, inbox) = channel();
    let theirs = handle.clone();
    let spawned = thread::Builder::new()
        .name(format!("settings-{name}"))
        .spawn(move || {
            let done = theirs.done.clone();
            body(theirs, inbox);
            *done.over.lock().unwrap_or_else(|p| p.into_inner()) = true;
            done.told.notify_all();
        });
    if let Err(error) = spawned {
        eprintln!("sengi-settings: could not start the {name} worker: {error}");
    }
    handle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_come_out_in_order() {
        let (handle, mut inbox) = channel();
        handle.send(1);
        handle.send(2);
        assert_eq!(inbox.wait(), Some(Next::Command(1)));
        assert_eq!(inbox.wait(), Some(Next::Command(2)));
    }

    #[test]
    fn a_burst_of_changes_is_one_refresh() {
        let (handle, mut inbox) = channel::<u8>();
        let notify = handle.notifier();
        for _ in 0..50 {
            notify();
        }
        assert_eq!(inbox.wait(), Some(Next::Refresh));
        handle.stop();
        assert_eq!(inbox.wait(), None);
    }

    #[test]
    fn a_command_in_a_burst_is_not_lost() {
        let (handle, mut inbox) = channel();
        handle.refresh();
        handle.refresh();
        handle.send(7);
        handle.refresh();
        assert_eq!(inbox.wait(), Some(Next::Refresh));
        assert_eq!(inbox.wait(), Some(Next::Command(7)));
        assert_eq!(inbox.wait(), Some(Next::Refresh));
    }

    #[test]
    fn a_burst_that_never_ends_is_still_read() {
        let (handle, mut inbox) = channel::<u8>();
        let noisy = thread::spawn({
            let handle = handle.clone();
            move || {
                let until = Instant::now() + PATIENCE * 3;
                while Instant::now() < until {
                    handle.refresh();
                    thread::sleep(QUIET / 4);
                }
            }
        });
        let started = Instant::now();
        assert_eq!(inbox.wait(), Some(Next::Refresh));
        assert!(started.elapsed() < PATIENCE * 2);
        noisy.join().unwrap();
    }

    #[test]
    fn stop_ends_the_loop_even_mid_burst() {
        let (handle, mut inbox) = channel::<u8>();
        handle.refresh();
        handle.stop();
        assert_eq!(inbox.wait(), None);
        assert_eq!(inbox.wait(), None);
        // And so does every handle going away.
        let (handle, mut inbox) = channel::<u8>();
        drop(handle);
        assert_eq!(inbox.wait(), None);
    }

    #[test]
    fn a_wait_with_a_deadline_gives_up_at_it() {
        let (handle, mut inbox) = channel::<u8>();
        let soon = Instant::now() + Duration::from_millis(30);
        assert_eq!(inbox.wait_until(Some(soon)), Waited::TimedOut);
        assert!(Instant::now() >= soon);
        // A deadline already past does not hide what is waiting.
        handle.send(4);
        assert_eq!(inbox.wait_until(Some(soon)), Waited::Command(4));
        handle.refresh();
        assert_eq!(inbox.wait_until(Some(soon)), Waited::Refresh);
        handle.stop();
        assert_eq!(inbox.wait_until(Some(soon)), Waited::Stopped);
        assert_eq!(inbox.wait_until(None), Waited::Stopped);
    }

    #[test]
    fn stopping_can_wait_for_the_worker_to_finish() {
        let (finished, was_finished) = mpsc::channel();
        let handle = spawn("test", move |_, mut inbox: Inbox<u8>| {
            while inbox.wait().is_some() {}
            thread::sleep(Duration::from_millis(40));
            finished.send(()).unwrap();
        });
        assert!(handle.stop_and_wait(Duration::from_secs(5)));
        // The last words were said before the wait returned.
        assert!(was_finished.try_recv().is_ok());
        let stuck = spawn("test", |_, _inbox: Inbox<u8>| {
            thread::sleep(Duration::from_millis(300));
        });
        assert!(!stuck.stop_and_wait(Duration::from_millis(20)));
    }

    #[test]
    fn two_who_stop_a_worker_both_wait_for_its_end() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let ended = Arc::new(AtomicBool::new(false));
        let handle = spawn("test", {
            let ended = ended.clone();
            move |_, mut inbox: Inbox<u8>| {
                while inbox.wait().is_some() {}
                thread::sleep(Duration::from_millis(150));
                ended.store(true, Ordering::SeqCst);
            }
        });
        // As a signal and the closing window both stop the display
        // worker: whichever comes second must not return early.
        let waits = |handle: Handle<u8>, ended: Arc<AtomicBool>| {
            move || handle.stop_and_wait(Duration::from_secs(5)) && ended.load(Ordering::SeqCst)
        };
        let other = thread::spawn(waits(handle.clone(), ended.clone()));
        assert!(waits(handle, ended)());
        assert!(other.join().unwrap());
    }

    #[test]
    fn a_spawned_worker_runs_and_answers() {
        let (done, was_done) = mpsc::channel();
        let handle = spawn("test", move |_, mut inbox: Inbox<u32>| {
            let mut sum = 0;
            while let Some(next) = inbox.wait() {
                if let Next::Command(n) = next {
                    sum += n;
                }
            }
            done.send(sum).unwrap();
        });
        handle.send(2);
        handle.send(3);
        handle.stop();
        assert_eq!(was_done.recv_timeout(Duration::from_secs(5)), Ok(5));
    }
}
