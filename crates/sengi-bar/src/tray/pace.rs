//! Keeping a chatty item from making the bar work.
//!
//! Every `NewIcon` and its kin makes us read the item again, which is a
//! dozen calls and perhaps an icon decode. An item may ask for that as
//! often as it likes; it gets it at most once per interval, and the
//! interval grows while it keeps asking faster than that.

use std::time::{Duration, Instant};

/// Shortest time between two reads of the same item.
pub const BASE_INTERVAL: Duration = Duration::from_millis(250);

/// What the interval grows to for an item that never stops.
pub const MAX_INTERVAL: Duration = Duration::from_secs(8);

#[derive(Debug, PartialEq, Eq)]
enum Admit {
    Now,
    At(Instant),
}

#[derive(Debug)]
struct Limiter {
    interval: Duration,
    last: Option<Instant>,
}

impl Limiter {
    fn admit(&mut self, now: Instant) -> Admit {
        let Some(last) = self.last else {
            self.last = Some(now);
            return Admit::Now;
        };
        let since = now.saturating_duration_since(last);
        if since < self.interval {
            return Admit::At(last + self.interval);
        }
        // Quiet for a good while: forgiven.
        if since >= self.interval * 4 {
            self.interval = BASE_INTERVAL;
        }
        self.last = Some(now);
        Admit::Now
    }

    /// A request had to wait, so they are coming faster than we serve
    /// them: serve them more slowly still.
    fn waited(&mut self, now: Instant) {
        self.last = Some(now);
        self.interval = (self.interval * 2).min(MAX_INTERVAL);
    }
}

/// Paces one kind of work for one item: at most one piece in flight, at
/// most one waiting, and no two started closer together than the limiter
/// allows.
#[derive(Debug)]
pub struct Pacer {
    limiter: Limiter,
    in_flight: bool,
    /// Asked for again while one was in flight.
    again: bool,
    due: Option<Instant>,
}

impl Default for Pacer {
    fn default() -> Self {
        Pacer {
            limiter: Limiter {
                interval: BASE_INTERVAL,
                last: None,
            },
            in_flight: false,
            again: false,
            due: None,
        }
    }
}

impl Pacer {
    /// Something changed. True means start the work now; otherwise it has
    /// been noted and [`Pacer::finished`] or [`Pacer::tick`] will say when.
    pub fn request(&mut self, now: Instant) -> bool {
        if self.in_flight {
            self.again = true;
            return false;
        }
        if self.due.is_some() {
            return false;
        }
        match self.limiter.admit(now) {
            Admit::Now => {
                self.in_flight = true;
                true
            }
            Admit::At(when) => {
                self.due = Some(when);
                false
            }
        }
    }

    /// The work came back. The interval runs from here, not from when the
    /// work started, so an app that is slow to answer is not asked again
    /// the moment it has: a request that came in meanwhile waits its turn
    /// and [`Pacer::tick`] says when.
    pub fn finished(&mut self, now: Instant) {
        self.in_flight = false;
        self.limiter.last = Some(now);
        if std::mem::take(&mut self.again) {
            self.request(now);
        }
    }

    #[cfg(test)]
    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// When [`Pacer::tick`] next has something to say.
    pub fn due(&self) -> Option<Instant> {
        self.due
    }

    /// True means the waiting work is due and should start now.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.due.is_none_or(|due| due > now) {
            return false;
        }
        self.due = None;
        self.limiter.waited(now);
        self.in_flight = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_first_request_starts_at_once() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        assert!(pacer.request(start));
        assert_eq!(pacer.due(), None);
    }

    #[test]
    fn requests_while_one_is_in_flight_collapse_into_one_more() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        assert!(pacer.request(start));
        for n in 1..1000 {
            assert!(!pacer.request(start + ms(n) / 100));
        }
        // The follow-up waits out the interval, counted from the reply.
        pacer.finished(start + ms(20));
        assert_eq!(pacer.due(), Some(start + ms(20) + BASE_INTERVAL));
        assert!(!pacer.tick(start + ms(100)));
        assert!(pacer.tick(start + ms(20) + BASE_INTERVAL));
        assert_eq!(pacer.due(), None);
        // And then nothing more is owed.
        pacer.finished(start + ms(300));
        assert_eq!(pacer.due(), None);
    }

    #[test]
    fn a_slow_reply_is_not_followed_up_at_once() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        assert!(pacer.request(start));
        assert!(!pacer.request(start + ms(10)));
        // The app took 1.9 s to answer. Asking again straight away would
        // keep a worker on it for good.
        pacer.finished(start + ms(1900));
        assert!(!pacer.in_flight());
        assert_eq!(pacer.due(), Some(start + ms(1900) + BASE_INTERVAL));
        // A fresh request in the meantime changes nothing.
        assert!(!pacer.request(start + ms(2000)));
        assert!(pacer.tick(start + ms(1900) + BASE_INTERVAL));
    }

    #[test]
    fn requests_after_the_interval_are_served_directly() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        assert!(pacer.request(start));
        pacer.finished(start + ms(5));
        assert!(pacer.request(start + ms(300)));
        pacer.finished(start + ms(305));
        assert!(pacer.request(start + ms(900)));
    }

    /// Runs a spammer that signals every millisecond against replies that
    /// take five, and returns when each read started.
    fn spam(pacer: &mut Pacer, start: Instant, millis: u64) -> Vec<u64> {
        let mut starts = Vec::new();
        let mut busy_until = None;
        for t in 0..millis {
            let now = start + ms(t);
            if busy_until == Some(t) {
                busy_until = None;
                pacer.finished(now);
            }
            if pacer.tick(now) || pacer.request(now) {
                starts.push(t);
                busy_until = Some(t + 5);
            }
        }
        starts
    }

    #[test]
    fn a_spammer_is_slowed_down_to_the_longest_interval() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        let starts = spam(&mut pacer, start, 60_000);
        // 0, then 250 ms, 500 ms, 1 s, 2 s, 4 s and 8 s after each reply.
        assert_eq!(&starts[..7], [0, 255, 760, 1765, 3770, 7775, 15780]);
        assert!(starts.len() <= 13, "{starts:?}");
        let gaps: Vec<u64> = starts.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps[6..].iter().all(|gap| *gap == 8005), "{gaps:?}");
    }

    #[test]
    fn an_item_that_calms_down_is_forgiven() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        spam(&mut pacer, start, 20_000);
        // Finish whatever is still in flight, then stay quiet.
        pacer.finished(start + ms(20_000));
        let later = start + ms(20_000) + MAX_INTERVAL * 5;
        // A due follow-up from the spam is served first.
        if pacer.tick(later) {
            pacer.finished(later + ms(5));
        }
        let calm = later + MAX_INTERVAL * 5;
        assert!(pacer.request(calm));
        pacer.finished(calm + ms(5));
        // Back to the short interval.
        assert!(!pacer.request(calm + ms(100)));
        assert_eq!(pacer.due(), Some(calm + ms(5) + BASE_INTERVAL));
    }
}
