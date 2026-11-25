//! The command line. sengi-shot is what runs this, not a person.

/// The longest countdown. A delayed screenshot is for opening a menu, not
/// for waiting out a coffee.
pub const MAX_DELAY: u32 = 60;

pub const USAGE: &str = "\
Usage: sengi-snip --select
       sengi-snip --countdown SECONDS

The two small screens that sengi-shot uses. --select dims the screen so a
rectangle can be dragged on it; Enter or letting go chooses it, Escape
cancels. It prints the output's name and the rectangle on one line, as
`NAME X,Y,WIDTH,HEIGHT`, and exits 1 if nothing was chosen. --countdown
shows the seconds ticking down in a corner and exits 0 when it reaches zero
and 1 if it is clicked.

  -h, --help       show this help
  -V, --version    show the version
";

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Select,
    Countdown(u32),
    Help,
    Version,
}

pub fn parse<S: AsRef<str>>(arguments: &[S]) -> Result<Mode, String> {
    let words: Vec<&str> = arguments.iter().map(AsRef::as_ref).collect();
    match words[..] {
        ["-h" | "--help"] => Ok(Mode::Help),
        ["-V" | "--version"] => Ok(Mode::Version),
        ["--select"] => Ok(Mode::Select),
        ["--countdown", seconds] => match seconds.parse::<u32>() {
            Ok(n) if (1..=MAX_DELAY).contains(&n) => Ok(Mode::Countdown(n)),
            _ => Err(format!(
                "'{seconds}' is not a number of seconds from 1 to {MAX_DELAY}"
            )),
        },
        ["--countdown"] => Err("--countdown needs a number of seconds".to_string()),
        [] => Err("say --select or --countdown SECONDS".to_string()),
        [other, ..] => Err(format!("unknown argument '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(parse(&["--select"]), Ok(Mode::Select));
        assert_eq!(parse(&["--countdown", "5"]), Ok(Mode::Countdown(5)));
        assert_eq!(parse(&["--countdown", "60"]), Ok(Mode::Countdown(60)));
        assert_eq!(parse(&["-h"]), Ok(Mode::Help));
        assert_eq!(parse(&["--version"]), Ok(Mode::Version));
    }

    #[test]
    fn mistakes() {
        for bad in [
            &["--countdown", "0"][..],
            &["--countdown", "61"],
            &["--countdown", "-3"],
            &["--countdown", "x"],
            &["--countdown"],
            &["--select", "now"],
            &["--wild"],
            &[],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
