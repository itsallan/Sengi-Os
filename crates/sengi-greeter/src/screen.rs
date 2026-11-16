//! What the login screen is doing and what it shows, apart from drawing
//! it and apart from greetd. Keys, greetd's questions and verdicts, and
//! timers come in; [`View`] and the [`Command`]s for greetd come out.
//!
//! The password field, its texts and its waits are the lock screen's own
//! ([`sengi_lock::screen::Screen`]). What is added here is what only a
//! login has: whom to log in, questions beyond the password, and turning
//! the machine off.

use sengi_lock::entry::is_text;
use sengi_lock::screen::{Next, Screen, Verdict, View as Field};
use sengi_lock::user::User;
use sengi_system::secret::Secret;

use crate::login::Command;
use crate::users::{typed_name, MAX_NAME};

/// The longest answer taken to a question that is asked out loud.
const MAX_ANSWER: usize = 255;

/// The longest text shown from PAM or greetd, in characters.
const MAX_SAID: usize = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Restart,
    ShutDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// No account is offered, so the name is typed first.
    Name,
    /// The password of the chosen user. Nothing is open with greetd.
    Password,
    /// greetd waits for the answer to a question it has put.
    Question { visible: bool },
    /// greetd has what was typed.
    Busy,
    /// The session is granted and this program is about to go.
    Starting,
}

/// Everything the window is told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    /// The field and the line under it, as on the lock screen.
    pub field: Field,
    /// What is typed where it may be seen: a name, or the answer to a
    /// question asked out loud. Shown in the field in place of dots.
    pub plain: String,
    /// Who is logging in. Empty until that is known.
    pub user: String,
    /// There is more than one account to choose from.
    pub choice: bool,
    /// Restart and Shut down can be pressed.
    pub power: bool,
}

pub struct Login {
    users: Vec<User>,
    selected: usize,
    /// The user whose name was typed, when no account is offered.
    named: Option<User>,
    stage: Stage,
    screen: Screen,
    plain: String,
    /// What PAM said before the question that is open.
    said: Option<String>,
    /// A restart or shut down that did not happen.
    refused: Option<Power>,
    /// A restart or shut down that has been asked for.
    leaving: Option<Power>,
}

fn said(text: Option<String>) -> Option<String> {
    text.map(|text| sengi_text::line(&text, MAX_SAID))
        .filter(|text| !text.is_empty())
}

/// PAM's "Password: " as the field words it: "Password".
fn label(question: &str) -> String {
    let line = sengi_text::line(question, 60);
    let label = line.trim_end_matches(':').trim_end();
    if label.is_empty() {
        "Answer".to_string()
    } else {
        label.to_string()
    }
}

impl Login {
    /// `users` are the accounts offered, the first of them chosen.
    /// `layouts` are the keyboard layouts, as for the lock screen.
    pub fn new(users: Vec<User>, layouts: &[String]) -> Login {
        Login {
            stage: if users.is_empty() {
                Stage::Name
            } else {
                Stage::Password
            },
            users,
            selected: 0,
            named: None,
            screen: Screen::new(layouts),
            plain: String::new(),
            said: None,
            refused: None,
            leaving: None,
        }
    }

    fn user(&self) -> Option<&User> {
        self.named.as_ref().or(self.users.get(self.selected))
    }

    fn typing_in_sight(&self) -> Option<usize> {
        match self.stage {
            Stage::Name => Some(MAX_NAME),
            Stage::Question { visible: true } => Some(MAX_ANSWER),
            _ => None,
        }
    }

    fn field_is_empty(&self) -> bool {
        self.plain.is_empty() && self.screen.view().length == 0
    }

    /// A key that types text was pressed.
    pub fn typed(&mut self, text: &str) {
        self.refused = None;
        match self.typing_in_sight() {
            Some(most) => {
                self.screen.stirred();
                if is_text(text) && self.plain.chars().count() + text.chars().count() <= most {
                    self.plain.push_str(text);
                }
            }
            None => self.screen.typed(text),
        }
    }

    /// Backspace.
    pub fn erased(&mut self) {
        self.refused = None;
        if self.typing_in_sight().is_some() {
            self.screen.stirred();
            self.plain.pop();
        } else {
            self.screen.erased();
        }
    }

    /// Escape. Empties the field; on an empty field it goes one step
    /// back: out of a question greetd has put, or back to the name.
    pub fn cleared(&mut self) -> Option<Command> {
        self.refused = None;
        if !self.field_is_empty() {
            self.plain.clear();
            self.screen.cleared();
            return None;
        }
        self.screen.cleared();
        match self.stage {
            Stage::Question { .. } => Some(self.give_up()),
            Stage::Password if self.users.is_empty() => {
                self.named = None;
                self.stage = Stage::Name;
                None
            }
            _ => None,
        }
    }

    /// Drops the question that is open. The caller passes the command on.
    fn give_up(&mut self) -> Command {
        self.screen.withdrawn();
        self.plain.clear();
        self.said = None;
        self.stage = Stage::Password;
        Command::Cancel
    }

    /// Return. Gives what is to be sent to greetd, if anything; an
    /// [`Login::asked`] or a [`Login::judged`] is owed for a `Begin` and
    /// for an `Answer`.
    pub fn submitted(&mut self) -> Option<Command> {
        self.refused = None;
        match self.stage {
            Stage::Name => {
                self.screen.stirred();
                let name = typed_name(&self.plain);
                if !name.is_empty() {
                    self.named = Some(User {
                        login: name.clone(),
                        shown: name,
                    });
                    self.plain.clear();
                    self.stage = Stage::Password;
                }
                None
            }
            Stage::Password => {
                let user = self.user()?.login.clone();
                let password = self.screen.submitted_anyway()?;
                self.stage = Stage::Busy;
                Some(Command::Begin { user, password })
            }
            Stage::Question { visible } => {
                let typed = self.screen.submitted_anyway()?;
                self.stage = Stage::Busy;
                Some(Command::Answer(if visible {
                    Secret::new(std::mem::take(&mut self.plain))
                } else {
                    typed
                }))
            }
            Stage::Busy | Stage::Starting => None,
        }
    }

    /// greetd cannot go on without an answer to `question`.
    pub fn asked(&mut self, question: &str, visible: bool, before: Option<String>) {
        if self.stage == Stage::Busy {
            self.screen.asked(&label(question));
            self.plain.clear();
            self.said = said(before);
            self.stage = Stage::Question { visible };
        }
    }

    /// The login that [`Login::submitted`] started has ended.
    pub fn judged(&mut self, verdict: Verdict) -> Next {
        if self.stage != Stage::Busy {
            return Next::Ask;
        }
        let verdict = match verdict {
            Verdict::Wrong(text) => Verdict::Wrong(said(text)),
            Verdict::Unchecked(text) => Verdict::Unchecked(said(text)),
            other => other,
        };
        let next = self.screen.judged(verdict);
        self.said = None;
        self.stage = if next == Next::Unlock {
            Stage::Starting
        } else {
            Stage::Password
        };
        next
    }

    /// The wait after a wrong password is over.
    pub fn released(&mut self) {
        self.screen.released();
    }

    /// Moves on to another of the accounts offered: `step` is 1 for the
    /// next and -1 for the one before, round and round.
    pub fn stepped(&mut self, step: isize) -> Option<Command> {
        self.screen.stirred();
        let count = self.users.len() as isize;
        if count < 2 || !matches!(self.stage, Stage::Password | Stage::Question { .. }) {
            return None;
        }
        self.refused = None;
        self.selected = (self.selected as isize + step).rem_euclid(count) as usize;
        // What was typed was meant for the other account.
        self.screen.cleared();
        match self.stage {
            Stage::Question { .. } => Some(self.give_up()),
            _ => None,
        }
    }

    /// The pointer moved, or a key that types nothing was pressed.
    pub fn stirred(&mut self) {
        self.screen.stirred();
    }

    /// True while the field is up and may be put away for lack of use.
    pub fn may_rest(&self) -> bool {
        self.screen.may_rest() && self.leaving.is_none()
    }

    /// Nothing has happened for a while. Puts the field away with what
    /// was typed and goes back to the start. True if that changed
    /// anything; the command, if there is one, is for greetd.
    pub fn rested(&mut self) -> (bool, Option<Command>) {
        if !self.may_rest() || !self.screen.rested() {
            return (false, None);
        }
        self.refused = None;
        self.plain.clear();
        let command = match self.stage {
            Stage::Question { .. } => Some(self.give_up()),
            _ => None,
        };
        if self.users.is_empty() {
            self.named = None;
            self.stage = Stage::Name;
        }
        (true, command)
    }

    pub fn keyboard(&mut self, caps_lock: bool, layout: u32) {
        self.screen.keyboard(caps_lock, layout);
    }

    /// Restart or Shut down was pressed. True if it is to be carried out;
    /// a [`Login::stayed`] is owed if it then does not happen.
    pub fn leave(&mut self, power: Power) -> bool {
        self.screen.stirred();
        if self.leaving.is_some() || self.stage == Stage::Starting {
            return false;
        }
        self.refused = None;
        self.leaving = Some(power);
        true
    }

    /// The machine would not restart or shut down.
    pub fn stayed(&mut self) {
        self.screen.stirred();
        self.refused = self.leaving.take();
    }

    pub fn view(&self) -> View {
        let mut field = self.screen.view();
        match self.stage {
            Stage::Name => {
                field.placeholder = "User name".to_string();
                field.length = 0;
            }
            Stage::Starting => {
                field.enabled = false;
                field.busy = true;
                field.placeholder = "Logging in\u{2026}".to_string();
            }
            _ => {}
        }
        let had_problem = field.note_is_problem;
        let mut tell = |note: &str, problem: bool| {
            field.note = note.to_string();
            field.note_is_problem = problem;
            field.note_is_warning = false;
        };
        match (self.leaving, self.refused) {
            (Some(Power::Restart), _) => tell("Restarting\u{2026}", false),
            (Some(Power::ShutDown), _) => tell("Shutting down\u{2026}", false),
            (None, Some(Power::Restart)) => tell("The computer could not be restarted", true),
            (None, Some(Power::ShutDown)) => tell("The computer could not be shut down", true),
            (None, None) => {
                if let (Some(said), false) = (&self.said, had_problem) {
                    tell(said, false);
                }
            }
        }
        if self.leaving.is_some() {
            field.enabled = false;
        }
        View {
            field,
            plain: self.plain.clone(),
            user: match self.stage {
                Stage::Name => String::new(),
                _ => self
                    .user()
                    .map(|user| user.shown.clone())
                    .unwrap_or_default(),
            },
            choice: self.users.len() > 1,
            power: self.leaving.is_none() && self.stage != Stage::Starting,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn user(login: &str, shown: &str) -> User {
        User {
            login: login.to_string(),
            shown: shown.to_string(),
        }
    }

    fn one() -> Login {
        Login::new(vec![user("live", "Sengi OS")], &[])
    }

    fn three() -> Login {
        Login::new(
            vec![
                user("ada", "Ada Lovelace"),
                user("grace", "Grace Hopper"),
                user("linus", "linus"),
            ],
            &[],
        )
    }

    fn type_in(login: &mut Login, text: &str) {
        for character in text.chars() {
            login.typed(&character.to_string());
        }
    }

    fn begun(command: Option<Command>) -> (String, String) {
        match command {
            Some(Command::Begin { user, password }) => (user, password.expose().to_string()),
            _ => panic!("no login was begun"),
        }
    }

    fn answered(command: Option<Command>) -> String {
        match command {
            Some(Command::Answer(answer)) => answer.expose().to_string(),
            _ => panic!("no answer was given"),
        }
    }

    #[test]
    fn the_one_user_is_there_and_the_password_logs_in() {
        let mut login = one();
        let view = login.view();
        assert_eq!(view.user, "Sengi OS");
        assert!(!view.choice && view.power && view.field.enabled);
        assert_eq!(view.field.placeholder, "Password");
        type_in(&mut login, "live");
        let view = login.view();
        assert_eq!((view.field.length, view.plain.as_str()), (4, ""));
        assert_eq!(
            begun(login.submitted()),
            ("live".to_string(), "live".to_string())
        );
        let view = login.view();
        assert!(view.field.busy && !view.field.enabled);
        assert_eq!(view.field.placeholder, "Checking\u{2026}");
        // Keys and Returns while greetd is at it go nowhere.
        type_in(&mut login, "more");
        assert!(login.submitted().is_none());
        assert!(login.stepped(1).is_none());
        assert_eq!(login.judged(Verdict::Right), Next::Unlock);
        let view = login.view();
        assert!(!view.field.enabled && view.field.busy && !view.power);
        assert_eq!(view.field.placeholder, "Logging in\u{2026}");
        assert!(login.submitted().is_none());
    }

    #[test]
    fn a_wrong_password_reads_as_on_the_lock_screen_and_waits_as_long() {
        let mut login = one();
        let mut waits = Vec::new();
        for _ in 0..5 {
            type_in(&mut login, "nope");
            assert!(login.submitted().is_some());
            match login.judged(Verdict::Wrong(None)) {
                Next::Ask => {
                    waits.push(0);
                    assert_eq!(login.view().field.note, "Wrong password");
                }
                Next::Hold(wait) => {
                    waits.push(wait.as_secs());
                    let view = login.view();
                    assert!(!view.field.enabled && view.field.rejected);
                    assert_eq!(
                        view.field.note,
                        format!("Wrong password. Wait {} seconds.", wait.as_secs())
                    );
                    type_in(&mut login, "x");
                    assert!(login.submitted().is_none());
                    login.released();
                }
                Next::Unlock => panic!("a wrong password logged in"),
            }
        }
        assert_eq!(waits, [0, 0, 2, 4, 8]);
        assert_eq!(Duration::from_secs(8), sengi_lock::backoff::delay(5));
    }

    #[test]
    fn switching_users_does_not_buy_fresh_tries() {
        let mut login = three();
        for _ in 0..2 {
            type_in(&mut login, "nope");
            login.submitted();
            login.judged(Verdict::Wrong(None));
            login.stepped(1);
        }
        type_in(&mut login, "nope");
        login.submitted();
        assert!(matches!(login.judged(Verdict::Wrong(None)), Next::Hold(_)));
    }

    #[test]
    fn nothing_but_a_right_verdict_on_a_real_login_starts_a_session() {
        let mut login = one();
        assert_eq!(login.judged(Verdict::Right), Next::Ask);
        assert!(login.view().field.enabled);
        login.asked("Password: ", false, None);
        assert_eq!(login.view().field.placeholder, "Password");
        for verdict in [
            Verdict::Wrong(None),
            Verdict::Barred,
            Verdict::Unchecked(None),
        ] {
            type_in(&mut login, "x");
            login.submitted();
            assert_ne!(login.judged(verdict), Next::Unlock);
        }
    }

    #[test]
    fn several_users_are_stepped_through_round_and_round() {
        let mut login = three();
        assert!(login.view().choice);
        assert_eq!(login.view().user, "Ada Lovelace");
        type_in(&mut login, "for ada");
        assert!(login.stepped(1).is_none());
        let view = login.view();
        assert_eq!(view.user, "Grace Hopper");
        // What was typed for Ada is not kept for Grace.
        assert_eq!(view.field.length, 0);
        login.stepped(1);
        assert_eq!(login.view().user, "linus");
        login.stepped(1);
        assert_eq!(login.view().user, "Ada Lovelace");
        login.stepped(-1);
        assert_eq!(login.view().user, "linus");
        type_in(&mut login, "pw");
        assert_eq!(
            begun(login.submitted()),
            ("linus".to_string(), "pw".to_string())
        );
        // One user has nobody to step to.
        let mut login = one();
        assert!(login.stepped(1).is_none());
        assert_eq!(login.view().user, "Sengi OS");
    }

    #[test]
    fn with_no_accounts_the_name_is_typed_and_shown() {
        let mut login = Login::new(Vec::new(), &[]);
        let view = login.view();
        assert_eq!(view.user, "");
        assert_eq!(view.field.placeholder, "User name");
        assert!(!view.choice);
        // Return on nothing stays where it is.
        assert!(login.submitted().is_none());
        type_in(&mut login, " allan");
        let view = login.view();
        assert_eq!((view.plain.as_str(), view.field.length), (" allan", 0));
        login.erased();
        type_in(&mut login, "n");
        assert!(login.submitted().is_none());
        let view = login.view();
        assert_eq!(view.user, "allan");
        assert_eq!(
            (view.plain.as_str(), view.field.placeholder.as_str()),
            ("", "Password")
        );
        type_in(&mut login, "pw");
        assert_eq!(login.view().plain, "");
        assert_eq!(
            begun(login.submitted()),
            ("allan".to_string(), "pw".to_string())
        );
        login.judged(Verdict::Wrong(None));
        // The name stays for the next try, and Escape goes back to it.
        assert_eq!(login.view().user, "allan");
        type_in(&mut login, "x");
        assert!(login.cleared().is_none());
        assert_eq!(login.view().user, "allan");
        assert!(login.cleared().is_none());
        let view = login.view();
        assert_eq!(
            (view.user.as_str(), view.field.placeholder.as_str()),
            ("", "User name")
        );
    }

    #[test]
    fn a_name_has_a_length_and_keys_that_type_nothing_add_nothing() {
        let mut login = Login::new(Vec::new(), &[]);
        for key in ["\u{f700}", "\t", "\u{10}"] {
            login.typed(key);
        }
        assert_eq!(login.view().plain, "");
        type_in(&mut login, &"n".repeat(200));
        assert_eq!(login.view().plain.chars().count(), MAX_NAME);
    }

    #[test]
    fn return_on_an_empty_field_asks_greetd_and_the_question_is_put() {
        let mut login = one();
        assert_eq!(
            begun(login.submitted()),
            ("live".to_string(), String::new())
        );
        login.asked("Password: ", false, None);
        let view = login.view();
        assert!(view.field.enabled && !view.field.busy);
        assert_eq!(view.field.placeholder, "Password");
        type_in(&mut login, "live");
        assert_eq!(login.view().field.length, 4);
        assert_eq!(answered(login.submitted()), "live");
        assert_eq!(login.judged(Verdict::Right), Next::Unlock);
    }

    #[test]
    fn a_question_asked_out_loud_is_answered_in_sight() {
        let mut login = one();
        type_in(&mut login, "live");
        login.submitted();
        login.asked(
            "Verification code: ",
            true,
            Some("A code was sent\nto your phone".to_string()),
        );
        let view = login.view();
        assert_eq!(view.field.placeholder, "Verification code");
        assert_eq!(view.field.note, "A code was sent to your phone");
        assert!(!view.field.note_is_problem);
        type_in(&mut login, "12 34");
        login.erased();
        let view = login.view();
        assert_eq!((view.plain.as_str(), view.field.length), ("12 3", 0));
        assert_eq!(answered(login.submitted()), "12 3");
        assert_eq!(login.view().plain, "");
        // And one that is not, with nothing at all for an answer.
        login.asked("", false, None);
        assert_eq!(login.view().field.placeholder, "Answer");
        assert_eq!(answered(login.submitted()), "");
        login.judged(Verdict::Wrong(Some("Not\u{202e} today".to_string())));
        let view = login.view();
        assert_eq!(view.field.note, "Not today");
        assert_eq!(view.field.placeholder, "Password");
    }

    #[test]
    fn escape_empties_the_field_and_then_gives_the_question_up() {
        let mut login = one();
        login.submitted();
        login.asked("Code: ", true, None);
        type_in(&mut login, "12");
        assert!(login.cleared().is_none());
        assert_eq!(login.view().plain, "");
        assert!(matches!(login.cleared(), Some(Command::Cancel)));
        assert_eq!(login.view().field.placeholder, "Password");
        // Nothing is open now, so there is nothing more to give up.
        assert!(login.cleared().is_none());
        type_in(&mut login, "live");
        assert_eq!(begun(login.submitted()).1, "live");
    }

    #[test]
    fn stepping_to_another_user_gives_an_open_question_up() {
        let mut login = three();
        login.submitted();
        login.asked("Code: ", false, None);
        type_in(&mut login, "12");
        assert!(matches!(login.stepped(1), Some(Command::Cancel)));
        let view = login.view();
        assert_eq!(view.user, "Grace Hopper");
        assert_eq!(
            (view.field.placeholder.as_str(), view.field.length),
            ("Password", 0)
        );
    }

    #[test]
    fn left_alone_it_goes_back_to_the_start_with_nothing_kept() {
        let mut login = one();
        type_in(&mut login, "half");
        assert!(login.may_rest());
        assert!(login.rested().0);
        let view = login.view();
        assert!(!view.field.prompt);
        assert_eq!(view.field.length, 0);
        assert!(!login.rested().0);
        login.stirred();
        assert!(login.view().field.prompt);

        // An open question is given up.
        login.submitted();
        login.asked("Code: ", true, None);
        type_in(&mut login, "12");
        let (changed, command) = login.rested();
        assert!(changed && matches!(command, Some(Command::Cancel)));
        login.stirred();
        let view = login.view();
        assert_eq!(
            (view.plain.as_str(), view.field.placeholder.as_str()),
            ("", "Password")
        );

        // Not while greetd is busy.
        type_in(&mut login, "x");
        login.submitted();
        assert!(!login.may_rest());
        assert!(!login.rested().0);

        // A typed name goes too.
        let mut login = Login::new(Vec::new(), &[]);
        type_in(&mut login, "allan");
        login.submitted();
        assert!(login.rested().0);
        login.stirred();
        let view = login.view();
        assert_eq!(
            (view.user.as_str(), view.field.placeholder.as_str()),
            ("", "User name")
        );
    }

    #[test]
    fn a_shut_down_that_does_not_happen_is_said_so() {
        let mut login = one();
        assert!(login.leave(Power::ShutDown));
        let view = login.view();
        assert_eq!(view.field.note, "Shutting down\u{2026}");
        assert!(!view.power && !view.field.enabled && !view.field.note_is_problem);
        // Once is enough.
        assert!(!login.leave(Power::Restart));
        assert!(!login.may_rest());
        login.stayed();
        let view = login.view();
        assert_eq!(view.field.note, "The computer could not be shut down");
        assert!(view.power && view.field.enabled && view.field.note_is_problem);
        // The next key takes the message away.
        login.typed("l");
        assert_eq!(login.view().field.note, "");
        assert!(login.leave(Power::Restart));
        assert_eq!(login.view().field.note, "Restarting\u{2026}");
        login.stayed();
        assert_eq!(
            login.view().field.note,
            "The computer could not be restarted"
        );
    }

    #[test]
    fn caps_lock_and_the_layout_are_shown_as_on_the_lock_screen() {
        let mut login = Login::new(vec![user("live", "Sengi OS")], &["us".into(), "fr".into()]);
        login.keyboard(true, 1);
        let view = login.view();
        assert_eq!(view.field.note, "Caps Lock is on");
        assert!(view.field.note_is_warning);
        assert_eq!(view.field.layout, "FR");
    }
}
