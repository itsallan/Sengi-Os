//! Reading what the CUPS tools print. All of it is read with `LC_ALL=C`,
//! so the words are CUPS's own and not a translation. Nothing that does
//! not look like what the tool prints is passed on: a line that cannot be
//! understood is dropped, and a name that would not pass
//! [`crate::valid`] is never returned, so that whatever comes out of here
//! can be handed back to a tool.

use crate::valid;

/// What a queue is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Idle,
    /// Printing, and which job when CUPS says.
    Printing(Option<String>),
    /// Stopped because the printer cannot be reached.
    Offline(String),
    /// Stopped for another reason, or paused.
    Stopped(String),
}

impl State {
    /// A word for the list.
    pub fn word(&self) -> &'static str {
        match self {
            State::Idle => "Idle",
            State::Printing(_) => "Printing",
            State::Offline(_) => "Offline",
            State::Stopped(_) => "Paused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    pub state: State,
    /// Where it is connected, as `lpstat -v` says.
    pub uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// `Printer-12`, as `cancel` takes it.
    pub id: String,
    pub printer: String,
    pub user: String,
    pub bytes: u64,
    /// The date and time as CUPS wrote them.
    pub when: String,
}

/// A printer found on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The name the printer gives itself, for showing.
    pub title: String,
    pub uri: String,
}

/// What `lpstat -d -p -v` said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub printers: Vec<Printer>,
    pub default: Option<String>,
}

/// Words in a reason that say the printer cannot be reached.
const OFFLINE_WORDS: [&str; 11] = [
    "offline",
    "connect",
    "unable",
    "unreachable",
    "unavailable",
    "may not exist",
    "not responding",
    "timed out",
    "no route",
    "refused",
    "network",
];

fn says_unreachable(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    OFFLINE_WORDS.iter().any(|w| lower.contains(w))
}

fn clean(text: &str, max: usize) -> String {
    sengi_text::line(text, max)
}

/// Reads the output of `lpstat -d -p -v`.
pub fn parse_status(text: &str) -> Status {
    let mut status = Status::default();
    let mut uris: Vec<(String, String)> = Vec::new();
    // The printer that the indented reason lines belong to.
    let mut reason_for: Option<usize> = None;
    for raw in text.lines() {
        if raw.starts_with([' ', '\t']) {
            // What CUPS says after the line of a printer: the reason that
            // follows "disabled since", or, for a printer that is idle or
            // printing, what it has to complain of.
            if let Some(i) = reason_for {
                let said = clean(raw, 120);
                match &mut status.printers[i].state {
                    State::Stopped(reason) => {
                        if reason.is_empty() {
                            *reason = said;
                        }
                    }
                    State::Idle | State::Printing(_) => {
                        if says_unreachable(&said) {
                            status.printers[i].state = State::Offline(said);
                        }
                    }
                    State::Offline(_) => {}
                }
            }
            continue;
        }
        reason_for = None;
        if let Some(rest) = raw.strip_prefix("system default destination: ") {
            let name = rest.trim();
            if valid::printer_name(name) {
                status.default = Some(name.to_string());
            }
        } else if let Some(rest) = raw.strip_prefix("device for ") {
            if let Some((name, uri)) = rest.split_once(": ") {
                let (name, uri) = (name.trim(), uri.trim());
                if valid::printer_name(name) && !uri.is_empty() {
                    uris.push((name.to_string(), clean(uri, 300)));
                }
            }
        } else if let Some(rest) = raw.strip_prefix("printer ") {
            let (name, after) = rest.split_once(' ').unwrap_or((rest, ""));
            if !valid::printer_name(name) {
                continue;
            }
            let state = if let Some(job) = after.strip_prefix("now printing ") {
                let job = job
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('.');
                State::Printing(valid::job_id(job).then(|| job.to_string()))
            } else if after.starts_with("disabled since") || after.starts_with("is disabled") {
                // The reason is on the next line, or after the last
                // hyphen of this one.
                let inline = after
                    .rsplit_once(" - ")
                    .map(|(_, reason)| clean(reason, 120))
                    .unwrap_or_default();
                State::Stopped(inline)
            } else if after.starts_with("is ") {
                // "is idle", or "is processing" for a queue with no job
                // that CUPS names.
                State::Idle
            } else {
                continue;
            };
            status.printers.push(Printer {
                name: name.to_string(),
                state,
                uri: None,
            });
            reason_for = Some(status.printers.len() - 1);
        }
    }
    for (name, uri) in uris {
        if let Some(printer) = status.printers.iter_mut().find(|p| p.name == name) {
            printer.uri = Some(uri);
        }
    }
    // A stopped printer whose reason is about the connection is offline.
    for printer in &mut status.printers {
        if let State::Stopped(reason) = &printer.state {
            if says_unreachable(reason) {
                printer.state = State::Offline(reason.clone());
            }
        }
    }
    status
}

/// Reads the output of `lpstat -o`: `Name-12  user  1024  Thu 26 Nov ...`.
pub fn parse_jobs(text: &str) -> Vec<Job> {
    let mut jobs = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let (Some(id), Some(user), Some(size)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !valid::job_id(id) {
            continue;
        }
        let Ok(bytes) = size.parse::<u64>() else {
            continue;
        };
        let when: Vec<&str> = fields.collect();
        let Some((printer, _)) = id.rsplit_once('-') else {
            continue;
        };
        jobs.push(Job {
            id: id.to_string(),
            printer: printer.to_string(),
            user: clean(user, 40),
            bytes,
            when: clean(&when.join(" "), 60),
        });
    }
    jobs
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `%20` and its kind decoded; a `%` that starts nothing stays.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The name a `dnssd://` address carries: `Brother HL._ipp._tcp.local`
/// is "Brother HL".
fn dnssd_title(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("dnssd://")?;
    let host = rest.split(['/', '?']).next()?;
    let decoded = percent_decode(host);
    let name = decoded
        .split("._ipp")
        .next()
        .or_else(|| decoded.split("._pdl-datastream").next())?;
    let name = clean(name, 80);
    (!name.is_empty()).then_some(name)
}

/// The host of an `ipp://` address, for a title when there is no name.
fn host_title(uri: &str) -> Option<String> {
    let rest = uri.split_once("://")?.1;
    let host = rest.split(['/', '?']).next()?;
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    let host = clean(host, 80);
    (!host.is_empty()).then_some(host)
}

/// Reads the output of `lpinfo --include-schemes dnssd,ipp -v`: lines such
/// as `network dnssd://Name%20x._ipp._tcp.local/?uuid=...`. A printer
/// that shows up under several schemes is listed once, under the name
/// and the address of the first; `dnssd` is asked for first, since it
/// carries the printer's own name. Lines with no address (`network ipp`
/// stands for the scheme itself) are not printers.
pub fn parse_devices(text: &str) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    let mut lines: Vec<&str> = text.lines().collect();
    // dnssd first, so that its title wins over a bare address.
    lines.sort_by_key(|line| !line.contains(" dnssd://"));
    for line in lines {
        let mut fields = line.split_whitespace();
        let (Some(class), Some(uri), None) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if !matches!(class, "network" | "direct") || !valid::device_uri(uri) {
            continue;
        }
        let title = if uri.starts_with("dnssd://") {
            dnssd_title(uri)
        } else if uri.starts_with("ipp://") || uri.starts_with("ipps://") {
            host_title(uri)
        } else {
            None
        };
        let Some(title) = title else { continue };
        // The same printer by its own name and by its host name.
        let same = |a: &str, b: &str| {
            let bare = |t: &str| t.strip_suffix(".local").unwrap_or(t).to_ascii_lowercase();
            bare(a) == bare(b)
        };
        if found.iter().any(|f| same(&f.title, &title)) {
            continue;
        }
        found.push(Found {
            title,
            uri: uri.to_string(),
        });
    }
    found
}

/// A number of bytes in the words of a job list.
pub fn size_words(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = "\
system default destination: Office
printer Office is idle.  enabled since Thu 26 Nov 2026 10:00:00 AM CET
printer Brother_HL-L2350DW now printing Brother_HL-L2350DW-14.  enabled since Thu 26 Nov 2026 10:05:00 AM CET
printer Garage disabled since Thu 26 Nov 2026 09:00:00 AM CET -
\tUnable to connect to printer
printer Study disabled since Thu 26 Nov 2026 09:00:00 AM CET -
\tPaused
device for Office: ipp://192.168.1.50/ipp/print
device for Brother_HL-L2350DW: dnssd://Brother%20HL-L2350DW%20series._ipp._tcp.local/?uuid=abc
device for Garage: ipp://garage.local/ipp/print
";

    #[test]
    fn the_status_lists_every_printer_with_its_state_and_address() {
        let got = parse_status(STATUS);
        assert_eq!(got.default.as_deref(), Some("Office"));
        let names: Vec<&str> = got.printers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Office", "Brother_HL-L2350DW", "Garage", "Study"]);
        assert_eq!(got.printers[0].state, State::Idle);
        assert_eq!(
            got.printers[0].uri.as_deref(),
            Some("ipp://192.168.1.50/ipp/print")
        );
        assert_eq!(
            got.printers[1].state,
            State::Printing(Some("Brother_HL-L2350DW-14".to_string()))
        );
        assert!(got.printers[1]
            .uri
            .as_deref()
            .unwrap()
            .starts_with("dnssd://"));
        assert_eq!(
            got.printers[2].state,
            State::Offline("Unable to connect to printer".to_string())
        );
        assert_eq!(got.printers[3].state, State::Stopped("Paused".to_string()));
        assert_eq!(got.printers[2].state.word(), "Offline");
        assert_eq!(got.printers[3].state.word(), "Paused");
    }

    // Taken from CUPS 2.4.10 (Debian 13) with a queue whose printer was
    // not there.
    #[test]
    fn a_printer_that_is_printing_but_cannot_be_reached_is_offline() {
        let got = parse_status(
            "system default destination: Fake_Printer\n\
             printer Fake_Printer now printing Fake_Printer-2.  enabled since Thu Nov 26 01:43:39 2026\n\
             \tThe printer may not exist or is unavailable at this time.\n\
             device for Fake_Printer: ipp://localhost:8631/ipp/print\n",
        );
        assert_eq!(
            got.printers[0].state,
            State::Offline("The printer may not exist or is unavailable at this time.".to_string())
        );
        assert_eq!(got.default.as_deref(), Some("Fake_Printer"));
    }

    #[test]
    fn a_paused_queue_as_cups_prints_it() {
        let got = parse_status(
            "printer Fake_Printer disabled since Thu Nov 26 01:43:39 2026 -\n\tPaused\n",
        );
        assert_eq!(got.printers[0].state, State::Stopped("Paused".to_string()));
    }

    #[test]
    fn a_reason_on_the_same_line_is_found_too() {
        let got = parse_status("printer A disabled since Mon 1 Jan 2026 -  Paused\n");
        assert_eq!(got.printers[0].state, State::Stopped("Paused".to_string()));
    }

    #[test]
    fn no_default_and_no_printers_are_nothing() {
        let got = parse_status("no system default destination\n");
        assert_eq!(got, Status::default());
        assert_eq!(parse_status(""), Status::default());
    }

    #[test]
    fn lines_that_are_not_printers_or_have_unsafe_names_are_dropped() {
        let got = parse_status(
            "printer -x is idle.\nprinter a;b is idle.\nprinter is idle.\n\
             printer Good is idle.\nprinter Odd weird words\nsystem default destination: -o\n\
             device for Good: \ndevice for : ipp://x/\nrandom words\n",
        );
        assert_eq!(got.printers.len(), 1);
        assert_eq!(got.printers[0].name, "Good");
        assert_eq!(got.printers[0].uri, None);
        assert_eq!(got.default, None);
    }

    #[test]
    fn the_job_list_has_the_id_the_user_and_the_size() {
        let jobs = parse_jobs(
            "Office-12  allan  1024  Thu 26 Nov 2026 10:00:00 AM CET\n\
             Brother_HL-L2350DW-104 allan 2097152 Thu 26 Nov 2026 10:01:00 AM CET\n",
        );
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, "Office-12");
        assert_eq!(jobs[0].printer, "Office");
        assert_eq!(jobs[0].user, "allan");
        assert_eq!(jobs[0].bytes, 1024);
        assert_eq!(jobs[0].when, "Thu 26 Nov 2026 10:00:00 AM CET");
        assert_eq!(jobs[1].printer, "Brother_HL-L2350DW");
    }

    #[test]
    fn job_lines_that_do_not_fit_are_dropped() {
        let jobs = parse_jobs(
            "\nnot a job\nOffice-12 allan\nOffice-x allan 10 now\n-1 allan 10 now\n\
             Office-3 allan big now\nOffice-4 allan 10 now\n",
        );
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "Office-4");
    }

    #[test]
    fn hostile_text_in_a_job_line_is_cleaned() {
        let jobs = parse_jobs("Office-1 a\u{202e}llan 5 x\u{200b}y\n");
        assert_eq!(jobs.len(), 1);
        assert!(!jobs[0].user.contains('\u{202e}'));
        assert!(!jobs[0].when.contains('\u{200b}'));
    }

    #[test]
    fn percent_encoding_is_undone() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%2"), "%zz%2");
        assert_eq!(percent_decode("%C3%BC"), "\u{fc}");
        assert_eq!(percent_decode(""), "");
    }

    const DEVICES: &str = "\
network ipp
network ipps
network lpd
network dnssd
network dnssd://Brother%20HL-L2350DW%20series._ipp._tcp.local/?uuid=e3248000
network ipp://BRW3C2AF4.local:631/ipp/print
network ipps://brother.local/ipp/print
network dnssd://Office%20Laser._ipps._tcp.local/?uuid=f00
direct usb://HP/LaserJet
network socket://10.0.0.9:9100
";

    #[test]
    fn network_printers_are_listed_by_the_name_they_give_themselves() {
        let found = parse_devices(DEVICES);
        let titles: Vec<&str> = found.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Brother HL-L2350DW series",
                "Office Laser",
                "BRW3C2AF4.local",
                "brother.local"
            ]
        );
        assert!(found[0].uri.starts_with("dnssd://"));
    }

    #[test]
    fn a_printer_found_twice_is_listed_once_with_its_own_name() {
        let found = parse_devices(
            "network ipp://Office.local/ipp/print\n\
             network dnssd://Office._ipp._tcp.local/?uuid=1\n",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Office");
        assert!(found[0].uri.starts_with("dnssd://"));
    }

    #[test]
    fn unsafe_device_lines_are_not_printers() {
        let found = parse_devices(
            "network file:///etc/passwd\nnetwork ipp://x/$(id)\nnetwork ipp://host/a b\n\
             direct\n\nnetwork dnssd://%0A._ipp._tcp.local/\nerror something\n",
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn sizes_are_in_plain_units() {
        assert_eq!(size_words(512), "512 bytes");
        assert_eq!(size_words(2048), "2 KB");
        assert_eq!(size_words(3 * 1024 * 1024), "3.0 MB");
    }
}
