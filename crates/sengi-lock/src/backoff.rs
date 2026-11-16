//! How long a wrong password costs.
//!
//! PAM already waits a couple of seconds before it says no. On top of
//! that the prompt stays shut for a time that grows with each wrong
//! password in a row, to make guessing at the keyboard slow. It never
//! grows past half a minute and never becomes a lock-out: the person at
//! the keyboard is most likely the owner with Caps Lock on.

use std::time::Duration;

/// Wrong passwords that cost nothing extra.
const FREE: u32 = 2;

/// The longest the prompt is ever shut.
const LONGEST: Duration = Duration::from_secs(30);

/// How long to keep the prompt shut after the `failures`-th wrong
/// password in a row.
pub fn delay(failures: u32) -> Duration {
    if failures <= FREE {
        return Duration::ZERO;
    }
    let doublings = (failures - FREE - 1).min(8);
    (Duration::from_secs(2) * 2u32.pow(doublings)).min(LONGEST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_slips_are_free_and_then_it_doubles_up_to_a_ceiling() {
        let seconds: Vec<u64> = (0..=8).map(|n| delay(n).as_secs()).collect();
        assert_eq!(seconds, [0, 0, 0, 2, 4, 8, 16, 30, 30]);
        assert_eq!(delay(1000), LONGEST);
        assert_eq!(delay(u32::MAX), LONGEST);
    }
}
