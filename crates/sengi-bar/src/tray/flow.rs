//! Two small pieces that keep the tray's threads from getting in each
//! other's way: a mailbox that holds only the newest message, and a clock
//! for work that must finish by a deadline.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A mailbox with room for one. What the tray wants shown goes to the UI
/// thread through here: a newer picture replaces one not yet collected,
/// and the UI is woken once for however many arrived. However fast items
/// change, the UI thread has at most one wake-up waiting.
pub struct Latest<T> {
    slot: Arc<Mutex<Option<T>>>,
}

impl<T> Clone for Latest<T> {
    fn clone(&self) -> Self {
        Latest {
            slot: self.slot.clone(),
        }
    }
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Latest {
            slot: Arc::default(),
        }
    }
}

impl<T> Latest<T> {
    /// Leaves a message. True means the box was empty, so whoever collects
    /// needs waking; false means a wake-up is already on its way.
    pub fn put(&self, message: T) -> bool {
        match self.slot.lock() {
            Ok(mut slot) => slot.replace(message).is_none(),
            Err(_) => false,
        }
    }

    /// Collects the newest message, if one is waiting.
    pub fn take(&self) -> Option<T> {
        self.slot.lock().ok()?.take()
    }
}

/// Time allowed for one piece of work made of several calls. Each call
/// has its own timeout, but ten calls that each take just under it would
/// add up to most of a minute; the budget is for the lot.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    deadline: Instant,
}

impl Budget {
    pub fn starting(now: Instant, allowed: Duration) -> Budget {
        Budget {
            deadline: now + allowed,
        }
    }

    /// How long the next call may take: what is left, but no more than
    /// `most`. None means the time is up and the work is to be given up.
    pub fn next(&self, now: Instant, most: Duration) -> Option<Duration> {
        let left = self.deadline.checked_duration_since(now)?;
        (!left.is_zero()).then_some(left.min(most))
    }
}

/// Remembers when the tray last changed under the pointer, so that a
/// click landing just afterwards is not taken for a click on whatever is
/// there now. An app could otherwise time a change to its own icon, or
/// the arrival of a new one, to catch a click meant for its neighbour.
#[derive(Debug, Default)]
pub struct ClickGuard {
    /// When the set of icons last changed.
    set: Option<Instant>,
    /// When each icon last changed its picture.
    icons: Vec<(u32, Instant)>,
}

impl ClickGuard {
    /// The icons were added to, removed from or moved.
    pub fn set_changed(&mut self, now: Instant) {
        self.set = Some(now);
    }

    /// One icon now looks different.
    pub fn icon_changed(&mut self, id: u32, now: Instant) {
        self.icons.retain(|(held, _)| *held != id);
        self.icons.push((id, now));
    }

    /// Forgets icons that are no longer shown.
    pub fn keep(&mut self, shown: &[u32]) {
        self.icons.retain(|(id, _)| shown.contains(id));
    }

    /// Whether a click on icon `id` at `now` should be ignored, `guard`
    /// being how long a change stays fresh.
    pub fn blocks(&self, id: u32, now: Instant, guard: Duration) -> bool {
        let fresh = |at: Instant| now.saturating_duration_since(at) < guard;
        self.set.is_some_and(fresh)
            || self
                .icons
                .iter()
                .any(|(held, at)| *held == id && fresh(*at))
    }
}

/// The icon to draw at `wanted` pixels out of the sizes an item was
/// decoded at: that size, else the next larger, else the largest.
pub fn icon_for<T>(icons: &[(u32, T)], wanted: u32) -> Option<&T> {
    icons
        .iter()
        .filter(|(size, _)| *size >= wanted)
        .min_by_key(|(size, _)| *size)
        .or_else(|| icons.iter().max_by_key(|(size, _)| *size))
        .map(|(_, icon)| icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_just_after_the_icons_moved_is_ignored() {
        let start = Instant::now();
        let guard = Duration::from_millis(400);
        let ms = Duration::from_millis;
        let mut clicks = ClickGuard::default();
        assert!(!clicks.blocks(1, start, guard));
        // A new icon arrives: nothing in the tray is safe to click yet.
        clicks.set_changed(start);
        assert!(clicks.blocks(1, start + ms(50), guard));
        assert!(clicks.blocks(2, start + ms(399), guard));
        assert!(!clicks.blocks(1, start + ms(400), guard));
    }

    #[test]
    fn a_click_just_after_an_icon_changed_its_picture_is_ignored_for_that_icon() {
        let start = Instant::now();
        let guard = Duration::from_millis(400);
        let ms = Duration::from_millis;
        let mut clicks = ClickGuard::default();
        clicks.icon_changed(1, start);
        assert!(clicks.blocks(1, start + ms(100), guard));
        // Its neighbours did not change and are not held up.
        assert!(!clicks.blocks(2, start + ms(100), guard));
        assert!(!clicks.blocks(1, start + ms(500), guard));
        // An icon that keeps changing keeps being guarded, and only one
        // time is kept for it.
        for n in 0..100 {
            clicks.icon_changed(1, start + ms(n));
        }
        assert_eq!(clicks.icons.len(), 1);
        clicks.keep(&[2, 3]);
        assert!(clicks.icons.is_empty());
    }

    #[test]
    fn each_bar_draws_the_size_decoded_for_its_scale() {
        let icons = [(16, "small"), (32, "large")];
        assert_eq!(icon_for(&icons, 16), Some(&"small"));
        assert_eq!(icon_for(&icons, 32), Some(&"large"));
        // A size not decoded yet: the next one up, shrunk when drawn.
        assert_eq!(icon_for(&icons, 24), Some(&"large"));
        assert_eq!(icon_for(&icons, 48), Some(&"large"));
        assert_eq!(icon_for::<&str>(&[], 16), None);
    }

    #[test]
    fn a_newer_message_replaces_one_not_yet_collected() {
        let latest = Latest::default();
        assert!(latest.put(1));
        // A thousand more before anyone looks: still one wake-up owed.
        for n in 2..=1000 {
            assert!(!latest.put(n));
        }
        assert_eq!(latest.take(), Some(1000));
        assert_eq!(latest.take(), None);
        // Collected: the next one wakes again.
        assert!(latest.put(1001));
    }

    #[test]
    fn both_ends_see_the_same_box() {
        let sender = Latest::default();
        let receiver = sender.clone();
        sender.put("picture");
        assert_eq!(receiver.take(), Some("picture"));
        assert_eq!(sender.take(), None);
    }

    #[test]
    fn calls_are_cut_to_what_is_left_of_the_budget() {
        let start = Instant::now();
        let secs = Duration::from_secs;
        let budget = Budget::starting(start, secs(3));
        assert_eq!(budget.next(start, secs(2)), Some(secs(2)));
        assert_eq!(budget.next(start + secs(2), secs(2)), Some(secs(1)));
        assert_eq!(
            budget.next(start + Duration::from_millis(2900), secs(2)),
            Some(Duration::from_millis(100))
        );
        assert_eq!(budget.next(start + secs(3), secs(2)), None);
        assert_eq!(budget.next(start + secs(60), secs(2)), None);
    }

    #[test]
    fn an_item_answering_just_inside_every_timeout_runs_out_of_budget() {
        let start = Instant::now();
        let budget = Budget::starting(start, Duration::from_secs(3));
        let mut now = start;
        let mut calls = 0;
        while let Some(allowed) = budget.next(now, Duration::from_secs(2)) {
            // Each answer comes 100 ms before it would have timed out.
            now += allowed.min(Duration::from_millis(1900));
            calls += 1;
        }
        assert_eq!(calls, 2);
        assert!(now - start <= Duration::from_secs(3));
    }
}
