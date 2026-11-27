//! The desktop's menus. The desktop is below every window, so a menu drawn on
//! it would be hidden by any window in the way. Each menu is shown on the
//! overlay layer instead, by a second `sengi-desktop --menu` that lives as
//! long as the menu does and prints what was chosen.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::thread;

/// Where a menu was asked for and what it is for.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The output it was asked on; empty where outputs have no names.
    pub output: String,
    pub x: f32,
    pub y: f32,
    /// For the selected icon rather than the bare desktop.
    pub item: bool,
}

/// A row of a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    NewFolder,
    NewFile,
    Terminal,
    Settings,
    Open,
    Rename,
    Trash,
}

const WORDS: [(Choice, &str); 7] = [
    (Choice::NewFolder, "new-folder"),
    (Choice::NewFile, "new-file"),
    (Choice::Terminal, "terminal"),
    (Choice::Settings, "settings"),
    (Choice::Open, "open"),
    (Choice::Rename, "rename"),
    (Choice::Trash, "trash"),
];

impl Choice {
    pub fn parse(word: &str) -> Option<Choice> {
        let word = word.trim();
        WORDS
            .iter()
            .find(|(_, known)| *known == word)
            .map(|(choice, _)| *choice)
    }
}

impl Request {
    /// The command line that shows this menu, after the program's name.
    pub fn arguments(&self) -> Vec<String> {
        let what = if self.item { "item" } else { "desktop" };
        vec![
            "--menu".into(),
            what.into(),
            self.x.to_string(),
            self.y.to_string(),
            self.output.clone(),
        ]
    }

    /// Reads back what `arguments` wrote.
    pub fn parse(arguments: &[&str]) -> Option<Request> {
        let ["--menu", what @ ("item" | "desktop"), x, y, output] = arguments else {
            return None;
        };
        Some(Request {
            output: output.to_string(),
            x: x.parse().ok()?,
            y: y.parse().ok()?,
            item: *what == "item",
        })
    }
}

/// Whether the menu's surface takes the keyboard, given the value of
/// `SENGI_COMPOSITOR_PID`. sengi-compositor hands the keyboard back to the
/// desktop when the menu goes. labwc, the fallback, does not, and a new
/// folder's name would then get no keys; there the desktop keeps the keyboard
/// all along, and its own Escape puts the menu away.
pub fn takes_keyboard(compositor: Option<&std::ffi::OsStr>) -> bool {
    compositor.is_some_and(|pid| !pid.is_empty())
}

/// A menu that is on screen. Dropping it takes it away.
pub struct Open {
    child: Child,
}

impl Drop for Open {
    fn drop(&mut self) {
        // Gone already if something was chosen; then this only collects it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Shows the menu of `request`. `answered` is called on another thread once
/// the menu has gone, with what was chosen if anything was.
pub fn show(
    request: &Request,
    answered: impl FnOnce(Option<Choice>) + Send + 'static,
) -> std::io::Result<Open> {
    let mut child = Command::new(std::env::current_exe()?)
        .args(request.arguments())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let output = child.stdout.take();
    let open = Open { child };
    thread::Builder::new().name("menu".into()).spawn(move || {
        let mut said = String::new();
        if let Some(mut output) = output {
            let _ = output.read_to_string(&mut said);
        }
        answered(Choice::parse(&said));
    })?;
    Ok(open)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_survives_the_command_line() {
        let request = Request {
            output: "eDP-1".into(),
            x: 600.5,
            y: 300.0,
            item: true,
        };
        let words = request.arguments();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        assert_eq!(Request::parse(&words), Some(request));
        assert_eq!(Request::parse(&["--menu", "other", "1", "2", ""]), None);
        assert_eq!(Request::parse(&["--menu", "item", "x", "2", ""]), None);
    }

    #[test]
    fn the_menu_takes_the_keyboard_only_under_our_compositor() {
        assert!(takes_keyboard(Some("812".as_ref())));
        assert!(!takes_keyboard(Some("".as_ref())));
        assert!(!takes_keyboard(None));
    }

    #[test]
    fn every_choice_is_read_from_its_word() {
        for (choice, word) in WORDS {
            assert_eq!(Choice::parse(&format!("{word}\n")), Some(choice));
        }
        assert_eq!(Choice::parse(""), None);
    }
}
