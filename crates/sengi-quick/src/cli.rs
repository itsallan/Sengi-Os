//! The command line. There is next to nothing on it: the bar runs
//! `sengi-quick` to open the panel and again to close it.

pub const USAGE: &str = "\
Usage: sengi-quick [OPTION]

Open the Sengi OS quick settings panel, or close it if it is open.

  -h, --help     show this help
  -V, --version  show the version
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Open the panel, or close the one that is open.
    Toggle,
    /// The same, over made-up services. Not in the usage text: it is for
    /// looking at the design, see `demo`.
    Demo(String),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let action = match args.next().as_deref() {
        None => Action::Toggle,
        Some("-h" | "--help") => Action::Help,
        Some("-V" | "--version") => Action::Version,
        Some("--demo") => match args.next() {
            Some(name) => Action::Demo(name),
            None => return Err("--demo needs the name of a scenario".to_string()),
        },
        Some(other) => return Err(format!("unknown argument: {other}")),
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
    fn no_arguments_toggles_the_panel() {
        assert_eq!(parsed(&[]), Ok(Action::Toggle));
    }

    #[test]
    fn options() {
        assert_eq!(parsed(&["--help"]), Ok(Action::Help));
        assert_eq!(parsed(&["-V"]), Ok(Action::Version));
        assert_eq!(
            parsed(&["--demo", "connected"]),
            Ok(Action::Demo("connected".to_string()))
        );
    }

    #[test]
    fn mistakes_are_refused() {
        assert!(parsed(&["--demo"]).is_err());
        assert!(parsed(&["--frobnicate"]).is_err());
        assert!(parsed(&["--help", "more"]).is_err());
        assert!(parsed(&["--demo", "connected", "more"]).is_err());
    }
}
