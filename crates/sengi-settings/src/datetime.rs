//! The Date & Time page: the time zone, the clock's format, and whether
//! the time is set from the network.
//!
//! The system's time zone is `/etc/localtime`, changed through
//! `org.freedesktop.timedate1.SetTimezone`. That call is guarded by the
//! polkit action `org.freedesktop.timedate1.set-timezone`, whose default
//! is administrator authentication even for the user at the console. The
//! Sengi OS image has no polkit at all, in which case systemd refuses
//! everyone but root outright; with polkit installed and no agent to ask
//! for a password, the answer is the same. So Settings does not pretend:
//! the zone chosen here is this user's, kept in `clock.time-zone` and
//! exported to the session as `TZ`, which is how a time zone is set
//! without root on any Unix. The system's zone is shown beside it.
//!
//! The same guard is on `SetNTP`, so network time is shown, not switched.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;

use slint::ComponentHandle;

use crate::app::{self, App, Choice, Controller};
use crate::bus::{self, Bus};
use crate::settings::zone_is_plain;
use crate::store::Value;
use crate::text;
use crate::ui::{DateTimeData, InfoRow};

const ZONEINFO: &str = "/usr/share/zoneinfo";
/// The table of zones people choose between, one per populated area.
const ZONE_TABLE: &str = "/usr/share/zoneinfo/zone1970.tab";
pub const MAX_ZONES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zone {
    /// The name that goes into `TZ`, such as `America/Argentina/Ushuaia`.
    pub name: String,
    /// The place: `Ushuaia`.
    pub city: String,
    /// Where that is: `America/Argentina`, and the table's note if any.
    pub region: String,
}

fn zone_from(name: &str, comment: &str) -> Option<Zone> {
    if !zone_is_plain(name) {
        return None;
    }
    let (area, city) = name.rsplit_once('/').unwrap_or(("", name));
    let comment = text::clean(comment, 48);
    let region = match (area.is_empty(), comment.is_empty()) {
        (_, true) => area.replace('_', " "),
        (true, false) => comment,
        (false, false) => format!("{} · {comment}", area.replace('_', " ")),
    };
    Some(Zone {
        name: name.to_string(),
        city: city.replace('_', " "),
        region,
    })
}

/// Reads `zone1970.tab`: tab-separated country codes, coordinates, zone
/// name and an optional comment, with `#` lines for people.
pub fn parse_zones(table: &str) -> Vec<Zone> {
    let mut zones: Vec<Zone> = table
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.nth(2)?;
            zone_from(name.trim(), fields.next().unwrap_or_default())
        })
        .take(MAX_ZONES)
        .collect();
    zones.sort_by(|a, b| (&a.city, &a.name).cmp(&(&b.city, &b.name)));
    zones.dedup_by(|a, b| a.name == b.name);
    zones
}

/// The zones to offer. UTC is always among them.
pub fn zones() -> Vec<Zone> {
    let meta = fs::metadata(ZONE_TABLE).ok();
    let table = meta
        .filter(|meta| meta.is_file() && meta.len() < 1024 * 1024)
        .and_then(|_| fs::read_to_string(ZONE_TABLE).ok())
        .unwrap_or_default();
    let mut zones = parse_zones(&table);
    zones.insert(
        0,
        Zone {
            name: "UTC".to_string(),
            city: "UTC".to_string(),
            region: "Coordinated Universal Time".to_string(),
        },
    );
    zones
}

/// Whether `zone` names a zone file this system has.
pub fn zone_exists(root: &Path, zone: &str) -> bool {
    zone_is_plain(zone) && root.join(zone).is_file()
}

/// The `date` format for the line that shows the time now.
pub fn now_format(twelve_hour: bool) -> &'static str {
    if twelve_hour {
        "+%A %-d %B %Y, %-I:%M %p %Z"
    } else {
        "+%A %-d %B %Y, %H:%M %Z"
    }
}

/// The time now in `zone`, or in the system's when it is empty, as
/// `date` writes it.
fn time_now(zone: &str, twelve_hour: bool) -> String {
    let mut command = Command::new("date");
    command
        .arg(now_format(twelve_hour))
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if zone.is_empty() {
        command.env_remove("TZ");
    } else if zone_exists(Path::new(ZONEINFO), zone) {
        command.env("TZ", zone);
    } else {
        return "Unknown time zone".to_string();
    }
    command
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| text::clean(&String::from_utf8_lossy(&output.stdout), 64))
        .unwrap_or_default()
}

/// What timedated says: the system's zone and the state of network time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct System {
    pub zone: String,
    pub ntp: Option<bool>,
    pub synchronised: bool,
}

pub fn ntp_words(system: &System) -> &'static str {
    match (system.ntp, system.synchronised) {
        (Some(true), true) => "On, and the clock is in step",
        (Some(true), false) => "On, waiting for a time server",
        (Some(false), _) => "Off",
        (None, _) => "Unknown",
    }
}

fn system(bus: &Bus) -> System {
    let props = bus.properties_of_helper(
        "org.freedesktop.timedate1",
        "/org/freedesktop/timedate1",
        "org.freedesktop.timedate1",
    );
    let Some(props) = props else {
        return System::default();
    };
    System {
        zone: text::clean(bus::text(&props, "Timezone").unwrap_or_default(), 64),
        ntp: bus::flag(&props, "NTP"),
        synchronised: bus::flag(&props, "NTPSynchronized").unwrap_or(false),
    }
}

pub struct DateTimePage {
    bus: Arc<Bus>,
}

impl DateTimePage {
    pub fn attach(app: &Rc<App>, bus: Arc<Bus>) -> Rc<DateTimePage> {
        let page = Rc::new(DateTimePage { bus });
        let data = app.window.global::<DateTimeData>();
        let (weak, weak_app) = (Rc::downgrade(&page), Rc::downgrade(app));
        data.on_choose_zone(move || {
            let (Some(page), Some(app)) = (weak.upgrade(), weak_app.upgrade()) else {
                return;
            };
            let now = app.settings.borrow().text("clock", "time-zone");
            let mut choices = vec![Choice {
                id: String::new(),
                title: "The system's time zone".to_string(),
                detail: "Whatever this computer is set to".to_string(),
                current: now.is_empty(),
            }];
            choices.extend(zones().into_iter().map(|zone| Choice {
                current: zone.name == now,
                id: zone.name,
                title: zone.city,
                detail: zone.region,
            }));
            let weak = Rc::downgrade(&page);
            app.choose("Time zone", true, choices, move |app, zone| {
                if app.set("clock", "time-zone", Value::Text(zone)) {
                    app.apply_to_session();
                    if let Some(page) = weak.upgrade() {
                        page.refresh(app);
                    }
                }
            });
        });
        let (weak, weak_app) = (Rc::downgrade(&page), Rc::downgrade(app));
        data.on_set_twelve_hour(move |twelve| {
            let (Some(page), Some(app)) = (weak.upgrade(), weak_app.upgrade()) else {
                return;
            };
            let format = if twelve { "12h" } else { "24h" };
            app.set("clock", "format", Value::text(format));
            page.refresh(&app);
        });
        page
    }

    fn refresh(&self, app: &Rc<App>) {
        let (zone, twelve) = {
            let settings = app.settings.borrow();
            (
                settings.text("clock", "time-zone"),
                settings.text("clock", "format") == "12h",
            )
        };
        let data = app.window.global::<DateTimeData>();
        data.set_twelve_hour(twelve);
        data.set_own_zone(!zone.is_empty());
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            let system = system(&bus);
            let now = time_now(&zone, twelve);
            app::post(move |app| {
                let data = app.window.global::<DateTimeData>();
                let system_zone = if system.zone.is_empty() {
                    "unknown"
                } else {
                    &system.zone
                };
                data.set_zone(
                    if zone.is_empty() {
                        format!("System ({system_zone})")
                    } else {
                        zone.replace('_', " ")
                    }
                    .into(),
                );
                data.set_now(now.into());
                data.set_system(app::model(vec![
                    InfoRow {
                        title: "System time zone".into(),
                        value: system_zone.replace('_', " ").into(),
                    },
                    InfoRow {
                        title: "Set the time from the network".into(),
                        value: ntp_words(&system).into(),
                    },
                ]));
            });
        });
    }
}

impl Controller for DateTimePage {
    fn show(&self, app: &Rc<App>) {
        self.refresh(app);
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const TABLE: &str = "\
# tzdb timezone descriptions
#codes\tcoordinates\tTZ\tcomments
AD\t+4230+00131\tEurope/Andorra
AQ\t-6617+11031\tAntarctica/Casey\tCasey
AR\t-5448-06818\tAmerica/Argentina/Ushuaia\tTierra del Fuego
FR,MC\t+4852+00220\tEurope/Paris
US\t+404251-0740023\tAmerica/New_York\tEastern (most areas)
XX\t+0000+0000\t../../etc/passwd\tnot a zone
XX\t+0000+0000
short line
XX\t+0\tEvil/Zone\u{202e}\tcomment
XX\t+0\tEurope/Paris\tlisted twice
";

    #[test]
    fn the_zone_table_is_read_and_sorted_by_place() {
        let zones = parse_zones(TABLE);
        let names: Vec<&str> = zones.iter().map(|zone| zone.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Europe/Andorra",
                "Antarctica/Casey",
                "America/New_York",
                "Europe/Paris",
                "America/Argentina/Ushuaia"
            ]
        );
        let york = &zones[2];
        assert_eq!(york.city, "New York");
        assert_eq!(york.region, "America · Eastern (most areas)");
        assert_eq!(zones[4].region, "America/Argentina · Tierra del Fuego");
        assert_eq!(zones[0].region, "Europe");
    }

    #[test]
    fn a_table_that_is_not_one_gives_no_zones() {
        assert!(parse_zones("").is_empty());
        assert!(parse_zones("\0\0\0\n\t\t\t\t\n# only comments\n").is_empty());
        let flood = "XX\t+0\tArea/Place\n".repeat(5000);
        assert_eq!(parse_zones(&flood).len(), 1);
        let many: String = (0..5000).map(|i| format!("XX\t+0\tArea/P{i}\n")).collect();
        assert_eq!(parse_zones(&many).len(), MAX_ZONES);
    }

    #[test]
    fn the_offered_zones_always_include_utc() {
        let zones = zones();
        assert_eq!(zones[0].name, "UTC");
        assert!(zones.iter().all(|zone| zone_is_plain(&zone.name)));
    }

    #[test]
    fn only_zones_that_exist_are_handed_to_date() {
        let tmp = TempDir::new();
        tmp.write("Europe/Paris", "TZif");
        assert!(zone_exists(tmp.path(), "Europe/Paris"));
        assert!(!zone_exists(tmp.path(), "Europe/Atlantis"));
        assert!(!zone_exists(tmp.path(), "Europe"));
        assert!(!zone_exists(tmp.path(), "../Europe/Paris"));
        assert!(!zone_exists(tmp.path(), ""));
        assert_eq!(time_now("No/Such_Zone", false), "Unknown time zone");
    }

    #[test]
    fn the_time_is_shown_in_the_chosen_format() {
        assert!(now_format(false).contains("%H:%M"));
        assert!(now_format(true).contains("%-I:%M %p"));
        // With a real `date`, the zone's own abbreviation comes back.
        if zone_exists(Path::new(ZONEINFO), "Asia/Tokyo") {
            assert!(time_now("Asia/Tokyo", false).ends_with("JST"));
        }
    }

    #[test]
    fn network_time_in_words() {
        let system = |ntp, synchronised| System {
            zone: "Etc/UTC".to_string(),
            ntp,
            synchronised,
        };
        assert_eq!(
            ntp_words(&system(Some(true), true)),
            "On, and the clock is in step"
        );
        assert_eq!(
            ntp_words(&system(Some(true), false)),
            "On, waiting for a time server"
        );
        assert_eq!(ntp_words(&system(Some(false), true)), "Off");
        assert_eq!(ntp_words(&System::default()), "Unknown");
    }
}
