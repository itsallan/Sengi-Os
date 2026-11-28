//! Errors and warnings since the computer was started.
//!
//! The journal holds everything, the kernel's messages included, and an
//! account reads all of it only as a member of the `systemd-journal` or
//! `adm` group. The kernel's own buffer is closed to ordinary accounts too
//! on Debian. Nothing here tries to get round either: when a source is
//! closed the report says so.

use super::command::{self, Ran};
use super::redact::plain;
use super::sections::refused;
use super::{Out, Radios, Sources, COMMAND_LIMIT};
use crate::bus::Listing;

/// The most of a command's output that is kept before lines are chosen.
const MAX_OUTPUT: usize = 4 << 20;
/// Lines kept from the start of a long log and from its end.
const KEPT_FIRST: usize = 300;
const KEPT_LAST: usize = 300;
/// Lines kept about firmware.
const KEPT_FIRMWARE: usize = 200;
/// Where a line is cut.
const MAX_LINE: usize = 400;

/// Said of a source this account may not read.
const CLOSED_TO_US: &str = "refused: this account has no permission to read it";

/// What a log source gave.
enum Log {
    /// Its messages, and why there may be more that were not given.
    Read {
        text: String,
        short: Option<&'static str>,
    },
    Closed(String),
}

fn log(sources: &Sources, program: &str, args: &[&str]) -> Log {
    match command::run(&sources.path, program, args, COMMAND_LIMIT, MAX_OUTPUT) {
        Ran::Done { code, out, err } => {
            // journalctl says this when only a part is open to the asker.
            let partial = err.contains("not seeing messages");
            let empty = out
                .lines()
                .all(|line| line.trim().is_empty() || line.starts_with("-- "));
            if (code != Some(0) && refused(&err)) || (partial && empty) {
                Log::Closed(CLOSED_TO_US.to_string())
            } else if code != Some(0) && empty {
                let said = err.lines().find(|l| !l.trim().is_empty());
                Log::Closed(format!(
                    "{program} failed: {}",
                    said.unwrap_or("it did not say why").trim()
                ))
            } else {
                Log::Read {
                    text: out,
                    short: partial.then_some("only this account's own messages could be read"),
                }
            }
        }
        Ran::Missing => Log::Closed(format!("{program} is not installed")),
        Ran::NotStarted(error) => Log::Closed(format!("{program} could not be started: {error}")),
        Ran::TimedOut { out } if !out.trim().is_empty() => Log::Read {
            text: out,
            short: Some("reading took too long, so this is only the part that arrived"),
        },
        Ran::TimedOut { .. } => Log::Closed(format!(
            "{program} did not finish within {} s",
            COMMAND_LIMIT.as_secs()
        )),
    }
}

/// The services whose messages name networks or devices. If their names
/// could not be listed to be taken out, their messages are left out.
fn unsafe_services(radios: &Radios) -> Vec<&'static str> {
    let mut services = Vec::new();
    if !matches!(radios.wifi, Listing::Objects(_)) {
        services.extend([" iwd[", " wpa_supplicant["]);
    }
    if !matches!(radios.bluetooth, Listing::Objects(_)) {
        services.push(" bluetoothd[");
    }
    services
}

/// The first and last lines of `lines`, each cut to length.
fn chosen(lines: &[&str], first: usize, last: usize) -> String {
    let mut kept = String::new();
    let skipped = lines.len().saturating_sub(first + last);
    for (number, line) in lines.iter().enumerate() {
        if skipped > 0 && number == first {
            kept.push_str(&format!("[... {skipped} lines left out ...]\n"));
        }
        if number >= first && number < first + skipped {
            continue;
        }
        kept.extend(plain(line).chars().take(MAX_LINE));
        kept.push('\n');
    }
    kept
}

/// Write one part: what the journal says, or failing that what the kernel's
/// buffer says, keeping the lines `wanted` picks.
#[allow(clippy::too_many_arguments)]
fn part(
    out: &mut Out,
    sources: &Sources,
    radios: &Radios,
    what: &str,
    journal: &[&str],
    kernel: &[&str],
    wanted: fn(&str) -> bool,
    (first, last): (usize, usize),
) {
    out.line(format!("{what}:"));
    let (text, short) = match log(sources, "journalctl", journal) {
        Log::Read { text, short } => (text, short),
        Log::Closed(why) => {
            out.unread(&format!("{what}, from the journal"), &why);
            match log(sources, "dmesg", kernel) {
                Log::Read { text, short } => {
                    out.line("From the kernel's buffer (dmesg) instead:");
                    (text, short)
                }
                Log::Closed(why) => {
                    out.unread(&format!("{what}, from the kernel (dmesg)"), &why);
                    return;
                }
            }
        }
    };
    if let Some(short) = short {
        out.unread(what, short);
    }
    let unsafe_services = unsafe_services(radios);
    let mut left_out = 0;
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.starts_with("-- ") && wanted(line))
        .filter(|line| {
            let named = unsafe_services.iter().any(|s| line.contains(s));
            left_out += usize::from(named);
            !named
        })
        .collect();
    if lines.is_empty() {
        out.line("none");
    } else {
        out.line(chosen(&lines, first, last).trim_end());
    }
    if left_out > 0 {
        out.line(format!(
            "[{left_out} messages of Wi-Fi or Bluetooth services left out: the names in them \
             could not be listed to be removed]"
        ));
    }
}

fn about_firmware(line: &str) -> bool {
    // Anything under /sys/firmware is named so whatever it is about.
    let line = line.to_ascii_lowercase().replace("/sys/firmware/", "");
    line.contains("firmware") || line.contains("microcode")
}

pub fn errors(sources: &Sources, radios: &Radios) -> Out {
    let mut parts = Out::default();
    messages(&mut parts, sources, radios);
    let mut out = Out::default();
    if parts.unread.iter().any(|what| what.contains(CLOSED_TO_US)) {
        out.line(
            "Messages are missing below because this account is not allowed to read the \
             system log or the kernel's messages. Nothing was done to get round that.",
        );
        out.line("");
    }
    out.text.push_str(&parts.text);
    out.unread = parts.unread;
    out
}

/// What the logs say, part by part.
fn messages(out: &mut Out, sources: &Sources, radios: &Radios) {
    part(
        out,
        sources,
        radios,
        "Kernel messages about firmware",
        &[
            "--dmesg",
            "--boot",
            "--no-pager",
            "--no-hostname",
            "--output=short-monotonic",
        ],
        &["--color=never"],
        about_firmware,
        (KEPT_FIRMWARE, 0),
    );
    out.line("");
    part(
        out,
        sources,
        radios,
        "Errors and warnings",
        &[
            "--boot",
            "--priority=warning",
            "--no-pager",
            "--no-hostname",
            "--output=short-monotonic",
        ],
        &["--color=never", "--level=emerg,alert,crit,err,warn"],
        |_| true,
        (KEPT_FIRST, KEPT_LAST),
    );
}
