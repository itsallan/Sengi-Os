//! Night light: the screen turns warm at night.
//!
//! The tint is a colour ramp on the display itself (the CRTC's gamma
//! table), not something drawn: it costs nothing per frame, damage
//! tracking and direct scanout are untouched, and what the compositor
//! hands to wlr-screencopy, so `sengi-shot` too, is the picture before the
//! ramp and has no tint. Where a driver has no gamma table, which is the
//! case of the virtual GPUs of a VM, there is no night light on that
//! output. The headless backend has no display to hold a ramp, so it
//! applies the same ramp to the pictures it saves for scripts, as a panel
//! would, and leaves screencopy alone.
//!
//! The times are fixed hours, `display.night-from` to `display.night-to`
//! in local time. One timer is set for the next of the two; nothing wakes
//! the compositor in between. It is set again on SIGHUP and when the
//! machine wakes from sleep, which a monotonic timer does not count.

use std::time::Duration;

use crate::{config::NightLight, state::State};
use calloop::timer::{TimeoutAction, Timer};

/// Red, green and blue as fractions of full.
pub type Tint = (f64, f64, f64);

pub const NEUTRAL: Tint = (1.0, 1.0, 1.0);

const DAYLIGHT_K: f64 = 6500.0;
/// The warmest the screen gets, at strength 100.
const WARMEST_K: f64 = 3000.0;

/// The colour of a black body at `kelvin`, 0 to 255 a channel.
fn black_body(kelvin: f64) -> (f64, f64, f64) {
    let t = kelvin / 100.0;
    let red = if t <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2)
    };
    let green = if t <= 66.0 {
        99.470_802_586_1 * t.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7
    };
    (
        red.clamp(0.0, 255.0),
        green.clamp(0.0, 255.0),
        blue.clamp(0.0, 255.0),
    )
}

/// What the three channels are multiplied by at `strength`, 0 to 100.
/// Strength 0 is no tint at all.
pub fn tint(strength: u32) -> Tint {
    let strength = f64::from(strength.min(100));
    let kelvin = DAYLIGHT_K - strength * (DAYLIGHT_K - WARMEST_K) / 100.0;
    let (r, g, b) = black_body(kelvin);
    let (r0, g0, b0) = black_body(DAYLIGHT_K);
    ((r / r0).min(1.0), (g / g0).min(1.0), (b / b0).min(1.0))
}

/// The three tables of a ramp of `size` entries, as DRM takes them.
pub fn ramp(tint: Tint, size: usize) -> [Vec<u16>; 3] {
    let channel = |factor: f64| -> Vec<u16> {
        (0..size)
            .map(|i| {
                let level = if size > 1 {
                    i as f64 / (size - 1) as f64
                } else {
                    1.0
                };
                (level * factor * 65535.0).round().clamp(0.0, 65535.0) as u16
            })
            .collect()
    };
    [channel(tint.0), channel(tint.1), channel(tint.2)]
}

/// `rgb` (three bytes a pixel) as a panel with the ramp would show it.
pub fn apply(rgb: &mut [u8], tint: Tint) {
    if tint == NEUTRAL {
        return;
    }
    for pixel in rgb.as_chunks_mut::<3>().0 {
        pixel[0] = (f64::from(pixel[0]) * tint.0).round() as u8;
        pixel[1] = (f64::from(pixel[1]) * tint.1).round() as u8;
        pixel[2] = (f64::from(pixel[2]) * tint.2).round() as u8;
    }
}

const DAY: u32 = 24 * 3600;

/// Whether it is night at `now`, seconds after midnight, with the night
/// from `from` to `to`, minutes after midnight. The night may run past
/// midnight. Equal times are no night at all.
pub fn is_night(now: u32, from: u32, to: u32) -> bool {
    let (from, to) = (from * 60, to * 60);
    match from.cmp(&to) {
        std::cmp::Ordering::Equal => false,
        std::cmp::Ordering::Less => (from..to).contains(&now),
        std::cmp::Ordering::Greater => now >= from || now < to,
    }
}

/// How long from `now` to the next time the state changes.
pub fn until_change(now: u32, from: u32, to: u32) -> Duration {
    let wait = |at: u32| {
        let at = at * 60;
        let wait = (at + DAY - now % DAY) % DAY;
        if wait == 0 {
            DAY
        } else {
            wait
        }
    };
    Duration::from_secs(u64::from(wait(from).min(wait(to))))
}

/// Seconds after midnight, local time. The tests set it with
/// `SENGI_COMPOSITOR_NIGHT_NOW=HH:MM:SS`, which then runs on from there.
pub fn local_seconds() -> u32 {
    #[cfg(feature = "test-hooks")]
    {
        use std::sync::OnceLock;
        use std::time::Instant;
        static FAKE: OnceLock<Option<(u32, Instant)>> = OnceLock::new();
        let fake = FAKE.get_or_init(|| {
            let text = std::env::var("SENGI_COMPOSITOR_NIGHT_NOW").ok()?;
            let mut parts = text.split(':').map(|part| part.parse::<u32>().ok());
            let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
            Some((h * 3600 + m * 60 + s, Instant::now()))
        });
        if let Some((start, since)) = fake {
            return (start + since.elapsed().as_secs() as u32) % DAY;
        }
    }
    extern "C" {
        fn tzset();
    }
    // SAFETY: plain libc time calls with locals; localtime_r is the
    // thread-safe one. tzset makes a changed zone count.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        tzset();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec) as u32 % DAY
    }
}

impl State {
    /// Works out whether the night light is on and sets the timer for its
    /// next change. At start, on SIGHUP and after waking.
    pub fn night_update(&mut self) {
        if let Some(token) = self.night_timer.take() {
            self.loop_handle.remove(token);
        }
        let night = self.config.display.night;
        self.night_apply(&night);
        if !night.enabled || self.login_screen {
            return;
        }
        let wait = until_change(local_seconds(), night.from, night.to);
        // A second late, so that the clock is surely past the edge.
        let timer = Timer::from_duration(wait + Duration::from_secs(1));
        self.night_timer = self
            .loop_handle
            .insert_source(timer, |_, _, state| {
                let night = state.config.display.night;
                state.night_apply(&night);
                let wait = until_change(local_seconds(), night.from, night.to);
                TimeoutAction::ToDuration(wait + Duration::from_secs(1))
            })
            .ok();
    }

    /// Puts the tint that `night` and the time call for in place.
    fn night_apply(&mut self, night: &NightLight) {
        let wanted = if night.enabled
            && !self.login_screen
            && is_night(local_seconds(), night.from, night.to)
        {
            tint(night.strength)
        } else {
            NEUTRAL
        };
        if wanted == self.night_tint {
            return;
        }
        self.night_tint = wanted;
        crate::say!(
            "night light: {}",
            if wanted == NEUTRAL { "off" } else { "on" }
        );
        if let Some(set) = self.set_night_tint.as_mut() {
            set(wanted);
        }
    }

    /// One line for the SIGUSR1 report.
    pub fn night_status(&self) -> String {
        let night = self.config.display.night;
        if !night.enabled {
            return "night light: off".to_string();
        }
        let (r, g, b) = self.night_tint;
        format!(
            "night light: {} {:02}:{:02} to {:02}:{:02}, strength {}, tint {r:.3} {g:.3} {b:.3}",
            if self.night_tint == NEUTRAL {
                "waiting,"
            } else {
                "on,"
            },
            night.from / 60,
            night.from % 60,
            night.to / 60,
            night.to % 60,
            night.strength
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u32 = 3600;

    #[test]
    fn no_strength_is_no_tint_and_full_strength_is_warm() {
        assert_eq!(tint(0), NEUTRAL);
        let (r, g, b) = tint(100);
        assert_eq!(r, 1.0);
        assert!(g < 0.85 && g > 0.5, "{g}");
        assert!(b < 0.55 && b > 0.1, "{b}");
        // Stronger is never cooler.
        let (_, g50, b50) = tint(50);
        assert!(g50 > g && b50 > b);
        assert_eq!(tint(500), tint(100));
    }

    #[test]
    fn a_ramp_runs_from_black_to_the_tinted_white() {
        let [r, g, b] = ramp((1.0, 0.8, 0.5), 256);
        assert_eq!((r[0], g[0], b[0]), (0, 0, 0));
        assert_eq!(r[255], 65535);
        assert_eq!(g[255], 52428);
        assert_eq!(b[255], 32768);
        assert!(g.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(ramp(NEUTRAL, 1)[0], vec![65535]);
    }

    #[test]
    fn the_panel_model_multiplies_each_channel() {
        let mut pixels = [200, 100, 50, 255, 255, 255];
        apply(&mut pixels, (1.0, 0.5, 0.0));
        assert_eq!(pixels, [200, 50, 0, 255, 128, 0]);
    }

    #[test]
    fn a_night_across_midnight_and_one_within_a_day() {
        assert!(is_night(22 * H, 21 * 60, 7 * 60));
        assert!(is_night(3 * H, 21 * 60, 7 * 60));
        assert!(!is_night(12 * H, 21 * 60, 7 * 60));
        assert!(!is_night(7 * H, 21 * 60, 7 * 60));
        assert!(is_night(21 * H, 21 * 60, 7 * 60));
        // 01:00 to 05:00, within one day.
        assert!(is_night(2 * H, 60, 5 * 60));
        assert!(!is_night(23 * H, 60, 5 * 60));
        assert!(!is_night(3 * H, 8 * 60, 8 * 60));
    }

    #[test]
    fn the_wait_is_to_the_nearer_edge() {
        let at = |now, from, to| until_change(now, from, to).as_secs();
        assert_eq!(at(20 * H, 21 * 60, 7 * 60), 3600);
        assert_eq!(at(22 * H, 21 * 60, 7 * 60), 9 * 3600);
        assert_eq!(at(6 * H + 1800, 21 * 60, 7 * 60), 1800);
        // Exactly on an edge, the next one is a whole stretch away.
        assert_eq!(at(21 * H, 21 * 60, 7 * 60), 10 * 3600);
        assert_eq!(at(7 * H, 21 * 60, 7 * 60), 14 * 3600);
    }
}
