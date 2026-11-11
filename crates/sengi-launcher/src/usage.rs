//! How often and how recently each application was launched, kept in a small
//! text file so favourites float to the top.
//!
//! One line per application: `count last-used-unix-seconds desktop-id`.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::matcher::Weight;
use sengi_apps::env::Env;
use sengi_apps::files::read_limited;

/// Applications remembered; the least used are forgotten beyond this.
const MAX_ENTRIES: usize = 256;
const DAY: u64 = 24 * 60 * 60;
/// Far above what MAX_ENTRIES lines can come to.
const MAX_FILE_BYTES: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Record {
    count: u32,
    last: u64,
}

#[derive(Debug, Default)]
pub struct Usage {
    records: HashMap<String, Record>,
}

/// Where the launch history is kept.
pub fn file(env: &Env) -> PathBuf {
    env.state_home.join("sengi-launcher/usage")
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Usage {
    /// Read the file. A missing or damaged file just means no history.
    pub fn load(path: &Path) -> Self {
        let Some(bytes) = read_limited(path, MAX_FILE_BYTES) else {
            return Usage::default();
        };
        let mut records = HashMap::new();
        for line in String::from_utf8_lossy(&bytes).lines() {
            let mut parts = line.splitn(3, ' ');
            let (Some(count), Some(last), Some(id)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Ok(count), Ok(last)) = (count.parse::<u32>(), last.parse::<u64>()) else {
                continue;
            };
            if id.is_empty() || count == 0 {
                continue;
            }
            records.insert(id.to_owned(), Record { count, last });
        }
        Usage { records }
    }

    /// Launch count, scaled up when the last launch was recent.
    pub fn weight(&self, id: &str, now: u64) -> Weight {
        let Some(record) = self.records.get(id) else {
            return (0, 0);
        };
        let age = now.saturating_sub(record.last);
        let recency = match age {
            a if a < DAY => 8,
            a if a < 7 * DAY => 4,
            a if a < 30 * DAY => 2,
            _ => 1,
        };
        (record.count.saturating_mul(recency), record.last)
    }

    pub fn record(&mut self, id: &str, now: u64) {
        let record = self
            .records
            .entry(id.to_owned())
            .or_insert(Record { count: 0, last: 0 });
        record.count = record.count.saturating_add(1);
        record.last = now;
    }

    /// Write the file, replacing it in one step so a crash cannot leave half.
    pub fn save(&self, path: &Path, now: u64) -> std::io::Result<()> {
        let mut rows: Vec<(&String, &Record)> = self.records.iter().collect();
        rows.sort_by(|a, b| {
            self.weight(b.0, now)
                .cmp(&self.weight(a.0, now))
                .then(a.0.cmp(b.0))
        });
        rows.truncate(MAX_ENTRIES);

        let mut text = String::new();
        for (id, record) in rows {
            // An ID with a line break could not be read back.
            if id.contains(['\n', '\r']) {
                continue;
            }
            text.push_str(&format!("{} {} {}\n", record.count, record.last, id));
        }

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut tmp = PathBuf::from(path);
        tmp.set_extension(format!("tmp{}", std::process::id()));
        let written = std::fs::File::create(&tmp)
            .and_then(|mut file| file.write_all(text.as_bytes()))
            .and_then(|()| std::fs::rename(&tmp, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const NOW: u64 = 1_800_000_000;

    #[test]
    fn missing_file_is_empty_history() {
        let usage = Usage::load(Path::new("/no/such/dir/usage"));
        assert_eq!(usage.weight("foot.desktop", NOW), (0, 0));
    }

    #[test]
    fn records_survive_a_round_trip() {
        let tmp = TempDir::new();
        let path = tmp.path().join("state/sengi-launcher/usage");
        let mut usage = Usage::default();
        usage.record("foot.desktop", NOW - 10);
        usage.record("foot.desktop", NOW);
        usage.record("id with spaces.desktop", NOW);
        usage.save(&path, NOW).unwrap();

        let loaded = Usage::load(&path);
        assert_eq!(loaded.weight("foot.desktop", NOW), (16, NOW));
        assert_eq!(loaded.weight("id with spaces.desktop", NOW), (8, NOW));
        assert_eq!(loaded.weight("other.desktop", NOW), (0, 0));

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("usage")]);
    }

    #[test]
    fn recent_use_counts_for_more() {
        let mut usage = Usage::default();
        for _ in 0..3 {
            usage.record("old.desktop", NOW - 60 * DAY);
        }
        usage.record("new.desktop", NOW - 60);
        usage.record("week.desktop", NOW - 3 * DAY);
        usage.record("month.desktop", NOW - 20 * DAY);
        assert_eq!(usage.weight("old.desktop", NOW).0, 3);
        assert_eq!(usage.weight("new.desktop", NOW).0, 8);
        assert_eq!(usage.weight("week.desktop", NOW).0, 4);
        assert_eq!(usage.weight("month.desktop", NOW).0, 2);
        // A clock that went backwards must not underflow.
        assert_eq!(usage.weight("new.desktop", 0).0, 8);
    }

    #[test]
    fn damaged_lines_are_skipped() {
        let tmp = TempDir::new();
        let mut bytes = b"3 100 good.desktop\n\nnot a line\nx 100 bad-count.desktop\n\
                          5 y bad-time.desktop\n99999999999 1 overflow.desktop\n0 5 zero.desktop\n\
                          2 200\n"
            .to_vec();
        bytes.extend_from_slice(b"4 300 caf\xe9.desktop\n\xff\xfe\x00\n1 400 last.desktop");
        let path = tmp.write("usage", bytes);

        let usage = Usage::load(&path);
        assert_eq!(usage.weight("good.desktop", NOW), (3, 100));
        assert_eq!(usage.weight("last.desktop", NOW), (1, 400));
        assert_eq!(usage.weight("caf\u{fffd}.desktop", NOW), (4, 300));
        assert_eq!(usage.records.len(), 3);
    }

    #[test]
    fn the_file_stays_small() {
        let tmp = TempDir::new();
        let path = tmp.path().join("usage");
        let mut usage = Usage::default();
        for i in 0..MAX_ENTRIES + 50 {
            usage.record(&format!("app{i}.desktop"), NOW - 90 * DAY);
        }
        usage.record("favourite.desktop", NOW);
        usage.record("bad\nid.desktop", NOW);
        usage.save(&path, NOW).unwrap();

        let loaded = Usage::load(&path);
        assert!(loaded.records.len() <= MAX_ENTRIES);
        assert_eq!(loaded.weight("favourite.desktop", NOW), (8, NOW));
    }

    #[test]
    fn save_fails_cleanly_when_the_directory_cannot_be_made() {
        let tmp = TempDir::new();
        let blocker = tmp.write("file", "");
        let usage = Usage::default();
        assert!(usage.save(&blocker.join("sub/usage"), NOW).is_err());
    }

    #[test]
    fn an_oversized_file_is_ignored() {
        let tmp = TempDir::new();
        let path = tmp.path().join("usage");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(4 << 30).unwrap();
        assert!(Usage::load(&path).records.is_empty());
    }
}
