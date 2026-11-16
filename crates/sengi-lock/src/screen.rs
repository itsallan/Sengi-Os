//! What the lock screen is doing and what it shows, apart from drawing
//! it. Keys, PAM's answers and timers come in; [`View`] comes out.

use std::time::Duration;

use sengi_system::secret::Secret;

use crate::backoff;
use crate::entry::{PasswordEntry, Typed};

/// What is said about a wrong password when no PAM module said more.
const WRONG: &str = "Wrong password";

/// How a check of the password ended, as far as the screen cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Right,
    /// With what a PAM module had to say about it, if anything: "3
    /// attempts left", "the account is locked for 10 minutes".
    Wrong(Option<String>),
    /// The password was right and the account is closed.
    Barred,
    /// It could not be checked. With what PAM said about it, if anything.
    Unchecked(Option<String>),
}

/// What to do after a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    Unlock,
    /// Ask again.
    Ask,
    /// Ask again once this long has passed; call [`Screen::released`] then.
    Hold(Duration),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Taking key presses.
    Asking,
    /// PAM has the password.
    Checking,
    /// Shut for a while after wrong passwords.
    Held(Duration),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Note {
    None,
    Wrong(Option<String>),
    TooLong,
    Barred,
    Unchecked(Option<String>),
}

/// Everything the window is told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    /// Characters typed so far.
    pub length: usize,
    /// Whether the name and the field are shown, or the clock alone.
    pub prompt: bool,
    /// Whether the field takes input.
    pub enabled: bool,
    /// The field is marked as having turned a password down.
    pub rejected: bool,
    pub placeholder: String,
    /// The line under the field.
    pub note: String,
    pub note_is_problem: bool,
    pub note_is_warning: bool,
    /// The active keyboard layout, or empty when there is only one.
    pub layout: String,
}

pub struct Screen {
    entry: PasswordEntry,
    phase: Phase,
    /// Wrong passwords in a row.
    failures: u32,
    note: Note,
    prompt: bool,
    caps_lock: bool,
    /// The configured layouts, as short labels. Fewer than two: no label.
    layouts: Vec<String>,
    layout: usize,
}

impl Screen {
    /// `layouts` are the keyboard layouts the session was set up with, in
    /// order, such as `us` and `fr`.
    pub fn new(layouts: &[String]) -> Screen {
        Screen {
            entry: PasswordEntry::new(),
            phase: Phase::Asking,
            failures: 0,
            note: Note::None,
            prompt: true,
            caps_lock: false,
            layouts: layouts.iter().map(|name| name.to_uppercase()).collect(),
            layout: 0,
        }
    }

    /// A key that types text was pressed.
    pub fn typed(&mut self, text: &str) {
        self.prompt = true;
        if self.phase != Phase::Asking {
            return;
        }
        match self.entry.push(text) {
            Typed::Added => self.note = Note::None,
            Typed::Full => self.note = Note::TooLong,
            Typed::Ignored => {}
        }
    }

    /// Backspace.
    pub fn erased(&mut self) {
        self.prompt = true;
        if self.phase == Phase::Asking && self.entry.erase() {
            self.note = Note::None;
        }
    }

    /// Escape: start over.
    pub fn cleared(&mut self) {
        self.prompt = true;
        if self.phase == Phase::Asking {
            self.entry.clear();
            self.note = Note::None;
        }
    }

    /// Return. Gives the password to check, if there is one and no check
    /// is under way; the caller owes a [`Screen::judged`] for it.
    pub fn submitted(&mut self) -> Option<Secret> {
        self.prompt = true;
        if self.phase != Phase::Asking {
            return None;
        }
        let password = self.entry.take()?;
        self.phase = Phase::Checking;
        self.note = Note::None;
        Some(password)
    }

    /// The check that [`Screen::submitted`] started has ended.
    pub fn judged(&mut self, verdict: Verdict) -> Next {
        if self.phase != Phase::Checking {
            // Nothing was being checked. Certainly not a reason to unlock.
            return Next::Ask;
        }
        self.prompt = true;
        self.phase = Phase::Asking;
        match verdict {
            Verdict::Right => return Next::Unlock,
            Verdict::Wrong(said) => {
                self.failures = self.failures.saturating_add(1);
                self.note = Note::Wrong(said);
            }
            Verdict::Barred => self.note = Note::Barred,
            Verdict::Unchecked(said) => self.note = Note::Unchecked(said),
        }
        let wait = backoff::delay(self.failures);
        if verdict_costs(&self.note) && !wait.is_zero() {
            self.phase = Phase::Held(wait);
            Next::Hold(wait)
        } else {
            Next::Ask
        }
    }

    /// The wait after a wrong password is over.
    pub fn released(&mut self) {
        if matches!(self.phase, Phase::Held(_)) {
            self.phase = Phase::Asking;
        }
    }

    /// The pointer moved, or a key that types nothing was pressed.
    pub fn stirred(&mut self) {
        self.prompt = true;
    }

    /// Nothing has happened for a while. Puts the field away, and with it
    /// whatever was typed and left there. True if that changed anything.
    pub fn rested(&mut self) -> bool {
        if self.phase != Phase::Asking || !self.prompt {
            return false;
        }
        self.prompt = false;
        self.entry.clear();
        self.note = Note::None;
        true
    }

    /// True while the field is up and may be put away for lack of use.
    pub fn may_rest(&self) -> bool {
        self.phase == Phase::Asking && self.prompt
    }

    pub fn keyboard(&mut self, caps_lock: bool, layout: u32) {
        self.caps_lock = caps_lock;
        self.layout = layout as usize;
    }

    pub fn view(&self) -> View {
        let (note, problem, warning) = match (&self.phase, &self.note) {
            (Phase::Checking, _) => (String::new(), false, false),
            (Phase::Held(wait), note) => {
                let why = match note {
                    Note::Wrong(Some(said)) => said.trim_end_matches('.'),
                    _ => WRONG,
                };
                (
                    format!("{why}. Wait {} seconds.", wait.as_secs()),
                    true,
                    false,
                )
            }
            (_, Note::Wrong(said)) => (
                said.clone().unwrap_or_else(|| WRONG.to_string()),
                true,
                false,
            ),
            (_, Note::TooLong) => (
                "That is as long as a password can be".to_string(),
                true,
                false,
            ),
            (_, Note::Barred) => (
                "This account cannot be used right now".to_string(),
                true,
                false,
            ),
            (_, Note::Unchecked(said)) => (
                said.clone()
                    .unwrap_or_else(|| "The password could not be checked".to_string()),
                true,
                false,
            ),
            (_, Note::None) if self.caps_lock => ("Caps Lock is on".to_string(), false, true),
            (_, Note::None) => (String::new(), false, false),
        };
        View {
            length: self.entry.chars(),
            prompt: self.prompt,
            enabled: self.phase == Phase::Asking,
            rejected: matches!(self.phase, Phase::Held(_))
                || (self.phase == Phase::Asking && matches!(self.note, Note::Wrong(_))),
            placeholder: match self.phase {
                Phase::Checking => "Checking\u{2026}".to_string(),
                _ => "Password".to_string(),
            },
            note,
            note_is_problem: problem,
            note_is_warning: warning,
            layout: if self.layouts.len() > 1 {
                self.layouts.get(self.layout).cloned().unwrap_or_default()
            } else {
                String::new()
            },
        }
    }
}

/// Only a wrong password is paid for with a wait. A check that could not
/// be made is not the user's doing.
fn verdict_costs(note: &Note) -> bool {
    matches!(note, Note::Wrong(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Screen {
        Screen::new(&[])
    }

    fn type_in(screen: &mut Screen, text: &str) {
        for character in text.chars() {
            screen.typed(&character.to_string());
        }
    }

    fn submit(screen: &mut Screen, text: &str) -> Secret {
        type_in(screen, text);
        screen.submitted().expect("a password to check")
    }

    #[test]
    fn the_right_password_unlocks() {
        let mut screen = screen();
        let password = submit(&mut screen, "live");
        assert_eq!(password.expose(), "live");
        let view = screen.view();
        assert!(!view.enabled);
        assert_eq!(view.placeholder, "Checking\u{2026}");
        assert_eq!(view.length, 0);
        assert_eq!(screen.judged(Verdict::Right), Next::Unlock);
    }

    #[test]
    fn nothing_but_a_right_verdict_on_a_real_check_unlocks() {
        let mut screen = screen();
        // No check is under way.
        assert_eq!(screen.judged(Verdict::Right), Next::Ask);
        for verdict in [
            Verdict::Wrong(None),
            Verdict::Barred,
            Verdict::Unchecked(None),
        ] {
            submit(&mut screen, "x");
            assert_ne!(screen.judged(verdict), Next::Unlock);
        }
        // A verdict that arrives twice counts once.
        submit(&mut screen, "x");
        assert_ne!(screen.judged(Verdict::Wrong(None)), Next::Unlock);
        assert_eq!(screen.judged(Verdict::Right), Next::Ask);
    }

    #[test]
    fn a_wrong_password_says_so_and_empties_the_field() {
        let mut screen = screen();
        submit(&mut screen, "wrong");
        assert_eq!(screen.judged(Verdict::Wrong(None)), Next::Ask);
        let view = screen.view();
        assert_eq!(view.note, "Wrong password");
        assert!(view.note_is_problem && view.rejected && view.enabled);
        assert_eq!(view.length, 0);
        // The mark goes when the next attempt begins.
        screen.typed("l");
        let view = screen.view();
        assert_eq!(
            (view.note.as_str(), view.rejected, view.length),
            ("", false, 1)
        );
    }

    #[test]
    fn return_does_nothing_twice_or_on_an_empty_field() {
        let mut screen = screen();
        assert!(screen.submitted().is_none());
        assert!(screen.view().enabled);
        submit(&mut screen, "one");
        // While PAM is busy, keys and a second Return are dropped.
        type_in(&mut screen, "two");
        assert!(screen.submitted().is_none());
        screen.erased();
        screen.cleared();
        assert_eq!(screen.view().length, 0);
        assert_eq!(screen.judged(Verdict::Wrong(None)), Next::Ask);
        assert!(screen.submitted().is_none());
    }

    #[test]
    fn repeated_failures_shut_the_prompt_for_longer_each_time() {
        let mut screen = screen();
        let mut waits = Vec::new();
        for _ in 0..8 {
            submit(&mut screen, "nope");
            match screen.judged(Verdict::Wrong(None)) {
                Next::Hold(wait) => {
                    waits.push(wait.as_secs());
                    let view = screen.view();
                    assert!(!view.enabled && view.rejected);
                    assert_eq!(
                        view.note,
                        format!("Wrong password. Wait {} seconds.", wait.as_secs())
                    );
                    // Typing while shut out goes nowhere.
                    type_in(&mut screen, "abc");
                    assert!(screen.submitted().is_none());
                    assert_eq!(screen.view().length, 0);
                    screen.released();
                    let view = screen.view();
                    assert!(view.enabled);
                    assert_eq!(view.note, "Wrong password");
                }
                Next::Ask => waits.push(0),
                Next::Unlock => panic!("a wrong password unlocked"),
            }
        }
        assert_eq!(waits, [0, 0, 2, 4, 8, 16, 30, 30]);
        // However many there were, the right one still gets in.
        submit(&mut screen, "live");
        assert_eq!(screen.judged(Verdict::Right), Next::Unlock);
    }

    #[test]
    fn what_a_module_says_about_a_denial_is_shown_in_place_of_ours() {
        let mut screen = screen();
        let said = "2 attempts left before the account is locked.";
        for _ in 0..2 {
            submit(&mut screen, "nope");
            assert_eq!(
                screen.judged(Verdict::Wrong(Some(said.to_string()))),
                Next::Ask
            );
            let view = screen.view();
            assert_eq!(view.note, said);
            assert!(view.note_is_problem && view.rejected);
        }
        // It still counts as a wrong password, and the wait is added to it.
        submit(&mut screen, "nope");
        assert!(matches!(
            screen.judged(Verdict::Wrong(Some(said.to_string()))),
            Next::Hold(_)
        ));
        assert_eq!(
            screen.view().note,
            "2 attempts left before the account is locked. Wait 2 seconds."
        );
        screen.released();
        assert_eq!(screen.view().note, said);
        // The next denial without a message goes back to the plain words.
        submit(&mut screen, "nope");
        screen.judged(Verdict::Wrong(None));
        assert_eq!(screen.view().note, "Wrong password. Wait 4 seconds.");
    }

    #[test]
    fn a_check_that_could_not_be_made_costs_no_wait() {
        let mut screen = screen();
        for _ in 0..5 {
            submit(&mut screen, "x");
            assert_eq!(screen.judged(Verdict::Unchecked(None)), Next::Ask);
        }
        assert_eq!(screen.view().note, "The password could not be checked");
        submit(&mut screen, "x");
        let said = Some("Authentication service cannot retrieve authentication info".to_string());
        screen.judged(Verdict::Unchecked(said.clone()));
        assert_eq!(Some(screen.view().note), said);
        submit(&mut screen, "x");
        screen.judged(Verdict::Barred);
        assert_eq!(screen.view().note, "This account cannot be used right now");
    }

    #[test]
    fn the_field_goes_away_with_what_was_left_in_it_and_comes_back() {
        let mut screen = screen();
        type_in(&mut screen, "half a pass");
        assert!(screen.may_rest());
        assert!(screen.rested());
        let view = screen.view();
        assert!(!view.prompt);
        assert_eq!(view.length, 0);
        assert!(!screen.rested());
        assert!(!screen.may_rest());
        // Nothing was kept to submit.
        assert!(screen.submitted().is_none());
        assert!(screen.view().prompt);

        screen.rested();
        screen.stirred();
        assert!(screen.view().prompt);
        screen.rested();
        // The key that wakes it is also the first of the password.
        screen.typed("l");
        let view = screen.view();
        assert!(view.prompt);
        assert_eq!(view.length, 1);
    }

    #[test]
    fn the_field_stays_while_a_check_or_a_wait_is_on() {
        let mut screen = screen();
        submit(&mut screen, "x");
        assert!(!screen.may_rest());
        assert!(!screen.rested());
        for _ in 0..2 {
            screen.judged(Verdict::Wrong(None));
            submit(&mut screen, "x");
        }
        assert!(matches!(screen.judged(Verdict::Wrong(None)), Next::Hold(_)));
        assert!(!screen.rested());
        assert!(screen.view().prompt);
    }

    #[test]
    fn a_password_at_the_limit_is_kept_and_the_limit_is_named() {
        let mut screen = screen();
        type_in(&mut screen, &"x".repeat(crate::entry::MAX_BYTES));
        assert_eq!(screen.view().note, "");
        screen.typed("y");
        let view = screen.view();
        assert_eq!(view.note, "That is as long as a password can be");
        assert_eq!(view.length, crate::entry::MAX_BYTES);
        screen.erased();
        assert_eq!(screen.view().note, "");
        assert_eq!(
            screen.submitted().unwrap().len(),
            crate::entry::MAX_BYTES - 1
        );
    }

    #[test]
    fn caps_lock_is_pointed_out_until_there_is_something_worse_to_say() {
        let mut screen = screen();
        screen.keyboard(true, 0);
        let view = screen.view();
        assert_eq!(view.note, "Caps Lock is on");
        assert!(view.note_is_warning && !view.note_is_problem);
        submit(&mut screen, "LIVE");
        screen.judged(Verdict::Wrong(None));
        assert_eq!(screen.view().note, "Wrong password");
        screen.typed("l");
        assert_eq!(screen.view().note, "Caps Lock is on");
        screen.keyboard(false, 0);
        assert_eq!(screen.view().note, "");
    }

    #[test]
    fn the_layout_is_named_only_when_there_is_a_choice() {
        let mut one = Screen::new(&["us".to_string()]);
        one.keyboard(false, 0);
        assert_eq!(one.view().layout, "");
        let mut two = Screen::new(&["us".to_string(), "fr".to_string()]);
        assert_eq!(two.view().layout, "US");
        two.keyboard(false, 1);
        assert_eq!(two.view().layout, "FR");
        // A group the settings do not know of gets no label, not a wrong one.
        two.keyboard(false, 5);
        assert_eq!(two.view().layout, "");
    }
}
