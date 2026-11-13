//! The clock: what it shows, and the two settings it follows.
//!
//! `clock.format` and `clock.time-zone` are read from the settings file
//! through sengi-config. Nothing tells the bar that the file changed. It
//! looks at the file's size and time when it is about to draw a new
//! minute anyway, and reads it again only if those moved, so following
//! the settings costs a `stat` or two a minute and no wake-up of its own.
//!
//! A zone chosen in Settings is read from tzdata and applied to the time
//! here, by hand. The usual way, setting `TZ` and letting the library find
//! out, means changing the environment of a process whose other threads
//! are starting programs and reading it at that very moment, which is not
//! safe. This way the environment is never written.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, FixedOffset, Local, Utc};
use sengi_config::settings::{zone_file, ZONE_DIR};
use sengi_config::store::{Dirs, FILE};
use sengi_config::Settings;

const HOURS_24: &str = "%a %-e %b  %H:%M";
const HOURS_12: &str = "%a %-e %b  %-I:%M %p";

/// Where the system's own zone is.
const LOCALTIME: &str = "/etc/localtime";

/// No zone file is anywhere near this large.
const MAX_ZONE_BYTES: u64 = 256 * 1024;

enum Zone {
    /// Whatever this process was started with: `TZ` if the session set
    /// it, the system's zone if not.
    Inherited,
    /// A zone read from a file.
    Loaded(tz::TimeZone),
}

/// What tells one version of a file from the next without reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
    // Settings replaces the file by renaming a new one over it, which
    // shows here even when the size and the time happen to be the same.
    inode: u64,
}

fn stamp(path: Option<PathBuf>) -> Option<Stamp> {
    let path = path?;
    let meta = fs::metadata(&path).ok()?;
    Some(Stamp {
        modified: meta.modified().ok(),
        size: meta.len(),
        inode: meta.ino(),
        path,
    })
}

fn load_zone(path: &Path) -> Option<tz::TimeZone> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ZONE_BYTES {
        return None;
    }
    tz::TimeZone::from_tz_data(&fs::read(path).ok()?).ok()
}

fn format(now: DateTime<Utc>, zone: &Zone, twelve: bool) -> String {
    let pattern = if twelve { HOURS_12 } else { HOURS_24 };
    match zone {
        Zone::Inherited => now.with_timezone(&Local).format(pattern).to_string(),
        Zone::Loaded(zone) => {
            let offset = zone
                .find_local_time_type(now.timestamp())
                .ok()
                .and_then(|kind| FixedOffset::east_opt(kind.ut_offset()))
                .unwrap_or(FixedOffset::east_opt(0).expect("zero is a valid offset"));
            now.with_timezone(&offset).format(pattern).to_string()
        }
    }
}

pub struct Clock {
    dirs: Dirs,
    zones: PathBuf,
    localtime: PathBuf,
    /// The user's file and the system's, as they were when last read.
    seen: (Option<Stamp>, Option<Stamp>),
    twelve: bool,
    /// The zone setting last applied.
    name: String,
    zone: Zone,
    /// A zone has been set at some point in this process's life.
    was_set: bool,
}

impl Clock {
    pub fn new() -> Clock {
        Clock::with(Dirs::from_env(), Path::new(ZONE_DIR), Path::new(LOCALTIME))
    }

    fn with(dirs: Dirs, zones: &Path, localtime: &Path) -> Clock {
        let mut clock = Clock {
            dirs,
            zones: zones.to_path_buf(),
            localtime: localtime.to_path_buf(),
            seen: (None, None),
            twelve: false,
            name: String::new(),
            zone: Zone::Inherited,
            was_set: false,
        };
        clock.seen = clock.stamps();
        clock.read();
        clock
    }

    fn stamps(&self) -> (Option<Stamp>, Option<Stamp>) {
        (stamp(self.dirs.user(FILE)), stamp(self.dirs.system(FILE)))
    }

    /// Reads the settings again if either file is not the one last read.
    /// True if it did.
    pub fn follow_settings(&mut self) -> bool {
        // Stamped before reading: a change that lands in between is seen
        // as a change again next time, never missed.
        let now = self.stamps();
        if now == self.seen {
            return false;
        }
        self.seen = now;
        self.read();
        true
    }

    fn read(&mut self) {
        // What is wrong with the file is for Settings to say, not for the
        // bar to repeat every time the file changes.
        let (settings, _) = Settings::load(&self.dirs);
        self.twelve = settings.text("clock", "format") == "12h";
        let name = settings.text("clock", "time-zone");
        if name == self.name {
            return;
        }
        self.zone = if name.is_empty() {
            self.system_zone()
        } else {
            self.was_set = true;
            match zone_file(&self.zones, &name).and_then(|path| load_zone(&path)) {
                Some(zone) => Zone::Loaded(zone),
                None => {
                    eprintln!("sengi-bar: no time zone called {name}, using the system's");
                    self.system_zone()
                }
            }
        };
        self.name = name;
    }

    /// The zone for when Settings names none.
    fn system_zone(&self) -> Zone {
        if !self.was_set {
            return Zone::Inherited;
        }
        // The session put the zone that was set into `TZ` when it started,
        // and this process still carries it. So "the system's zone" can no
        // longer be asked of the environment and is read from its file.
        Zone::Loaded(load_zone(&self.localtime).unwrap_or_else(tz::TimeZone::utc))
    }

    pub fn text(&self, now: DateTime<Utc>) -> String {
        format(now, &self.zone, self.twelve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    use chrono::TimeZone;

    struct TempDir(PathBuf);

    static NEXT: AtomicU32 = AtomicU32::new(0);

    impl TempDir {
        fn new() -> TempDir {
            let dir = std::env::temp_dir().join(format!(
                "sengi-bar-clock-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        /// Puts a file in place the way Settings does: whole, by rename.
        fn write(&self, path: &str, contents: &[u8]) {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let beside = path.with_extension("new");
            fs::write(&beside, contents).unwrap();
            fs::rename(&beside, &path).unwrap();
        }

        fn clock(&self) -> Clock {
            let dir = |name: &str| self.0.join(name).display().to_string();
            Clock::with(
                Dirs::from(None, Some(&dir("home")), Some(&dir("etc"))),
                &self.0.join("zoneinfo"),
                &self.0.join("localtime"),
            )
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A zone file for a place that is always `seconds` east of UTC.
    fn zone_data(seconds: i32) -> Vec<u8> {
        let mut data = b"TZif".to_vec();
        data.extend([0; 16]);
        // No UT or standard flags, leap seconds or transitions; one kind
        // of local time; four bytes of name.
        for count in [0u32, 0, 0, 0, 1, 4] {
            data.extend(count.to_be_bytes());
        }
        data.extend(seconds.to_be_bytes());
        data.extend([0, 0]);
        data.extend(b"XXX\0");
        data
    }

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        // A Thursday.
        Utc.with_ymd_and_hms(2026, 11, 12, hour, minute, 0).unwrap()
    }

    fn fixed(seconds: i32) -> Zone {
        Zone::Loaded(tz::TimeZone::fixed(seconds).unwrap())
    }

    #[test]
    fn both_formats() {
        let utc = fixed(0);
        assert_eq!(format(at(13, 7), &utc, false), "Thu 12 Nov  13:07");
        assert_eq!(format(at(13, 7), &utc, true), "Thu 12 Nov  1:07 PM");
        assert_eq!(format(at(0, 5), &utc, false), "Thu 12 Nov  00:05");
        assert_eq!(format(at(0, 5), &utc, true), "Thu 12 Nov  12:05 AM");
        assert_eq!(format(at(12, 0), &utc, true), "Thu 12 Nov  12:00 PM");
        assert_eq!(format(at(9, 30), &utc, true), "Thu 12 Nov  9:30 AM");
    }

    #[test]
    fn a_zone_moves_the_day_as_well_as_the_hour() {
        assert_eq!(format(at(23, 30), &fixed(3600), false), "Fri 13 Nov  00:30");
        assert_eq!(
            format(at(2, 15), &fixed(-5 * 3600), true),
            "Wed 11 Nov  9:15 PM"
        );
        // Not every zone is a whole number of hours away.
        assert_eq!(
            format(at(12, 0), &fixed(5 * 3600 + 1800), false),
            "Thu 12 Nov  17:30"
        );
    }

    #[test]
    fn summer_time_is_the_zone_files_business() {
        let paris =
            Zone::Loaded(tz::TimeZone::from_posix_tz("CET-1CEST,M3.5.0,M10.5.0/3").unwrap());
        assert_eq!(format(at(12, 0), &paris, false), "Thu 12 Nov  13:00");
        let july = Utc.with_ymd_and_hms(2026, 7, 1, 12, 0, 0).unwrap();
        assert_eq!(format(july, &paris, false), "Wed 1 Jul  14:00");
    }

    #[test]
    fn with_no_settings_file_the_clock_is_as_it_always_was() {
        let tmp = TempDir::new();
        let mut clock = tmp.clock();
        assert!(!clock.twelve);
        assert!(matches!(clock.zone, Zone::Inherited));
        assert!(!clock.follow_settings());
    }

    #[test]
    fn the_users_file_wins_over_the_systems() {
        let tmp = TempDir::new();
        tmp.write("zoneinfo/Test/East", &zone_data(2 * 3600));
        tmp.write(
            "etc/sengi-os/settings.toml",
            b"[clock]\nformat = \"12h\"\ntime-zone = \"Test/East\"\n",
        );
        let clock = tmp.clock();
        assert_eq!(clock.text(at(13, 7)), "Thu 12 Nov  3:07 PM");
        tmp.write(
            "home/sengi-os/settings.toml",
            b"[clock]\nformat = \"24h\"\n",
        );
        let clock = tmp.clock();
        assert_eq!(clock.text(at(13, 7)), "Thu 12 Nov  15:07");
    }

    #[test]
    fn a_changed_file_is_read_again_and_an_unchanged_one_is_not() {
        let tmp = TempDir::new();
        tmp.write("zoneinfo/Test/East", &zone_data(2 * 3600));
        tmp.write("zoneinfo/Test/West", &zone_data(-3 * 3600));
        tmp.write("localtime", &zone_data(3600));
        let mut clock = tmp.clock();
        assert!(!clock.follow_settings());

        // The file appears.
        tmp.write(
            "home/sengi-os/settings.toml",
            b"[clock]\nformat = \"12h\"\n",
        );
        assert!(clock.follow_settings());
        assert!(clock.twelve);
        assert!(!clock.follow_settings());

        // Same size, new contents.
        tmp.write(
            "home/sengi-os/settings.toml",
            b"[clock]\nformat = \"24h\"\n",
        );
        assert!(clock.follow_settings());
        assert!(!clock.twelve);

        tmp.write(
            "home/sengi-os/settings.toml",
            b"[clock]\ntime-zone = \"Test/East\"\n",
        );
        assert!(clock.follow_settings());
        assert_eq!(clock.text(at(13, 7)), "Thu 12 Nov  15:07");
        tmp.write(
            "home/sengi-os/settings.toml",
            b"[clock]\ntime-zone = \"Test/West\"\n",
        );
        assert!(clock.follow_settings());
        assert_eq!(clock.text(at(13, 7)), "Thu 12 Nov  10:07");
        assert!(!clock.follow_settings());

        // The system's file counts too, where the user's says nothing.
        tmp.write("etc/sengi-os/settings.toml", b"[clock]\nformat = \"12h\"\n");
        assert!(clock.follow_settings());
        assert_eq!(clock.text(at(13, 7)), "Thu 12 Nov  10:07 AM");

        // The zone is taken away again: the system's own, from its file,
        // because the environment may still name the old one.
        tmp.write("home/sengi-os/settings.toml", b"# nothing\n");
        assert!(clock.follow_settings());
        assert_eq!(clock.text(at(13, 7)), "Thu 12 Nov  2:07 PM");

        // And the file goes.
        fs::remove_file(tmp.0.join("home/sengi-os/settings.toml")).unwrap();
        fs::remove_file(tmp.0.join("etc/sengi-os/settings.toml")).unwrap();
        assert!(clock.follow_settings());
        assert_eq!(clock.text(at(13, 7)), "Thu 12 Nov  14:07");
    }

    #[test]
    fn a_zone_that_cannot_be_used_falls_back_to_the_systems() {
        let tmp = TempDir::new();
        tmp.write("zoneinfo/Test/Broken", b"not a zone file");
        tmp.write("localtime", &zone_data(3600));
        for zone in ["Test/Broken", "Test/Missing", "../localtime", "/etc/passwd"] {
            let text = format!("[clock]\ntime-zone = \"{zone}\"\n");
            tmp.write("home/sengi-os/settings.toml", text.as_bytes());
            let clock = tmp.clock();
            // The last two are not zone names, so the setting reads as
            // empty and the process's own zone stands.
            if zone.starts_with("Test") {
                assert_eq!(clock.text(at(13, 7)), "Thu 12 Nov  14:07", "{zone}");
            } else {
                assert!(matches!(clock.zone, Zone::Inherited), "{zone}");
            }
        }
    }
}
