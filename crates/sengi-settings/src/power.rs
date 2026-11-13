//! The Power page: what the kernel says about the battery, when the
//! screen switches off and locks, and what the power button and the lid
//! do.
//!
//! The idle times are settings of ours (`power.*` in `settings.toml`).
//! The session's `swayidle` is started with them by the autostart script,
//! which asks `sengi-settings --get`; a running `swayidle` cannot be told
//! new times, so after a change the one the session marked as restartable
//! is ended and the session starts it again with the new ones.
//!
//! The power button and the lid belong to logind and are the same for
//! every user. Changing them means writing `/etc/systemd/logind.conf`,
//! which takes root; they are shown, with where to change them.

use std::fs;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Timer, TimerMode};

use crate::app::{self, App, Choice, Controller};
use crate::bus::{self, Bus};
use crate::store::Value;
use crate::text;
use crate::ui::{InfoRow, PowerData};

/// The battery is read again this often, and only while the page shows.
const BATTERY_PERIOD: Duration = Duration::from_secs(30);
/// More batteries than any laptop has.
pub const MAX_BATTERIES: usize = 8;

/// The idle times offered, in seconds. Zero is never.
pub const IDLE_CHOICES: [i64; 8] = [60, 120, 300, 600, 900, 1800, 3600, 0];

/// In the environment of a `swayidle` the session will start again if it
/// exits. Only one that carries it is ever signalled.
pub const SUPERVISED: &str = "SENGI_IDLE_SUPERVISED=1";

pub fn idle_words(seconds: i64) -> String {
    match seconds {
        0 => "Never".to_string(),
        60 => "After 1 minute".to_string(),
        3600 => "After 1 hour".to_string(),
        s if s % 3600 == 0 => format!("After {} hours", s / 3600),
        s if s % 60 == 0 => format!("After {} minutes", s / 60),
        s => format!("After {s} seconds"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    Full,
    /// Plugged in and holding, as at a charge limit.
    Holding,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Battery {
    /// What the kernel calls it, such as `BAT0`. Empty for several
    /// taken together.
    pub name: String,
    pub percent: u32,
    pub charge: Charge,
    /// Watt-hours it holds now when full, and held when new.
    pub full_wh: Option<f64>,
    pub design_wh: Option<f64>,
    pub now_wh: Option<f64>,
    /// The rate it is charging or discharging at.
    pub watts: Option<f64>,
    pub cycles: Option<u32>,
    /// Cleaned for display.
    pub kind: String,
}

impl Battery {
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

    pub fn summary(&self) -> String {
        let state = match self.charge {
            Charge::Charging => "charging",
            Charge::Discharging => "on battery",
            Charge::Full => "full",
            Charge::Holding => "plugged in, not charging",
            Charge::Unknown => "state unknown",
        };
        let left = self.minutes_left().map(|minutes| {
            let (h, m) = (minutes / 60, minutes % 60);
            let time = if h == 0 {
                format!("{m} min")
            } else {
                format!("{h} h {m:02} min")
            };
            match self.charge {
                Charge::Charging => format!(", about {time} until full"),
                _ => format!(", about {time} left"),
            }
        });
        format!("{}%, {state}{}", self.percent, left.unwrap_or_default())
    }
}

/// Reads one battery's directory. Energy is in µWh; where the battery
/// counts charge instead (µAh), the design voltage turns it into energy.
fn read_battery(dir: &Path) -> Option<Battery> {
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
        .map(|raw| text::clean(&raw, 24))
        .filter(|word| !word.is_empty() && word != "Unknown")
        .collect();
    let name = dir.file_name().map(|name| name.to_string_lossy());
    Some(Battery {
        name: text::clean(&name.unwrap_or_default(), 24),
        percent: percent.round().clamp(0.0, 100.0) as u32,
        charge: match file("status").unwrap_or_default().trim() {
            "Charging" => Charge::Charging,
            "Discharging" => Charge::Discharging,
            "Full" => Charge::Full,
            "Not charging" => Charge::Holding,
            _ => Charge::Unknown,
        },
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

/// The system's batteries, by name. Some laptops have two.
pub fn batteries(power_supply: &Path) -> Vec<Battery> {
    let mut entries: Vec<_> = fs::read_dir(power_supply)
        .into_iter()
        .flatten()
        .flatten()
        .take(32)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    entries
        .iter()
        .filter_map(|entry| read_battery(&entry.path()))
        .take(MAX_BATTERIES)
        .collect()
}

/// Several batteries as the one supply they are to the user: energies
/// and power added up, and the charge worked out from the energy, so
/// that a large battery counts for more than a small one.
pub fn combined(batteries: &[Battery]) -> Option<Battery> {
    let (first, rest) = batteries.split_first()?;
    if rest.is_empty() {
        return Some(first.clone());
    }
    // A sum only means something if every battery gave its part.
    let total =
        |part: fn(&Battery) -> Option<f64>| -> Option<f64> { batteries.iter().map(part).sum() };
    let (now_wh, full_wh) = (total(|b| b.now_wh), total(|b| b.full_wh));
    let percent = match (now_wh, full_wh) {
        (Some(now), Some(full)) if full > 0.0 => now / full * 100.0,
        _ => batteries.iter().map(|b| f64::from(b.percent)).sum::<f64>() / batteries.len() as f64,
    };
    let any = |charge: Charge| batteries.iter().any(|b| b.charge == charge);
    let charge = if any(Charge::Charging) {
        Charge::Charging
    } else if any(Charge::Discharging) {
        Charge::Discharging
    } else if batteries.iter().all(|b| b.charge == Charge::Full) {
        Charge::Full
    } else if any(Charge::Holding) || any(Charge::Full) {
        Charge::Holding
    } else {
        Charge::Unknown
    };
    let rates: Vec<f64> = batteries.iter().filter_map(|b| b.watts).collect();
    Some(Battery {
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

/// The system's battery, or all of them taken together.
pub fn battery(power_supply: &Path) -> Option<Battery> {
    combined(&batteries(power_supply))
}

/// What logind does for a key or the lid, in words.
pub fn action_words(action: &str) -> &'static str {
    match action {
        "ignore" => "Nothing",
        "poweroff" => "Shut down",
        "reboot" => "Restart",
        "halt" => "Halt",
        "suspend" => "Sleep",
        "hibernate" => "Hibernate",
        "hybrid-sleep" | "suspend-then-hibernate" => "Sleep, then hibernate",
        "lock" => "Lock the screen",
        "kexec" => "Restart",
        _ => "Unknown",
    }
}

/// logind's settings for the power button and the lid, if it will say.
fn logind_actions(bus: &Bus) -> Option<(String, String)> {
    let props = bus.properties(
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )?;
    Some((
        bus::text(&props, "HandlePowerKey")?.to_string(),
        bus::text(&props, "HandleLidSwitch")?.to_string(),
    ))
}

/// Whether the process whose `/proc` directory is `dir` is a `swayidle`
/// of this user and this compositor that the session has said it will
/// start again.
fn restartable(dir: &Path, uid: u32, wayland_display: &str) -> bool {
    use std::os::unix::fs::MetadataExt;
    let wanted = format!("WAYLAND_DISPLAY={wayland_display}");
    let is = || -> Option<bool> {
        if fs::metadata(dir).ok()?.uid() != uid
            || fs::read_to_string(dir.join("comm")).ok()?.trim() != "swayidle"
        {
            return Some(false);
        }
        let environment = fs::read(dir.join("environ")).ok()?;
        let has = |wanted: &str| {
            environment
                .split(|byte| *byte == 0)
                .any(|entry| entry == wanted.as_bytes())
        };
        Some(has(SUPERVISED) && has(&wanted))
    };
    is().unwrap_or(false)
}

/// The process ids, under `proc`, of this session's `swayidle` processes
/// that the session has said it will start again: same user, same
/// compositor, and the mark in their environment.
pub fn restartable_idlers(proc: &Path, uid: u32, wayland_display: &str) -> Vec<i32> {
    fs::read_dir(proc)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            (pid > 1 && restartable(&entry.path(), uid, wayland_display)).then_some(pid)
        })
        .collect()
}

/// Send SIGTERM to process `pid` if `still` says it is the one meant.
///
/// A process id found in `/proc` a moment ago may by now belong to some
/// other process. So the process is first taken hold of with a pidfd,
/// which names that one process for as long as it is open, and looked at
/// again only then. If the look passes, the pidfd names what was looked
/// at; had the number changed hands in between, the look would be at a
/// stranger and fail, or the pidfd would name a process already dead, to
/// which a signal does nothing.
fn end_if(pid: i32, still: impl FnOnce() -> bool) -> bool {
    // SAFETY: pidfd_open takes a process id and flags and returns a new
    // descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return false;
    }
    // SAFETY: the descriptor was just returned to us and is ours alone.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) };
    // SAFETY: pidfd_send_signal is given the pidfd, a signal number, no
    // siginfo and no flags.
    still()
        && unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                libc::SIGTERM,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } == 0
}

/// End the restartable `swayidle` of this session so that it comes back
/// with the new times. True if there was one.
fn restart_idler() -> bool {
    let Ok(display) = std::env::var("WAYLAND_DISPLAY") else {
        return false;
    };
    // SAFETY: getuid has no arguments and cannot fail.
    let uid = unsafe { libc::getuid() };
    let proc = Path::new("/proc");
    let mut ended = false;
    for pid in restartable_idlers(proc, uid, &display) {
        ended |= end_if(pid, || {
            restartable(&proc.join(pid.to_string()), uid, &display)
        });
    }
    ended
}

struct Facts {
    batteries: Vec<Battery>,
    actions: Option<(String, String)>,
}

pub struct PowerPage {
    bus: std::sync::Arc<Bus>,
    class: std::path::PathBuf,
    clock: Timer,
}

fn row(title: &str, value: &str) -> InfoRow {
    InfoRow {
        title: title.into(),
        value: value.into(),
    }
}

fn health_words(battery: &Battery) -> Option<String> {
    let (health, full, design) = (battery.health()?, battery.full_wh?, battery.design_wh?);
    Some(format!(
        "{health}% of its original capacity ({full:.1} of {design:.1} Wh)"
    ))
}

/// The card's rows. One battery is described in full. Several are added
/// up for the charge and the power, and then each has a row for what is
/// its own: how full it is and how worn.
fn battery_rows(batteries: &[Battery]) -> Vec<InfoRow> {
    let Some(whole) = combined(batteries) else {
        return Vec::new();
    };
    let mut rows = vec![row("Charge", &whole.summary())];
    if let [battery] = batteries {
        if let Some(health) = health_words(battery) {
            rows.push(row("Health", &health));
        }
    }
    if let Some(watts) = whole.watts.filter(|watts| *watts > 0.05) {
        rows.push(row("Power", &format!("{watts:.1} W")));
    }
    match batteries {
        [battery] => {
            if let Some(cycles) = battery.cycles {
                rows.push(row("Charge cycles", &cycles.to_string()));
            }
            if !battery.kind.is_empty() {
                rows.push(row("Type", &battery.kind));
            }
        }
        several => {
            for (index, battery) in several.iter().enumerate() {
                let mut words = vec![format!("{}%", battery.percent)];
                words.extend(health_words(battery).map(|health| format!("holds {health}")));
                words.extend(
                    battery
                        .cycles
                        .map(|cycles| format!("{cycles} charge cycles")),
                );
                let title = if battery.name.is_empty() {
                    format!("Battery {}", index + 1)
                } else {
                    format!("Battery {} ({})", index + 1, battery.name)
                };
                rows.push(row(&title, &words.join(", ")));
            }
        }
    }
    rows
}

impl PowerPage {
    pub fn attach(
        app: &Rc<App>,
        bus: std::sync::Arc<Bus>,
        class: std::path::PathBuf,
    ) -> Rc<PowerPage> {
        let page = Rc::new(PowerPage {
            bus,
            class,
            clock: Timer::default(),
        });
        let data = app.window.global::<PowerData>();
        let weak_app = Rc::downgrade(app);
        data.on_choose(move |key| {
            let Some(app) = weak_app.upgrade() else {
                return;
            };
            let key = key.to_string();
            let now = app.settings.borrow().number("power", &key);
            let choices = IDLE_CHOICES
                .iter()
                .map(|seconds| Choice {
                    id: seconds.to_string(),
                    title: idle_words(*seconds),
                    detail: String::new(),
                    current: *seconds == now,
                })
                .collect();
            let title = if key == "blank-after" {
                "Switch the screen off"
            } else {
                "Lock the screen"
            };
            app.choose(title, false, choices, move |app, id| {
                if let Ok(seconds) = id.parse() {
                    if app.set("power", &key, Value::Number(seconds)) {
                        PowerPage::settings_changed(app);
                    }
                }
            });
        });
        let weak_app = Rc::downgrade(app);
        data.on_set_lock_on_sleep(move |on| {
            if let Some(app) = weak_app.upgrade() {
                if app.set("power", "lock-on-sleep", Value::Flag(on)) {
                    PowerPage::settings_changed(&app);
                }
            }
        });
        PowerPage::render_settings(app, None);
        page
    }

    fn settings_changed(app: &Rc<App>) {
        PowerPage::render_settings(app, None);
        // The session's swayidle reads the settings file as it starts,
        // so it is ended only after the file has been written.
        app.after_saved(|| {
            let restarted = restart_idler();
            app::post(move |app| PowerPage::render_settings(app, Some(restarted)));
        });
    }

    /// `applied` is whether the last change reached the running session,
    /// when one has just been made.
    fn render_settings(app: &Rc<App>, applied: Option<bool>) {
        let data = app.window.global::<PowerData>();
        let settings = app.settings.borrow();
        data.set_blank(idle_words(settings.number("power", "blank-after")).into());
        data.set_lock(idle_words(settings.number("power", "lock-after")).into());
        data.set_lock_on_sleep(settings.flag("power", "lock-on-sleep"));
        data.set_idle_note(
            match applied {
                Some(false) => "The new times take effect the next time you log in.",
                _ => "",
            }
            .into(),
        );
    }

    fn refresh(&self) {
        let (bus, class) = (self.bus.clone(), self.class.clone());
        let spawned = std::thread::Builder::new()
            .name("settings-power".to_string())
            .spawn(move || {
                let facts = Facts {
                    batteries: batteries(&class.join("power_supply")),
                    actions: logind_actions(&bus),
                };
                app::post(move |app| {
                    let data = app.window.global::<PowerData>();
                    data.set_has_battery(!facts.batteries.is_empty());
                    data.set_battery(app::model(battery_rows(&facts.batteries)));
                    let (key, lid) = match &facts.actions {
                        Some((key, lid)) => (action_words(key), action_words(lid)),
                        None => ("Unknown", "Unknown"),
                    };
                    data.set_buttons(app::model(vec![
                        row("Power button", key),
                        row("Closing the lid", lid),
                    ]));
                });
            });
        if let Err(error) = spawned {
            eprintln!("sengi-settings: could not start a thread to read the battery: {error}");
        }
    }
}

impl Controller for PowerPage {
    fn show(&self, app: &Rc<App>) {
        self.refresh();
        let weak = Rc::downgrade(app);
        self.clock
            .start(TimerMode::Repeated, BATTERY_PERIOD, move || {
                if let Some(page) = weak.upgrade().and_then(|app| app.page::<PowerPage>()) {
                    page.refresh();
                }
            });
    }

    /// Nobody is looking: nothing is read.
    fn hide(&self, _app: &Rc<App>) {
        self.clock.stop();
    }

    fn shutdown(&self) {
        self.clock.stop();
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

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

    #[test]
    fn a_laptop_battery_is_read_with_its_health() {
        let tmp = TempDir::new();
        laptop(&tmp, "Discharging");
        let battery = battery(tmp.path()).unwrap();
        assert_eq!(battery.percent, 58);
        assert_eq!(battery.charge, Charge::Discharging);
        assert_eq!(battery.health(), Some(86));
        assert_eq!(battery.cycles, Some(312));
        assert_eq!(battery.kind, "Li-ion, ASUSTeK, ASUS Battery");
        // 24.07 Wh at 9.5 W.
        assert_eq!(battery.minutes_left(), Some(152));
        assert_eq!(battery.summary(), "58%, on battery, about 2 h 32 min left");
        assert_eq!(battery.name, "BAT0");
        let rows = battery_rows(&[battery]);
        assert_eq!(rows.len(), 5);
        assert_eq!(
            rows[1].value,
            "86% of its original capacity (41.5 of 48.0 Wh)"
        );
    }

    #[test]
    fn two_batteries_are_one_charge_weighted_by_what_each_holds() {
        let tmp = TempDir::new();
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
        let both = batteries(tmp.path());
        assert_eq!(both.len(), 2);
        assert_eq!((both[0].percent, both[1].percent), (90, 20));
        let whole = battery(tmp.path()).unwrap();
        // 30 of 80 Wh, not the 55% that averaging the two would give.
        assert_eq!(whole.percent, 38);
        assert_eq!(whole.charge, Charge::Discharging);
        assert_eq!(whole.now_wh, Some(30.0));
        assert_eq!(whole.watts, Some(12.0));
        assert_eq!(whole.minutes_left(), Some(150));
        let rows = battery_rows(&both);
        let shown: Vec<(String, String)> = rows
            .iter()
            .map(|row| (row.title.to_string(), row.value.to_string()))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "Charge".to_string(),
                    "38%, on battery, about 2 h 30 min left".to_string()
                ),
                ("Power".to_string(), "12.0 W".to_string()),
                (
                    "Battery 1 (BAT0)".to_string(),
                    "90%, holds 83% of its original capacity (20.0 of 24.0 Wh), 410 charge cycles"
                        .to_string()
                ),
                (
                    "Battery 2 (BAT1)".to_string(),
                    "20%, holds 83% of its original capacity (60.0 of 72.0 Wh), 95 charge cycles"
                        .to_string()
                ),
            ]
        );
    }

    #[test]
    fn batteries_that_do_not_give_their_energy_are_averaged() {
        let full = Battery {
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
        let half = Battery {
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
    fn a_process_is_ended_only_if_it_is_still_the_one_meant() {
        use std::os::unix::process::ExitStatusExt;
        use std::process::{Command, Stdio};
        let sleeper = || {
            Command::new("/bin/sleep")
                .arg("30")
                .stdin(Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut spared = sleeper();
        assert!(!end_if(spared.id() as i32, || false));
        assert_eq!(spared.try_wait().unwrap(), None);
        spared.kill().unwrap();
        spared.wait().unwrap();
        // Gone and reaped: there is nothing to take hold of.
        assert!(!end_if(spared.id() as i32, || true));

        let mut ended = sleeper();
        assert!(end_if(ended.id() as i32, || true));
        assert_eq!(ended.wait().unwrap().signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn charging_counts_up_to_full_and_full_counts_nothing() {
        let tmp = TempDir::new();
        laptop(&tmp, "Charging");
        let charging = battery(tmp.path()).unwrap();
        assert_eq!(charging.minutes_left(), Some(110));
        assert!(charging.summary().ends_with("about 1 h 50 min until full"));
        laptop(&tmp, "Full");
        assert_eq!(battery(tmp.path()).unwrap().summary(), "58%, full");
        laptop(&tmp, "Not charging");
        assert_eq!(battery(tmp.path()).unwrap().charge, Charge::Holding);
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
        let battery = battery(tmp.path()).unwrap();
        assert_eq!(battery.percent, 50);
        assert_eq!(battery.health(), Some(80));
        assert_eq!(battery.full_wh, Some(44.0));
        assert_eq!(battery.minutes_left(), Some(120));
        assert_eq!(battery.cycles, None);
    }

    #[test]
    fn no_battery_and_nonsense_are_handled() {
        let tmp = TempDir::new();
        assert_eq!(battery(tmp.path()), None);
        assert_eq!(battery(&tmp.path().join("missing")), None);
        tmp.write("AC0/type", "Mains\n");
        tmp.write("BAT0/type", "Battery\n");
        tmp.write("BAT0/present", "0\n");
        assert_eq!(battery(tmp.path()), None);
        tmp.write("BAT0/present", "1\n");
        // Present but says nothing usable.
        assert_eq!(battery(tmp.path()), None);
        tmp.write("BAT0/capacity", "250\n");
        tmp.write("BAT0/energy_full", "-5\n");
        tmp.write("BAT0/energy_full_design", "0\n");
        tmp.write("BAT0/power_now", "NaN\n");
        tmp.write("BAT0/status", "\u{0}\u{1b}[31m\n");
        let odd = battery(tmp.path()).unwrap();
        assert_eq!(odd.percent, 100);
        assert_eq!(odd.charge, Charge::Unknown);
        assert_eq!(odd.health(), None);
        assert_eq!(odd.minutes_left(), None);
        assert_eq!(odd.summary(), "100%, state unknown");
    }

    #[test]
    fn idle_times_and_actions_in_words() {
        assert_eq!(idle_words(0), "Never");
        assert_eq!(idle_words(60), "After 1 minute");
        assert_eq!(idle_words(300), "After 5 minutes");
        assert_eq!(idle_words(3600), "After 1 hour");
        assert_eq!(idle_words(7200), "After 2 hours");
        assert_eq!(idle_words(45), "After 45 seconds");
        assert_eq!(action_words("poweroff"), "Shut down");
        assert_eq!(action_words("suspend"), "Sleep");
        assert_eq!(action_words("ignore"), "Nothing");
        assert_eq!(action_words("rm -rf"), "Unknown");
        for seconds in IDLE_CHOICES {
            let value = Value::Number(seconds);
            assert!(crate::settings::check("power", "lock-after", &value).is_ok());
        }
    }

    #[test]
    fn only_a_marked_swayidle_of_this_session_is_restarted() {
        use std::os::unix::fs::MetadataExt;
        let tmp = TempDir::new();
        let uid = std::fs::metadata(tmp.path()).unwrap().uid();
        let process = |pid: u32, comm: &str, environ: &str| {
            tmp.write(&format!("{pid}/comm"), &format!("{comm}\n"));
            tmp.write(&format!("{pid}/environ"), environ);
        };
        process(
            100,
            "swayidle",
            "HOME=/h\0WAYLAND_DISPLAY=wayland-0\0SENGI_IDLE_SUPERVISED=1\0",
        );
        // Not marked: ending it would leave the session with no idle lock.
        process(101, "swayidle", "WAYLAND_DISPLAY=wayland-0\0");
        // Another session's.
        process(
            102,
            "swayidle",
            "WAYLAND_DISPLAY=wayland-1\0SENGI_IDLE_SUPERVISED=1\0",
        );
        // Something else that happens to carry the mark.
        process(
            103,
            "bash",
            "WAYLAND_DISPLAY=wayland-0\0SENGI_IDLE_SUPERVISED=1\0",
        );
        // The mark as part of another value does not count.
        process(
            104,
            "swayidle",
            "WAYLAND_DISPLAY=wayland-0\0X=SENGI_IDLE_SUPERVISED=1\0",
        );
        tmp.write("self/comm", "swayidle\n");
        tmp.write("1/comm", "swayidle\n");
        tmp.write(
            "1/environ",
            "WAYLAND_DISPLAY=wayland-0\0SENGI_IDLE_SUPERVISED=1\0",
        );
        assert_eq!(restartable_idlers(tmp.path(), uid, "wayland-0"), [100]);
        assert!(restartable_idlers(tmp.path(), uid + 1, "wayland-0").is_empty());
        assert!(restartable_idlers(&tmp.path().join("missing"), uid, "wayland-0").is_empty());
    }
}
