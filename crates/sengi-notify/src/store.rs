//! Which notifications are on screen, which are waiting, and which are
//! remembered. Pure bookkeeping: time is passed in, in milliseconds on any
//! clock that only goes forward, and what happened is handed back as a
//! list of closures for the caller to announce on the bus.

use std::collections::VecDeque;

use crate::notification::{Notification, Urgency};

/// Why a notification went away, with the numbers the spec gives them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Closed {
    pub id: u32,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Cards on screen at once.
    pub visible: usize,
    /// Notifications waiting for a place on screen.
    pub queued: usize,
    /// Notifications remembered for the notification centre.
    pub history: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            visible: 4,
            queued: 32,
            history: 50,
        }
    }
}

/// A card on screen.
#[derive(Clone, Debug)]
pub struct Popup {
    pub note: Notification,
    /// Changes when the card's content is replaced.
    pub version: u64,
    deadline: Option<u64>,
}

/// A line in the notification centre.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Unlike the id, never reused.
    pub key: u64,
    pub note: Notification,
}

pub struct Store {
    limits: Limits,
    /// Newest first, as drawn from the top.
    shown: Vec<Popup>,
    /// Next to show first.
    queue: VecDeque<Notification>,
    /// Newest first.
    history: VecDeque<Entry>,
    dnd: bool,
    /// Since when the countdowns have been standing still.
    paused_at: Option<u64>,
    counter: u64,
}

impl Store {
    pub fn new(limits: Limits) -> Store {
        Store {
            limits,
            shown: Vec::new(),
            queue: VecDeque::new(),
            history: VecDeque::new(),
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

    pub fn dnd(&self) -> bool {
        self.dnd
    }

    /// The notification with this id, if it has not been closed yet.
    pub fn active(&self, id: u32) -> Option<&Notification> {
        self.shown
            .iter()
            .map(|popup| &popup.note)
            .chain(&self.queue)
            .find(|note| note.id == id)
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

    /// Takes in a new notification, or new content for one that is still
    /// open under the same id.
    pub fn add(&mut self, note: Notification, now: u64, closed: &mut Vec<Closed>) {
        let replaces = self.active(note.id).is_some();
        self.remember(&note, replaces);

        if let Some(index) = self.shown.iter().position(|popup| popup.note.id == note.id) {
            let deadline = self.deadline(&note, now);
            let version = self.next();
            self.shown[index] = Popup {
                note,
                version,
                deadline,
            };
            return;
        }
        if let Some(waiting) = self.queue.iter_mut().find(|waiting| waiting.id == note.id) {
            *waiting = note;
            return;
        }

        // Quiet hours: only what cannot wait gets through. The rest is in
        // the history and is over as far as the sender is concerned.
        if self.dnd && note.urgency != Urgency::Critical {
            closed.push(Closed {
                id: note.id,
                reason: Reason::Expired,
            });
            return;
        }

        if self.shown.len() < self.limits.visible && self.queue.is_empty() {
            self.show(note, now);
            return;
        }
        self.enqueue(note, closed);
    }

    fn show(&mut self, note: Notification, now: u64) {
        let deadline = self.deadline(&note, now);
        let version = self.next();
        self.shown.insert(
            0,
            Popup {
                note,
                version,
                deadline,
            },
        );
    }

    /// Critical notifications wait ahead of everything else. When the
    /// queue is full the oldest ordinary one gives up its place.
    fn enqueue(&mut self, note: Notification, closed: &mut Vec<Closed>) {
        if self.queue.len() >= self.limits.queued {
            let victim = self
                .queue
                .iter()
                .position(|waiting| waiting.urgency != Urgency::Critical);
            let dropped = match victim {
                Some(index) => self.queue.remove(index),
                None if note.urgency == Urgency::Critical => self.queue.pop_front(),
                None => None,
            };
            match dropped {
                Some(dropped) => closed.push(Closed {
                    id: dropped.id,
                    reason: Reason::Expired,
                }),
                None => {
                    closed.push(Closed {
                        id: note.id,
                        reason: Reason::Expired,
                    });
                    return;
                }
            }
        }
        if note.urgency == Urgency::Critical {
            let ahead = self
                .queue
                .iter()
                .take_while(|waiting| waiting.urgency == Urgency::Critical)
                .count();
            self.queue.insert(ahead, note);
        } else {
            self.queue.push_back(note);
        }
    }

    fn remember(&mut self, note: &Notification, replaces: bool) {
        if replaces {
            // Still open: its line in the centre follows the new content.
            if let Some(entry) = self
                .history
                .iter_mut()
                .find(|entry| entry.note.id == note.id)
            {
                if note.transient {
                    let key = entry.key;
                    self.history.retain(|entry| entry.key != key);
                } else {
                    entry.note = note.clone();
                }
                return;
            }
        }
        if note.transient || self.limits.history == 0 {
            return;
        }
        let key = self.next();
        self.history.push_front(Entry {
            key,
            note: note.clone(),
        });
        self.history.truncate(self.limits.history);
    }

    /// Moves waiting notifications into free places on screen.
    fn promote(&mut self, now: u64) {
        while self.shown.len() < self.limits.visible {
            let Some(note) = self.queue.pop_front() else {
                break;
            };
            self.show(note, now);
        }
    }

    /// Closes one notification. False if there was none with this id.
    pub fn close(&mut self, id: u32, reason: Reason, now: u64, closed: &mut Vec<Closed>) -> bool {
        let before = self.shown.len() + self.queue.len();
        self.shown.retain(|popup| popup.note.id != id);
        self.queue.retain(|waiting| waiting.id != id);
        if self.shown.len() + self.queue.len() == before {
            return false;
        }
        closed.push(Closed { id, reason });
        self.promote(now);
        true
    }

    fn close_where(
        &mut self,
        reason: Reason,
        closed: &mut Vec<Closed>,
        mut goes: impl FnMut(&Notification) -> bool,
    ) {
        let mut ids = Vec::new();
        self.shown.retain(|popup| {
            let goes = goes(&popup.note);
            if goes {
                ids.push(popup.note.id);
            }
            !goes
        });
        self.queue.retain(|waiting| {
            let goes = goes(waiting);
            if goes {
                ids.push(waiting.id);
            }
            !goes
        });
        closed.extend(ids.into_iter().map(|id| Closed { id, reason }));
    }

    /// Closes everything on screen and waiting. The history stays.
    pub fn dismiss_all(&mut self, closed: &mut Vec<Closed>) {
        self.close_where(Reason::Dismissed, closed, |_| true);
    }

    /// Closes the cards whose time is up and fills their places.
    pub fn expire(&mut self, now: u64, closed: &mut Vec<Closed>) {
        if self.paused_at.is_some() {
            return;
        }
        let before = closed.len();
        self.shown.retain(|popup| {
            let due = popup.deadline.is_some_and(|deadline| deadline <= now);
            if due {
                closed.push(Closed {
                    id: popup.note.id,
                    reason: Reason::Expired,
                });
            }
            !due
        });
        if closed.len() != before {
            self.promote(now);
        }
    }

    /// When [`Store::expire`] next has something to do, if ever.
    pub fn next_deadline(&self) -> Option<u64> {
        if self.paused_at.is_some() {
            return None;
        }
        self.shown.iter().filter_map(|popup| popup.deadline).min()
    }

    /// Stops or restarts the countdowns, for as long as the pointer rests
    /// on the cards or the centre is open. Time spent paused does not count.
    pub fn set_paused(&mut self, paused: bool, now: u64) {
        match (self.paused_at, paused) {
            (None, true) => self.paused_at = Some(now),
            (Some(since), false) => {
                let waited = now.saturating_sub(since);
                for popup in &mut self.shown {
                    popup.deadline = popup
                        .deadline
                        .map(|deadline| deadline.saturating_add(waited));
                }
                self.paused_at = None;
            }
            _ => {}
        }
    }

    /// Turning do-not-disturb on also takes down what is up, except for
    /// critical notifications.
    pub fn set_dnd(&mut self, dnd: bool, now: u64, closed: &mut Vec<Closed>) {
        self.dnd = dnd;
        if dnd {
            self.close_where(Reason::Expired, closed, |note| {
                note.urgency != Urgency::Critical
            });
            self.promote(now);
        }
    }

    /// Removes one line from the centre. If its notification is still
    /// open, that is dismissed too.
    pub fn forget(&mut self, key: u64, now: u64, closed: &mut Vec<Closed>) {
        let Some(index) = self.history.iter().position(|entry| entry.key == key) else {
            return;
        };
        if let Some(entry) = self.history.remove(index) {
            self.close(entry.note.id, Reason::Dismissed, now, closed);
        }
    }

    /// Empties the centre and dismisses everything that is still open.
    pub fn clear(&mut self, closed: &mut Vec<Closed>) {
        self.history.clear();
        self.dismiss_all(closed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32) -> Notification {
        Notification {
            id,
            summary: format!("note {id}"),
            timeout: Some(5_000),
            ..Notification::default()
        }
    }

    fn critical(id: u32) -> Notification {
        Notification {
            urgency: Urgency::Critical,
            timeout: None,
            ..note(id)
        }
    }

    fn store() -> Store {
        Store::new(Limits {
            visible: 4,
            queued: 3,
            history: 5,
        })
    }

    fn shown(store: &Store) -> Vec<u32> {
        store.shown().iter().map(|popup| popup.note.id).collect()
    }

    fn remembered(store: &Store) -> Vec<u32> {
        store.history().map(|entry| entry.note.id).collect()
    }

    fn closed(id: u32, reason: Reason) -> Closed {
        Closed { id, reason }
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
        assert_eq!(out, [closed(1, Reason::Expired)]);
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
        assert_eq!(out, [closed(2, Reason::Expired)]);
        assert_eq!(store.next_deadline(), Some(5_000));
        store.expire(u64::MAX, &mut out);
        assert_eq!(shown(&store), [3]);
        assert_eq!(store.next_deadline(), None);
    }

    #[test]
    fn replacing_updates_the_card_in_place_and_restarts_its_time() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=3 {
            store.add(note(id), 0, &mut out);
        }
        let before = store.shown()[1].version;
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
        store.close(1, Reason::Dismissed, 0, &mut out);
        store.add(note(1), 10, &mut out);
        assert_eq!(shown(&store), [1]);
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
        assert_eq!(out, [closed(1, Reason::Closed)]);
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
        assert_eq!(out, [closed(5, Reason::Expired)]);
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
        assert_eq!(out, [closed(5, Reason::Expired)]);
        store.add(critical(10), 0, &mut out);
        assert_eq!(out[1], closed(6, Reason::Expired));
        // All critical now: an ordinary one is turned away itself.
        store.add(note(11), 0, &mut out);
        assert_eq!(out[2], closed(11, Reason::Expired));
        // And another critical one pushes out the oldest.
        store.add(critical(12), 0, &mut out);
        assert_eq!(out[3], closed(8, Reason::Expired));
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
    fn do_not_disturb_keeps_ordinary_notifications_off_the_screen() {
        let mut store = store();
        let mut out = Vec::new();
        store.set_dnd(true, 0, &mut out);
        assert!(store.dnd());
        store.add(note(1), 0, &mut out);
        assert!(shown(&store).is_empty());
        assert_eq!(store.queued(), 0);
        assert_eq!(remembered(&store), [1]);
        assert_eq!(out, [closed(1, Reason::Expired)]);

        // Critical ones still come up.
        store.add(critical(2), 0, &mut out);
        assert_eq!(shown(&store), [2]);
        // Transient ones leave no trace at all.
        store.add(
            Notification {
                transient: true,
                ..note(3)
            },
            0,
            &mut out,
        );
        assert_eq!(remembered(&store), [2, 1]);
        assert_eq!(out.last(), Some(&closed(3, Reason::Expired)));

        store.set_dnd(false, 0, &mut out);
        store.add(note(4), 0, &mut out);
        assert_eq!(shown(&store), [4, 2]);
    }

    #[test]
    fn turning_do_not_disturb_on_takes_down_what_is_up() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(note(1), 0, &mut out);
        store.add(critical(2), 0, &mut out);
        for id in 3..=6 {
            store.add(note(id), 0, &mut out);
        }
        store.add(critical(7), 0, &mut out);
        store.set_dnd(true, 0, &mut out);
        assert_eq!(shown(&store), [7, 2]);
        assert_eq!(store.queued(), 0);
        let mut gone: Vec<u32> = out.iter().map(|closed| closed.id).collect();
        gone.sort_unstable();
        assert_eq!(gone, [1, 3, 4, 5, 6]);
        assert!(out.iter().all(|closed| closed.reason == Reason::Expired));
    }

    #[test]
    fn a_card_on_screen_can_still_be_replaced_during_do_not_disturb() {
        let mut store = store();
        let mut out = Vec::new();
        store.add(critical(1), 0, &mut out);
        store.set_dnd(true, 0, &mut out);
        store.add(
            Notification {
                summary: "less urgent now".into(),
                ..note(1)
            },
            0,
            &mut out,
        );
        assert_eq!(store.shown()[0].note.summary, "less urgent now");
        assert!(out.is_empty());
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
    fn dismissing_everything_leaves_the_history() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=6 {
            store.add(note(id), 0, &mut out);
        }
        store.dismiss_all(&mut out);
        assert!(shown(&store).is_empty());
        assert_eq!(store.queued(), 0);
        assert_eq!(out.len(), 6);
        assert!(out.iter().all(|closed| closed.reason == Reason::Dismissed));
        assert_eq!(store.history().len(), 5);
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
        assert_eq!(out, [closed(2, Reason::Dismissed)]);
        assert!(shown(&store).is_empty());
        // A key that is gone does nothing.
        store.forget(keys[0], 0, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn clearing_empties_the_centre_and_the_screen() {
        let mut store = store();
        let mut out = Vec::new();
        for id in 1..=5 {
            store.add(note(id), 0, &mut out);
        }
        store.clear(&mut out);
        assert_eq!(store.history().len(), 0);
        assert!(shown(&store).is_empty());
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn history_keys_are_never_reused() {
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
