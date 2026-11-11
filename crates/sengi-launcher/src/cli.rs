//! Command-line arguments.

/// Rows shown when `--lines` is not given. Matches `size.launcher-lines` in
/// the tokens, which the old launcher's config was rendered from.
pub const DEFAULT_LINES: usize = 8;
const MAX_LINES: usize = 20;

pub const USAGE: &str = "\
Usage: sengi-launcher [OPTIONS]

Search and start applications, or choose from a menu.

  (no options)        search installed applications and start one
  --menu              read lines from standard input, print the chosen one;
                      exits 0 on a choice and 1 if dismissed
  --prompt TEXT       label in front of the search field
  --lines N           rows to show (default 8)
  --list [QUERY]      print the ranked results and exit, without a window
  --help, --version
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Apps,
    Menu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Open the panel.
    Show,
    /// Print ranked results for the query.
    List(String),
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub mode: Mode,
    pub action: Action,
    pub prompt: Option<String>,
    pub lines: usize,
}

impl Invocation {
    pub fn prompt(&self) -> &str {
        match (&self.prompt, self.mode) {
            (Some(prompt), _) => prompt.trim(),
            (None, Mode::Apps) => "Search",
            (None, Mode::Menu) => "",
        }
    }
}

pub fn parse<I, S>(args: I) -> Result<Invocation, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut invocation = Invocation {
        mode: Mode::Apps,
        action: Action::Show,
        prompt: None,
        lines: DEFAULT_LINES,
    };
    let mut args = args.into_iter().map(Into::into);

    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag.as_str() {
            // --dmenu so scripts written for other launchers keep working.
            "--menu" | "--dmenu" | "-d" => invocation.mode = Mode::Menu,
            "--prompt" | "-p" => invocation.prompt = Some(value("--prompt")?),
            "--lines" | "-l" => {
                let text = value("--lines")?;
                invocation.lines = text
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| format!("--lines needs a number from 1, not '{text}'"))?
                    .min(MAX_LINES);
            }
            "--list" => {
                let mut words: Vec<String> = inline.into_iter().collect();
                words.extend(args.by_ref());
                invocation.action = Action::List(words.join(" "));
            }
            "--help" | "-h" => invocation.action = Action::Help,
            "--version" | "-V" => invocation.action = Action::Version,
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(invocation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(args: &[&str]) -> Invocation {
        parse(args.iter().copied()).expect("parses")
    }

    #[test]
    fn no_arguments_opens_the_app_search() {
        let inv = ok(&[]);
        assert_eq!(inv.mode, Mode::Apps);
        assert_eq!(inv.action, Action::Show);
        assert_eq!(inv.lines, DEFAULT_LINES);
        assert_eq!(inv.prompt(), "Search");
    }

    #[test]
    fn menu_mode_with_prompt_and_lines() {
        let inv = ok(&["--menu", "--prompt", "Session  ", "--lines", "4"]);
        assert_eq!(inv.mode, Mode::Menu);
        assert_eq!(inv.action, Action::Show);
        assert_eq!(inv.prompt(), "Session");
        assert_eq!(inv.lines, 4);

        let inv = ok(&["--dmenu", "--lines=3", "--prompt=Pick"]);
        assert_eq!((inv.mode, inv.lines, inv.prompt()), (Mode::Menu, 3, "Pick"));
        assert_eq!(ok(&["--menu"]).prompt(), "");
        assert_eq!(ok(&["-d", "-p", "x=y", "-l", "2"]).prompt(), "x=y");
    }

    #[test]
    fn list_takes_the_rest_as_the_query() {
        assert_eq!(ok(&["--list"]).action, Action::List(String::new()));
        assert_eq!(
            ok(&["--list", "web", "browser"]).action,
            Action::List("web browser".into())
        );
        assert_eq!(
            ok(&["--list", "--menu"]).action,
            Action::List("--menu".into())
        );
        let inv = ok(&["--menu", "--list", "lo"]);
        assert_eq!(
            (inv.mode, inv.action),
            (Mode::Menu, Action::List("lo".into()))
        );
        assert_eq!(ok(&["--list=a b"]).action, Action::List("a b".into()));
    }

    #[test]
    fn lines_are_checked_and_capped() {
        assert_eq!(ok(&["--lines", "1000"]).lines, MAX_LINES);
        assert!(parse(["--lines", "0"]).is_err());
        assert!(parse(["--lines", "many"]).is_err());
        assert!(parse(["--lines", "-3"]).is_err());
        assert!(parse(["--lines"]).is_err());
        assert!(parse(["--prompt"]).is_err());
    }

    #[test]
    fn help_version_and_unknown_arguments() {
        assert_eq!(ok(&["--help"]).action, Action::Help);
        assert_eq!(ok(&["-V"]).action, Action::Version);
        assert!(parse(["--frobnicate"]).is_err());
        assert!(parse(["stray"]).is_err());
    }
}
