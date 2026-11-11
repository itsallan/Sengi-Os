//! Turns the `Exec` line of a desktop entry into an argument vector, following
//! the Desktop Entry spec, so nothing has to go through a shell.

use std::fmt;

use crate::desktop::Entry;

#[derive(Debug, PartialEq, Eq)]
pub enum ExecError {
    /// The entry has no Exec line, or it is blank.
    Empty,
    /// A double quote was opened and never closed.
    UnclosedQuote,
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "the entry has no command"),
            Self::UnclosedQuote => write!(f, "the command has an unclosed quote"),
        }
    }
}

struct Word {
    text: String,
    /// Field codes are not expanded in words that were quoted.
    quoted: bool,
}

/// Splits on unquoted whitespace. Inside double quotes a backslash escapes
/// the quote, backtick, dollar sign and backslash.
fn split(exec: &str) -> Result<Vec<Word>, ExecError> {
    let mut words = Vec::new();
    let mut current: Option<Word> = None;
    let mut chars = exec.chars();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if let Some(word) = current.take() {
                    words.push(word);
                }
            }
            '"' => {
                let word = current.get_or_insert(Word {
                    text: String::new(),
                    quoted: true,
                });
                word.quoted = true;
                loop {
                    match chars.next() {
                        None => return Err(ExecError::UnclosedQuote),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            None => return Err(ExecError::UnclosedQuote),
                            Some(escaped @ ('"' | '`' | '$' | '\\')) => word.text.push(escaped),
                            Some(other) => {
                                word.text.push('\\');
                                word.text.push(other);
                            }
                        },
                        Some(other) => word.text.push(other),
                    }
                }
            }
            other => current
                .get_or_insert(Word {
                    text: String::new(),
                    quoted: false,
                })
                .text
                .push(other),
        }
    }
    if let Some(word) = current {
        words.push(word);
    }
    Ok(words)
}

/// The arguments to start `entry` with no files to open.
pub fn command(entry: &Entry) -> Result<Vec<String>, ExecError> {
    let exec = entry.exec.as_deref().unwrap_or("");
    let mut args = Vec::new();

    for word in split(exec)? {
        if word.quoted {
            args.push(word.text);
            continue;
        }
        match word.text.as_str() {
            // The dock never has files or URLs to pass.
            "%f" | "%F" | "%u" | "%U" => continue,
            "%i" => {
                if !entry.icon.is_empty() {
                    args.push("--icon".into());
                    args.push(entry.icon.clone());
                }
                continue;
            }
            _ => {}
        }

        let mut text = String::with_capacity(word.text.len());
        let mut chars = word.text.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                text.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => text.push('%'),
                Some('c') => text.push_str(&entry.name),
                Some('k') => text.push_str(&entry.path.to_string_lossy()),
                // File codes, %i inside a word, and the deprecated codes all
                // expand to nothing.
                _ => {}
            }
        }
        // A word made only of such codes disappears with them.
        if !text.is_empty() {
            args.push(text);
        }
    }

    if args.is_empty() {
        return Err(ExecError::Empty);
    }
    Ok(args)
}

/// The name of the program an Exec line starts, without its directory:
/// `/usr/bin/env FOO=1 /opt/app/bin/app --flag` gives `app`.
pub fn program_name(exec: &str) -> Option<String> {
    let words = split(exec).ok()?;
    let mut words = words.iter().map(|w| w.text.as_str()).peekable();
    let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();

    let first = words.next()?;
    if base(first) != "env" {
        return Some(base(first)).filter(|name| !name.is_empty());
    }
    words
        .find(|word| !word.contains('=') && !word.starts_with('-'))
        .map(base)
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::testing::entry;

    fn run(exec: &str) -> Result<Vec<String>, ExecError> {
        let mut entry = entry("web", "Web Browser", exec);
        entry.icon = "web-icon".into();
        command(&entry)
    }

    fn args(list: &[&str]) -> Result<Vec<String>, ExecError> {
        Ok(list.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn plain_words_are_split_on_whitespace() {
        assert_eq!(run("foot"), args(&["foot"]));
        assert_eq!(
            run("  foot   --server\t-x "),
            args(&["foot", "--server", "-x"])
        );
    }

    #[test]
    fn file_and_url_codes_are_dropped() {
        assert_eq!(run("web %u"), args(&["web"]));
        assert_eq!(
            run("web --new-window %U --tail"),
            args(&["web", "--new-window", "--tail"])
        );
        assert_eq!(run("edit %f %F"), args(&["edit"]));
        assert_eq!(run("web --url=%u"), args(&["web", "--url="]));
    }

    #[test]
    fn name_icon_and_path_codes_are_filled_in() {
        assert_eq!(
            run("web --title %c"),
            args(&["web", "--title", "Web Browser"])
        );
        assert_eq!(run("web %i"), args(&["web", "--icon", "web-icon"]));
        assert_eq!(
            run("web --desktop=%k"),
            args(&["web", "--desktop=/usr/share/applications/web.desktop"])
        );
    }

    #[test]
    fn the_icon_code_vanishes_when_there_is_no_icon() {
        let mut entry = entry("web", "Web", "web %i");
        entry.icon.clear();
        assert_eq!(command(&entry), args(&["web"]));
    }

    #[test]
    fn percent_and_deprecated_codes() {
        assert_eq!(run("calc 100%%"), args(&["calc", "100%"]));
        assert_eq!(run("old %d %n %v %m x"), args(&["old", "x"]));
    }

    #[test]
    fn quoted_arguments_keep_spaces_and_escapes() {
        assert_eq!(
            run(r#"sh -c "echo \"a  b\" \$HOME \\ \`x\`""#),
            args(&["sh", "-c", r#"echo "a  b" $HOME \ `x`"#])
        );
        assert_eq!(
            run(r#""/opt/My App/run" --flag"#),
            args(&["/opt/My App/run", "--flag"])
        );
        assert_eq!(run(r#"app """#), args(&["app", ""]));
    }

    #[test]
    fn field_codes_inside_quotes_are_left_alone() {
        assert_eq!(run(r#"app "%u""#), args(&["app", "%u"]));
    }

    #[test]
    fn broken_and_empty_commands_are_errors() {
        assert_eq!(run(r#"app "unfinished"#), Err(ExecError::UnclosedQuote));
        assert_eq!(run(r#"app "bad\"#), Err(ExecError::UnclosedQuote));
        assert_eq!(run(""), Err(ExecError::Empty));
        assert_eq!(run("%u"), Err(ExecError::Empty));
        let mut none = entry("x", "X", "");
        none.exec = None;
        assert_eq!(command(&none), Err(ExecError::Empty));
    }

    #[test]
    fn the_program_name_skips_paths_and_env() {
        assert_eq!(program_name("foot").as_deref(), Some("foot"));
        assert_eq!(
            program_name("/usr/lib/firefox-esr/firefox-esr %u").as_deref(),
            Some("firefox-esr")
        );
        assert_eq!(
            program_name("env GDK_BACKEND=wayland /opt/app/bin/app --flag").as_deref(),
            Some("app")
        );
        assert_eq!(
            program_name(r#""/opt/My App/run" %f"#).as_deref(),
            Some("run")
        );
        assert_eq!(program_name(""), None);
        assert_eq!(program_name("env A=1"), None);
    }
}
