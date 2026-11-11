//! Keeps talkative clients from taking over the screen: each sender on
//! the bus gets a small allowance of notifications that refills slowly.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct Allowance {
    /// Notifications a sender may post in one go.
    pub burst: u32,
    /// Milliseconds for one more to become available.
    pub refill: u64,
    /// Senders remembered at once.
    pub senders: usize,
}

impl Allowance {
    /// What one connection may post: a burst, then ten a second.
    pub fn per_sender() -> Allowance {
        Allowance {
            burst: 30,
            refill: 100,
            senders: 64,
        }
    }

    /// What all connections may post between them, counted under a single
    /// name. A fresh connection gets a fresh allowance of its own, so this
    /// is the limit that a client reconnecting for every call meets.
    pub fn everyone() -> Allowance {
        Allowance {
            burst: 60,
            refill: 50,
            senders: 1,
        }
    }

    /// Commands for the centre and do-not-disturb, which come from clicks.
    pub fn control() -> Allowance {
        Allowance {
            burst: 10,
            refill: 200,
            senders: 1,
        }
    }
}

struct Bucket {
    left: u32,
    /// When `left` was last topped up.
    since: u64,
}

pub struct RateLimiter {
    allowance: Allowance,
    buckets: HashMap<String, Bucket>,
}

impl RateLimiter {
    pub fn new(allowance: Allowance) -> RateLimiter {
        RateLimiter {
            allowance,
            buckets: HashMap::new(),
        }
    }

    /// Whether `sender` may post a notification at `now`, in milliseconds.
    /// Counts it if so.
    pub fn allow(&mut self, sender: &str, now: u64) -> bool {
        let Allowance {
            burst,
            refill,
            senders,
        } = self.allowance;
        let refill = refill.max(1);

        if !self.buckets.contains_key(sender) {
            if self.buckets.len() >= senders {
                self.make_room(now);
            }
            self.buckets.insert(
                sender.to_string(),
                Bucket {
                    left: burst,
                    since: now,
                },
            );
        }
        let Some(bucket) = self.buckets.get_mut(sender) else {
            return false;
        };

        let earned = now.saturating_sub(bucket.since) / refill;
        if earned > 0 {
            let room = u64::from(burst - bucket.left);
            if earned >= room {
                bucket.left = burst;
                bucket.since = now;
            } else {
                bucket.left += earned as u32;
                bucket.since += earned * refill;
            }
        }
        if bucket.left == 0 {
            return false;
        }
        bucket.left -= 1;
        true
    }

    /// Forgets senders whose allowance would be full again by now, which
    /// loses nothing. If every sender is still short, the one that has
    /// been quiet longest goes.
    fn make_room(&mut self, now: u64) {
        let Allowance { burst, refill, .. } = self.allowance;
        let refill = refill.max(1);
        self.buckets.retain(|_, bucket| {
            let earned = now.saturating_sub(bucket.since) / refill;
            earned < u64::from(burst - bucket.left)
        });
        if self.buckets.len() >= self.allowance.senders {
            let oldest = self
                .buckets
                .iter()
                .min_by_key(|(_, bucket)| bucket.since)
                .map(|(sender, _)| sender.clone());
            if let Some(oldest) = oldest {
                self.buckets.remove(&oldest);
            }
        }
    }

    #[cfg(test)]
    fn remembered(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter() -> RateLimiter {
        RateLimiter::new(Allowance {
            burst: 3,
            refill: 1_000,
            senders: 4,
        })
    }

    #[test]
    fn a_burst_is_allowed_and_then_no_more() {
        let mut limiter = limiter();
        assert!(limiter.allow(":1.5", 0));
        assert!(limiter.allow(":1.5", 0));
        assert!(limiter.allow(":1.5", 10));
        assert!(!limiter.allow(":1.5", 20));
        assert!(!limiter.allow(":1.5", 999));
    }

    #[test]
    fn the_allowance_refills_one_at_a_time() {
        let mut limiter = limiter();
        for _ in 0..3 {
            assert!(limiter.allow(":1.5", 0));
        }
        assert!(limiter.allow(":1.5", 1_000));
        assert!(!limiter.allow(":1.5", 1_500));
        // The half second already waited still counts.
        assert!(limiter.allow(":1.5", 2_000));
        assert!(!limiter.allow(":1.5", 2_000));
    }

    #[test]
    fn it_never_refills_past_the_burst() {
        let mut limiter = limiter();
        for _ in 0..3 {
            assert!(limiter.allow(":1.5", 0));
        }
        let later = 1_000_000;
        for _ in 0..3 {
            assert!(limiter.allow(":1.5", later));
        }
        assert!(!limiter.allow(":1.5", later));
    }

    #[test]
    fn senders_do_not_share_an_allowance() {
        let mut limiter = limiter();
        for _ in 0..3 {
            assert!(limiter.allow(":1.5", 0));
        }
        assert!(!limiter.allow(":1.5", 0));
        assert!(limiter.allow(":1.6", 0));
    }

    #[test]
    fn the_table_of_senders_stays_small() {
        let mut limiter = limiter();
        for sender in 0..10_000 {
            assert!(limiter.allow(&format!(":1.{sender}"), sender));
            assert!(limiter.remembered() <= 4);
        }
    }

    #[test]
    fn a_sender_that_is_still_short_is_not_forgotten_for_an_idle_one() {
        let mut limiter = limiter();
        for _ in 0..3 {
            assert!(limiter.allow(":1.1", 0));
        }
        // Three senders that used one each, long enough ago to be whole.
        for sender in [":1.2", ":1.3", ":1.4"] {
            assert!(limiter.allow(sender, 0));
        }
        assert!(limiter.allow(":1.9", 1_500));
        assert!(limiter.remembered() <= 4);
        // The noisy one is still counted: one earned back, no more.
        assert!(limiter.allow(":1.1", 1_500));
        assert!(!limiter.allow(":1.1", 1_500));
    }

    #[test]
    fn a_clock_that_stands_still_or_a_zero_refill_does_not_panic() {
        let mut limiter = RateLimiter::new(Allowance {
            burst: 1,
            refill: 0,
            senders: 1,
        });
        assert!(limiter.allow("a", 5));
        assert!(!limiter.allow("a", 5));
        assert!(limiter.allow("a", 6));
        assert!(limiter.allow("b", 0));
    }
}
