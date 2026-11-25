//! The steps of the welcome and how they follow each other.

/// The steps, in the order they are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Look,
    Network,
    Keyboard,
    Apps,
    Done,
}

impl Step {
    pub const ALL: [Step; 6] = [
        Step::Welcome,
        Step::Look,
        Step::Network,
        Step::Keyboard,
        Step::Apps,
        Step::Done,
    ];

    /// The number the window knows the step by.
    pub fn index(self) -> i32 {
        Step::ALL.iter().position(|s| *s == self).unwrap_or(0) as i32
    }
}

/// Where the user is. The network step is left out when a cable is already
/// connected: there is nothing to set up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wizard {
    current: Step,
    skip_network: bool,
}

impl Wizard {
    pub fn new(wired: bool) -> Wizard {
        Wizard {
            current: Step::Welcome,
            skip_network: wired,
        }
    }

    pub fn current(&self) -> Step {
        self.current
    }

    fn shown(&self) -> Vec<Step> {
        Step::ALL
            .into_iter()
            .filter(|s| !(self.skip_network && *s == Step::Network))
            .collect()
    }

    fn at(&self) -> usize {
        self.shown()
            .iter()
            .position(|s| *s == self.current)
            .unwrap_or(0)
    }

    /// Whether a cable is connected. The step the user is on is never taken
    /// away from under them.
    pub fn set_wired(&mut self, wired: bool) {
        self.skip_network = wired && self.current != Step::Network;
    }

    /// "Step 2 of 5": place and total among the steps that are shown.
    pub fn position(&self) -> (usize, usize) {
        (self.at() + 1, self.shown().len())
    }

    pub fn is_first(&self) -> bool {
        self.at() == 0
    }

    pub fn is_last(&self) -> bool {
        self.current == Step::Done
    }

    /// Goes on. False on the last step, where there is nowhere to go.
    pub fn advance(&mut self) -> bool {
        let shown = self.shown();
        match shown.get(self.at() + 1) {
            Some(step) => {
                self.current = *step;
                true
            }
            None => false,
        }
    }

    pub fn back(&mut self) -> bool {
        let at = self.at();
        if at == 0 {
            return false;
        }
        self.current = self.shown()[at - 1];
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_walks_every_step_in_order_and_back() {
        let mut wizard = Wizard::new(false);
        let mut seen = vec![wizard.current()];
        while wizard.advance() {
            seen.push(wizard.current());
        }
        assert_eq!(seen, Step::ALL);
        assert!(wizard.is_last());
        assert_eq!(wizard.position(), (6, 6));
        while wizard.back() {}
        assert_eq!(wizard.current(), Step::Welcome);
        assert!(wizard.is_first());
        assert!(!wizard.back());
    }

    #[test]
    fn a_wired_connection_skips_the_network_step() {
        let mut wizard = Wizard::new(true);
        assert!(wizard.advance());
        assert_eq!(wizard.current(), Step::Look);
        assert_eq!(wizard.position(), (2, 5));
        assert!(wizard.advance());
        assert_eq!(wizard.current(), Step::Keyboard);
        assert!(wizard.back());
        assert_eq!(wizard.current(), Step::Look);
    }

    #[test]
    fn the_step_the_user_is_on_is_not_taken_away() {
        let mut wizard = Wizard::new(false);
        wizard.advance();
        wizard.advance();
        assert_eq!(wizard.current(), Step::Network);
        wizard.set_wired(true);
        assert_eq!(wizard.current(), Step::Network);
        assert_eq!(wizard.position(), (3, 6));
    }

    #[test]
    fn indexes_match_the_order() {
        for (i, step) in Step::ALL.into_iter().enumerate() {
            assert_eq!(step.index(), i as i32);
        }
    }
}
