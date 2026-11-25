//! What the last check found, kept in the user's cache directory so that
//! Settings can show it, and the badge on its sidebar, the moment it
//! opens, and know whether a day has gone by.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::apt::Upgrade;
use crate::flatpak::AppUpdate;

/// A check is repeated when the last one is older than this.
pub const MAX_AGE: u64 = 24 * 60 * 60;
/// And an attempt that did not work is not repeated for this long.
pub const RETRY_AFTER: u64 = 6 * 60 * 60;
/// More lines than this in the file are not read.
const MAX_LINES: usize = 4000;

/// `~/.cache/sengi-os`, or under `$XDG_CACHE_HOME`.
pub fn dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?).join(".cache"),
    };
    base.is_absolute().then(|| base.join("sengi-os"))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// When the lists were last read, in seconds since 1970. 0: never.
    pub checked: u64,
    /// When a check was last started, worked or not.
    pub attempted: u64,
    pub system: Vec<Upgrade>,
    pub apps: Vec<AppUpdate>,
    /// Bytes of packages still to download.
    pub download: Option<u64>,
}

fn field(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

impl Snapshot {
    pub fn total(&self) -> usize {
        self.system.len() + self.apps.len()
    }

    pub fn security(&self) -> usize {
        self.system.iter().filter(|u| u.security).count()
    }

    /// Whether it is time to look again.
    pub fn stale(&self, now: u64) -> bool {
        let old = now.saturating_sub(self.checked) > MAX_AGE;
        let tried_lately = now.saturating_sub(self.attempted) < RETRY_AFTER;
        old && !tried_lately
    }

    pub fn to_text(&self) -> String {
        let mut text = format!("checked {}\nattempted {}\n", self.checked, self.attempted);
        if let Some(bytes) = self.download {
            text.push_str(&format!("download {bytes}\n"));
        }
        for u in &self.system {
            text.push_str(&format!(
                "sys\t{}\t{}\t{}\t{}\n",
                field(&u.name),
                field(u.from.as_deref().unwrap_or("")),
                field(&u.to),
                u8::from(u.security)
            ));
        }
        for a in &self.apps {
            text.push_str(&format!(
                "app\t{}\t{}\t{}\t{}\n",
                field(&a.id),
                field(&a.name),
                field(&a.version),
                field(&a.size)
            ));
        }
        text
    }

    pub fn from_text(text: &str) -> Snapshot {
        let mut found = Snapshot::default();
        for line in text.lines().take(MAX_LINES) {
            let parts: Vec<&str> = line.split('\t').collect();
            match parts.as_slice() {
                [first] => {
                    let (key, value) = first.split_once(' ').unwrap_or((*first, ""));
                    let number = value.trim().parse::<u64>().ok();
                    match (key, number) {
                        ("checked", Some(n)) => found.checked = n,
                        ("attempted", Some(n)) => found.attempted = n,
                        ("download", Some(n)) => found.download = Some(n),
                        _ => {}
                    }
                }
                ["sys", name, from, to, security] if !name.is_empty() => {
                    found.system.push(Upgrade {
                        name: name.to_string(),
                        from: (!from.is_empty()).then(|| from.to_string()),
                        to: to.to_string(),
                        security: *security == "1",
                    });
                }
                ["app", id, name, version, size] if !id.is_empty() => {
                    found.apps.push(AppUpdate {
                        id: id.to_string(),
                        name: name.to_string(),
                        version: version.to_string(),
                        size: size.to_string(),
                    });
                }
                _ => {}
            }
        }
        found
    }

    /// Reads the file; what is not there or not understood is empty.
    pub fn load(path: &Path) -> Snapshot {
        match fs::metadata(path) {
            Ok(meta) if meta.len() <= 4 << 20 => {}
            _ => return Snapshot::default(),
        }
        fs::read_to_string(path)
            .map(|text| Snapshot::from_text(&text))
            .unwrap_or_default()
    }

    /// Replaces the file in one step.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("no parent directory"))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".updates.{}.tmp", std::process::id()));
        let written = (|| {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(self.to_text().as_bytes())?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    fn sample() -> Snapshot {
        Snapshot {
            checked: 1_700_000_000,
            attempted: 1_700_000_100,
            system: vec![
                Upgrade {
                    name: "openssl".to_string(),
                    from: Some("3.0.15".to_string()),
                    to: "3.0.16".to_string(),
                    security: true,
                },
                Upgrade {
                    name: "libnew1".to_string(),
                    from: None,
                    to: "1.0".to_string(),
                    security: false,
                },
            ],
            apps: vec![AppUpdate {
                id: "org.gimp.GIMP".to_string(),
                name: "GIMP".to_string(),
                version: "2.10".to_string(),
                size: "112 MB".to_string(),
            }],
            download: Some(36_200_000),
        }
    }

    #[test]
    fn it_survives_the_file() {
        let dir = scratch("cache");
        let path = dir.join("deeper/updates");
        let one = sample();
        one.save(&path).unwrap();
        assert_eq!(Snapshot::load(&path), one);
        assert_eq!(one.total(), 3);
        assert_eq!(one.security(), 1);
        // Nothing is left beside it.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn a_missing_or_broken_file_is_empty() {
        let dir = scratch("cache-broken");
        assert_eq!(Snapshot::load(&dir.join("none")), Snapshot::default());
        let path = dir.join("updates");
        fs::write(
            &path,
            "checked lots\nsys\tonly\ttwo\nbinary\u{0}\n\u{1}\u{2}",
        )
        .unwrap();
        assert_eq!(Snapshot::load(&path), Snapshot::default());
    }

    #[test]
    fn a_day_is_the_time_to_look_again() {
        let mut s = Snapshot {
            checked: 1_000_000,
            ..Snapshot::default()
        };
        assert!(!s.stale(1_000_000 + MAX_AGE));
        assert!(s.stale(1_000_000 + MAX_AGE + 1));
        // A try that failed an hour ago is not repeated yet.
        s.attempted = 1_000_000 + MAX_AGE;
        assert!(!s.stale(1_000_000 + MAX_AGE + 3600));
        assert!(s.stale(1_000_000 + MAX_AGE + RETRY_AFTER + 1));
        // Never checked: stale.
        assert!(Snapshot::default().stale(1_700_000_000));
    }

    #[test]
    fn tabs_in_names_cannot_break_the_file() {
        let mut s = sample();
        s.system[0].name = "evil\tname\n".to_string();
        let back = Snapshot::from_text(&s.to_text());
        assert_eq!(back.system[0].name, "evilname");
    }
}
