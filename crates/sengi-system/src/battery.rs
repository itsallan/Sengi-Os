//! The battery, as the kernel describes it under `/sys/class/power_supply`.
//!
//! [`read`] is the charge for a panel or a bar: one number, what it is
//! doing and how long is left. [`packs`] is every battery in full, for a
//! page that describes them, and [`combined`] is several taken as the
//! one supply they are to the user. A machine with two batteries gets
//! the same charge from both.

use std::fs;
use std::path::Path;

/// What the battery is doing, from its `status` file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    /// Taking charge.
    Charging,
    /// Running the machine.
    Discharging,
    /// Full.
    Full,
    /// On mains but holding its level, as with a charge threshold.
    NotCharging,
    /// The kernel does not know, or says something else.
    Unknown,
}

/// The state of one battery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    /// Charge left, 0 to 100.
    pub percent: u32,
    /// What it is doing.
    pub charge: Charge,
    /// Minutes until empty when discharging, until full when charging.
    pub minutes: Option<u32>,
}

/// The files of one battery, unparsed. Missing files are `None`.
#[derive(Debug, Default, Clone)]
pub struct Raw {
    /// `capacity`: percent.
    pub capacity: Option<String>,
    /// `status`: `Charging`, `Discharging` and so on.
    pub status: Option<String>,
    /// `energy_now`, in microwatt-hours.
    pub energy_now: Option<String>,
    /// `energy_full`, in microwatt-hours.
    pub energy_full: Option<String>,
    /// `power_now`, in microwatts.
    pub power_now: Option<String>,
    /// `charge_now`, in microampere-hours.
    pub charge_now: Option<String>,
    /// `charge_full`, in microampere-hours.
    pub charge_full: Option<String>,
    /// `current_now`, in microamperes.
    pub current_now: Option<String>,
}

/// Below these the battery is drawn in the warning and danger colours, the
/// same levels the bar uses.
pub const WARNING_PERCENT: u32 = 20;
/// At or below this, discharging, the battery is drawn as critical.
pub const CRITICAL_PERCENT: u32 = 10;

/// Estimates past a day are noise from a battery that is barely draining.
const MAX_MINUTES: u64 = 24 * 60;

/// More batteries than any laptop has.
pub const MAX_BATTERIES: usize = 8;

/// One battery in full. Energy is in watt-hours and power in watts,
/// whichever of energy and charge the battery itself counts in.
#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    /// What the kernel calls it, such as `BAT0`. Empty for several
    /// taken together.
    pub name: String,
    /// Charge left, 0 to 100.
    pub percent: u32,
    /// What it is doing.
    pub charge: Charge,
    /// What it holds now when full.
    pub full_wh: Option<f64>,
    /// What it held when new.
    pub design_wh: Option<f64>,
    /// What it holds now.
    pub now_wh: Option<f64>,
    /// The rate it is charging or discharging at.
    pub watts: Option<f64>,
    /// Charge cycles so far, if it counts them.
    pub cycles: Option<u32>,
    /// Its chemistry, maker and model, cleaned for display.
    pub kind: String,
}

impl Pack {
    /// What is left of the capacity it had when new, in percent.
    pub fn health(&self) -> Option<u32> {
        let (full, design) = (self.full_wh?, self.design_wh?);
        (design > 0.0).then(|| ((full / design) * 100.0).round().clamp(0.0, 100.0) as u32)
    }

    /// Minutes until empty, or until full.
    pub fn minutes_left(&self) -> Option<u32> {
        let (now, watts) = (self.now_wh?, self.watts.filter(|w| *w > 0.5)?);
        let hours = match self.charge {
            Charge::Discharging => now / watts,
            Charge::Charging => (self.full_wh? - now).max(0.0) / watts,
            _ => return None,
        };
        (hours.is_finite() && hours < 100.0).then(|| (hours * 60.0).round() as u32)
    }
}

fn charge_of(status: &str) -> Charge {
    match status.trim() {
        "Charging" => Charge::Charging,
        "Discharging" => Charge::Discharging,
        "Full" => Charge::Full,
        "Not charging" => Charge::NotCharging,
        _ => Charge::Unknown,
    }
}

/// Reads one battery's directory. Energy is in µWh; where the battery
/// counts charge instead (µAh), the design voltage turns it into energy.
fn read_pack(dir: &Path) -> Option<Pack> {
    let file = |name: &str| fs::read_to_string(dir.join(name)).ok();
    let number = |name: &str| -> Option<f64> {
        let value: f64 = file(name)?.trim().parse().ok()?;
        (value.is_finite() && (0.0..1e12).contains(&value)).then_some(value)
    };
    if file("type")?.trim() != "Battery" || file("present").is_some_and(|p| p.trim() == "0") {
        return None;
    }
    // A mouse's or a headset's battery also shows up here.
    if file("scope").is_some_and(|scope| scope.trim() == "Device") {
        return None;
    }
    let volts = number("voltage_min_design")
        .or(number("voltage_now"))
        .map(|microvolts| microvolts / 1e6);
    let energy = |energy: &str, charge: &str| -> Option<f64> {
        number(energy)
            .map(|microwatt_hours| microwatt_hours / 1e6)
            .or_else(|| Some(number(charge)? / 1e6 * volts?))
    };
    let watts = number("power_now")
        .map(|microwatts| microwatts / 1e6)
        .or_else(|| Some(number("current_now")? / 1e6 * volts?));
    let (now_wh, full_wh) = (
        energy("energy_now", "charge_now"),
        energy("energy_full", "charge_full"),
    );
    let percent = number("capacity").or_else(|| {
        let (now, full) = (now_wh?, full_wh?);
        (full > 0.0).then(|| now / full * 100.0)
    })?;
    let words: Vec<String> = ["technology", "manufacturer", "model_name"]
        .iter()
        .filter_map(|name| file(name))
        .map(|raw| sengi_text::line(&raw, 24))
        .filter(|word| !word.is_empty() && word != "Unknown")
        .collect();
    let name = dir.file_name().map(|name| name.to_string_lossy());
    Some(Pack {
        name: sengi_text::line(&name.unwrap_or_default(), 24),
        percent: percent.round().clamp(0.0, 100.0) as u32,
        charge: charge_of(&file("status").unwrap_or_default()),
        full_wh,
        design_wh: energy("energy_full_design", "charge_full_design"),
        now_wh,
        watts,
        cycles: number("cycle_count")
            .filter(|cycles| *cycles > 0.0)
            .map(|cycles| cycles as u32),
        kind: words.join(", "),
    })
}

/// The system's batteries under `root`, by name. Some laptops have two.
pub fn packs(root: &Path) -> Vec<Pack> {
    let mut entries: Vec<_> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .take(32)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    entries
        .iter()
        .filter_map(|entry| read_pack(&entry.path()))
        .take(MAX_BATTERIES)
        .collect()
}

/// Several batteries as the one supply they are to the user: energies
/// and power added up, and the charge worked out from the energy, so
/// that a large battery counts for more than a small one. One battery is
/// itself.
pub fn combined(packs: &[Pack]) -> Option<Pack> {
    let (first, rest) = packs.split_first()?;
    if rest.is_empty() {
        return Some(first.clone());
    }
    // A sum only means something if every battery gave its part.
    let total = |part: fn(&Pack) -> Option<f64>| -> Option<f64> { packs.iter().map(part).sum() };
    let (now_wh, full_wh) = (total(|b| b.now_wh), total(|b| b.full_wh));
    let percent = match (now_wh, full_wh) {
        (Some(now), Some(full)) if full > 0.0 => now / full * 100.0,
        _ => packs.iter().map(|b| f64::from(b.percent)).sum::<f64>() / packs.len() as f64,
    };
    let any = |charge: Charge| packs.iter().any(|b| b.charge == charge);
    let charge = if any(Charge::Charging) {
        Charge::Charging
    } else if any(Charge::Discharging) {
        Charge::Discharging
    } else if packs.iter().all(|b| b.charge == Charge::Full) {
        Charge::Full
    } else if any(Charge::NotCharging) || any(Charge::Full) {
        Charge::NotCharging
    } else {
        Charge::Unknown
    };
    let rates: Vec<f64> = packs.iter().filter_map(|b| b.watts).collect();
    Some(Pack {
        name: String::new(),
        percent: percent.round().clamp(0.0, 100.0) as u32,
        charge,
        full_wh,
        design_wh: total(|b| b.design_wh),
        now_wh,
        watts: (!rates.is_empty()).then(|| rates.iter().sum()),
        cycles: None,
        kind: String::new(),
    })
}

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

/// Makes sense of one battery's files. `None` without a usable
/// `capacity`.
pub fn parse(raw: &Raw) -> Option<Battery> {
    let percent = raw
        .capacity
        .as_deref()?
        .trim()
        .parse::<u32>()
        .ok()?
        .min(100);
    let charge = charge_of(raw.status.as_deref().unwrap_or_default());
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
    /// Whether it is taking charge.
    pub fn charging(&self) -> bool {
        self.charge == Charge::Charging
    }

    /// Discharging and at or below [`CRITICAL_PERCENT`].
    pub fn low(&self) -> bool {
        self.charge == Charge::Discharging && self.percent <= CRITICAL_PERCENT
    }

    /// Discharging and at or below [`WARNING_PERCENT`], but not yet
    /// [`low`](Battery::low).
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

/// The system battery under `root`. Batteries inside mice and headsets
/// show up in the same directory and are skipped. Where the machine has
/// several of its own they are read as one: see [`combined`].
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
    if paths.len() > 1 {
        if let Some(whole) = combined(&packs(root)) {
            return Some(Battery {
                percent: whole.percent,
                charge: whole.charge,
                minutes: whole
                    .minutes_left()
                    .filter(|minutes| (1..=MAX_MINUTES).contains(&u64::from(*minutes))),
            });
        }
    }
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

    fn laptop(tmp: &TempDir, status: &str) {
        for (file, value) in [
            ("type", "Battery"),
            ("present", "1"),
            ("status", status),
            ("capacity", "58"),
            ("energy_now", "24070000"),
            ("energy_full", "41500000"),
            ("energy_full_design", "48000000"),
            ("power_now", "9500000"),
            ("cycle_count", "312"),
            ("technology", "Li-ion"),
            ("manufacturer", "ASUSTeK"),
            ("model_name", "ASUS Battery\u{202e}\n"),
        ] {
            tmp.write(&format!("BAT0/{file}"), &format!("{value}\n"));
        }
        tmp.write("AC0/type", "Mains\n");
        tmp.write("hidpp_battery_0/type", "Battery\n");
        tmp.write("hidpp_battery_0/scope", "Device\n");
        tmp.write("hidpp_battery_0/capacity", "90\n");
    }

    fn whole(root: &Path) -> Option<Pack> {
        combined(&packs(root))
    }

    #[test]
    fn a_laptop_battery_is_read_in_full() {
        let tmp = TempDir::new();
        laptop(&tmp, "Discharging");
        let all = packs(tmp.path());
        assert_eq!(all.len(), 1);
        let pack = &all[0];
        assert_eq!(pack.name, "BAT0");
        assert_eq!(pack.percent, 58);
        assert_eq!(pack.charge, Charge::Discharging);
        assert_eq!(pack.health(), Some(86));
        assert_eq!(pack.cycles, Some(312));
        assert_eq!(pack.kind, "Li-ion, ASUSTeK, ASUS Battery");
        // 24.07 Wh at 9.5 W.
        assert_eq!(pack.minutes_left(), Some(152));
        // One battery taken together is itself.
        assert_eq!(whole(tmp.path()).as_ref(), Some(pack));
        // And the panel's reading of it is the plain one, unchanged.
        assert_eq!(
            read(tmp.path()),
            Some(Battery {
                percent: 58,
                charge: Charge::Discharging,
                minutes: Some(152),
            })
        );
    }

    fn two(tmp: &TempDir) {
        // A small one nearly full and a large one nearly empty.
        for (name, status, now, full, design, power, cycles) in [
            (
                "BAT0", "Unknown", "18000000", "20000000", "24000000", "0", "410",
            ),
            (
                "BAT1",
                "Discharging",
                "12000000",
                "60000000",
                "72000000",
                "12000000",
                "95",
            ),
        ] {
            for (file, value) in [
                ("type", "Battery"),
                ("status", status),
                ("energy_now", now),
                ("energy_full", full),
                ("energy_full_design", design),
                ("power_now", power),
                ("cycle_count", cycles),
            ] {
                tmp.write(&format!("{name}/{file}"), value);
            }
        }
    }

    #[test]
    fn two_batteries_are_one_charge_weighted_by_what_each_holds() {
        let tmp = TempDir::new();
        two(&tmp);
        let both = packs(tmp.path());
        assert_eq!(both.len(), 2);
        assert_eq!((both[0].percent, both[1].percent), (90, 20));
        let whole = whole(tmp.path()).unwrap();
        // 30 of 80 Wh, not the 55% that averaging the two would give.
        assert_eq!(whole.percent, 38);
        assert_eq!(whole.charge, Charge::Discharging);
        assert_eq!(whole.now_wh, Some(30.0));
        assert_eq!(whole.watts, Some(12.0));
        assert_eq!(whole.minutes_left(), Some(150));
        assert!(whole.name.is_empty() && whole.cycles.is_none());
        // The panel shows the same charge the page does.
        assert_eq!(
            read(tmp.path()),
            Some(Battery {
                percent: 38,
                charge: Charge::Discharging,
                minutes: Some(150),
            })
        );
    }

    #[test]
    fn batteries_that_do_not_give_their_energy_are_averaged() {
        let full = Pack {
            name: "BAT0".to_string(),
            percent: 100,
            charge: Charge::Full,
            full_wh: None,
            design_wh: None,
            now_wh: None,
            watts: None,
            cycles: None,
            kind: String::new(),
        };
        let half = Pack {
            name: "BAT1".to_string(),
            percent: 50,
            charge: Charge::Charging,
            now_wh: Some(10.0),
            full_wh: Some(20.0),
            ..full.clone()
        };
        let whole = combined(&[full.clone(), half]).unwrap();
        assert_eq!(whole.percent, 75);
        assert_eq!(whole.charge, Charge::Charging);
        assert_eq!(
            (whole.now_wh, whole.full_wh, whole.watts),
            (None, None, None)
        );
        assert_eq!(
            combined(&[full.clone(), full.clone()]).unwrap().charge,
            Charge::Full
        );
        assert_eq!(combined(&[]), None);
        assert_eq!(combined(std::slice::from_ref(&full)), Some(full));
    }

    #[test]
    fn charging_counts_up_to_full_and_full_counts_nothing() {
        let tmp = TempDir::new();
        laptop(&tmp, "Charging");
        assert_eq!(whole(tmp.path()).unwrap().minutes_left(), Some(110));
        laptop(&tmp, "Full");
        assert_eq!(whole(tmp.path()).unwrap().minutes_left(), None);
        laptop(&tmp, "Not charging");
        assert_eq!(whole(tmp.path()).unwrap().charge, Charge::NotCharging);
    }

    #[test]
    fn a_battery_that_counts_charge_is_converted_with_its_voltage() {
        let tmp = TempDir::new();
        for (file, value) in [
            ("type", "Battery"),
            ("status", "Discharging"),
            ("charge_now", "2000000"),
            ("charge_full", "4000000"),
            ("charge_full_design", "5000000"),
            ("current_now", "1000000"),
            ("voltage_min_design", "11000000"),
        ] {
            tmp.write(&format!("BAT1/{file}"), value);
        }
        let pack = whole(tmp.path()).unwrap();
        assert_eq!(pack.percent, 50);
        assert_eq!(pack.health(), Some(80));
        assert_eq!(pack.full_wh, Some(44.0));
        assert_eq!(pack.minutes_left(), Some(120));
        assert_eq!(pack.cycles, None);
    }

    #[test]
    fn an_absent_battery_and_nonsense_are_handled() {
        let tmp = TempDir::new();
        assert_eq!(whole(tmp.path()), None);
        assert_eq!(whole(&tmp.path().join("missing")), None);
        tmp.write("AC0/type", "Mains\n");
        tmp.write("BAT0/type", "Battery\n");
        tmp.write("BAT0/present", "0\n");
        assert_eq!(whole(tmp.path()), None);
        tmp.write("BAT0/present", "1\n");
        // Present but says nothing usable.
        assert_eq!(whole(tmp.path()), None);
        tmp.write("BAT0/capacity", "250\n");
        tmp.write("BAT0/energy_full", "-5\n");
        tmp.write("BAT0/energy_full_design", "0\n");
        tmp.write("BAT0/power_now", "NaN\n");
        tmp.write("BAT0/status", "\u{0}\u{1b}[31m\n");
        let odd = whole(tmp.path()).unwrap();
        assert_eq!(odd.percent, 100);
        assert_eq!(odd.charge, Charge::Unknown);
        assert_eq!(odd.health(), None);
        assert_eq!(odd.minutes_left(), None);
    }
}
