//! The thread a page's service runs on.
//!
//! Every call to a service may block: on a bus, a child process, a file.
//! None is made on the UI thread. A page sends its worker commands; the
//! service tells it when something changed outside; after either, the
//! worker reads the state again and posts it to the window. A worker that
//! has nothing to do is asleep in `recv`, not polling.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
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

/// The page's end.
pub struct Handle<C> {
    tx: Sender<Signal<C>>,
}

impl<C> Clone for Handle<C> {
    fn clone(&self) -> Handle<C> {
        Handle {
            tx: self.tx.clone(),
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
        if self.stopped {
            return None;
        }
        if let Some(command) = self.held.take() {
            return Some(Next::Command(command));
        }
        match self.rx.recv().ok()? {
            Signal::Command(command) => Some(Next::Command(command)),
            Signal::Stop => None,
            Signal::Changed => {
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
                            return None;
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                Some(Next::Refresh)
            }
        }
    }
}

pub fn channel<C>() -> (Handle<C>, Inbox<C>) {
    let (tx, rx) = mpsc::channel();
    (
        Handle { tx },
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
        .spawn(move || body(theirs, inbox));
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
