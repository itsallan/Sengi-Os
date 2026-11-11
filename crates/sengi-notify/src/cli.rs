//! The command line: no arguments runs the service, one option tells the
//! running service to do something.

pub const USAGE: &str = "\
Usage: sengi-notify [OPTION]

With no option, runs the Sengi OS notification service.

Options, sent to the running service:
  --toggle-centre       open or close the notification centre
  --dismiss-all         dismiss every notification on screen
  --dnd on|off|toggle   set do-not-disturb
  --dnd status          print whether do-not-disturb is on or off
  -h, --help            show this help
  -V, --version         show the version
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dnd {
    On,
    Off,
    Toggle,
}

/// Something to ask of the running service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    ToggleCentre,
    DismissAll,
    Dnd(Dnd),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invocation {
    Serve,
    Send(Command),
    /// Print `on` or `off`.
    DndStatus,
    Help,
    Version,
}

pub fn parse<S: AsRef<str>>(arguments: &[S]) -> Result<Invocation, String> {
    let arguments: Vec<&str> = arguments.iter().map(AsRef::as_ref).collect();
    match arguments.as_slice() {
        [] => Ok(Invocation::Serve),
        ["-h" | "--help"] => Ok(Invocation::Help),
        ["-V" | "--version"] => Ok(Invocation::Version),
        ["--toggle-centre"] => Ok(Invocation::Send(Command::ToggleCentre)),
        ["--dismiss-all"] => Ok(Invocation::Send(Command::DismissAll)),
        ["--dnd", state] => match *state {
            "on" => Ok(Invocation::Send(Command::Dnd(Dnd::On))),
            "off" => Ok(Invocation::Send(Command::Dnd(Dnd::Off))),
            "toggle" => Ok(Invocation::Send(Command::Dnd(Dnd::Toggle))),
            "status" => Ok(Invocation::DndStatus),
            other => Err(format!(
                "--dnd takes on, off, toggle or status, not '{other}'"
            )),
        },
        ["--dnd"] => Err("--dnd takes on, off, toggle or status".to_string()),
        [other, ..] => Err(format!("unknown option '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_runs_the_service() {
        assert_eq!(parse::<&str>(&[]), Ok(Invocation::Serve));
    }

    #[test]
    fn each_option_is_one_command() {
        assert_eq!(
            parse(&["--toggle-centre"]),
            Ok(Invocation::Send(Command::ToggleCentre))
        );
        assert_eq!(
            parse(&["--dismiss-all"]),
            Ok(Invocation::Send(Command::DismissAll))
        );
        assert_eq!(
            parse(&["--dnd", "on"]),
            Ok(Invocation::Send(Command::Dnd(Dnd::On)))
        );
        assert_eq!(
            parse(&["--dnd", "off"]),
            Ok(Invocation::Send(Command::Dnd(Dnd::Off)))
        );
        assert_eq!(
            parse(&["--dnd", "toggle"]),
            Ok(Invocation::Send(Command::Dnd(Dnd::Toggle)))
        );
        assert_eq!(parse(&["--dnd", "status"]), Ok(Invocation::DndStatus));
        assert_eq!(parse(&["--help"]), Ok(Invocation::Help));
        assert_eq!(parse(&["-V"]), Ok(Invocation::Version));
    }

    #[test]
    fn anything_else_is_refused() {
        assert!(parse(&["--dnd"]).is_err());
        assert!(parse(&["--dnd", "maybe"]).is_err());
        assert!(parse(&["--dnd", "on", "extra"]).is_err());
        assert!(parse(&["--toggle-centre", "--dismiss-all"]).is_err());
        assert!(parse(&["centre"]).is_err());
        assert!(parse(&[""]).is_err());
    }
}
