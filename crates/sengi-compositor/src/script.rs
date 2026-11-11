//! Scripted input for the headless backend.
//!
//! A script is a text file with one step per line: the time in milliseconds
//! since start, then what happens.
//!
//! ```text
//! # comment
//! 1500 move 400 300
//! 1600 press left
//! 1700 move 500 350
//! 1800 release left
//! 2000 type hello
//! 2100 key super+q
//! 2500 screenshot /tmp/after.png
//! 3000 exit
//! ```
//!
//! Keys are named for a US layout, which is what the compositor uses.

use std::{path::PathBuf, time::Duration};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A key by its evdev code.
    Key {
        code: u32,
        pressed: bool,
    },
    /// Pointer position in output pixels.
    Move {
        x: f64,
        y: f64,
    },
    /// A pointer button by its evdev code.
    Button {
        code: u32,
        pressed: bool,
    },
    Screenshot(PathBuf),
    Exit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub at: Duration,
    pub action: Action,
}

const KEY_LEFTSHIFT: u32 = 42;

/// Evdev codes of the named keys.
const NAMED_KEYS: &[(&str, u32)] = &[
    ("esc", 1),
    ("backspace", 14),
    ("tab", 15),
    ("enter", 28),
    ("ctrl", 29),
    ("shift", KEY_LEFTSHIFT),
    ("alt", 56),
    ("space", 57),
    ("up", 103),
    ("left", 105),
    ("right", 106),
    ("down", 108),
    ("super", 125),
];

/// Characters in evdev code order, starting at the code in the first field.
const ROWS: &[(u32, &str, &str)] = &[
    (2, "1234567890-=", "!@#$%^&*()_+"),
    (16, "qwertyuiop[]", "QWERTYUIOP{}"),
    (30, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
    (43, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
    (57, " ", ""),
];

const BUTTONS: &[(&str, u32)] = &[("left", 0x110), ("right", 0x111), ("middle", 0x112)];

/// The key for `c` and whether shift is needed.
fn key_for_char(c: char) -> Option<(u32, bool)> {
    ROWS.iter().find_map(|&(first, plain, shifted)| {
        let code = |index: usize| first + index as u32;
        plain
            .chars()
            .position(|p| p == c)
            .map(|i| (code(i), false))
            .or_else(|| {
                shifted
                    .chars()
                    .position(|s| s == c)
                    .map(|i| (code(i), true))
            })
    })
}

fn key_by_name(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    NAMED_KEYS
        .iter()
        .find(|(known, _)| *known == lower)
        .map(|&(_, code)| code)
        .or_else(|| {
            let mut chars = lower.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => key_for_char(c).map(|(code, _)| code),
                _ => None,
            }
        })
}

pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    let mut last = Duration::ZERO;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let err = |message: &str| format!("script line {}: {message}", index + 1);

        let (time, rest) = line
            .split_once(' ')
            .ok_or_else(|| err("expected a command"))?;
        let at = time
            .parse()
            .map(Duration::from_millis)
            .map_err(|_| err("expected a time in milliseconds"))?;
        if at < last {
            return Err(err("steps must be in time order"));
        }
        last = at;

        let rest = rest.trim_start();
        let (command, argument) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut push = |action| steps.push(Step { at, action });

        match command {
            "move" => {
                let mut numbers = argument.split_whitespace().map(str::parse::<f64>);
                match (numbers.next(), numbers.next(), numbers.next()) {
                    (Some(Ok(x)), Some(Ok(y)), None) if x.is_finite() && y.is_finite() => {
                        push(Action::Move { x, y })
                    }
                    _ => return Err(err("move needs X and Y")),
                }
            }
            "press" | "release" | "click" => {
                let code = BUTTONS
                    .iter()
                    .find(|(name, _)| *name == argument.trim())
                    .map(|&(_, code)| code)
                    .ok_or_else(|| err("unknown button"))?;
                if command != "release" {
                    push(Action::Button {
                        code,
                        pressed: true,
                    });
                }
                if command != "press" {
                    push(Action::Button {
                        code,
                        pressed: false,
                    });
                }
            }
            "key" => {
                let codes = argument
                    .trim()
                    .split('+')
                    .map(|name| key_by_name(name).ok_or_else(|| err("unknown key")))
                    .collect::<Result<Vec<_>, _>>()?;
                for &code in &codes {
                    push(Action::Key {
                        code,
                        pressed: true,
                    });
                }
                for &code in codes.iter().rev() {
                    push(Action::Key {
                        code,
                        pressed: false,
                    });
                }
            }
            "type" => {
                for c in argument.chars() {
                    let (code, shift) =
                        key_for_char(c).ok_or_else(|| err("cannot type that character"))?;
                    if shift {
                        push(Action::Key {
                            code: KEY_LEFTSHIFT,
                            pressed: true,
                        });
                    }
                    push(Action::Key {
                        code,
                        pressed: true,
                    });
                    push(Action::Key {
                        code,
                        pressed: false,
                    });
                    if shift {
                        push(Action::Key {
                            code: KEY_LEFTSHIFT,
                            pressed: false,
                        });
                    }
                }
            }
            "screenshot" if !argument.trim().is_empty() => {
                push(Action::Screenshot(PathBuf::from(argument.trim())));
            }
            "exit" => push(Action::Exit),
            _ => return Err(err("unknown command")),
        }
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions(text: &str) -> Vec<Action> {
        parse(text).unwrap().into_iter().map(|s| s.action).collect()
    }

    fn key(code: u32, pressed: bool) -> Action {
        Action::Key { code, pressed }
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        assert_eq!(parse("# nothing\n\n   \n"), Ok(vec![]));
    }

    #[test]
    fn pointer_steps() {
        let steps = parse("100 move 40 30.5\n200 click left\n300 press right").unwrap();
        assert_eq!(steps[0].at, Duration::from_millis(100));
        assert_eq!(steps[0].action, Action::Move { x: 40.0, y: 30.5 });
        assert_eq!(
            steps[1].action,
            Action::Button {
                code: 0x110,
                pressed: true
            }
        );
        assert_eq!(
            steps[2].action,
            Action::Button {
                code: 0x110,
                pressed: false
            }
        );
        assert_eq!(steps[2].at, Duration::from_millis(200));
        assert_eq!(
            steps[3].action,
            Action::Button {
                code: 0x111,
                pressed: true
            }
        );
    }

    #[test]
    fn typing_uses_shift_for_capitals() {
        assert_eq!(
            actions("0 type aQ"),
            vec![
                key(30, true),
                key(30, false),
                key(42, true),
                key(16, true),
                key(16, false),
                key(42, false),
            ]
        );
    }

    #[test]
    fn typed_text_keeps_inner_spaces() {
        assert_eq!(actions("0 type a b").len(), 6);
        assert_eq!(actions("0 type a b")[2], key(57, true));
    }

    #[test]
    fn chords_release_in_reverse() {
        assert_eq!(
            actions("0 key super+q"),
            vec![
                key(125, true),
                key(16, true),
                key(16, false),
                key(125, false)
            ]
        );
        assert_eq!(actions("0 key Enter"), vec![key(28, true), key(28, false)]);
    }

    #[test]
    fn screenshot_and_exit() {
        assert_eq!(
            actions("5 screenshot /tmp/a.png\n6 exit"),
            vec![Action::Screenshot("/tmp/a.png".into()), Action::Exit]
        );
    }

    #[test]
    fn errors_name_the_line() {
        let err = parse("0 exit\n10 jump 3").unwrap_err();
        assert!(err.starts_with("script line 2:"), "{err}");
        assert!(parse("soon exit").is_err());
        assert!(parse("10").is_err());
        assert!(parse("10 move 1").is_err());
        assert!(parse("10 move nan 5").is_err());
        assert!(parse("10 key hyper").is_err());
        assert!(parse("10 type \u{e9}").is_err());
        assert!(parse("10 screenshot").is_err());
        assert!(parse("20 exit\n10 exit").is_err());
    }
}
