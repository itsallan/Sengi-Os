//! Reading how far flatpak has got from what it prints.
//!
//! It only says without `--noninteractive`: with it, there is one line for
//! each step and nothing more. Without it, and with its output on a pipe,
//! it first lists what it is going to do, one tab separated row a step,
//!
//! ```text
//!  2.\t   \torg.example.App\tstable\ti\tflathub\t< 300.1 MB
//! ```
//!
//! and then, about three times a second, a line about the step under way:
//!
//! ```text
//! Installing 2/2… ████▍                 21%  19.9 MB/s  00:11
//! ```
//!
//! The size in the list is what a step may download at most. How much has
//! arrived is not printed. What is not there is left out.

/// A line longer than this is none of the two, and is not kept.
const MAX_LINE: usize = 1024;
/// More steps than this are not remembered.
const MAX_STEPS: usize = 256;

const VERBS: [&str; 3] = ["Installing", "Updating", "Uninstalling"];
const UNITS: [&str; 6] = ["byte", "bytes", "kB", "MB", "GB", "TB"];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// What flatpak is doing: "Installing", "Updating".
    pub verb: Option<String>,
    /// Which step this is, and how many there are. Flatpak leaves both out
    /// when there is only one.
    pub step: Option<(u32, u32)>,
    /// The percentage of the step under way.
    pub percent: Option<u8>,
    /// What this step downloads, from the list: "up to 300.1 MB".
    pub size: Option<String>,
    /// How fast, as flatpak wrote it: "19.9 MB/s".
    pub speed: Option<String>,
    /// The time flatpak thinks the step has left: "00:11".
    pub left: Option<String>,
}

impl Progress {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// "Step 2 of 2 · up to 300.1 MB"; with one step, what flatpak is
    /// doing takes the place of the number.
    pub fn detail(&self) -> String {
        let first = match self.step {
            Some((n, of)) => Some(format!("Step {n} of {of}")),
            None => self.verb.clone(),
        };
        join([first, self.size.clone()])
    }

    /// "19.9 MB/s · 00:11 left", or nothing.
    pub fn pace(&self) -> String {
        join([
            self.speed.clone(),
            self.left.as_ref().map(|left| format!("{left} left")),
        ])
    }
}

fn join<const N: usize>(parts: [Option<String>; N]) -> String {
    parts
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// Takes flatpak's standard output as it comes, in pieces of any size.
#[derive(Debug, Default)]
pub struct Reader {
    /// The line that has not ended yet.
    line: Vec<u8>,
    /// It grew too long and is being skipped.
    skipping: bool,
    /// The download size of each step of the list, by its number.
    sizes: Vec<(u32, String)>,
    now: Progress,
}

impl Reader {
    /// Reads more of the output.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                if !self.skipping {
                    let line = std::mem::take(&mut self.line);
                    self.read(&String::from_utf8_lossy(&line));
                }
                self.line.clear();
                self.skipping = false;
            } else if self.line.len() < MAX_LINE {
                self.line.push(byte);
            } else {
                self.skipping = true;
            }
        }
    }

    /// How far it has got, as of the last whole line.
    pub fn now(&self) -> &Progress {
        &self.now
    }

    fn read(&mut self, line: &str) {
        if let Some((number, size)) = row(line) {
            // A new list starts again at 1.
            if number == 1 {
                self.sizes.clear();
            }
            if self.sizes.len() < MAX_STEPS {
                self.sizes.push((number, size));
            }
        } else if let Some(mut found) = status(line) {
            let number = found.step.map_or(1, |(n, _)| n);
            found.size = self
                .sizes
                .iter()
                .find(|(n, _)| *n == number)
                .filter(|_| found.step.is_some() || self.sizes.len() == 1)
                .map(|(_, size)| size.clone());
            self.now = found;
        }
    }
}

/// A row of the list: the step's number and what it downloads.
fn row(line: &str) -> Option<(u32, String)> {
    let mut columns = line.split('\t');
    let number = columns.next()?.trim().strip_suffix('.')?.parse().ok()?;
    let last = columns.next_back()?.trim();
    let (most, last) = match last.strip_prefix('<') {
        Some(rest) => (true, rest),
        None => (false, last),
    };
    let mut words = last.split_whitespace();
    let (amount, unit) = (words.next()?, words.next()?);
    if !is_amount(amount) || !UNITS.contains(&unit) {
        return None;
    }
    let upto = if most { "up to " } else { "" };
    Some((number, format!("{upto}{amount} {unit}")))
}

/// A line about the step under way. The first word is what flatpak is
/// doing, followed by "2/4" when there are several steps, and an ellipsis.
fn status(line: &str) -> Option<Progress> {
    let mut words = line.split_whitespace().peekable();
    let first = words.next()?;
    let (verb, mut marked) = without_ellipsis(first);
    if !VERBS.contains(&verb) {
        return None;
    }
    let mut found = Progress {
        verb: Some(verb.to_string()),
        ..Progress::default()
    };
    if !marked {
        let (word, ellipsis) = without_ellipsis(words.next()?);
        found.step = Some(step(word)?);
        marked = ellipsis;
    }
    if !marked {
        return None;
    }
    while let Some(word) = words.next() {
        if let Some(number) = word.strip_suffix('%') {
            found.percent = number.parse().ok().filter(|p| *p <= 100);
        } else if found.percent.is_some() && is_amount(word) {
            let unit = words.peek().and_then(|unit| unit.strip_suffix("/s"));
            if unit.is_some_and(|unit| UNITS.contains(&unit)) {
                found.speed = words.next().map(|unit| format!("{word} {unit}"));
            }
        } else if found.percent.is_some() && is_time(word) {
            found.left = Some(word.to_string());
        }
    }
    Some(found)
}

/// The ellipsis is three dots where the locale has no UTF-8.
fn without_ellipsis(word: &str) -> (&str, bool) {
    match word
        .strip_suffix('\u{2026}')
        .or_else(|| word.strip_suffix("..."))
    {
        Some(rest) => (rest, true),
        None => (word, false),
    }
}

fn step(word: &str) -> Option<(u32, u32)> {
    let (n, of) = word.split_once('/')?;
    let (n, of): (u32, u32) = (n.parse().ok()?, of.parse().ok()?);
    (n >= 1 && of >= n && of < 1000).then_some((n, of))
}

fn is_amount(word: &str) -> bool {
    word.len() <= 8
        && word.starts_with(|c: char| c.is_ascii_digit())
        && word.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// "00:11", or with hours.
fn is_time(word: &str) -> bool {
    word.len() <= 9
        && word.contains(':')
        && word
            .split(':')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// The files are what Flatpak 1.16.6 wrote on a pipe, installing and
/// updating from a repository of our own.
#[cfg(test)]
mod tests {
    use super::*;

    const INSTALL: &str = include_str!("../tests/fixtures/install.out");
    const ONE_STEP: &str = include_str!("../tests/fixtures/one-step.out");
    const UPDATE: &str = include_str!("../tests/fixtures/update.out");
    const NOTHING: &str = include_str!("../tests/fixtures/update-nothing.out");
    const NONINTERACTIVE: &str = include_str!("../tests/fixtures/noninteractive.out");

    /// What is known after each whole line, whenever that has changed.
    fn reports(output: &str) -> Vec<Progress> {
        let mut reader = Reader::default();
        let mut seen: Vec<Progress> = Vec::new();
        for line in output.split_inclusive('\n') {
            reader.feed(line.as_bytes());
            if !reader.now().is_empty() && seen.last() != Some(reader.now()) {
                seen.push(reader.now().clone());
            }
        }
        seen
    }

    fn percents(seen: &[Progress], step: Option<(u32, u32)>) -> Vec<u8> {
        seen.iter()
            .filter(|p| p.step == step)
            .filter_map(|p| p.percent)
            .collect()
    }

    #[test]
    fn an_install_goes_through_both_its_steps() {
        let seen = reports(INSTALL);
        for step in [(1, 2), (2, 2)] {
            let found = percents(&seen, Some(step));
            assert_eq!(found.first(), Some(&0), "{step:?}");
            assert_eq!(found.last(), Some(&100), "{step:?}");
            assert!(found.len() > 5, "{found:?}");
        }
        assert!(seen.iter().all(|p| p.verb.as_deref() == Some("Installing")));
        let first = seen.iter().find(|p| p.step == Some((1, 2))).unwrap();
        assert_eq!(first.size.as_deref(), Some("up to 40.0 MB"));
        let last = seen.last().unwrap();
        assert_eq!(last.step, Some((2, 2)));
        assert_eq!(last.detail(), "Step 2 of 2 \u{b7} up to 300.1 MB");
        assert_eq!(last.pace(), "20.0 MB/s \u{b7} 00:00 left");
    }

    #[test]
    fn a_line_with_everything() {
        let line = INSTALL
            .lines()
            .find(|l| l.starts_with("Installing 2/2") && l.contains("00:0"))
            .unwrap();
        let found = status(line).unwrap();
        assert_eq!(found.step, Some((2, 2)));
        assert!(found.percent.is_some());
        assert!(found.speed.unwrap().ends_with("MB/s"));
        assert!(found.left.unwrap().starts_with("00:"));
    }

    #[test]
    fn one_step_has_no_number() {
        let seen = reports(ONE_STEP);
        assert!(seen.iter().all(|p| p.step.is_none()));
        assert_eq!(percents(&seen, None).last(), Some(&100));
        let last = seen.last().unwrap();
        assert_eq!(last.detail(), "Installing \u{b7} up to 300.1 MB");
    }

    #[test]
    fn an_update_is_read_the_same_way() {
        let seen = reports(UPDATE);
        assert!(seen.iter().all(|p| p.verb.as_deref() == Some("Updating")));
        assert_eq!(percents(&seen, Some((2, 2))).last(), Some(&100));
        assert_eq!(seen.last().unwrap().size.as_deref(), Some("up to 300.1 MB"));
        assert!(reports(NOTHING).is_empty());
    }

    /// Why the bar never moved: asked not to ask questions this way,
    /// flatpak says which ref it is on and no number at all.
    #[test]
    fn noninteractive_flatpak_says_nothing_of_progress() {
        assert!(NONINTERACTIVE.contains("Installing app/"));
        assert!(reports(NONINTERACTIVE).is_empty());
    }

    #[test]
    fn pieces_of_any_size_give_the_same() {
        let mut whole = Reader::default();
        whole.feed(INSTALL.as_bytes());
        for size in [1, 7, 64] {
            let mut reader = Reader::default();
            for piece in INSTALL.as_bytes().chunks(size) {
                reader.feed(piece);
            }
            assert_eq!(reader.now(), whole.now());
        }
    }

    #[test]
    fn half_a_line_is_not_read() {
        let mut reader = Reader::default();
        reader.feed("Installing 1/2\u{2026} ##  40%\nInstalling 1/2\u{2026} ###  5".as_bytes());
        assert_eq!(reader.now().percent, Some(40));
        reader.feed(b"5%\n");
        assert_eq!(reader.now().percent, Some(55));
    }

    #[test]
    fn without_utf8_the_marks_are_plain() {
        let found = status("Updating 3/5... ###        31%  4.1 MB/s  01:02:03").unwrap();
        assert_eq!(found.step, Some((3, 5)));
        assert_eq!(found.percent, Some(31));
        assert_eq!(found.left.as_deref(), Some("01:02:03"));
    }

    #[test]
    fn no_numbers_means_nothing_is_claimed() {
        for line in [
            "",
            "Looking for matches\u{2026}",
            "Installing org.example.App/x86_64/stable 50%",
            "Installation complete.",
            "Required runtime for org.example.App/x86_64/stable found in remote flathub",
        ] {
            assert_eq!(status(line), None, "{line}");
            assert_eq!(row(line), None, "{line}");
        }
        assert_eq!(status("Installing\u{2026} 250%").unwrap().percent, None);
    }

    #[test]
    fn a_line_without_end_is_dropped() {
        let mut reader = Reader::default();
        reader.feed(&vec![b'x'; 100_000]);
        assert!(reader.line.len() <= MAX_LINE);
        reader.feed("\nInstalling\u{2026} 3%\n".as_bytes());
        assert_eq!(reader.now().percent, Some(3));
    }
}
