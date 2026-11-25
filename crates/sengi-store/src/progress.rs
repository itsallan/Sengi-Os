//! Reading how far an install has got from what flatpak prints.
//!
//! Flatpak writes progress as it goes: a line for each step ("Installing
//! 2/4…") and, for the step under way, a bar with a percentage and
//! sometimes a size and a speed, redrawn in place with carriage returns
//! and escape codes. Only the end of the output is looked at. What is not
//! there is left out; nothing is guessed.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// The percentage of the step under way, when flatpak gave one.
    pub percent: Option<u8>,
    /// Which step this is, and how many there are.
    pub step: Option<(u32, u32)>,
    /// What flatpak is doing: "Installing", "Downloading".
    pub verb: Option<String>,
    /// What has arrived, as flatpak wrote it: "12.3 MB".
    pub size: Option<String>,
    /// How fast, as flatpak wrote it: "4.1 MB/s".
    pub speed: Option<String>,
}

impl Progress {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The words for under the app's name: "Installing 2 of 4 · 12.3 MB · 4.1 MB/s".
    pub fn detail(&self) -> String {
        let mut parts = Vec::new();
        let verb = self.verb.as_deref().unwrap_or("Installing");
        match self.step {
            Some((n, of)) => parts.push(format!("{verb} {n} of {of}")),
            None => parts.push(verb.to_string()),
        }
        parts.extend(self.size.clone());
        parts.extend(self.speed.clone());
        parts.join(" \u{b7} ")
    }

    /// Reads the ends of what flatpak wrote, one slice for each stream,
    /// newest information first.
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
        let text = clean(&String::from_utf8_lossy(tail));
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

/// The text without escape sequences.
fn clean(text: &str) -> String {
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
    fn a_bar_with_everything() {
        let tail = b"Installing 2/4\xe2\x80\xa6\r\nInstalling\xe2\x80\xa6 \xe2\x96\x88\xe2\x96\x88\xe2\x96\x88\xe2\x96\x88\xe2\x96\x88          45%  12.3 MB  4.1 MB/s\r";
        let p = Progress::from_tail(tail);
        assert_eq!(p.percent, Some(45));
        assert_eq!(p.step, Some((2, 4)));
        assert_eq!(p.verb.as_deref(), Some("Installing"));
        assert_eq!(p.size.as_deref(), Some("12.3 MB"));
        assert_eq!(p.speed.as_deref(), Some("4.1 MB/s"));
        assert_eq!(
            p.detail(),
            "Installing 2 of 4 \u{b7} 12.3 MB \u{b7} 4.1 MB/s"
        );
    }

    #[test]
    fn escape_codes_and_redraws_are_ignored() {
        let p = Progress::from_tail(
            b"\x1b[1mInstalling 1/3\xe2\x80\xa6\x1b[0m \x1b[2K\rInstalling 1/3\xe2\x80\xa6 7%\rInstalling 1/3\xe2\x80\xa6 8%\r",
        );
        assert_eq!(p.percent, Some(8));
        assert_eq!(p.step, Some((1, 3)));
    }

    #[test]
    fn a_step_line_alone_has_no_percentage() {
        let p =
            Progress::from_tail(b"Looking for matches\xe2\x80\xa6\nInstalling 3/5\xe2\x80\xa6\n");
        assert_eq!(p.percent, None);
        assert_eq!(p.step, Some((3, 5)));
        assert_eq!(p.detail(), "Installing 3 of 5");
    }

    #[test]
    fn no_numbers_means_nothing_is_claimed() {
        assert!(Progress::from_tail(b"Looking for matches\xe2\x80\xa6\n").is_empty());
        assert!(Progress::from_tail(b"").is_empty());
        // A size or a path is not a step, and a number over 100 is not a percentage.
        let p = Progress::from_tail(b"Installing org.x.Y/x86_64/stable 250%\n");
        assert_eq!(p.percent, None);
        assert_eq!(p.step, None);
    }

    #[test]
    fn the_other_stream_fills_what_the_first_lacks() {
        let p = Progress::from_tails(&[
            b"Installing 1/2\xe2\x80\xa6\n",
            b"Installing\xe2\x80\xa6 30%\r",
        ]);
        assert_eq!(p.step, Some((1, 2)));
        assert_eq!(p.percent, Some(30));
    }
}
