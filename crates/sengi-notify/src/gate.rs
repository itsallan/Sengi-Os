//! Holds what the bus side has for the UI until there is a UI.
//!
//! The service owns its name a little before the compositor connection
//! and the UI loop exist, and on a busy machine "a little" can be seconds.
//! The call that started the service is delivered in exactly that time.
//! It must not be refused, and it must not make the bus side wait either:
//! the thread that handles calls is the one that reads every reply the
//! start-up itself is waiting for. So messages are kept here, in order,
//! and passed on the moment the way is open.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

type Forward<T> = Box<dyn Fn(T) -> bool + Send>;

enum State<T> {
    /// No UI yet. What has arrived so far, oldest first.
    Waiting(VecDeque<T>),
    /// Straight through. The function says whether the UI took it.
    Open(Forward<T>),
    Closed,
}

pub struct Gate<T> {
    state: Mutex<State<T>>,
}

impl<T> Default for Gate<T> {
    fn default() -> Gate<T> {
        Gate {
            state: Mutex::new(State::Waiting(VecDeque::new())),
        }
    }
}

impl<T> Gate<T> {
    pub fn new() -> Gate<T> {
        Gate::default()
    }

    fn state(&self) -> MutexGuard<'_, State<T>> {
        // The state is one assignment away from consistent at all times,
        // so a panic elsewhere while it was held spoils nothing.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Passes a message to the UI, or keeps it for when there is one.
    /// Never waits. False only when the UI is gone for good.
    ///
    /// Nothing here limits how much is kept: callers count what the UI
    /// has not yet handled, held messages included, and stop at their cap.
    pub fn send(&self, message: T) -> bool {
        match &mut *self.state() {
            State::Waiting(held) => {
                held.push_back(message);
                true
            }
            State::Open(forward) => forward(message),
            State::Closed => false,
        }
    }

    /// The UI is there. Everything held goes through `forward` in the
    /// order it came, ahead of anything sent from now on.
    pub fn open(&self, forward: impl Fn(T) -> bool + Send + 'static) {
        let mut state = self.state();
        if let State::Waiting(held) = &mut *state {
            for message in held.drain(..) {
                forward(message);
            }
        }
        *state = State::Open(Box::new(forward));
    }

    /// There will be no UI, or no more of it. What was held is dropped.
    pub fn close(&self) {
        *self.state() = State::Closed;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;

    use super::*;

    fn into(sender: mpsc::Sender<u32>) -> impl Fn(u32) -> bool + Send + 'static {
        let sender = Mutex::new(sender);
        move |message| {
            sender
                .lock()
                .is_ok_and(|sender| sender.send(message).is_ok())
        }
    }

    #[test]
    fn messages_sent_before_the_ui_exists_are_accepted_and_kept() {
        let gate = Gate::new();
        assert!(gate.send(1));
        assert!(gate.send(2));
        assert!(gate.send(3));
        let (sender, receiver) = mpsc::channel();
        gate.open(into(sender));
        assert_eq!(receiver.try_iter().collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn once_open_messages_go_straight_through_after_the_held_ones() {
        let gate = Gate::new();
        gate.send(1);
        let (sender, receiver) = mpsc::channel();
        gate.open(into(sender));
        assert!(gate.send(2));
        assert!(gate.send(3));
        assert_eq!(receiver.try_iter().collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn sending_never_waits_for_the_ui() {
        // The whole point: this returns although nothing will ever open
        // the gate, where waiting for the UI would hang the test.
        let gate = Gate::new();
        for message in 0..1_000 {
            assert!(gate.send(message));
        }
    }

    #[test]
    fn order_holds_when_the_gate_opens_while_others_are_sending() {
        let gate = Arc::new(Gate::new());
        let (sender, receiver) = mpsc::channel();
        let writer = {
            let gate = gate.clone();
            thread::spawn(move || {
                for message in 0..5_000 {
                    assert!(gate.send(message));
                }
            })
        };
        gate.open(into(sender));
        writer.join().expect("the writer finishes");
        let seen: Vec<u32> = receiver.try_iter().collect();
        assert_eq!(seen, (0..5_000).collect::<Vec<_>>());
    }

    #[test]
    fn a_message_is_only_refused_once_the_ui_is_gone() {
        let gate = Gate::new();
        assert!(gate.send(1));
        gate.close();
        assert!(!gate.send(2));

        let gate = Gate::new();
        let (sender, receiver) = mpsc::channel();
        gate.open(into(sender));
        assert!(gate.send(1));
        drop(receiver);
        assert!(!gate.send(2));
    }

    #[test]
    fn held_messages_are_dropped_when_there_will_be_no_ui() {
        let gate = Gate::new();
        gate.send(1);
        gate.close();
        let (sender, receiver) = mpsc::channel();
        // Opening afterwards does not bring back what was dropped.
        gate.open(into(sender));
        assert!(receiver.try_iter().next().is_none());
    }
}
