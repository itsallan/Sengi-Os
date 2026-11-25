//! What the notification centre does with its history beyond keeping it:
//! grouping lines by application, saying how long ago they came in, and the
//! small file in the user's state directory that lets them outlive a
//! restart.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use chrono::TimeZone;

use crate::markup;
use crate::notification::{Notification, Urgency, APP_MAX, BODY_MAX, SUMMARY_MAX};

/// The file is never larger than this. When the lines do not fit, the
/// oldest are left out.
pub const FILE_MAX: usize = 64 * 1024;

const MAGIC: &str = "sengi-notify-history 1";

/// One line of the centre: the heading of a group, or a notification
/// below it. Indexes are into the history, newest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    Group { first: usize, count: usize },
    Entry(usize),
}

/// Lays the history out by application. Groups are ordered by their newest
/// line, and the lines inside one are newest first.
pub fn group<'a>(apps: impl IntoIterator<Item = &'a str>) -> Vec<Line> {
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    let mut total = 0;
    for (index, app) in apps.into_iter().enumerate() {
        total += 1;
        match groups.iter_mut().find(|(name, _)| *name == app) {
            Some((_, members)) => members.push(index),
            None => groups.push((app, vec![index])),
        }
    }
    let mut lines = Vec::with_capacity(total + groups.len());
    for (_, members) in groups {
        lines.push(Line::Group {
            first: members[0],
            count: members.len(),
        });
        lines.extend(members.into_iter().map(Line::Entry));
    }
    lines
}

/// "now", "2 min ago", "3 hours ago", "yesterday", "5 days ago". Both
/// times are in seconds since the epoch. A time in the future, after the
/// clock was set back, counts as now.
pub fn relative(now: u64, at: u64) -> String {
    let seconds = now.saturating_sub(at);
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    let plural = |count: u64, unit: &str| {
        if count == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{count} {unit}s ago")
        }
    };
    match (minutes, hours, days) {
        (0, _, _) => "now".to_string(),
        (_, 0, _) => format!("{minutes} min ago"),
        (_, _, 0) => plural(hours, "hour"),
        (_, _, 1) => "yesterday".to_string(),
        _ => plural(days, "day"),
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The clock time shown on a card, for a moment in the past.
pub fn clock(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|at| chrono::Local.timestamp_opt(at, 0).single())
        .map_or_else(String::new, |time| time.format("%H:%M").to_string())
}

fn escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => {}
            c => out.push(c),
        }
    }
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The text of the file for these lines, newest first. Stops before the
/// file would pass [`FILE_MAX`].
pub fn encode<'a>(notes: impl IntoIterator<Item = &'a Notification>) -> String {
    let mut out = String::from(MAGIC);
    out.push('\n');
    for note in notes {
        let urgency = match note.urgency {
            Urgency::Low => 0,
            Urgency::Normal => 1,
            Urgency::Critical => 2,
        };
        let mut line = format!("{}\t{urgency}\t", note.at);
        escape(&note.app, &mut line);
        line.push('\t');
        escape(&note.summary, &mut line);
        line.push('\t');
        escape(&note.body, &mut line);
        line.push('\n');
        if out.len() + line.len() > FILE_MAX {
            break;
        }
        out.push_str(&line);
    }
    out
}

/// The notifications in the text of a file, newest first, at most `max`.
/// A file of another version, or lines that make no sense, give nothing:
/// the history is a convenience and never worth refusing to start for.
pub fn decode(text: &str, max: usize) -> Vec<Notification> {
    let mut lines = text.lines();
    if lines.next() != Some(MAGIC) {
        return Vec::new();
    }
    let mut notes = Vec::new();
    for line in lines {
        if notes.len() >= max {
            break;
        }
        let mut fields = line.splitn(5, '\t');
        let (Some(at), Some(urgency), Some(app), Some(summary), Some(body)) = (
            fields.next().and_then(|at| at.parse::<u64>().ok()),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        notes.push(Notification {
            app: markup::tidy(&unescape(app), APP_MAX, false),
            summary: markup::tidy(&unescape(summary), SUMMARY_MAX, false),
            body: markup::tidy(&unescape(body), BODY_MAX, true),
            urgency: match urgency {
                "0" => Urgency::Low,
                "2" => Urgency::Critical,
                _ => Urgency::Normal,
            },
            at,
            time: clock(at),
            ..Notification::default()
        });
    }
    notes
}

/// `$XDG_STATE_HOME/sengi-os/notifications`, or under `~/.local/state`.
pub fn path() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let state = absolute("XDG_STATE_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".local/state")))?;
    Some(state.join("sengi-os").join("notifications"))
}

/// What the file at `path` holds, or nothing.
pub fn load(path: &Path, max: usize) -> Vec<Notification> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    // One byte more than allowed: a file that size is cut, not trusted.
    if file
        .take(FILE_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Vec::new();
    }
    bytes.truncate(FILE_MAX);
    decode(&String::from_utf8_lossy(&bytes), max)
}

/// Writes the text beside the file and moves it into place, so that a
/// crash in the middle leaves the old file. Readable by the user only:
/// it holds what their messages said.
pub fn save(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let temporary = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    drop(file);
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(app: &str, summary: &str, at: u64) -> Notification {
        Notification {
            app: app.to_string(),
            summary: summary.to_string(),
            at,
            ..Notification::default()
        }
    }

    #[test]
    fn groups_follow_their_newest_line_and_keep_the_order_inside() {
        let lines = group(["Mail", "Chat", "Mail", "Mail", "Chat", "Files"]);
        assert_eq!(
            lines,
            [
                Line::Group { first: 0, count: 3 },
                Line::Entry(0),
                Line::Entry(2),
                Line::Entry(3),
                Line::Group { first: 1, count: 2 },
                Line::Entry(1),
                Line::Entry(4),
                Line::Group { first: 5, count: 1 },
                Line::Entry(5),
            ]
        );
        assert!(group([]).is_empty());
    }

    #[test]
    fn times_are_said_the_way_a_person_would() {
        let at = 1_000_000;
        let ago = |seconds| relative(at + seconds, at);
        assert_eq!(ago(0), "now");
        assert_eq!(ago(59), "now");
        assert_eq!(ago(60), "1 min ago");
        assert_eq!(ago(120), "2 min ago");
        assert_eq!(ago(59 * 60 + 59), "59 min ago");
        assert_eq!(ago(3_600), "1 hour ago");
        assert_eq!(ago(3 * 3_600), "3 hours ago");
        assert_eq!(ago(23 * 3_600 + 3_599), "23 hours ago");
        assert_eq!(ago(24 * 3_600), "yesterday");
        assert_eq!(ago(48 * 3_600), "2 days ago");
        assert_eq!(ago(10 * 24 * 3_600), "10 days ago");
        // The clock went back.
        assert_eq!(relative(at - 500, at), "now");
    }

    #[test]
    fn the_file_round_trips_text_that_needs_escaping() {
        let mut first = note("Mail", "Tab\there", 1_700_000_000);
        first.body = "line one\nline two \\ done".to_string();
        first.urgency = Urgency::Critical;
        let second = note("Chat", "hi", 1_699_999_000);
        let back = decode(&encode([&first, &second]), 50);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].summary, "Tab here");
        assert_eq!(back[0].body, "line one\nline two \\ done");
        assert_eq!(back[0].urgency, Urgency::Critical);
        assert_eq!(back[0].at, 1_700_000_000);
        assert_eq!(back[1].app, "Chat");
        assert_eq!(back[1].urgency, Urgency::Normal);
    }

    #[test]
    fn the_file_never_passes_its_cap_and_keeps_the_newest() {
        let big: Vec<Notification> = (0..200)
            .map(|n| {
                let mut note = note("App", &format!("note {n}"), n);
                note.body = "x".repeat(600);
                note
            })
            .collect();
        let text = encode(&big);
        assert!(text.len() <= FILE_MAX);
        let back = decode(&text, 500);
        assert!(!back.is_empty() && back.len() < 200);
        assert_eq!(back[0].summary, "note 0");
    }

    #[test]
    fn decode_takes_at_most_max_and_ignores_what_it_does_not_understand() {
        let many: Vec<Notification> = (0..10).map(|n| note("A", "s", n)).collect();
        assert_eq!(decode(&encode(&many), 4).len(), 4);
        assert!(decode("something else\n1\t1\ta\tb\tc\n", 5).is_empty());
        assert!(decode("", 5).is_empty());
        let text = format!("{MAGIC}\nnot a line\n5\t1\tA\tsummary\tbody\nx\t1\ta\tb\tc\n");
        let back = decode(&text, 5);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].summary, "summary");
    }

    #[test]
    fn saving_and_loading_use_a_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("sengi-notify-test-{}", std::process::id()));
        let file = dir.join("state/sengi-os/notifications");
        save(&file, &encode([&note("Mail", "hello", 42)])).unwrap();
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
        let back = load(&file, 50);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].summary, "hello");
        assert!(load(&dir.join("missing"), 50).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
