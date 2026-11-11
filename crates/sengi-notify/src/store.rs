//! Which notifications are on screen, which are waiting, which are open
//! but kept out of sight, and which are remembered. Pure bookkeeping: time
//! is passed in, in milliseconds on any clock that only goes forward, and
//! what happened is handed back as a list of closures for the caller to
//! announce on the bus.

use std::collections::VecDeque;

use crate::notification::{Notification, Urgency};

/// Why a notification went away, with the numbers the spec gives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

/// A notification that is over, with what the bus side needs to tell its
/// sender and to release its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closed {
    pub id: u32,
    pub reason: Reason,
    pub sender: String,
    pub token: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Cards on screen at once.
    pub visible: usize,
    /// Notifications waiting for a place on screen.
    pub queued: usize,
    /// Notifications open but not shown: held back by do-not-disturb, or
    /// turned away by a queue full of more urgent ones.
    pub unseen: usize,
    /// Notifications remembered for the notification centre.
    pub history: usize,
    /// Of these, how many one sender may hold, so that no single program
    /// can take the whole screen, queue or history for itself.
    pub sender_visible: usize,
    pub sender_queued: usize,
    pub sender_unseen: usize,
    pub sender_history: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            visible: 4,
            queued: 32,
            unseen: 32,
            history: 50,
            sender_visible: 2,
            sender_queued: 4,
            sender_unseen: 4,
            sender_history: 20,
        }
    }
}

/// A card on screen.
#[derive(Clone, Debug)]
pub struct Popup {
    pub note: Notification,
    /// Names this notification from the moment it is accepted until it is
    /// closed. Unlike the id, never used again.
    pub generation: u64,
    /// Changes when the card's content is replaced.
    pub version: u64,
    deadline: Option<u64>,
}

/// An open notification that is not on screen.
#[derive(Clone, Debug)]
struct Held {
    note: Notification,
    generation: u64,
    /// Only kept for the unseen; a queued one starts counting when shown.
    deadline: Option<u64>,
}

/// A line in the notification centre.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The generation of the notification the line is about.
    pub key: u64,
    pub note: Notification,
}

pub struct Store {
    limits: Limits,
    /// Newest first, as drawn from the top.
    shown: Vec<Popup>,
    /// Next to show first.
    queue: VecDeque<Held>,
    /// Oldest first.
    unseen: VecDeque<Held>,
    /// Newest first.
    history: VecDeque<Entry>,
    /// Lines added since the centre was last looked at.
    unread: usize,
    dnd: bool,
    /// Since when the countdowns have been standing still.
    paused_at: Option<u64>,
    counter: u64,
}

fn over(note: &Notification, reason: Reason) -> Closed {
    Closed {
        id: note.id,
        reason,
        sender: note.sender.clone(),
        token: note.token,
    }
}

impl Store {
    pub fn new(limits: Limits) -> Store {
        Store {
            limits,
            shown: Vec::new(),
            queue: VecDeque::new(),
            unseen: VecDeque::new(),
            history: VecDeque::new(),
            unread: 0,
            dnd: false,
            paused_at: None,
            counter: 0,
        }
    }

    pub fn shown(&self) -> &[Popup] {
        &self.shown
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn history(&self) -> impl ExactSizeIterator<Item = &Entry> {
        self.history.iter()
    }

    /// Lines in the centre that came in since [`Store::mark_read`].
    pub fn unread(&self) -> usize {
        self.unread
    }

    pub fn mark_read(&mut self) {
        self.unread = 0;
    }

    pub fn dnd(&self) -> bool {
        self.dnd
    }

    /// The notification with this id, if it has not been closed yet.
    pub fn active(&self, id: u32) -> Option<&Notification> {
        self.open()
            .find(|(note, _)| note.id == id)
            .map(|(note, _)| note)
    }

    fn open(&self) -> impl Iterator<Item = (&Notification, u64)> {
        let shown = self
            .shown
            .iter()
            .map(|popup| (&popup.note, popup.generation));
        let held = self.queue.iter().chain(&self.unseen);
        shown.chain(held.map(|held| (&held.note, held.generation)))
    }

    fn next(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    fn deadline(&self, note: &Notification, now: u64) -> Option<u64> {
        // While paused, time stands at the moment of the pause.
        let from = self.paused_at.unwrap_or(now);
        note.timeout.map(|timeout| from.saturating_add(timeout))
    }

    /// Do-not-disturb holds back everything that is not critical.
    fn quiet(&self, note: &Notification) -> bool {
        self.dnd && note.urgency != Urgency::Critical
    }

    /// Takes in a new notification, or new content for one that is still
    /// open under the same id. The bus side has already made sure that a
    /// replacement comes from the sender of the original, or that the
    /// original sender has left the bus; then the notification is the new
    /// sender's from here on.
    pub fn add(&mut self, note: Notification, now: u64, closed: &mut Vec<Closed>) {
        let existing = self
            .open()
            .find(|(open, _)| open.id == note.id)
            .map(|(_, generation)| generation);
        let generation = existing.unwrap_or_else(|| self.next());
        self.remember(&note, generation, existing.is_some());

        // New content is judged afresh: a card that was let through as
        // critical does not stay up once it is replaced by something that
        // do-not-disturb would have held back.
        if !self.quiet(&note) {
            if let Some(index) = self.shown.iter().position(|popup| popup.note.id == note.id) {
                let deadline = self.deadline(&note, now);
                let version = self.next();
                self.shown[index] = Popup {
                    note,
                    generation,
                    version,
                    deadline,
                };
                return;
            }
            if let Some(waiting) = self.queue.iter_mut().find(|held| held.note.id == note.id) {
                waiting.note = note;
                return;
            }
        }
        if existing.is_some() {
            self.take_out(note.id);
        }
        let held = Held {
            note,
            generation,
            deadline: None,
        };
        if self.quiet(&held.note) {
            self.hold_unseen(held, now, closed);
        } else {
            self.place(held, now, closed);
        }
        self.promote(now);
    }

    /// Removes an open notification from wherever it is, without closing it.
    fn take_out(&mut self, id: u32) {
        self.shown.retain(|popup| popup.note.id != id);
        self.queue.retain(|held| held.note.id != id);
        self.unseen.retain(|held| held.note.id != id);
    }

    fn shown_from(&self, sender: &str) -> usize {
        self.shown
            .iter()
            .filter(|popup| popup.note.sender == sender)
            .count()
    }

    /// Whether there is a place on screen that this sender may take.
    fn has_room(&self, sender: &str) -> bool {
        self.shown.len() < self.limits.visible
            && self.shown_from(sender) < self.limits.sender_visible
    }

    /// Shows the notification at once if nothing that could be shown is
    /// waiting ahead of it, and queues it otherwise.
    fn place(&mut self, held: Held, now: u64, closed: &mut Vec<Closed>) {
        let waiting = self
            .queue
            .iter()
            .any(|waiting| self.has_room(&waiting.note.sender));
        if self.has_room(&held.note.sender) && !waiting {
            self.show(held, now);
        } else {
            self.enqueue(held, now, closed);
        }
    }

    fn show(&mut self, held: Held, now: u64) {
        let deadline = self.deadline(&held.note, now);
        let version = self.next();
        self.shown.insert(
            0,
            Popup {
                note: held.note,
                generation: held.generation,
                version,
                deadline,
            },
        );
    }

    /// Critical notifications wait ahead of everything else. A sender with
    /// too many waiting loses its own oldest; when the whole queue is full
    /// the oldest ordinary one gives up its place.
    fn enqueue(&mut self, held: Held, now: u64, closed: &mut Vec<Closed>) {
        let ordinary = |held: &Held| held.note.urgency != Urgency::Critical;
        let sender = held.note.sender.as_str();
        let own: Vec<usize> = (0..self.queue.len())
            .filter(|&index| self.queue[index].note.sender == sender)
            .collect();
        if own.len() >= self.limits.sender_queued {
            let victim = own
                .iter()
                .copied()
                .find(|&index| ordinary(&self.queue[index]))
                .or(own.first().copied());
            if let Some(dropped) = victim.and_then(|index| self.queue.remove(index)) {
                closed.push(over(&dropped.note, Reason::Expired));
            }
        }
        if self.queue.len() >= self.limits.queued {
            let victim = self.queue.iter().position(ordinary);
            let dropped = match victim {
                Some(index) => self.queue.remove(index),
                None if !ordinary(&held) => self.queue.pop_front(),
                None => {
                    // Everything waiting is more urgent. Rather than close
                    // it the moment it arrives, let it run out unseen.
                    self.hold_unseen(held, now, closed);
                    return;
                }
            };
            if let Some(dropped) = dropped {
                closed.push(over(&dropped.note, Reason::Expired));
            }
        }
        if ordinary(&held) {
            self.queue.push_back(held);
        } else {
            let ahead = self
                .queue
                .iter()
                .take_while(|waiting| !ordinary(waiting))
                .count();
            self.queue.insert(ahead, held);
        }
    }

    /// Keeps a notification open without showing it. It is in the history,
    /// and it ends when its time is up like any other.
    fn hold_unseen(&mut self, mut held: Held, now: u64, closed: &mut Vec<Closed>) {
        if held.deadline.is_none() {
            held.deadline = self.deadline(&held.note, now);
        }
        let sender = held.note.sender.as_str();
        let own = self
            .unseen
            .iter()
            .filter(|unseen| unseen.note.sender == sender)
            .count();
        let victim = if own >= self.limits.sender_unseen {
            self.unseen
                .iter()
                .position(|unseen| unseen.note.sender == sender)
        } else if self.unseen.len() >= self.limits.unseen {
            Some(0)
        } else {
            None
        };
        if let Some(dropped) = victim.and_then(|index| self.unseen.remove(index)) {
            closed.push(over(&dropped.note, Reason::Expired));
        }
        self.unseen.push_back(held);
    }

    fn remember(&mut self, note: &Notification, generation: u64, replaces: bool) {
        if replaces {
            // Still open: its line in the centre follows the new content.
            if let Some(index) = self
                .history
                .iter()
                .position(|entry| entry.key == generation)
            {
                if note.transient {
                    self.history.remove(index);
                    self.unread = self.unread.min(self.history.len());
                } else {
                    self.history[index].note = note.clone();
                }
                return;
            }
        }
        if note.transient || self.limits.history == 0 {
            return;
        }
        self.history.push_front(Entry {
            key: generation,
            note: note.clone(),
        });
        self.unread += 1;

        // A sender with too many lines loses its own oldest first, so its
        // chatter cannot push everybody else's out.
        let own = self
            .history
            .iter()
            .filter(|entry| entry.note.sender == note.sender)
            .count();
        if own > self.limits.sender_history {
            let oldest = self
                .history
                .iter()
                .rposition(|entry| entry.note.sender == note.sender);
            if let Some(index) = oldest {
                self.history.remove(index);
            }
        }
        self.history.truncate(self.limits.history);
        self.unread = self.unread.min(self.history.len());
    }

    /// Moves waiting notifications into free places on screen, in order,
    /// passing over those whose sender already has its share.
    fn promote(&mut self, now: u64) {
        while self.shown.len() < self.limits.visible {
            let next = self
                .queue
                .iter()
                .position(|waiting| self.has_room(&waiting.note.sender));
            let Some(held) = next.and_then(|index| self.queue.remove(index)) else {
                break;
            };
            self.show(held, now);
        }
    }

    fn close_where(
        &mut self,
        reason: Reason,
        closed: &mut Vec<Closed>,
        mut goes: impl FnMut(&Notification, u64) -> bool,
    ) {
        self.shown.retain(|popup| {
            let goes = goes(&popup.note, popup.generation);
            if goes {
                closed.push(over(&popup.note, reason));
            }
            !goes
        });
        for held in [&mut self.queue, &mut self.unseen] {
            held.retain(|held| {
                let goes = goes(&held.note, held.generation);
                if goes {
                    closed.push(over(&held.note, reason));
                }
                !goes
            });
        }
    }

    /// Closes one notification. False if there was none with this id.
    pub fn close(&mut self, id: u32, reason: Reason, now: u64, closed: &mut Vec<Closed>) -> bool {
        let before = closed.len();
        self.close_where(reason, closed, |note, _| note.id == id);
        self.promote(now);
        closed.len() != before
    }

    /// Closes everything on screen and waiting for the screen. The history
    /// stays, and so does what do-not-disturb is holding back.
    pub fn dismiss_all(&mut self, closed: &mut Vec<Closed>) {
        let unseen = std::mem::take(&mut self.unseen);
        self.close_where(Reason::Dismissed, closed, |_, _| true);
        self.unseen = unseen;
    }

    /// Closes what has run out of time and fills the places that frees.
    pub fn expire(&mut self, now: u64, closed: &mut Vec<Closed>) {
        if self.paused_at.is_some() {
            return;
        }
        let due = |deadline: Option<u64>| deadline.is_some_and(|deadline| deadline <= now);
        self.shown.retain(|popup| {
            if due(popup.deadline) {
                closed.push(over(&popup.note, Reason::Expired));
            }
            !due(popup.deadline)
        });
        self.unseen.retain(|held| {
            if due(held.deadline) {
                closed.push(over(&held.note, Reason::Expired));
            }
            !due(held.deadline)
        });
        self.promote(now);
    }

    /// When [`Store::expire`] next has something to do, if ever.
    pub fn next_deadline(&self) -> Option<u64> {
        if self.paused_at.is_some() {
            return None;
        }
        let shown = self.shown.iter().filter_map(|popup| popup.deadline);
        let unseen = self.unseen.iter().filter_map(|held| held.deadline);
        shown.chain(unseen).min()
    }

    /// Stops or restarts the countdowns, for as long as the pointer rests
    /// on the cards or the centre is open. Time spent paused does not count.
    pub fn set_paused(&mut self, paused: bool, now: u64) {
        match (self.paused_at, paused) {
            (None, true) => self.paused_at = Some(now),
            (Some(since), false) => {
                let waited = now.saturating_sub(since);
                let later = |deadline: &mut Option<u64>| {
                    *deadline = deadline.map(|deadline| deadline.saturating_add(waited));
                };
                self.shown
                    .iter_mut()
                    .for_each(|popup| later(&mut popup.deadline));
                self.unseen
                    .iter_mut()
                    .for_each(|held| later(&mut held.deadline));
                self.paused_at = None;
            }
            _ => {}
        }
    }

    /// Turning do-not-disturb on takes what is up out of sight, except for
    /// critical notifications. Nothing is closed by it: a card keeps the
    /// time it had left, and what was waiting gets its full time, unseen.
    pub fn set_dnd(&mut self, dnd: bool, now: u64, closed: &mut Vec<Closed>) {
        self.dnd = dnd;
        if !dnd {
            return;
        }
        let mut hidden = Vec::new();
        let mut index = self.shown.len();
        // Oldest first, so that the caps drop the oldest.
        while index > 0 {
            index -= 1;
            if self.quiet(&self.shown[index].note) {
                let popup = self.shown.remove(index);
                hidden.push(Held {
                    note: popup.note,
                    generation: popup.generation,
                    // One that never expires stays open until its sender
                    // closes it or the caps on the unseen push it out.
                    deadline: popup.deadline,
                });
            }
        }
        let mut index = 0;
        while index < self.queue.len() {
            if self.quiet(&self.queue[index].note) {
                hidden.extend(self.queue.remove(index));
            } else {
                index += 1;
            }
        }
        for held in hidden {
            self.hold_unseen(held, now, closed);
        }
        self.promote(now);
    }

    /// Removes one line from the centre. If the very notification it is
    /// about is still open, that is dismissed too. A later notification
    /// that happens to reuse the id is left alone.
    pub fn forget(&mut self, key: u64, now: u64, closed: &mut Vec<Closed>) {
        let Some(index) = self.history.iter().position(|entry| entry.key == key) else {
            return;
        };
        self.history.remove(index);
        self.unread = self.unread.min(self.history.len());
        self.close_where(Reason::Dismissed, closed, |_, generation| generation == key);
        self.promote(now);
    }

    /// Empties the centre and dismisses everything that is still open.
    pub fn clear(&mut self, closed: &mut Vec<Closed>) {
        self.history.clear();
        self.unread = 0;
        self.close_where(Reason::Dismissed, closed, |_, _| true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(sender: &str, id: u32) -> Notification {
        Notification {
            id,
            sender: sender.to_string(),
            token: u64::from(id) + 1_000,
            summary: format!("note {id}"),
            timeout: Some(5_000),
            ..Notification::default()
        }
    }

    /// Every note from its own sender, as with one program per notification.
    fn note(id: u32) -> Notification {
        from(&format!(":1.{id}"), id)
    }

    fn critical(id: u32) -> Notification {
        Notification {
            urgency: Urgency::Critical,
            timeout: None,
            ..note(id)
        }
    }

    fn limits() -> Limits {
        Limits {
            visible: 4,
            queued: 3,
            unseen: 3,
            history: 5,
            sender_visible: 2,
            sender_queued: 2,
            sender_unseen: 2,
            sender_history: 3,
        }
    }

    fn store() -> Store {
        Store::new(limits())
    }

    fn shown(store: &Store) -> Vec<u32> {
        store.shown().iter().map(|popup| popup.note.id).collect()
    }

    fn remembered(store: &Store) -> Vec<u32> {
        store.history().map(|entry| entry.note.id).collect()
    }

    fn gone(out: &[Closed]) -> Vec<(u32, Reason)> {
        out.iter()
            .map(|closed| (closed.id, closed.reason))
            .collect()
    }

    #[test]
    fn the_newest_card_is_on_top() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=3 {
            store.add(note(id), 0, &mut out);
        }
        assert_eq!(shown(&store), [3, 2, 1]);
        assert_eq!(remembered(&store), [3, 2, 1]);
        assert!(out.is_empty());
    }

    #[test]
    fn a_fifth_notification_waits_for_a_place() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=5 {
            store.add(note(id), id as u64 * 100, &mut out);
        }
        assert_eq!(shown(&store), [4, 3, 2, 1]);
        assert_eq!(store.queued(), 1);
        assert!(store.active(5).is_some());

        // The first card's time is up; the one waiting takes the top.
        assert_eq!(store.next_deadline(), Some(5_100));
        store.expire(5_100, &mut out);
        assert_eq!(gone(&out), [(1, Reason::Expired)]);
        assert_eq!(shown(&store), [5, 4, 3, 2]);
        assert_eq!(store.queued(), 0);
        // Its own time starts when it is shown, not when it arrived.
        store.expire(10_099, &mut out);
        assert_eq!(shown(&store), [5]);
        store.expire(10_100, &mut out);
        assert!(shown(&store).is_empty());
        assert_eq!(store.next_deadline(), None);
    }

    #[test]
    fn nothing_jumps_the_queue_while_others_wait() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=5 {
            store.add(note(id), 0, &mut out);
        }
        store.close(4, Reason::Dismissed, 10, &mut out);
        assert_eq!(shown(&store), [5, 3, 2, 1]);
        store.add(note(6), 20, &mut out);
        assert_eq!(shown(&store), [5, 3, 2, 1]);
        assert_eq!(store.queued(), 1);
    }

    #[test]
    fn expiry_closes_everything_due_and_nothing_else() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        store.add(
            Notification {
                timeout: Some(1_000),
                ..note(2)
            },
            0,
            &mut out,
        );
        store.add(
            Notification {
                timeout: None,
                ..note(3)
            },
            0,
            &mut out,
        );
        assert_eq!(store.next_deadline(), Some(1_000));
        store.expire(999, &mut out);
        assert!(out.is_empty());
        store.expire(1_000, &mut out);
        assert_eq!(gone(&out), [(2, Reason::Expired)]);
        assert_eq!(store.next_deadline(), Some(5_000));
        store.expire(u64::MAX, &mut out);
        assert_eq!(shown(&store), [3]);
        assert_eq!(store.next_deadline(), None);
    }

    #[test]
    fn a_closure_names_the_sender_and_the_call_it_belongs_to() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(from(":1.7", 1), 0, &mut out);
        // The token follows the latest content.
        store.add(
            Notification {
                token: 77,
                ..from(":1.7", 1)
            },
            0,
            &mut out,
        );
        store.close(1, Reason::Closed, 0, &mut out);
        assert_eq!(
            out,
            [Closed {
                id: 1,
                reason: Reason::Closed,
                sender: ":1.7".into(),
                token: 77,
            }]
        );
    }

    #[test]
    fn replacing_updates_the_card_in_place_and_restarts_its_time() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=3 {
            store.add(note(id), 0, &mut out);
        }
        let before = store.shown()[1].version;
        let generation = store.shown()[1].generation;
        store.add(
            Notification {
                summary: "changed".into(),
                ..note(2)
            },
            3_000,
            &mut out,
        );
        assert_eq!(shown(&store), [3, 2, 1]);
        assert_eq!(store.shown()[1].note.summary, "changed");
        assert_ne!(store.shown()[1].version, before);
        assert_eq!(store.shown()[1].generation, generation);
        // One line in the centre, with the new text.
        assert_eq!(remembered(&store), [3, 2, 1]);
        assert_eq!(
            store.history().nth(1).map(|e| e.note.summary.as_str()),
            Some("changed")
        );

        store.expire(5_000, &mut out);
        assert_eq!(shown(&store), [2]);
        store.expire(8_000, &mut out);
        assert!(shown(&store).is_empty());
        assert!(out.iter().all(|closed| closed.reason == Reason::Expired));
    }

    #[test]
    fn a_card_taken_over_from_a_departed_sender_belongs_to_the_new_one() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(from("gone", 1), 0, &mut out);
        store.add(
            Notification {
                token: 9,
                ..from("here", 1)
            },
            0,
            &mut out,
        );
        assert_eq!(shown(&store), [1]);
        assert_eq!(remembered(&store), [1]);
        store.close(1, Reason::Dismissed, 0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].sender, "here");
        assert_eq!(out[0].token, 9);
    }

    #[test]
    fn replacing_one_that_waits_keeps_its_place() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=6 {
            store.add(note(id), 0, &mut out);
        }
        store.add(
            Notification {
                summary: "changed".into(),
                ..note(5)
            },
            0,
            &mut out,
        );
        assert_eq!(store.queued(), 2);
        store.close(1, Reason::Closed, 0, &mut out);
        assert_eq!(store.shown()[0].note.summary, "changed");
    }

    #[test]
    fn replacing_one_that_is_gone_shows_it_anew() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        let first = store.shown()[0].generation;
        store.close(1, Reason::Dismissed, 0, &mut out);
        store.add(note(1), 10, &mut out);
        assert_eq!(shown(&store), [1]);
        assert_ne!(store.shown()[0].generation, first);
        // The old line stays in the centre; this is a new event.
        assert_eq!(remembered(&store), [1, 1]);
    }

    #[test]
    fn closing_reports_the_reason_once() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        assert!(store.close(1, Reason::Closed, 0, &mut out));
        assert!(!store.close(1, Reason::Closed, 0, &mut out));
        assert!(!store.close(99, Reason::Dismissed, 0, &mut out));
        assert_eq!(gone(&out), [(1, Reason::Closed)]);
        assert_eq!(Reason::Expired as u32, 1);
        assert_eq!(Reason::Dismissed as u32, 2);
        assert_eq!(Reason::Closed as u32, 3);
    }

    #[test]
    fn closing_one_that_waits_works_too() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=5 {
            store.add(note(id), 0, &mut out);
        }
        assert!(store.close(5, Reason::Closed, 0, &mut out));
        assert_eq!(store.queued(), 0);
        assert_eq!(shown(&store), [4, 3, 2, 1]);
    }

    #[test]
    fn a_full_queue_drops_its_oldest_ordinary_notification() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=7 {
            store.add(note(id), 0, &mut out);
        }
        assert_eq!(store.queued(), 3);
        assert!(out.is_empty());
        store.add(note(8), 0, &mut out);
        assert_eq!(gone(&out), [(5, Reason::Expired)]);
        assert_eq!(store.queued(), 3);
        assert!(store.active(5).is_none());
        assert!(store.active(8).is_some());
    }

    #[test]
    fn a_flood_stays_bounded() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=10_000 {
            store.add(note(id), 0, &mut out);
        }
        assert_eq!(store.shown().len(), 4);
        assert_eq!(store.queued(), 3);
        assert_eq!(store.history().len(), 5);
        assert_eq!(out.len(), 10_000 - 7);
        assert_eq!(remembered(&store), [10_000, 9_999, 9_998, 9_997, 9_996]);
    }

    #[test]
    fn one_sender_gets_two_cards_and_the_rest_wait() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=3 {
            store.add(from("chatty", id), 0, &mut out);
        }
        assert_eq!(shown(&store), [2, 1]);
        assert_eq!(store.queued(), 1);
        // Someone else is shown at once, ahead of the one that waits.
        store.add(from("other", 10), 0, &mut out);
        assert_eq!(shown(&store), [10, 2, 1]);
        // When one of its cards goes, the next of its own comes up.
        store.close(1, Reason::Dismissed, 0, &mut out);
        assert_eq!(shown(&store), [3, 10, 2]);
        assert_eq!(store.queued(), 0);
    }

    #[test]
    fn a_sender_over_its_share_of_the_queue_loses_its_own_oldest() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(from("other", 100), 0, &mut out);
        for id in 1..=4 {
            store.add(from("chatty", id), 0, &mut out);
        }
        // Two shown, two waiting: at its limits.
        assert!(out.is_empty());
        store.add(from("other", 101), 0, &mut out);
        store.add(from("chatty", 5), 0, &mut out);
        assert_eq!(gone(&out), [(3, Reason::Expired)]);
        store.add(from("chatty", 6), 0, &mut out);
        assert_eq!(gone(&out)[1], (4, Reason::Expired));
        // Nobody else lost anything.
        assert!(store.active(100).is_some());
        assert!(store.active(101).is_some());
        assert_eq!(shown(&store), [101, 2, 1, 100]);
    }

    #[test]
    fn a_critical_flood_from_one_sender_leaves_room_for_others() {
        let mut store = Store::new(Limits::default());
        let mut out = Vec::new();
        for id in 1..=36 {
            store.add(
                Notification {
                    urgency: Urgency::Critical,
                    timeout: None,
                    ..from("hog", id)
                },
                0,
                &mut out,
            );
        }
        // Two on screen, four waiting, and the other thirty turned away,
        // oldest first.
        assert_eq!(shown(&store), [2, 1]);
        assert_eq!(store.queued(), 4);
        assert_eq!(out.len(), 30);
        assert_eq!(out[0].id, 3);
        assert!(out.iter().all(|closed| closed.sender == "hog"));

        store.add(from("mail", 100), 0, &mut out);
        assert_eq!(shown(&store), [100, 2, 1]);
        assert_eq!(out.len(), 30);
    }

    #[test]
    fn critical_notifications_wait_in_front_and_are_dropped_last() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=6 {
            store.add(note(id), 0, &mut out);
        }
        store.add(critical(7), 0, &mut out);
        store.close(1, Reason::Dismissed, 0, &mut out);
        assert_eq!(shown(&store)[0], 7);
        out.clear();

        // Queue: 5, 6. Fill it with critical ones.
        store.add(critical(8), 0, &mut out);
        store.add(critical(9), 0, &mut out);
        assert_eq!(gone(&out), [(5, Reason::Expired)]);
        store.add(critical(10), 0, &mut out);
        assert_eq!(gone(&out)[1], (6, Reason::Expired));
        // All critical now. An ordinary one is not closed on arrival: it
        // stays open, out of sight, until its time is up.
        store.add(
            Notification {
                timeout: Some(1_000),
                ..note(11)
            },
            0,
            &mut out,
        );
        assert_eq!(out.len(), 2);
        assert!(store.active(11).is_some());
        assert!(!shown(&store).contains(&11));
        store.expire(1_000, &mut out);
        assert_eq!(gone(&out)[2], (11, Reason::Expired));
        // And another critical one pushes out the oldest.
        store.add(critical(12), 0, &mut out);
        assert_eq!(gone(&out)[3], (8, Reason::Expired));
        assert_eq!(store.queued(), 3);
    }

    #[test]
    fn critical_notifications_never_expire() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(critical(1), 0, &mut out);
        assert_eq!(store.next_deadline(), None);
        store.expire(u64::MAX, &mut out);
        assert_eq!(shown(&store), [1]);
    }

    #[test]
    fn the_history_is_capped_and_skips_transient_notifications() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(
            Notification {
                transient: true,
                ..note(1)
            },
            0,
            &mut out,
        );
        assert_eq!(shown(&store), [1]);
        assert!(remembered(&store).is_empty());
        for id in 2..=9 {
            store.add(note(id), 0, &mut out);
            store.close(id, Reason::Dismissed, 0, &mut out);
        }
        assert_eq!(remembered(&store), [9, 8, 7, 6, 5]);
    }

    #[test]
    fn one_senders_flood_does_not_empty_the_history_of_others() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(from("mail", 1), 0, &mut out);
        store.add(from("calendar", 2), 0, &mut out);
        for id in 10..=200 {
            store.add(from("chatty", id), 0, &mut out);
        }
        assert_eq!(remembered(&store), [200, 199, 198, 2, 1]);
    }

    #[test]
    fn a_replacement_that_turns_transient_leaves_the_history() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        store.add(
            Notification {
                transient: true,
                ..note(1)
            },
            0,
            &mut out,
        );
        assert!(remembered(&store).is_empty());
        assert_eq!(shown(&store), [1]);
    }

    #[test]
    fn do_not_disturb_keeps_ordinary_notifications_open_but_out_of_sight() {
        let mut store = store();
        let mut out = Vec::new();
        store.set_dnd(true, 0, &mut out);
        assert!(store.dnd());
        store.add(note(1), 0, &mut out);
        assert!(shown(&store).is_empty());
        assert_eq!(store.queued(), 0);
        assert_eq!(remembered(&store), [1]);
        // Not closed on arrival: the sender hears about it when its time
        // is up, as it would have without do-not-disturb.
        assert!(out.is_empty());
        assert!(store.active(1).is_some());
        assert_eq!(store.next_deadline(), Some(5_000));
        store.expire(4_999, &mut out);
        assert!(out.is_empty());
        store.expire(5_000, &mut out);
        assert_eq!(gone(&out), [(1, Reason::Expired)]);

        // Critical ones still come up.
        store.add(critical(2), 0, &mut out);
        assert_eq!(shown(&store), [2]);
        // Transient ones leave no line.
        store.add(
            Notification {
                transient: true,
                ..note(3)
            },
            0,
            &mut out,
        );
        assert_eq!(remembered(&store), [2, 1]);

        // Turning it off does not bring up what was held back.
        store.set_dnd(false, 0, &mut out);
        assert_eq!(shown(&store), [2]);
        store.add(note(4), 0, &mut out);
        assert_eq!(shown(&store), [4, 2]);
    }

    #[test]
    fn what_is_held_back_can_be_closed_and_replaced() {
        let mut store = store();
        let mut out = Vec::new();
        store.set_dnd(true, 0, &mut out);
        store.add(note(1), 0, &mut out);
        store.add(
            Notification {
                summary: "changed".into(),
                ..note(1)
            },
            2_000,
            &mut out,
        );
        assert_eq!(remembered(&store), [1]);
        assert_eq!(store.next_deadline(), Some(7_000));
        assert!(store.close(1, Reason::Closed, 2_000, &mut out));
        assert_eq!(gone(&out), [(1, Reason::Closed)]);

        // Replaced after do-not-disturb ends, it comes into view.
        store.add(note(2), 3_000, &mut out);
        store.set_dnd(false, 3_000, &mut out);
        store.add(note(2), 3_500, &mut out);
        assert_eq!(shown(&store), [2]);
        assert_eq!(remembered(&store), [2, 1]);
    }

    #[test]
    fn what_is_held_back_is_bounded_per_sender_and_in_all() {
        let mut store = store();
        let mut out = Vec::new();
        store.set_dnd(true, 0, &mut out);
        let forever = |sender: &str, id| Notification {
            timeout: None,
            ..from(sender, id)
        };
        for id in 1..=4 {
            store.add(forever("chatty", id), 0, &mut out);
        }
        assert_eq!(gone(&out), [(1, Reason::Expired), (2, Reason::Expired)]);
        store.add(forever("a", 10), 0, &mut out);
        store.add(forever("b", 11), 0, &mut out);
        assert_eq!(gone(&out)[2], (3, Reason::Expired));
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn turning_do_not_disturb_on_hides_what_is_up_without_closing_it() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        store.add(critical(2), 0, &mut out);
        store.add(note(3), 1_000, &mut out);
        store.set_dnd(true, 2_000, &mut out);
        assert_eq!(shown(&store), [2]);
        assert!(out.is_empty());
        assert!(store.active(1).is_some());
        // Each keeps the time it had left.
        store.expire(5_000, &mut out);
        assert_eq!(gone(&out), [(1, Reason::Expired)]);
        store.expire(6_000, &mut out);
        assert_eq!(gone(&out)[1], (3, Reason::Expired));
    }

    #[test]
    fn a_critical_card_replaced_by_ordinary_content_obeys_do_not_disturb() {
        let mut store = store();
        let mut out = Vec::new();
        store.set_dnd(true, 0, &mut out);
        store.add(critical(1), 0, &mut out);
        assert_eq!(shown(&store), [1]);
        store.add(
            Notification {
                summary: "an advert, really".into(),
                ..note(1)
            },
            0,
            &mut out,
        );
        assert!(shown(&store).is_empty());
        assert!(out.is_empty());
        assert!(store.active(1).is_some());
        assert_eq!(remembered(&store), [1]);
        assert_eq!(
            store.history().next().map(|e| e.note.summary.as_str()),
            Some("an advert, really")
        );
        // Without do-not-disturb the same replacement is just an update.
        let mut store = self::store();
        store.add(critical(1), 0, &mut out);
        store.add(note(1), 0, &mut out);
        assert_eq!(shown(&store), [1]);
    }

    #[test]
    fn time_spent_paused_does_not_count() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        store.set_paused(true, 2_000);
        assert_eq!(store.next_deadline(), None);
        store.expire(60_000, &mut out);
        assert_eq!(shown(&store), [1]);

        // Arrives during the pause: its time starts when the pause ends.
        store.add(note(2), 30_000, &mut out);
        // Pausing twice changes nothing.
        store.set_paused(true, 40_000);
        store.set_paused(false, 60_000);
        assert_eq!(store.next_deadline(), Some(63_000));
        store.expire(63_000, &mut out);
        assert_eq!(shown(&store), [2]);
        assert_eq!(store.next_deadline(), Some(65_000));
        store.set_paused(false, 64_000);
        assert_eq!(store.next_deadline(), Some(65_000));
    }

    #[test]
    fn dismissing_everything_leaves_the_history_and_what_is_held_back() {
        let mut store = store();
        let mut out = Vec::new();
        store.set_dnd(true, 0, &mut out);
        store.add(note(50), 0, &mut out);
        store.set_dnd(false, 0, &mut out);
        for id in 1..=6 {
            store.add(note(id), 0, &mut out);
        }
        store.dismiss_all(&mut out);
        assert!(shown(&store).is_empty());
        assert_eq!(store.queued(), 0);
        assert_eq!(out.len(), 6);
        assert!(out.iter().all(|closed| closed.reason == Reason::Dismissed));
        assert_eq!(store.history().len(), 5);
        assert!(store.active(50).is_some());
    }

    #[test]
    fn forgetting_a_line_dismisses_its_open_notification() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        store.add(note(2), 0, &mut out);
        store.close(1, Reason::Expired, 0, &mut out);
        out.clear();

        let keys: Vec<u64> = store.history().map(|entry| entry.key).collect();
        // Note 1 is already closed: only the line goes.
        store.forget(keys[1], 0, &mut out);
        assert!(out.is_empty());
        assert_eq!(remembered(&store), [2]);
        // Note 2 is still up.
        store.forget(keys[0], 0, &mut out);
        assert_eq!(gone(&out), [(2, Reason::Dismissed)]);
        assert!(shown(&store).is_empty());
        // A key that is gone does nothing.
        store.forget(keys[0], 0, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn forgetting_an_old_line_leaves_a_live_notification_with_the_same_id() {
        // A progress notification: the program keeps using one id, long
        // after the first card under that id has gone.
        let mut store = store();
        let mut out = Vec::new();
        store.add(from("copy", 7), 0, &mut out);
        store.expire(5_000, &mut out);
        store.add(from("copy", 7), 6_000, &mut out);
        assert_eq!(remembered(&store), [7, 7]);
        out.clear();

        let keys: Vec<u64> = store.history().map(|entry| entry.key).collect();
        assert_ne!(keys[0], keys[1]);
        store.forget(keys[1], 6_500, &mut out);
        assert!(out.is_empty(), "the live card was closed by the old line");
        assert_eq!(shown(&store), [7]);
        assert_eq!(remembered(&store), [7]);

        // Replacing the live one still updates the line that is left.
        store.add(
            Notification {
                summary: "90%".into(),
                ..from("copy", 7)
            },
            6_600,
            &mut out,
        );
        assert_eq!(remembered(&store), [7]);
        assert_eq!(
            store.history().next().map(|e| e.note.summary.as_str()),
            Some("90%")
        );
        // And its own line does close it.
        store.forget(keys[0], 6_700, &mut out);
        assert_eq!(gone(&out), [(7, Reason::Dismissed)]);
    }

    #[test]
    fn clearing_empties_the_centre_and_closes_everything_open() {
        let mut store = store();
        let mut out = Vec::new();
        store.set_dnd(true, 0, &mut out);
        store.add(note(50), 0, &mut out);
        store.set_dnd(false, 0, &mut out);
        for id in 1..=5 {
            store.add(note(id), 0, &mut out);
        }
        store.clear(&mut out);
        assert_eq!(store.history().len(), 0);
        assert!(shown(&store).is_empty());
        assert_eq!(out.len(), 6);
        assert_eq!(store.unread(), 0);
    }

    #[test]
    fn unread_counts_lines_since_the_centre_was_last_seen() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        store.add(note(2), 0, &mut out);
        // A replacement is not a new line.
        store.add(note(2), 0, &mut out);
        // Nor is a transient notification.
        store.add(
            Notification {
                transient: true,
                ..note(3)
            },
            0,
            &mut out,
        );
        assert_eq!(store.unread(), 2);
        store.mark_read();
        assert_eq!(store.unread(), 0);
        for id in 10..=30 {
            store.add(note(id), 0, &mut out);
        }
        // Never more than there are lines.
        assert_eq!(store.unread(), 5);
        let key = store.history().next().map(|entry| entry.key).unwrap_or(0);
        store.forget(key, 0, &mut out);
        assert_eq!(store.unread(), 4);
    }

    #[test]
    fn generations_are_never_reused() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        let first = store.history().next().map(|entry| entry.key);
        store.clear(&mut out);
        store.add(note(1), 0, &mut out);
        assert_ne!(store.history().next().map(|entry| entry.key), first);
    }

    #[test]
    fn deadlines_far_in_the_future_do_not_overflow() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(
            Notification {
                timeout: Some(u64::MAX),
                ..note(1)
            },
            u64::MAX - 5,
            &mut out,
        );
        assert_eq!(store.next_deadline(), Some(u64::MAX));
        store.set_paused(true, u64::MAX - 4);
        store.set_paused(false, u64::MAX);
        assert_eq!(store.next_deadline(), Some(u64::MAX));
    }
}
