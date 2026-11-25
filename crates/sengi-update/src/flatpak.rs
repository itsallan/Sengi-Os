//! Reading what Flatpak says: the list of apps with an update waiting
//! (`flatpak --user remote-ls --updates`), and how far an update has got.
//!
//! The progress reader is the one the Store has in `sengi-store`'s
//! `progress.rs`, kept in step by hand: that crate links a window toolkit,
//! and this one must not. Flatpak writes a line for each step
//! ("Updating 2/4…") and, for the step under way, a bar with a percentage
//! and sometimes a size and a speed, redrawn in place with carriage
//! returns and escape codes. Only the end of the output is looked at, and
//! what is not there is left out.

/// An app (or runtime) that has an update waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUpdate {
    pub id: String,
    pub name: String,
    pub version: String,
    /// What is to be downloaded, as Flatpak wrote it: `12.3 MB`.
    pub size: String,
}

/// The columns asked of `remote-ls`, in this order, tab separated.
pub const COLUMNS: &str = "--columns=name,application,version,download-size";

fn app_id(text: &str) -> bool {
    text.contains('.')
        && text.len() <= 255
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The updates out of `remote-ls`'s output. A header line, or a line
/// without an application id in the second column, is skipped.
pub fn parse_remote_ls(output: &str) -> Vec<AppUpdate> {
    let mut found: Vec<AppUpdate> = Vec::new();
    for line in output.lines() {
        let columns: Vec<&str> = line.split('\t').map(str::trim).collect();
        if columns.len() < 2 || !app_id(columns[1]) {
            continue;
        }
        let id = columns[1].to_string();
        // The same app comes once for each remote and branch it has an
        // update on: it is one update to the person.
        if found.iter().any(|app| app.id == id) {
            continue;
        }
        let name = sengi_text::line(columns[0], 80);
        found.push(AppUpdate {
            name: if name.is_empty() { id.clone() } else { name },
            id,
            version: sengi_text::line(columns.get(2).copied().unwrap_or(""), 48),
            size: sengi_text::line(columns.get(3).copied().unwrap_or(""), 24),
        });
    }
    found
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// The percentage of the step under way, when Flatpak gave one.
    pub percent: Option<u8>,
    /// Which step this is, and how many there are.
    pub step: Option<(u32, u32)>,
    /// What Flatpak is doing: "Updating", "Downloading".
    pub verb: Option<String>,
    /// What has arrived, as Flatpak wrote it: "12.3 MB".
    pub size: Option<String>,
    /// How fast, as Flatpak wrote it: "4.1 MB/s".
    pub speed: Option<String>,
}

impl Progress {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Reads the ends of what Flatpak wrote, one slice for each stream.
    pub fn from_tails(tails: &[&[u8]]) -> Self {
        let mut found = Self::default();
        for tail in tails {
            let one = Self::from_tail(tail);
            found.percent = found.percent.or(one.percent);
            found.step = found.step.or(one.step);
            if found.verb.is_none() {
                found.verb = one.verb;
            }
            found.size = found.size.take().or(one.size);
            found.speed = found.speed.take().or(one.speed);
        }
        found
    }

    pub fn from_tail(tail: &[u8]) -> Self {
        let text = strip_escapes(&String::from_utf8_lossy(tail));
        let mut found = Self::default();
        // Newest first. A percentage and the words around it come from the
        // same piece, so that a size is not taken from an older bar.
        for piece in text.split(['\r', '\n']).rev() {
            let piece = piece.trim();
            if piece.is_empty() {
                continue;
            }
            if found.percent.is_none() {
                if let Some(percent) = percent_in(piece) {
                    found.percent = Some(percent);
                    let (size, speed) = size_and_speed(piece);
                    found.size = size;
                    found.speed = speed;
                }
            }
            if found.step.is_none() {
                if let Some(step) = step_in(piece) {
                    found.step = Some(step);
                    found.verb = verb_in(piece);
                }
            }
            if found.percent.is_some() && found.step.is_some() {
                break;
            }
        }
        if found.verb.is_none() && found.percent.is_some() {
            found.verb = text
                .split(['\r', '\n'])
                .rev()
                .map(str::trim)
                .find_map(verb_in);
        }
        found
    }
}

fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if !c.is_control() || c == '\r' || c == '\n' {
            out.push(c);
        }
    }
    out
}

fn percent_in(piece: &str) -> Option<u8> {
    let at = piece.rfind('%')?;
    let digits: String = piece[..at]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let value: u32 = digits.parse().ok()?;
    (value <= 100).then_some(value as u8)
}

fn step_in(piece: &str) -> Option<(u32, u32)> {
    piece.split_whitespace().find_map(|word| {
        let word = word.trim_end_matches(['\u{2026}', '.', ':']);
        let (a, b) = word.split_once('/')?;
        let (a, b): (u32, u32) = (a.parse().ok()?, b.parse().ok()?);
        (a >= 1 && b >= a && b < 1000).then_some((a, b))
    })
}

fn verb_in(piece: &str) -> Option<String> {
    let first = piece.split_whitespace().next()?;
    let first = first.trim_end_matches(['\u{2026}', '.', ':']);
    ["Installing", "Updating", "Downloading", "Uninstalling"]
        .contains(&first)
        .then(|| first.to_string())
}

const UNITS: [&str; 7] = ["bytes", "kB", "KB", "MB", "GB", "MiB", "GiB"];

fn size_and_speed(piece: &str) -> (Option<String>, Option<String>) {
    let words: Vec<&str> = piece.split_whitespace().collect();
    let (mut size, mut speed) = (None, None);
    for (i, word) in words.iter().enumerate() {
        // "12.3 MB/s", "12.3MB/s" or a unit alone after a number.
        let (number, unit) = match word.find(|c: char| !(c.is_ascii_digit() || c == '.')) {
            Some(0) | None => match (words.get(i + 1), word.parse::<f64>().is_ok()) {
                (Some(unit), true) => (*word, *unit),
                _ => continue,
            },
            Some(at) => (&word[..at], &word[at..]),
        };
        if number.parse::<f64>().is_err() {
            continue;
        }
        if let Some(base) = unit.strip_suffix("/s") {
            if UNITS.contains(&base) && speed.is_none() {
                speed = Some(format!("{number} {unit}"));
            }
        } else if UNITS.contains(&unit) && size.is_none() {
            size = Some(format!("{number} {unit}"));
        }
    }
    (size, speed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_of_updates() {
        let output = "Name\tApplication ID\tVersion\tDownload size\n\
                      GIMP\torg.gimp.GIMP\t2.10.38\t112.4 MB\n\
                      Firefox\torg.mozilla.firefox\t128.0\t60.1 MB\n\
                      Firefox\torg.mozilla.firefox\t128.0\t60.1 MB\n\
                      \torg.freedesktop.Platform\t23.08\t5 MB\n\
                      \n\
                      no tabs here\n";
        let found = parse_remote_ls(output);
        let ids: Vec<&str> = found.iter().map(|app| app.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "org.gimp.GIMP",
                "org.mozilla.firefox",
                "org.freedesktop.Platform"
            ]
        );
        assert_eq!(found[0].name, "GIMP");
        assert_eq!(found[0].version, "2.10.38");
        assert_eq!(found[0].size, "112.4 MB");
        // No name falls back to the id.
        assert_eq!(found[2].name, "org.freedesktop.Platform");
        assert!(parse_remote_ls("").is_empty());
    }

    #[test]
    fn a_bar_with_everything() {
        let tail = b"Updating 2/4\xe2\x80\xa6\r\nUpdating\xe2\x80\xa6 \xe2\x96\x88\xe2\x96\x88\xe2\x96\x88          45%  12.3 MB  4.1 MB/s\r";
        let p = Progress::from_tail(tail);
        assert_eq!(p.percent, Some(45));
        assert_eq!(p.step, Some((2, 4)));
        assert_eq!(p.verb.as_deref(), Some("Updating"));
        assert_eq!(p.size.as_deref(), Some("12.3 MB"));
        assert_eq!(p.speed.as_deref(), Some("4.1 MB/s"));
    }

    #[test]
    fn escape_codes_and_redraws_are_ignored() {
        let p = Progress::from_tail(
            b"\x1b[1mUpdating 1/3\xe2\x80\xa6\x1b[0m \x1b[2K\rUpdating 1/3\xe2\x80\xa6 7%\rUpdating 1/3\xe2\x80\xa6 8%\r",
        );
        assert_eq!(p.percent, Some(8));
        assert_eq!(p.step, Some((1, 3)));
    }

    #[test]
    fn no_numbers_means_nothing_is_claimed() {
        assert!(Progress::from_tail(b"Looking for matches\xe2\x80\xa6\n").is_empty());
        assert!(Progress::from_tail(b"").is_empty());
        let p = Progress::from_tail(b"Updating org.x.Y/x86_64/stable 250%\n");
        assert_eq!(p.percent, None);
        assert_eq!(p.step, None);
    }

    #[test]
    fn the_other_stream_fills_what_the_first_lacks() {
        let p =
            Progress::from_tails(&[b"Updating 1/2\xe2\x80\xa6\n", b"Updating\xe2\x80\xa6 30%\r"]);
        assert_eq!(p.step, Some((1, 2)));
        assert_eq!(p.percent, Some(30));
    }
}
