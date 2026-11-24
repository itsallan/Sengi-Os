//! What each app in the list is doing: waiting to be installed, being
//! installed, installed, or not installed after trying.

use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Available,
    Installing,
    Installed,
    /// The last try ended in an error. It may be tried again.
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It is installed already.
    Done,
    /// It is being installed, or another app is: they take turns, as
    /// Flatpak's own lock makes them.
    Busy,
}

#[derive(Debug, Default)]
pub struct Rows {
    installed: HashSet<String>,
    marks: HashMap<String, State>,
}

impl Rows {
    /// What the system says is installed, which replaces what was known.
    /// An app being installed stays so until its own install ends.
    pub fn set_installed(&mut self, installed: HashSet<String>) {
        self.installed = installed;
    }

    pub fn state(&self, id: &str) -> State {
        match self.marks.get(id) {
            Some(State::Installing) => State::Installing,
            _ if self.installed.contains(id) => State::Installed,
            Some(State::Failed) => State::Failed,
            _ => State::Available,
        }
    }

    pub fn busy(&self) -> bool {
        self.marks.values().any(|s| *s == State::Installing)
    }

    /// Starts an install, if the app may have one now.
    pub fn begin(&mut self, id: &str) -> Result<(), Refused> {
        match self.state(id) {
            State::Installed => return Err(Refused::Done),
            State::Installing => return Err(Refused::Busy),
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
            self.installed.insert(id.to_string());
        } else {
            self.marks.insert(id.to_string(), State::Failed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_goes_from_available_to_installed() {
        let mut rows = Rows::default();
        assert_eq!(rows.state("a.b"), State::Available);
        rows.begin("a.b").unwrap();
        assert_eq!(rows.state("a.b"), State::Installing);
        assert!(rows.busy());
        rows.finish("a.b", true);
        assert_eq!(rows.state("a.b"), State::Installed);
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
        rows.set_installed(["a.b".to_string()].into());
        assert_eq!(rows.state("a.b"), State::Installed);
        assert_eq!(rows.begin("a.b"), Err(Refused::Done));
    }

    #[test]
    fn a_new_list_does_not_end_an_install_in_progress() {
        let mut rows = Rows::default();
        rows.begin("a.b").unwrap();
        rows.set_installed(HashSet::new());
        assert_eq!(rows.state("a.b"), State::Installing);
    }
}
