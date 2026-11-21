//! The command line.

pub const USAGE: &str = "\
Usage: sengi-lock [OPTION]

Lock the session. It is unlocked with the password of the user it belongs to.

  -f, --fork     return once the screen is locked, and stay locked in the
                 background; without it, return when it is unlocked
  -h, --help     show this help
  -V, --version  show the version

Exit status: 0 unlocked (with -f: locked), 2 the session is already locked
by another program or the compositor refused, 3 no compositor that can
lock, 1 anything else.
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Lock, and with `detach` return once locked.
    Lock {
        detach: bool,
    },
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Action, String> {
    let mut action = Action::Lock { detach: false };
    for arg in args {
        action = match (arg.as_str(), action) {
            ("-f" | "--fork", Action::Lock { .. }) => Action::Lock { detach: true },
            ("-h" | "--help", Action::Lock { detach: false }) => Action::Help,
            ("-V" | "--version", Action::Lock { detach: false }) => Action::Version,
            ("-f" | "--fork" | "-h" | "--help" | "-V" | "--version", _) => {
                return Err(format!("unexpected argument: {arg}"))
            }
            _ => return Err(format!("unknown argument: {arg}")),
        };
    }
    Ok(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Action, String> {
        parse(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn what_it_takes() {
        assert_eq!(parsed(&[]), Ok(Action::Lock { detach: false }));
        assert_eq!(parsed(&["-f"]), Ok(Action::Lock { detach: true }));
        assert_eq!(parsed(&["--fork"]), Ok(Action::Lock { detach: true }));
        assert_eq!(parsed(&["--help"]), Ok(Action::Help));
        assert_eq!(parsed(&["-V"]), Ok(Action::Version));
    }

    #[test]
    fn what_it_does_not() {
        // No password, user name or command is ever taken from here.
        for args in [
            &["--password", "x"][..],
            &["live"],
            &["-f", "-h"],
            &["-h", "-V"],
        ] {
            assert!(parsed(args).is_err(), "{args:?}");
        }
    }
}
