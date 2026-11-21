//! What the installing program says while it works: one line on standard
//! output for each thing that happens. The window reads them; so can a
//! person or a script.
//!
//! ```text
//! begin copy
//! percent copy 42
//! end copy
//! warning The firmware would not take a boot entry
//! done 73
//! failed partition sfdisk ended with status 1
//! log /var/log/sengi-install.log
//! disk /dev/sda
//! ```

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A step has started. Steps are named by their id.
    Begin(String),
    /// How far a step is, where it can tell.
    Percent(String, u8),
    End(String),
    /// Something went wrong that the installation survives.
    Warning(String),
    /// All of it is done, after this many seconds.
    Done(u64),
    /// The step that failed and why. Nothing follows but the log.
    Failed(String, String),
    /// Where the full record is.
    Log(String),
    /// The disk that is being installed on. Said first, so that a window
    /// opened while an installation runs knows what it is looking at.
    Disk(String),
}

/// One line: what is said never breaks the framing.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Event {
    pub fn line(&self) -> String {
        match self {
            Event::Begin(step) => format!("begin {step}"),
            Event::Percent(step, percent) => format!("percent {step} {}", (*percent).min(100)),
            Event::End(step) => format!("end {step}"),
            Event::Warning(text) => format!("warning {}", flat(text)),
            Event::Done(seconds) => format!("done {seconds}"),
            Event::Failed(step, why) => format!("failed {step} {}", flat(why)),
            Event::Log(path) => format!("log {path}"),
            Event::Disk(path) => format!("disk {path}"),
        }
    }

    /// The event a line stands for. `None` for anything else, which a
    /// reader skips: a later version may say more.
    pub fn parse(line: &str) -> Option<Event> {
        let line = line.trim_end_matches(['\r', '\n']);
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        let step = |text: &str| {
            let fine = !text.is_empty()
                && text.len() <= 32
                && text.chars().all(|c| c.is_ascii_lowercase() || c == '-');
            fine.then(|| text.to_string())
        };
        match word {
            "begin" => step(rest).map(Event::Begin),
            "end" => step(rest).map(Event::End),
            "percent" => {
                let (name, percent) = rest.split_once(' ')?;
                let percent = percent.parse::<u8>().ok().filter(|p| *p <= 100)?;
                Some(Event::Percent(step(name)?, percent))
            }
            "warning" if !rest.is_empty() => Some(Event::Warning(rest.to_string())),
            "done" => rest.parse().ok().map(Event::Done),
            "failed" => {
                let (name, why) = rest.split_once(' ').unwrap_or((rest, ""));
                Some(Event::Failed(step(name)?, why.to_string()))
            }
            "log" if rest.starts_with('/') => Some(Event::Log(rest.to_string())),
            "disk" if rest.starts_with("/dev/") && !rest.contains(char::is_whitespace) => {
                Some(Event::Disk(rest.to_string()))
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_survives_the_line_it_is_written_as() {
        for event in [
            Event::Begin("copy".to_string()),
            Event::Percent("copy".to_string(), 0),
            Event::Percent("copy".to_string(), 100),
            Event::End("boot-loader".to_string()),
            Event::Warning("The firmware would not take a boot entry".to_string()),
            Event::Done(73),
            Event::Failed(
                "partition".to_string(),
                "sfdisk ended with status 1".to_string(),
            ),
            Event::Failed("check".to_string(), String::new()),
            Event::Log("/var/log/sengi-install.log".to_string()),
            Event::Disk("/dev/nvme0n1".to_string()),
        ] {
            assert_eq!(
                Event::parse(&event.line()),
                Some(event.clone()),
                "{event:?}"
            );
            assert!(!event.line().contains('\n'));
        }
    }

    #[test]
    fn what_is_said_stays_on_one_line() {
        let event = Event::Failed(
            "copy".to_string(),
            "cp: error\nwriting  file\r\n".to_string(),
        );
        assert_eq!(event.line(), "failed copy cp: error writing file");
        assert_eq!(
            Event::Percent("copy".to_string(), 250).line(),
            "percent copy 100"
        );
    }

    #[test]
    fn lines_that_are_not_events_are_skipped() {
        for line in [
            "",
            "hello",
            "begin",
            "begin Two Words",
            "percent copy",
            "percent copy many",
            "percent copy 101",
            "done soon",
            "log relative/path",
            "disk sda",
            "disk /dev/sda and more",
            "warning",
            "failed",
            "begin ../../x",
        ] {
            assert_eq!(Event::parse(line), None, "{line:?}");
        }
        assert_eq!(
            Event::parse("end copy\r\n"),
            Some(Event::End("copy".to_string()))
        );
    }
}
