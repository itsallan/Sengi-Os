//! The command line: which page to open, and the two things the session's
//! start-up scripts ask of this program without opening a window.

pub const USAGE: &str = "\
Usage: sengi-settings [PAGE]
       sengi-settings --get SECTION.KEY
       sengi-settings --apply

Open Sengi OS Settings, at PAGE if one is named. If Settings is already
open, that window is brought to the page instead.

Pages: network, bluetooth, sound, display, power, date-time, keyboard,
       mouse, notifications, about

  --get SECTION.KEY  print the setting in effect, for scripts
  --apply            write the files other programs read from the settings
  -h, --help         show this help
  -V, --version      show the version
";

/// The pages, in sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Page {
    Network,
    Bluetooth,
    Sound,
    Display,
    Power,
    DateTime,
    Keyboard,
    Mouse,
    Notifications,
    About,
}

impl Page {
    pub const ALL: [Page; 10] = [
        Page::Network,
        Page::Bluetooth,
        Page::Sound,
        Page::Display,
        Page::Power,
        Page::DateTime,
        Page::Keyboard,
        Page::Mouse,
        Page::Notifications,
        Page::About,
    ];

    /// The name on the command line and on the instance socket.
    pub fn name(self) -> &'static str {
        match self {
            Page::Network => "network",
            Page::Bluetooth => "bluetooth",
            Page::Sound => "sound",
            Page::Display => "display",
            Page::Power => "power",
            Page::DateTime => "date-time",
            Page::Keyboard => "keyboard",
            Page::Mouse => "mouse",
            Page::Notifications => "notifications",
            Page::About => "about",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Page::Network => "Network",
            Page::Bluetooth => "Bluetooth",
            Page::Sound => "Sound",
            Page::Display => "Display",
            Page::Power => "Power",
            Page::DateTime => "Date & Time",
            Page::Keyboard => "Keyboard",
            Page::Mouse => "Mouse & Touchpad",
            Page::Notifications => "Notifications",
            Page::About => "About",
        }
    }

    /// A page by its name, or by one of the other words people try.
    pub fn parse(name: &str) -> Option<Page> {
        let name = name.to_ascii_lowercase();
        Page::ALL
            .into_iter()
            .find(|page| page.name() == name)
            .or(match name.as_str() {
                "wifi" | "wi-fi" | "wireless" => Some(Page::Network),
                "audio" | "volume" => Some(Page::Sound),
                "displays" | "screen" | "monitor" => Some(Page::Display),
                "battery" => Some(Page::Power),
                "datetime" | "date" | "time" | "clock" => Some(Page::DateTime),
                "touchpad" | "pointer" => Some(Page::Mouse),
                "notification" => Some(Page::Notifications),
                _ => None,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Open the window, at a page if one was named.
    Open(Option<Page>),
    /// Print one setting.
    Get {
        section: String,
        key: String,
    },
    /// Write the derived files and exit.
    Apply,
    /// Open the window over made-up services. Debug builds only, and not
    /// in the usage text.
    #[cfg(debug_assertions)]
    Demo(String),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let action = match args.next().as_deref() {
        None => Action::Open(None),
        Some("-h" | "--help") => Action::Help,
        Some("-V" | "--version") => Action::Version,
        Some("--apply") => Action::Apply,
        Some("--get") => {
            let name = args.next().ok_or("--get needs SECTION.KEY")?;
            let (section, key) = name
                .split_once('.')
                .ok_or("--get needs SECTION.KEY, for example power.lock-after")?;
            Action::Get {
                section: section.to_string(),
                key: key.to_string(),
            }
        }
        #[cfg(debug_assertions)]
        Some("--demo") => match args.next() {
            Some(name) => Action::Demo(name),
            None => return Err("--demo needs the name of a scenario".to_string()),
        },
        Some(option) if option.starts_with('-') => return Err(format!("unknown option: {option}")),
        Some(name) => match Page::parse(name) {
            Some(page) => Action::Open(Some(page)),
            None => return Err(format!("no page called {name}")),
        },
    };
    match args.next() {
        Some(extra) => Err(format!("unexpected argument: {extra}")),
        None => Ok(action),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Action, String> {
        parse(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn no_arguments_opens_the_window() {
        assert_eq!(parsed(&[]), Ok(Action::Open(None)));
    }

    #[test]
    fn every_page_can_be_named() {
        for page in Page::ALL {
            assert_eq!(parsed(&[page.name()]), Ok(Action::Open(Some(page))));
            assert_eq!(Page::parse(page.name()), Some(page));
            assert!(USAGE.contains(page.name()));
            assert!(!page.title().is_empty());
        }
        assert_eq!(parsed(&["WiFi"]), Ok(Action::Open(Some(Page::Network))));
        assert_eq!(parsed(&["touchpad"]), Ok(Action::Open(Some(Page::Mouse))));
        assert_eq!(parsed(&["time"]), Ok(Action::Open(Some(Page::DateTime))));
    }

    #[test]
    fn options() {
        assert_eq!(parsed(&["--help"]), Ok(Action::Help));
        assert_eq!(parsed(&["-V"]), Ok(Action::Version));
        assert_eq!(parsed(&["--apply"]), Ok(Action::Apply));
        assert_eq!(
            parsed(&["--get", "power.lock-after"]),
            Ok(Action::Get {
                section: "power".to_string(),
                key: "lock-after".to_string()
            })
        );
    }

    #[test]
    fn mistakes_are_refused() {
        assert!(parsed(&["nowhere"]).is_err());
        assert!(parsed(&["--frobnicate"]).is_err());
        assert!(parsed(&["network", "sound"]).is_err());
        assert!(parsed(&["--get"]).is_err());
        assert!(parsed(&["--get", "nodot"]).is_err());
        assert!(parsed(&["--apply", "now"]).is_err());
    }
}
