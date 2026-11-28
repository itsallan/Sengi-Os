//! What each app in the list is doing: waiting to be installed, being
//! installed or removed, installed, or not installed after trying.

use std::collections::HashMap;

use crate::catalog::Installed;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Available,
    Installing,
    Installed,
    Removing,
    /// The last try to install it ended in an error. It may be tried again.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It is installed already.
    Done,
    /// It, or another app, is being installed or removed: they take turns,
    /// as Flatpak's own lock makes them. It also keeps the clearing of
    /// unused runtimes away from an install that is not finished.
    Busy,
    /// It is not in the user's own installation, so it is not the Store's
    /// to remove.
    NotYours,
}

#[derive(Debug, Default)]
pub struct Rows {
    installed: Installed,
    marks: HashMap<String, State>,
}

impl Rows {
    /// What the system says is installed, which replaces what was known.
    /// An app being installed or removed stays so until that ends.
    pub fn set_installed(&mut self, installed: Installed) {
        self.installed = installed;
    }

    pub fn state(&self, id: &str) -> State {
        match self.marks.get(id) {
            Some(state @ (State::Installing | State::Removing)) => *state,
            _ if self.installed.all.contains(id) => State::Installed,
            Some(State::Failed) => State::Failed,
            _ => State::Available,
        }
    }

    /// Whether the app is there to be run, which is when Open is shown.
    pub fn may_open(&self, id: &str) -> bool {
        self.state(id) == State::Installed
    }

    /// Whether Remove is shown: installed, in the user's own installation.
    pub fn may_remove(&self, id: &str) -> bool {
        self.state(id) == State::Installed && self.installed.user.contains(id)
    }

    pub fn busy(&self) -> bool {
        self.marks
            .values()
            .any(|s| matches!(s, State::Installing | State::Removing))
    }

    /// Starts an install, if the app may have one now.
    pub fn begin(&mut self, id: &str) -> Result<(), Refused> {
        match self.state(id) {
            State::Installed => return Err(Refused::Done),
            State::Installing | State::Removing => return Err(Refused::Busy),
            State::Available | State::Failed => {}
        }
        if self.busy() {
            return Err(Refused::Busy);
        }
        self.marks.insert(id.to_string(), State::Installing);
        Ok(())
    }

    /// Ends the install `begin` started.
    pub fn finish(&mut self, id: &str, worked: bool) {
        if worked {
            self.marks.remove(id);
            self.installed.all.insert(id.to_string());
            self.installed.user.insert(id.to_string());
        } else {
            self.marks.insert(id.to_string(), State::Failed);
        }
    }

    /// Ends an install the user stopped, which is no failure.
    pub fn stopped(&mut self, id: &str) {
        self.marks.remove(id);
    }

    /// Starts a removal, if the app may have one now.
    pub fn begin_removal(&mut self, id: &str) -> Result<(), Refused> {
        if self.busy() {
            return Err(Refused::Busy);
        }
        if !self.may_remove(id) {
            return Err(Refused::NotYours);
        }
        self.marks.insert(id.to_string(), State::Removing);
        Ok(())
    }

    /// Ends the removal `begin_removal` started.
    pub fn finish_removal(&mut self, id: &str, worked: bool) {
        self.marks.remove(id);
        if worked {
            self.installed.all.remove(id);
            self.installed.user.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installed for the user, and for the system only.
    fn installed(user: &[&str], system: &[&str]) -> Installed {
        let own = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        Installed {
            all: own(user).into_iter().chain(own(system)).collect(),
            user: own(user).into_iter().collect(),
        }
    }

    #[test]
    fn an_app_goes_from_available_to_installed() {
        let mut rows = Rows::default();
        assert_eq!(rows.state("a.b"), State::Available);
        rows.begin("a.b").unwrap();
        assert_eq!(rows.state("a.b"), State::Installing);
        assert!(rows.busy());
        rows.finish("a.b", true);
        assert_eq!(rows.state("a.b"), State::Installed);
        assert!(rows.may_remove("a.b"));
        assert!(!rows.busy());
    }

    #[test]
    fn a_failed_install_may_be_tried_again() {
        let mut rows = Rows::default();
        rows.begin("a.b").unwrap();
        rows.finish("a.b", false);
        assert_eq!(rows.state("a.b"), State::Failed);
        assert!(!rows.busy());
        rows.begin("a.b").unwrap();
        rows.finish("a.b", true);
        assert_eq!(rows.state("a.b"), State::Installed);
    }

    #[test]
    fn a_stopped_install_is_not_a_failed_one() {
        let mut rows = Rows::default();
        rows.begin("a.b").unwrap();
        rows.finish("a.b", false);
        rows.begin("a.b").unwrap();
        rows.stopped("a.b");
        assert_eq!(rows.state("a.b"), State::Available);
        assert!(!rows.busy());
        assert!(rows.begin("a.b").is_ok());
    }

    #[test]
    fn installs_take_turns() {
        let mut rows = Rows::default();
        rows.begin("a.b").unwrap();
        assert_eq!(rows.begin("a.b"), Err(Refused::Busy));
        assert_eq!(rows.begin("c.d"), Err(Refused::Busy));
        rows.finish("a.b", true);
        assert!(rows.begin("c.d").is_ok());
    }

    #[test]
    fn installed_apps_are_not_installed_again() {
        let mut rows = Rows::default();
        rows.set_installed(installed(&["a.b"], &[]));
        assert_eq!(rows.state("a.b"), State::Installed);
        assert_eq!(rows.begin("a.b"), Err(Refused::Done));
    }

    #[test]
    fn only_an_installed_app_may_be_opened() {
        let mut rows = Rows::default();
        rows.set_installed(installed(&["a.b"], &[]));
        assert!(rows.may_open("a.b"));
        assert!(!rows.may_open("c.d"));
        rows.begin("c.d").unwrap();
        assert!(!rows.may_open("c.d"));
        rows.finish("c.d", true);
        assert!(rows.may_open("c.d"));
    }

    #[test]
    fn only_the_users_own_app_may_be_removed() {
        let mut rows = Rows::default();
        rows.set_installed(installed(&["a.b"], &["s.y"]));
        assert!(rows.may_open("s.y") && !rows.may_remove("s.y"));
        assert_eq!(rows.begin_removal("s.y"), Err(Refused::NotYours));
        assert_eq!(rows.begin_removal("c.d"), Err(Refused::NotYours));
        rows.begin_removal("a.b").unwrap();
        assert_eq!(rows.state("a.b"), State::Removing);
        assert!(!rows.may_open("a.b") && !rows.may_remove("a.b"));
        rows.finish_removal("a.b", true);
        assert_eq!(rows.state("a.b"), State::Available);
    }

    #[test]
    fn a_removal_that_failed_leaves_the_app_installed() {
        let mut rows = Rows::default();
        rows.set_installed(installed(&["a.b"], &[]));
        rows.begin_removal("a.b").unwrap();
        rows.finish_removal("a.b", false);
        assert!(rows.may_remove("a.b"));
        assert!(!rows.busy());
    }

    #[test]
    fn installs_and_removals_take_turns() {
        let mut rows = Rows::default();
        rows.set_installed(installed(&["a.b", "e.f"], &[]));
        rows.begin("c.d").unwrap();
        assert_eq!(rows.begin_removal("a.b"), Err(Refused::Busy));
        rows.finish("c.d", true);
        rows.begin_removal("a.b").unwrap();
        assert_eq!(rows.begin("g.h"), Err(Refused::Busy));
        assert_eq!(rows.begin_removal("e.f"), Err(Refused::Busy));
    }

    #[test]
    fn a_new_list_does_not_end_an_install_in_progress() {
        let mut rows = Rows::default();
        rows.begin("a.b").unwrap();
        rows.set_installed(Installed::default());
        assert_eq!(rows.state("a.b"), State::Installing);
    }
}
