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

use sengi_system::battery::{self, Charge, Pack};

use crate::app::{self, App, Choice, Controller};
use crate::bus::{self, Bus};
use crate::store::Value;
use crate::ui::{InfoRow, PowerData};

/// The battery is read again this often, and only while the page shows.
const BATTERY_PERIOD: Duration = Duration::from_secs(30);

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

/// The line that says how the battery stands.
pub fn summary(battery: &Pack) -> String {
    let state = match battery.charge {
        Charge::Charging => "charging",
        Charge::Discharging => "on battery",
        Charge::Full => "full",
        Charge::NotCharging => "plugged in, not charging",
        Charge::Unknown => "state unknown",
    };
    let left = battery.minutes_left().map(|minutes| {
        let time = battery::duration(minutes);
        match battery.charge {
            Charge::Charging => format!(", about {time} until full"),
            _ => format!(", about {time} left"),
        }
    });
    format!("{}%, {state}{}", battery.percent, left.unwrap_or_default())
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
    batteries: Vec<Pack>,
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

fn health_words(battery: &Pack) -> Option<String> {
    let (health, full, design) = (battery.health()?, battery.full_wh?, battery.design_wh?);
    Some(format!(
        "{health}% of its original capacity ({full:.1} of {design:.1} Wh)"
    ))
}

/// The card's rows. One battery is described in full. Several are added
/// up for the charge and the power, and then each has a row for what is
/// its own: how full it is and how worn.
fn battery_rows(batteries: &[Pack]) -> Vec<InfoRow> {
    let Some(whole) = battery::combined(batteries) else {
        return Vec::new();
    };
    let mut rows = vec![row("Charge", &summary(&whole))];
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
                    batteries: battery::packs(&class.join("power_supply")),
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

    fn pack(name: &str, percent: u32, charge: Charge, now: f64, full: f64, design: f64) -> Pack {
        Pack {
            name: name.to_string(),
            percent,
            charge,
            full_wh: Some(full),
            design_wh: Some(design),
            now_wh: Some(now),
            watts: Some(0.0),
            cycles: None,
            kind: String::new(),
        }
    }

    #[test]
    fn one_battery_is_described_in_full() {
        let battery = Pack {
            watts: Some(9.5),
            cycles: Some(312),
            kind: "Li-ion, ASUSTeK, ASUS Battery".to_string(),
            ..pack("BAT0", 58, Charge::Discharging, 24.07, 41.5, 48.0)
        };
        assert_eq!(summary(&battery), "58%, on battery, about 2 h 32 min left");
        let rows = battery_rows(&[battery]);
        let shown: Vec<(String, String)> = rows
            .iter()
            .map(|row| (row.title.to_string(), row.value.to_string()))
            .collect();
        let expected = [
            ("Charge", "58%, on battery, about 2 h 32 min left"),
            ("Health", "86% of its original capacity (41.5 of 48.0 Wh)"),
            ("Power", "9.5 W"),
            ("Charge cycles", "312"),
            ("Type", "Li-ion, ASUSTeK, ASUS Battery"),
        ];
        assert_eq!(
            shown,
            expected.map(|(title, value)| (title.to_string(), value.to_string()))
        );
        assert!(battery_rows(&[]).is_empty());
    }

    #[test]
    fn two_batteries_are_added_up_and_then_each_has_a_row() {
        let small = Pack {
            cycles: Some(410),
            ..pack("BAT0", 90, Charge::Unknown, 18.0, 20.0, 24.0)
        };
        let large = Pack {
            watts: Some(12.0),
            cycles: Some(95),
            ..pack("BAT1", 20, Charge::Discharging, 12.0, 60.0, 72.0)
        };
        let rows = battery_rows(&[small, large]);
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
    fn the_charge_in_words() {
        let battery = |charge| Pack {
            watts: Some(9.5),
            ..pack("BAT0", 58, charge, 24.07, 41.5, 48.0)
        };
        assert!(summary(&battery(Charge::Charging)).ends_with("about 1 h 50 min until full"));
        assert_eq!(summary(&battery(Charge::Full)), "58%, full");
        assert_eq!(
            summary(&battery(Charge::NotCharging)),
            "58%, plugged in, not charging"
        );
        assert_eq!(summary(&battery(Charge::Unknown)), "58%, state unknown");
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
