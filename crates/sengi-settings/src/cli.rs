//! The command line: which page to open, and the two things the session's
//! start-up scripts ask of this program without opening a window.

pub const USAGE: &str = "\
Usage: sengi-settings [PAGE]
       sengi-settings --get SECTION.KEY
       sengi-settings --set SECTION.KEY VALUE
       sengi-settings --apply
       sengi-settings --wallpaper
       sengi-settings --screen off|on

Open Sengi OS Settings, at PAGE if one is named. If Settings is already
open, that window is brought to the page instead.

Pages: network, bluetooth, sound, display, wallpaper, power, date-time,
       keyboard, mouse, printers, notifications, updates, accessibility, about

  --get SECTION.KEY  print the setting in effect, for scripts
  --set SECTION.KEY VALUE
                     change a setting as the window would: saved, written
                     out for the session and taken up by it at once.
                     Lists are given with commas, flags as true or false.
                     The idle times in [power] take effect at the next login
  --apply            write the files other programs read from the settings
  --wallpaper       print the picture to show behind the desktop, if there is
                     one that can be shown, for the session's start-up
  --screen off|on    switch every display off, or back on
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
    Wallpaper,
    Power,
    DateTime,
    Keyboard,
    Mouse,
    Printers,
    Notifications,
    Updates,
    Accessibility,
    About,
}

impl Page {
    pub const ALL: [Page; 13] = [
        Page::Network,
        Page::Bluetooth,
        Page::Sound,
        Page::Display,
        Page::Wallpaper,
        Page::Power,
        Page::DateTime,
        Page::Keyboard,
        Page::Mouse,
        Page::Printers,
        Page::Notifications,
        Page::Updates,
        Page::Accessibility,
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
            Page::Printers => "printers",
            Page::Notifications => "notifications",
            Page::Accessibility => "accessibility",
            Page::Wallpaper => "wallpaper",
            Page::Updates => "updates",
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
            Page::Printers => "Printers",
            Page::Notifications => "Notifications",
            Page::Accessibility => "Accessibility",
            Page::Wallpaper => "Appearance",
            Page::Updates => "Updates",
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
                "printer" | "printing" | "print" => Some(Page::Printers),
                "notification" => Some(Page::Notifications),
                "update" | "software-update" | "upgrade" => Some(Page::Updates),
                "a11y" | "access" => Some(Page::Accessibility),
                "background" | "desktop-background" | "appearance" | "theme" => {
                    Some(Page::Wallpaper)
                }
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
    /// Change one setting and have the session take it up.
    Set {
        section: String,
        key: String,
        value: String,
    },
    /// Write the derived files and exit.
    Apply,
    /// Print the picture to show behind the desktop, or nothing.
    Wallpaper,
    /// Switch the displays on or off and exit.
    Screen(bool),
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
        Some("--wallpaper") => Action::Wallpaper,
        Some("--screen") => match args.next().as_deref() {
            Some("on") => Action::Screen(true),
            Some("off") => Action::Screen(false),
            _ => return Err("--screen takes on or off".to_string()),
        },
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
        Some("--set") => {
            let name = args.next().ok_or("--set needs SECTION.KEY and a value")?;
            let (section, key) = name
                .split_once('.')
                .ok_or("--set needs SECTION.KEY, for example mouse.natural-scroll")?;
            let value = args.next().ok_or("--set needs a value after SECTION.KEY")?;
            Action::Set {
                section: section.to_string(),
                key: key.to_string(),
                value,
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
    fn set_takes_a_name_and_a_value() {
        assert_eq!(
            parsed(&["--set", "keyboard.layouts", "us,de"]),
            Ok(Action::Set {
                section: "keyboard".to_string(),
                key: "layouts".to_string(),
                value: "us,de".to_string(),
            })
        );
        // An empty value is a value: it is how a text setting is cleared.
        assert!(parsed(&["--set", "keyboard.switch", ""]).is_ok());
        assert!(parsed(&["--set"]).is_err());
        assert!(parsed(&["--set", "keyboard.layouts"]).is_err());
        assert!(parsed(&["--set", "layouts", "us"]).is_err());
        assert!(parsed(&["--set", "keyboard.layouts", "us", "de"]).is_err());
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
        assert_eq!(parsed(&["--wallpaper"]), Ok(Action::Wallpaper));
        assert_eq!(
            parsed(&["background"]),
            Ok(Action::Open(Some(Page::Wallpaper)))
        );
        assert_eq!(parsed(&["--screen", "off"]), Ok(Action::Screen(false)));
        assert_eq!(parsed(&["--screen", "on"]), Ok(Action::Screen(true)));
        assert!(parsed(&["--screen"]).is_err());
        assert!(parsed(&["--screen", "dim"]).is_err());
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
