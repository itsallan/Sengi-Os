//! The command line.

/// What a login starts unless told otherwise.
pub const SESSION: &str = "/usr/bin/sengi-session";

pub const USAGE: &str = "\
Usage: sengi-greeter [OPTION]

The Sengi OS login screen. It is started by greetd, inside a compositor,
and asks greetd to log a user in.

  -s, --session COMMAND  the program a login starts, without arguments
                         (default: /usr/bin/sengi-session)
  -h, --help             show this help
  -V, --version          show the version

Exit status: 0 a session was granted and greetd starts it now, 1 greetd or
the compositor could not be reached or went away, 64 a bad command line.
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Show the login screen; a login starts `session`.
    Greet {
        session: String,
    },
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let mut session = None;
    let mut other = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-s" | "--session" if session.is_none() => match args.next() {
                Some(command) if command.starts_with('/') => session = Some(command),
                Some(command) => return Err(format!("not a full path: {command}")),
                None => return Err(format!("{arg} needs a command")),
            },
            "-h" | "--help" if other.is_none() => other = Some(Action::Help),
            "-V" | "--version" if other.is_none() => other = Some(Action::Version),
            "-s" | "--session" | "-h" | "--help" | "-V" | "--version" => {
                return Err(format!("unexpected argument: {arg}"))
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    match (other, session) {
        (Some(_), Some(_)) => Err("unexpected argument: --session".to_string()),
        (Some(action), None) => Ok(action),
        (None, session) => Ok(Action::Greet {
            session: session.unwrap_or_else(|| SESSION.to_string()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Action, String> {
        parse(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn what_it_takes() {
        assert_eq!(
            parsed(&[]),
            Ok(Action::Greet {
                session: "/usr/bin/sengi-session".to_string()
            })
        );
        assert_eq!(
            parsed(&["--session", "/bin/true"]),
            Ok(Action::Greet {
                session: "/bin/true".to_string()
            })
        );
        assert_eq!(parsed(&["-h"]), Ok(Action::Help));
        assert_eq!(parsed(&["--version"]), Ok(Action::Version));
    }

    #[test]
    fn what_it_does_not() {
        // No password and no user name is ever taken from here.
        for args in [
            &["--password", "x"][..],
            &["live"],
            &["--session"],
            &["--session", "true"],
            &["-s", "/a", "-s", "/b"],
            &["-h", "-V"],
            &["-h", "-s", "/a"],
        ] {
            assert!(parsed(args).is_err(), "{args:?}");
        }
    }
}
