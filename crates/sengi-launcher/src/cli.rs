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
  --lines N           rows to show (default 8; a menu shows all its lines
                      up to that)
  --wide              give a menu the full width of the application search
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
    /// Rows asked for with `--lines`.
    pub lines: Option<usize>,
    /// A menu as wide as the application search, for long lines.
    pub wide: bool,
}

impl Invocation {
    pub fn prompt(&self) -> &str {
        match (&self.prompt, self.mode) {
            (Some(prompt), _) => prompt.trim(),
            (None, Mode::Apps) => "Search",
            (None, Mode::Menu) => "",
        }
    }

    /// Rows to show for `count` candidates. A menu is as tall as its lines.
    pub fn lines_for(&self, count: usize) -> usize {
        match (self.lines, self.mode) {
            (Some(lines), _) => lines,
            (None, Mode::Apps) => DEFAULT_LINES,
            (None, Mode::Menu) => count.clamp(1, DEFAULT_LINES),
        }
    }

    /// Short menus get a narrow panel; the application search is full width.
    pub fn compact(&self) -> bool {
        self.mode == Mode::Menu && !self.wide
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
        lines: None,
        wide: false,
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
                let lines = text
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| format!("--lines needs a number from 1, not '{text}'"))?;
                invocation.lines = Some(lines.min(MAX_LINES));
            }
            "--list" => {
                let mut words: Vec<String> = inline.into_iter().collect();
                words.extend(args.by_ref());
                invocation.action = Action::List(words.join(" "));
            }
            "--wide" => invocation.wide = true,
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
        assert_eq!(inv.lines, None);
        assert_eq!(inv.lines_for(3), DEFAULT_LINES);
        assert_eq!(inv.prompt(), "Search");
        assert!(!inv.compact());
    }

    #[test]
    fn menu_mode_with_prompt_and_lines() {
        let inv = ok(&["--menu", "--prompt", "Session  ", "--lines", "4"]);
        assert_eq!(inv.mode, Mode::Menu);
        assert_eq!(inv.action, Action::Show);
        assert_eq!(inv.prompt(), "Session");
        assert_eq!(inv.lines_for(9), 4);
        assert!(inv.compact());

        let inv = ok(&["--dmenu", "--lines=3", "--prompt=Pick"]);
        assert_eq!(
            (inv.mode, inv.lines, inv.prompt()),
            (Mode::Menu, Some(3), "Pick")
        );
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
        assert_eq!(ok(&["--lines", "1000"]).lines, Some(MAX_LINES));
        assert!(parse(["--lines", "0"]).is_err());
        assert!(parse(["--lines", "many"]).is_err());
        assert!(parse(["--lines", "-3"]).is_err());
        assert!(parse(["--lines"]).is_err());
        assert!(parse(["--prompt"]).is_err());
    }

    #[test]
    fn a_menu_is_as_tall_as_its_lines_and_narrow_unless_asked() {
        let menu = ok(&["--menu"]);
        assert_eq!(menu.lines_for(4), 4);
        assert_eq!(menu.lines_for(0), 1);
        assert_eq!(menu.lines_for(100), DEFAULT_LINES);
        assert!(menu.compact());
        assert!(!ok(&["--menu", "--wide"]).compact());
        assert!(!ok(&["--wide"]).compact());
    }

    #[test]
    fn help_version_and_unknown_arguments() {
        assert_eq!(ok(&["--help"]).action, Action::Help);
        assert_eq!(ok(&["-V"]).action, Action::Version);
        assert!(parse(["--frobnicate"]).is_err());
        assert!(parse(["stray"]).is_err());
    }
}
