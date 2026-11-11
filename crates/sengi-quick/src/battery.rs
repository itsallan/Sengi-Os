//! The battery, as the kernel describes it under `/sys/class/power_supply`.

use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    Full,
    /// On mains but holding its level, as with a charge threshold.
    NotCharging,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    pub percent: u32,
    pub charge: Charge,
    /// Minutes until empty when discharging, until full when charging.
    pub minutes: Option<u32>,
}

/// The files of one battery, unparsed. Missing files are `None`.
#[derive(Debug, Default, Clone)]
pub struct Raw {
    pub capacity: Option<String>,
    pub status: Option<String>,
    pub energy_now: Option<String>,
    pub energy_full: Option<String>,
    pub power_now: Option<String>,
    pub charge_now: Option<String>,
    pub charge_full: Option<String>,
    pub current_now: Option<String>,
}

/// Below these the battery is drawn in the warning and danger colours, the
/// same levels the bar uses.
pub const WARNING_PERCENT: u32 = 20;
pub const CRITICAL_PERCENT: u32 = 10;

/// Estimates past a day are noise from a battery that is barely draining.
const MAX_MINUTES: u64 = 24 * 60;

fn number(text: &Option<String>) -> Option<u64> {
    // Some drivers report the current as negative while discharging.
    let value: i64 = text.as_deref()?.trim().parse().ok()?;
    Some(value.unsigned_abs())
}

/// Minutes left at the present rate. `now` and `full` are in the same unit
/// (µWh or µAh) and `rate` is that unit per hour.
pub fn minutes_left(charge: Charge, now: u64, full: Option<u64>, rate: u64) -> Option<u32> {
    if rate == 0 {
        return None;
    }
    let amount = match charge {
        Charge::Discharging => now,
        Charge::Charging => full?.saturating_sub(now),
        _ => return None,
    };
    let minutes = amount.saturating_mul(60) / rate;
    (minutes > 0 && minutes <= MAX_MINUTES).then_some(minutes as u32)
}

pub fn parse(raw: &Raw) -> Option<Battery> {
    let percent = raw
        .capacity
        .as_deref()?
        .trim()
        .parse::<u32>()
        .ok()?
        .min(100);
    let charge = match raw.status.as_deref().map(str::trim) {
        Some("Charging") => Charge::Charging,
        Some("Discharging") => Charge::Discharging,
        Some("Full") => Charge::Full,
        Some("Not charging") => Charge::NotCharging,
        _ => Charge::Unknown,
    };
    // Energy when the kernel has it, charge otherwise; never a mix.
    let by_energy = || {
        let (now, rate) = (number(&raw.energy_now)?, number(&raw.power_now)?);
        minutes_left(charge, now, number(&raw.energy_full), rate)
    };
    let by_charge = || {
        let (now, rate) = (number(&raw.charge_now)?, number(&raw.current_now)?);
        minutes_left(charge, now, number(&raw.charge_full), rate)
    };
    Some(Battery {
        percent,
        charge,
        minutes: by_energy().or_else(by_charge),
    })
}

impl Battery {
    pub fn charging(&self) -> bool {
        self.charge == Charge::Charging
    }

    pub fn low(&self) -> bool {
        self.charge == Charge::Discharging && self.percent <= CRITICAL_PERCENT
    }

    pub fn warning(&self) -> bool {
        self.charge == Charge::Discharging && self.percent <= WARNING_PERCENT && !self.low()
    }

    /// The line under the percentage.
    pub fn detail(&self) -> String {
        let time = self.minutes.map(duration);
        match (self.charge, time) {
            (Charge::Charging, Some(time)) => format!("Charging, {time} until full"),
            (Charge::Charging, None) => "Charging".to_string(),
            (Charge::Discharging, Some(time)) => format!("{time} remaining"),
            (Charge::Discharging, None) => "On battery".to_string(),
            (Charge::Full, _) => "Fully charged".to_string(),
            (Charge::NotCharging, _) => "Plugged in, not charging".to_string(),
            (Charge::Unknown, _) => String::new(),
        }
    }
}

/// "3 h 05 min", "42 min".
pub fn duration(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} min"),
        (hours, minutes) => format!("{hours} h {minutes:02} min"),
    }
}

fn read_trimmed(path: &Path) -> Option<String> {
    // Sysfs attributes are a line; anything long is not what was expected.
    let text = fs::read_to_string(path).ok()?;
    (text.len() <= 64).then(|| text.trim().to_string())
}

/// The first system battery under `root`. Batteries inside mice and
/// headsets show up in the same directory and are skipped.
pub fn read(root: &Path) -> Option<Battery> {
    let mut paths: Vec<_> = fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            read_trimmed(&path.join("type")).as_deref() == Some("Battery")
                && read_trimmed(&path.join("scope")).as_deref() != Some("Device")
        })
        .collect();
    paths.sort();
    let path = paths.first()?;
    let file = |name: &str| read_trimmed(&path.join(name));
    parse(&Raw {
        capacity: file("capacity"),
        status: file("status"),
        energy_now: file("energy_now"),
        energy_full: file("energy_full"),
        power_now: file("power_now"),
        charge_now: file("charge_now"),
        charge_full: file("charge_full"),
        current_now: file("current_now"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn raw(capacity: &str, status: &str) -> Raw {
        Raw {
            capacity: Some(capacity.to_string()),
            status: Some(status.to_string()),
            ..Raw::default()
        }
    }

    fn some(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[test]
    fn percent_and_state() {
        let battery = parse(&raw("87\n", "Discharging\n")).unwrap();
        assert_eq!(battery.percent, 87);
        assert_eq!(battery.charge, Charge::Discharging);
        assert_eq!(battery.minutes, None);
        assert_eq!(battery.detail(), "On battery");
        assert_eq!(
            parse(&raw("100", "Full")).unwrap().detail(),
            "Fully charged"
        );
        assert_eq!(
            parse(&raw("80", "Not charging")).unwrap().detail(),
            "Plugged in, not charging"
        );
        assert_eq!(parse(&raw("50", "Weird")).unwrap().charge, Charge::Unknown);
    }

    #[test]
    fn odd_readings() {
        assert_eq!(parse(&raw("", "Full")), None);
        assert_eq!(parse(&raw("lots", "Full")), None);
        assert_eq!(parse(&Raw::default()), None);
        assert_eq!(parse(&raw("250", "Full")).unwrap().percent, 100);
    }

    #[test]
    fn time_remaining_from_energy() {
        let battery = parse(&Raw {
            energy_now: some("30000000"),
            energy_full: some("50000000"),
            power_now: some("10000000"),
            ..raw("60", "Discharging")
        })
        .unwrap();
        assert_eq!(battery.minutes, Some(180));
        assert_eq!(battery.detail(), "3 h 00 min remaining");
    }

    #[test]
    fn time_until_full_from_charge() {
        let battery = parse(&Raw {
            charge_now: some("3000000"),
            charge_full: some("4000000"),
            current_now: some("1500000"),
            ..raw("75", "Charging")
        })
        .unwrap();
        assert_eq!(battery.minutes, Some(40));
        assert_eq!(battery.detail(), "Charging, 40 min until full");
    }

    #[test]
    fn a_negative_current_is_a_rate_all_the_same() {
        let battery = parse(&Raw {
            charge_now: some("2000000"),
            current_now: some("-1000000"),
            ..raw("50", "Discharging")
        })
        .unwrap();
        assert_eq!(battery.minutes, Some(120));
    }

    #[test]
    fn no_estimate_without_a_usable_rate() {
        use Charge::*;
        assert_eq!(minutes_left(Discharging, 1000, None, 0), None);
        // Charging with no idea of what full is.
        assert_eq!(minutes_left(Charging, 1000, None, 500), None);
        // Fuller than full.
        assert_eq!(minutes_left(Charging, 1000, Some(900), 500), None);
        assert_eq!(minutes_left(Full, 1000, Some(1000), 500), None);
        // Days of battery from a trickle of a reading.
        assert_eq!(minutes_left(Discharging, 50_000_000, None, 1000), None);
        assert_eq!(minutes_left(Discharging, u64::MAX, None, 1), None);
        assert_eq!(minutes_left(Discharging, 1000, None, 1000), Some(60));
    }

    #[test]
    fn low_and_warning_only_while_draining() {
        assert!(parse(&raw("8", "Discharging")).unwrap().low());
        assert!(!parse(&raw("8", "Charging")).unwrap().low());
        let battery = parse(&raw("18", "Discharging")).unwrap();
        assert!(battery.warning() && !battery.low());
        assert!(!parse(&raw("60", "Discharging")).unwrap().warning());
    }

    #[test]
    fn durations() {
        assert_eq!(duration(5), "5 min");
        assert_eq!(duration(60), "1 h 00 min");
        assert_eq!(duration(185), "3 h 05 min");
    }

    #[test]
    fn the_system_battery_is_found_among_other_supplies() {
        let sys = TempDir::new();
        sys.write("AC0/type", "Mains\n");
        sys.write("hidpp_battery_0/type", "Battery\n");
        sys.write("hidpp_battery_0/scope", "Device\n");
        sys.write("hidpp_battery_0/capacity", "12\n");
        sys.write("BAT1/type", "Battery\n");
        sys.write("BAT1/capacity", "64\n");
        sys.write("BAT1/status", "Charging\n");
        sys.write("BAT1/energy_now", "32000000\n");
        sys.write("BAT1/energy_full", "50000000\n");
        sys.write("BAT1/power_now", "18000000\n");
        assert_eq!(
            read(sys.path()),
            Some(Battery {
                percent: 64,
                charge: Charge::Charging,
                minutes: Some(60),
            })
        );
    }

    #[test]
    fn no_battery() {
        let sys = TempDir::new();
        sys.write("AC0/type", "Mains\n");
        assert_eq!(read(sys.path()), None);
        assert_eq!(read(Path::new("/nonexistent/sengi-quick")), None);
    }
}
