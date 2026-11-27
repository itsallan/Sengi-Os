//! What went wrong, in plain words. What flatpak said is for the log; the
//! window gets a sentence a person can act on.

use crate::flatpak::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// There is no way to Flathub: no network, no names, no route.
    Offline,
    /// The network answers too slowly or not at all.
    Slow,
    /// The secure connection could not be made.
    Secure,
    /// Flathub itself has trouble.
    Server,
    /// The disk is full.
    Space,
    /// There is no flatpak.
    Missing,
    /// The user's Flathub source is not the real one.
    Source,
    /// Anything else.
    Other,
}

/// What flatpak (and the libraries under it) says for each, looked for
/// without regard to case. The first line that fits wins, so the more
/// particular ones come first.
const SIGNS: [(Problem, &[&str]); 5] = [
    (
        Problem::Space,
        &["no space left", "not enough disk space", "disk quota"],
    ),
    (
        Problem::Secure,
        &[
            " ssl ",
            " tls ",
            "certificate",
            "[35]",
            "[60]",
            "secure connection",
        ],
    ),
    (
        Problem::Offline,
        &[
            "resolve host",
            "resolve proxy",
            "name or service not known",
            "name resolution",
            "could not connect",
            "couldn't connect",
            "failed to connect",
            "connection refused",
            "connection reset",
            "network is unreachable",
            "network unreachable",
            "no route to host",
            "[5]",
            "[6]",
            "[7]",
        ],
    ),
    (
        Problem::Slow,
        &[
            "timeout",
            "timed out",
            "operation too slow",
            "[28]",
            "recv failure",
            "transferred a partial file",
        ],
    ),
    (
        Problem::Server,
        &[
            "server returned status 5",
            "http 5",
            "service unavailable",
            "bad gateway",
        ],
    ),
];

impl Problem {
    pub fn of(error: &Error) -> Problem {
        match error {
            Error::Missing => Problem::Missing,
            Error::Refused(_) => Problem::Source,
            Error::TimedOut => Problem::Slow,
            Error::Failed(text) => Problem::read(text),
        }
    }

    fn read(text: &str) -> Problem {
        // Spaces for anything that is not part of a word, so that "tls" is
        // not found in the name of an app.
        let text: String = text
            .to_lowercase()
            .chars()
            .map(|c| if "():,.;/\"".contains(c) { ' ' } else { c })
            .collect();
        let text = format!(" {text} ");
        SIGNS
            .iter()
            .find(|(_, signs)| signs.iter().any(|sign| text.contains(sign)))
            .map_or(Problem::Other, |(problem, _)| *problem)
    }

    /// Whether the network coming back may put it right.
    pub fn network(self) -> bool {
        matches!(
            self,
            Problem::Offline | Problem::Slow | Problem::Secure | Problem::Server | Problem::Other
        )
    }

    /// The banner over the apps when the list could not be had.
    pub fn banner(self) -> &'static str {
        match self {
            Problem::Offline => {
                "You\u{2019}re offline. Connect to the internet to browse apps."
            }
            Problem::Slow => "The connection is too slow to reach Flathub right now.",
            Problem::Secure => {
                "Could not make a secure connection to Flathub. Check the date and time, then try again."
            }
            Problem::Server => "Flathub is having trouble right now. Try again in a while.",
            Problem::Space => "The disk is full. Free some space, then try again.",
            Problem::Missing => "Flatpak is not installed, so apps cannot be shown.",
            Problem::Source => {
                "The \u{201c}flathub\u{201d} source of this user is not Flathub or does not check signatures, so it is not used."
            }
            Problem::Other => "Could not get the list of apps.",
        }
    }

    /// The same when there is a list from an earlier time to show.
    pub fn stale(self) -> &'static str {
        match self {
            Problem::Offline => {
                "You\u{2019}re offline. These are the apps from last time; installing needs the internet."
            }
            Problem::Slow | Problem::Server | Problem::Other | Problem::Secure => {
                "Could not update the list of apps. These are the apps from last time."
            }
            other => other.banner(),
        }
    }

    /// Why an install did not happen.
    pub fn install(self, name: &str) -> String {
        let why = match self {
            Problem::Offline => "You\u{2019}re offline. Connect to the internet and try again.",
            Problem::Slow => "The connection is too slow or stopped. Try again.",
            Problem::Secure => "A secure connection could not be made. Check the date and time.",
            Problem::Server => "Flathub is having trouble. Try again in a while.",
            Problem::Space => "The disk is full.",
            Problem::Missing => "Flatpak is not installed.",
            Problem::Source => "The Flathub source of this user is not safe to use.",
            Problem::Other => "Something went wrong. Try again.",
        };
        format!("Could not install {name}. {why}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(text: &str) -> Problem {
        Problem::of(&Error::Failed(text.to_string()))
    }

    #[test]
    fn what_was_seen_on_a_laptop_with_no_network_is_offline() {
        assert_eq!(
            failed(
                "Unable to load summary from remote flathub: While fetching \
                 https://dl.flathub.org/repo/summary.idx: [6] Could not resolve hostname"
            ),
            Problem::Offline
        );
    }

    #[test]
    fn network_errors_are_told_apart() {
        for (text, problem) in [
            ("[7] Couldn't connect to server", Problem::Offline),
            ("Temporary failure in name resolution", Problem::Offline),
            ("connect: Network is unreachable", Problem::Offline),
            ("[28] Timeout was reached", Problem::Slow),
            ("Operation timed out after 30000 ms", Problem::Slow),
            (
                "[60] SSL peer certificate or SSH remote key was not OK",
                Problem::Secure,
            ),
            ("TLS handshake failed", Problem::Secure),
            ("Server returned status 503", Problem::Server),
            (
                "Error writing file: No space left on device",
                Problem::Space,
            ),
            ("Nothing matches org.x.Y in remote flathub", Problem::Other),
            (
                "Nothing matches com.usebottles.bottles in remote flathub",
                Problem::Other,
            ),
            ("OpenSSL: SSL connection error", Problem::Secure),
        ] {
            assert_eq!(failed(text), problem, "{text}");
        }
        assert_eq!(Problem::of(&Error::TimedOut), Problem::Slow);
        assert_eq!(Problem::of(&Error::Missing), Problem::Missing);
        assert_eq!(Problem::of(&Error::Refused("x".into())), Problem::Source);
    }

    #[test]
    fn no_sentence_carries_a_url_or_a_code() {
        for problem in [
            Problem::Offline,
            Problem::Slow,
            Problem::Secure,
            Problem::Server,
            Problem::Space,
            Problem::Missing,
            Problem::Source,
            Problem::Other,
        ] {
            for text in [
                problem.banner().to_string(),
                problem.stale().to_string(),
                problem.install("GIMP"),
            ] {
                assert!(!text.contains("http") && !text.contains('['), "{text}");
            }
        }
        assert_eq!(
            Problem::Offline.banner(),
            "You\u{2019}re offline. Connect to the internet to browse apps."
        );
        assert!(!Problem::Missing.network() && !Problem::Source.network());
    }
}
