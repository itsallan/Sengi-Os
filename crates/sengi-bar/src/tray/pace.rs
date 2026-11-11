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

/// Reads of one item that can be kept up per minute. The spacing above
/// only catches changes that come faster than it; an item that changes
/// three times a second, each change a little slower than the interval,
/// would be read three times a second for ever. Instead an item has an
/// allowance: it earns a read every three seconds, can save up
/// [`BURST`] of them, and when they are spent waits for the next. An
/// icon that animates for a while is followed at first and then every
/// three seconds, so where it comes to rest is shown soon after.
pub const READS_PER_MINUTE: u32 = 20;

/// Reads an item can have saved up.
pub const BURST: u32 = 10;

/// How long a read takes to earn.
const EARN: Duration = Duration::from_millis(60_000 / READS_PER_MINUTE as u64);

#[derive(Debug, PartialEq, Eq)]
enum Admit {
    Now,
    At(Instant),
}

#[derive(Debug)]
struct Limiter {
    interval: Duration,
    last: Option<Instant>,
    /// Reads saved up, and since when the next one is being earned.
    saved: u32,
    earning: Option<Instant>,
}

impl Limiter {
    /// Adds the reads earned since last time. Returns when the next one
    /// will have been.
    fn earn(&mut self, now: Instant) -> Instant {
        let mut since = self.earning.unwrap_or(now);
        while self.saved < BURST && now.saturating_duration_since(since) >= EARN {
            self.saved += 1;
            since += EARN;
        }
        // With the allowance full, nothing is being earned.
        if self.saved >= BURST {
            since = now;
        }
        self.earning = Some(since);
        since + EARN
    }

    fn admit(&mut self, now: Instant) -> Admit {
        let next = self.earn(now);
        let mut earliest = now;
        if let Some(last) = self.last {
            let since = now.saturating_duration_since(last);
            if since < self.interval {
                earliest = last + self.interval;
            } else if since >= self.interval * 4 {
                // Quiet for a good while: forgiven.
                self.interval = BASE_INTERVAL;
            }
        }
        if self.saved == 0 {
            earliest = earliest.max(next);
        }
        if earliest > now {
            return Admit::At(earliest);
        }
        self.last = Some(now);
        self.saved -= 1;
        Admit::Now
    }

    /// A request had to wait, so they are coming faster than we serve
    /// them: serve them more slowly still.
    fn waited(&mut self, now: Instant) {
        self.earn(now);
        self.saved = self.saved.saturating_sub(1);
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
                saved: BURST,
                earning: None,
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

    /// Runs an item that changes every `every` ms against replies that
    /// take five, and returns when each read started.
    fn steady(pacer: &mut Pacer, start: Instant, every: u64, millis: u64) -> Vec<u64> {
        let mut starts = Vec::new();
        let mut busy_until = None;
        for t in 0..millis {
            let now = start + ms(t);
            if busy_until == Some(t) {
                busy_until = None;
                pacer.finished(now);
            }
            if pacer.tick(now) || (t % every == 0 && pacer.request(now)) {
                starts.push(t);
                busy_until = Some(t + 5);
            }
        }
        starts
    }

    #[test]
    fn changes_just_slower_than_the_interval_run_out_of_allowance() {
        // Every 300 ms: never closer together than the spacing allows, so
        // the spacing alone would read it more than three times a second.
        let mut pacer = Pacer::default();
        let start = Instant::now();
        let starts = steady(&mut pacer, start, 300, 5 * 60_000);
        for minute in 0..5u64 {
            let reads = starts.iter().filter(|t| **t / 60_000 == minute).count();
            // What was saved up can be spent in the first minute as well.
            let most = READS_PER_MINUTE + if minute == 0 { BURST } else { 1 };
            assert!(reads <= most as usize, "minute {minute}: {reads}");
        }
        // What was saved up goes at once, and then it is a read as each
        // is earned: the item is still followed, three seconds behind.
        assert_eq!(starts[BURST as usize - 1], 9 * 300);
        assert_eq!(starts[BURST as usize], 3_000);
        assert_eq!(starts[BURST as usize + 1], 6_000);
        let last = *starts.last().unwrap();
        assert!(5 * 60_000 - last <= 8_100, "last read at {last}");
    }

    #[test]
    fn an_allowance_that_is_not_used_is_not_saved_up_without_limit() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        // An hour of silence, then a burst.
        let later = start + Duration::from_secs(3600);
        let starts = steady(&mut pacer, later, 300, 20_000);
        let in_burst = starts.iter().filter(|t| **t < 3_000).count();
        assert_eq!(in_burst, BURST as usize);
    }

    #[test]
    fn an_item_that_changes_now_and_then_never_meets_the_allowance() {
        // Every five seconds, like a network applet: read every time.
        let mut pacer = Pacer::default();
        let start = Instant::now();
        let starts = steady(&mut pacer, start, 5_000, 10 * 60_000);
        assert_eq!(starts.len(), 120);
        assert!(starts.iter().all(|t| t % 5_000 == 0));
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
