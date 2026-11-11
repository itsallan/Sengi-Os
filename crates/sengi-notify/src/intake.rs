//! Hands out notification ids and remembers who owns each open one.
//!
//! An id belongs to the client that was given it for as long as its
//! notification is open. Only that client can replace it: anyone else
//! asking for the same id gets a new notification under a new id, so one
//! program cannot rewrite another's card and collect the click meant for
//! the original.

use std::collections::HashMap;

struct Owner {
    sender: String,
    token: u64,
}

/// An id for a `Notify` call, and a token that names this call alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Admitted {
    pub id: u32,
    pub token: u64,
}

#[derive(Default)]
pub struct Intake {
    last_id: u32,
    last_token: u64,
    open: HashMap<u32, Owner>,
}

impl Intake {
    /// Chooses the id for a call from `sender` asking to replace
    /// `replaces_id`, where 0 asks for nothing.
    ///
    /// The requested id is kept unless another sender's open notification
    /// has it. That an id was never handed out, or was closed long ago, is
    /// no reason to refuse: nothing open can be touched through it, and
    /// programs do keep one id for a progress bar or a volume display.
    pub fn admit(&mut self, sender: &str, replaces_id: u32) -> Admitted {
        let theirs = self
            .open
            .get(&replaces_id)
            .is_some_and(|owner| owner.sender != sender);
        let id = if replaces_id != 0 && !theirs {
            replaces_id
        } else {
            self.fresh()
        };
        self.last_token += 1;
        let token = self.last_token;
        self.open.insert(
            id,
            Owner {
                sender: sender.to_string(),
                token,
            },
        );
        Admitted { id, token }
    }

    /// Ids count up from 1, skip 0 when they wrap, as the spec asks, and
    /// skip any that is still open.
    fn fresh(&mut self) -> u32 {
        loop {
            self.last_id = match self.last_id.wrapping_add(1) {
                0 => 1,
                id => id,
            };
            if !self.open.contains_key(&self.last_id) {
                return self.last_id;
            }
        }
    }

    /// The notification under `id` is closed. `token` is that of the
    /// content that was closed; if the id has been admitted again since,
    /// it stays with its new owner.
    pub fn release(&mut self, id: u32, token: u64) {
        if self.open.get(&id).is_some_and(|owner| owner.token == token) {
            self.open.remove(&id);
        }
    }

    /// Takes back an admission that never reached the UI.
    pub fn withdraw(&mut self, admitted: Admitted) {
        self.release(admitted.id, admitted.token);
    }

    #[cfg(test)]
    pub fn open(&self) -> usize {
        self.open.len()
    }

    #[cfg(test)]
    fn at(last_id: u32) -> Intake {
        Intake {
            last_id,
            ..Intake::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_count_up_from_one() {
        let mut intake = Intake::default();
        assert_eq!(intake.admit(":1.1", 0).id, 1);
        assert_eq!(intake.admit(":1.1", 0).id, 2);
        assert_eq!(intake.admit(":1.2", 0).id, 3);
    }

    #[test]
    fn every_call_has_its_own_token() {
        let mut intake = Intake::default();
        let first = intake.admit(":1.1", 0);
        let again = intake.admit(":1.1", first.id);
        assert_eq!(again.id, first.id);
        assert_ne!(again.token, first.token);
    }

    #[test]
    fn the_owner_may_replace_its_notification() {
        let mut intake = Intake::default();
        let first = intake.admit(":1.1", 0);
        assert_eq!(intake.admit(":1.1", first.id).id, first.id);
        assert_eq!(intake.open(), 1);
    }

    #[test]
    fn another_sender_gets_a_new_id_instead() {
        let mut intake = Intake::default();
        let theirs = intake.admit(":1.1", 0);
        let mine = intake.admit(":1.66", theirs.id);
        assert_ne!(mine.id, theirs.id);
        assert_eq!(intake.open(), 2);
        // And the original is still its owner's to replace.
        assert_eq!(intake.admit(":1.1", theirs.id).id, theirs.id);
    }

    #[test]
    fn a_closed_id_can_be_used_again_by_anyone() {
        let mut intake = Intake::default();
        let first = intake.admit(":1.1", 0);
        intake.release(first.id, first.token);
        assert_eq!(intake.open(), 0);
        assert_eq!(intake.admit(":1.2", first.id).id, first.id);
        // Now it is theirs.
        assert_ne!(intake.admit(":1.1", first.id).id, first.id);
    }

    #[test]
    fn an_id_that_was_never_handed_out_is_accepted() {
        let mut intake = Intake::default();
        assert_eq!(intake.admit(":1.1", 9_999).id, 9_999);
        assert_eq!(intake.admit(":1.1", u32::MAX).id, u32::MAX);
        // Counting goes on where it was.
        assert_eq!(intake.admit(":1.1", 0).id, 1);
    }

    #[test]
    fn a_late_closure_does_not_release_newer_content() {
        let mut intake = Intake::default();
        let first = intake.admit(":1.1", 0);
        // Replaced while the UI was already closing the first content.
        let second = intake.admit(":1.1", first.id);
        intake.release(first.id, first.token);
        assert_eq!(intake.open(), 1);
        assert_ne!(intake.admit(":1.2", first.id).id, first.id);
        intake.release(second.id, second.token);
        assert_eq!(intake.open(), 1);
    }

    #[test]
    fn fresh_ids_skip_zero_and_ids_that_are_open() {
        let mut intake = Intake::at(u32::MAX - 1);
        let held = intake.admit(":1.1", 1);
        assert_eq!(held.id, 1);
        assert_eq!(intake.admit(":1.2", 0).id, u32::MAX);
        // Wraps past 0, and past 1, which is still open.
        assert_eq!(intake.admit(":1.2", 0).id, 2);
    }

    #[test]
    fn replacing_works_the_same_after_the_counter_has_wrapped() {
        let mut intake = Intake::at(u32::MAX);
        let old = intake.admit(":1.1", 0);
        assert_eq!(old.id, 1);
        let high = intake.admit(":1.1", 4_000_000_000);
        assert_eq!(high.id, 4_000_000_000);
        assert_eq!(intake.admit(":1.1", high.id).id, high.id);
        assert_ne!(intake.admit(":1.9", high.id).id, high.id);
    }

    #[test]
    fn a_withdrawn_admission_leaves_nothing_behind() {
        let mut intake = Intake::default();
        let admitted = intake.admit(":1.1", 0);
        intake.withdraw(admitted);
        assert_eq!(intake.open(), 0);
    }
}
