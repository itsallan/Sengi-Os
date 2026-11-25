//! What the helper tells whoever runs it: one JSON object to a line on
//! standard output. Settings reads them with [`Event::from_line`]. A line
//! it does not understand is ignored, so that a newer helper can say more
//! than an older Settings knows.

use crate::apt::Upgrade;
use crate::json::{self, Object};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `apt-get update`.
    Refresh,
    /// Fetching the packages. Can be cancelled.
    Download,
    /// dpkg at work. Cannot be.
    Install,
    /// Flatpak's own update.
    Flatpak,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Refresh => "refresh",
            Phase::Download => "download",
            Phase::Install => "install",
            Phase::Flatpak => "flatpak",
        }
    }

    pub fn parse(name: &str) -> Option<Phase> {
        [
            Phase::Refresh,
            Phase::Download,
            Phase::Install,
            Phase::Flatpak,
        ]
        .into_iter()
        .find(|phase| phase.name() == name)
    }

    /// For the page.
    pub fn label(self) -> &'static str {
        match self {
            Phase::Refresh => "Looking for updates",
            Phase::Download => "Downloading system updates",
            Phase::Install => "Installing system updates",
            Phase::Flatpak => "Updating apps",
        }
    }

    /// Whether a cancel is still welcome. After this, the update is
    /// allowed to finish.
    pub fn can_cancel(self) -> bool {
        !matches!(self, Phase::Install)
    }
}

/// How far an update has got, as far as the tools said.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub phase: Phase,
    /// 0 to 100, when the tool gave one.
    pub percent: Option<f32>,
    /// "n of m", when it gave that.
    pub step: Option<(u32, u32)>,
    /// What has been fetched: "12 MB of 36 MB".
    pub size: Option<String>,
    /// What the tool says it is doing.
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Phase(Phase),
    Progress(Progress),
    /// One package an update would install (the result of a check).
    Package(Upgrade),
    /// A check is over: how much would be downloaded, and when.
    Checked {
        download: Option<u64>,
        time: u64,
    },
    /// What stands in the way of an update.
    Status {
        live: bool,
        busy: bool,
        reboot_required: bool,
        battery: Option<u8>,
    },
    /// The update is over.
    Done {
        upgraded: u32,
        reboot_required: bool,
    },
    /// Stopped by request, before anything was installed.
    Cancelled,
    /// Not started, and why.
    Refused {
        code: String,
        message: String,
    },
    /// Started and did not work.
    Failed {
        message: String,
    },
}

impl Event {
    pub fn to_line(&self) -> String {
        let event = |name: &str| Object::new().str("event", name);
        match self {
            Event::Phase(phase) => event("phase").str("phase", phase.name()).finish(),
            Event::Progress(p) => {
                let mut line = event("progress").str("phase", p.phase.name());
                if let Some(percent) = p.percent {
                    line = line.num("percent", f64::from(percent));
                }
                if let Some((n, m)) = p.step {
                    line = line.num("step", f64::from(n)).num("steps", f64::from(m));
                }
                if let Some(size) = &p.size {
                    line = line.str("size", size);
                }
                line.str("detail", &p.detail).finish()
            }
            Event::Package(upgrade) => {
                let mut line = event("package")
                    .str("name", &upgrade.name)
                    .str("to", &upgrade.to)
                    .bool("security", upgrade.security);
                if let Some(from) = &upgrade.from {
                    line = line.str("from", from);
                }
                line.finish()
            }
            Event::Checked { download, time } => {
                let mut line = event("checked").num("time", *time as f64);
                if let Some(bytes) = download {
                    line = line.num("download", *bytes as f64);
                }
                line.finish()
            }
            Event::Status {
                live,
                busy,
                reboot_required,
                battery,
            } => {
                let mut line = event("status")
                    .bool("live", *live)
                    .bool("busy", *busy)
                    .bool("reboot_required", *reboot_required);
                if let Some(percent) = battery {
                    line = line.num("battery", f64::from(*percent));
                }
                line.finish()
            }
            Event::Done {
                upgraded,
                reboot_required,
            } => event("done")
                .num("upgraded", f64::from(*upgraded))
                .bool("reboot_required", *reboot_required)
                .finish(),
            Event::Cancelled => event("cancelled").finish(),
            Event::Refused { code, message } => event("refused")
                .str("code", code)
                .str("message", message)
                .finish(),
            Event::Failed { message } => event("failed").str("message", message).finish(),
        }
    }

    /// The event a line carries, or `None` for anything else: not JSON,
    /// an event this version does not know, or one missing what it needs.
    pub fn from_line(line: &str) -> Option<Event> {
        let fields = json::parse(line)?;
        let text = |key: &str, max: usize| {
            json::get_str(&fields, key).map(|text| sengi_text::line(text, max))
        };
        let count = |key: &str| {
            json::get_num(&fields, key)
                .filter(|n| *n >= 0.0 && *n < 4.0e9)
                .map(|n| n as u32)
        };
        match json::get_str(&fields, "event")? {
            "phase" => Phase::parse(json::get_str(&fields, "phase")?).map(Event::Phase),
            "progress" => Some(Event::Progress(Progress {
                phase: Phase::parse(json::get_str(&fields, "phase")?)?,
                percent: json::get_num(&fields, "percent")
                    .filter(|p| p.is_finite())
                    .map(|p| p.clamp(0.0, 100.0) as f32),
                step: count("step")
                    .zip(count("steps"))
                    .filter(|(n, m)| *n >= 1 && m >= n),
                size: text("size", 64),
                detail: text("detail", 160).unwrap_or_default(),
            })),
            "package" => Some(Event::Package(Upgrade {
                name: text("name", 80)?,
                from: text("from", 64),
                to: text("to", 64)?,
                security: json::get_bool(&fields, "security").unwrap_or(false),
            })),
            "checked" => Some(Event::Checked {
                download: json::get_num(&fields, "download")
                    .filter(|n| *n >= 0.0)
                    .map(|n| n as u64),
                time: json::get_num(&fields, "time")
                    .filter(|n| *n >= 0.0)
                    .map(|n| n as u64)
                    .unwrap_or(0),
            }),
            "status" => Some(Event::Status {
                live: json::get_bool(&fields, "live")?,
                busy: json::get_bool(&fields, "busy")?,
                reboot_required: json::get_bool(&fields, "reboot_required")?,
                battery: count("battery").map(|n| n.min(100) as u8),
            }),
            "done" => Some(Event::Done {
                upgraded: count("upgraded").unwrap_or(0),
                reboot_required: json::get_bool(&fields, "reboot_required").unwrap_or(false),
            }),
            "cancelled" => Some(Event::Cancelled),
            "refused" => Some(Event::Refused {
                code: text("code", 16)?,
                message: text("message", 300).unwrap_or_default(),
            }),
            "failed" => Some(Event::Failed {
                message: text("message", 300).unwrap_or_default(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(event: Event) {
        let line = event.to_line();
        assert!(!line.contains('\n'));
        assert_eq!(Event::from_line(&line), Some(event), "{line}");
    }

    #[test]
    fn every_event_survives_the_wire() {
        round_trip(Event::Phase(Phase::Download));
        round_trip(Event::Progress(Progress {
            phase: Phase::Install,
            percent: Some(45.5),
            step: Some((2, 7)),
            size: Some("12 MB of 36 MB".to_string()),
            detail: "Unpacking \"x\" ...".to_string(),
        }));
        round_trip(Event::Progress(Progress {
            phase: Phase::Flatpak,
            percent: None,
            step: None,
            size: None,
            detail: String::new(),
        }));
        round_trip(Event::Package(Upgrade {
            name: "libssl3t64".to_string(),
            from: Some("3.0.15".to_string()),
            to: "3.0.16".to_string(),
            security: true,
        }));
        round_trip(Event::Package(Upgrade {
            name: "libnew1".to_string(),
            from: None,
            to: "1.0".to_string(),
            security: false,
        }));
        round_trip(Event::Checked {
            download: Some(36_200_000),
            time: 1_700_000_000,
        });
        round_trip(Event::Status {
            live: false,
            busy: true,
            reboot_required: false,
            battery: Some(18),
        });
        round_trip(Event::Done {
            upgraded: 4,
            reboot_required: true,
        });
        round_trip(Event::Cancelled);
        round_trip(Event::Refused {
            code: "battery".to_string(),
            message: "The battery is at 12%.".to_string(),
        });
        round_trip(Event::Failed {
            message: "E: Unable to fetch".to_string(),
        });
    }

    #[test]
    fn lines_that_are_not_understood_are_ignored() {
        assert_eq!(Event::from_line("hello"), None);
        assert_eq!(Event::from_line(r#"{"event":"from-the-future"}"#), None);
        assert_eq!(
            Event::from_line(r#"{"event":"phase","phase":"coffee"}"#),
            None
        );
        assert_eq!(Event::from_line(r#"{"event":"package","name":"x"}"#), None);
        assert_eq!(Event::from_line(r#"{"no":"event"}"#), None);
        // Extra fields are fine: a newer helper may say more.
        assert_eq!(
            Event::from_line(r#"{"event":"cancelled","extra":1}"#),
            Some(Event::Cancelled)
        );
    }

    #[test]
    fn what_comes_from_outside_is_clamped_and_cleaned() {
        let Some(Event::Progress(p)) = Event::from_line(
            r#"{"event":"progress","phase":"download","percent":900,"step":5,"steps":2,"detail":"a\u001b[31mb"}"#,
        ) else {
            panic!("a progress event");
        };
        assert_eq!(p.percent, Some(100.0));
        // 5 of 2 is not a step.
        assert_eq!(p.step, None);
        assert!(!p.detail.contains('\u{1b}'));
    }

    #[test]
    fn cancelling_is_welcome_until_dpkg_starts() {
        assert!(Phase::Refresh.can_cancel());
        assert!(Phase::Download.can_cancel());
        assert!(!Phase::Install.can_cancel());
        assert!(Phase::Flatpak.can_cancel());
    }
}
