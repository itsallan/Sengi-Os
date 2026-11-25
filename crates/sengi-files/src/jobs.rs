//! The things Files does to several items at once, each returning what to
//! say about it. They are run on a thread of their own when they could take
//! a while, and have no window in them.

use std::path::{Path, PathBuf};

use sengi_system::trash;

use crate::fsops;
use crate::guard::Guard;

/// What a job did, as a line to show.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Items done.
    pub done: usize,
    /// Of those, ones that got a numbered name because theirs was taken.
    pub renamed: usize,
    /// Of those, ones that were already where they were meant to go.
    pub unchanged: usize,
    /// Parts that were left out because they are not files.
    pub skipped: usize,
    pub errors: Vec<String>,
    /// The last item made or moved, to select afterwards.
    pub last: Option<PathBuf>,
}

fn plural(count: usize) -> String {
    if count == 1 {
        "1 item".to_string()
    } else {
        format!("{count} items")
    }
}

impl Report {
    pub fn failed(&self) -> bool {
        !self.errors.is_empty()
    }

    /// One line: what happened, then the first thing that went wrong.
    pub fn say(&self, verb: &str) -> String {
        self.say_with(verb, "")
    }

    /// As [`Report::say`], with words after the count: "Moved 2 items to the
    /// Trash".
    pub fn say_with(&self, verb: &str, after: &str) -> String {
        let mut text = if self.done == 0 && self.unchanged > 0 && self.errors.is_empty() {
            "It is already there".to_string()
        } else if self.done == 0 {
            String::new()
        } else {
            format!("{verb} {}{after}", plural(self.done))
        };
        if self.renamed > 0 {
            text.push_str(&format!(
                ", {} with a new name because the old one was taken",
                self.renamed
            ));
        }
        if !text.is_empty() {
            text.push('.');
        }
        if self.skipped > 0 {
            text.push_str(&format!(
                " {} not copied: not a file or folder.",
                plural(self.skipped)
            ));
        }
        if let Some(first) = self.errors.first() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(first);
            if self.errors.len() > 1 {
                text.push_str(&format!(" And {} more went wrong.", self.errors.len() - 1));
            }
        }
        text
    }
}

fn error_text(verb: &str, path: &Path, error: &std::io::Error) -> String {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    format!("Cannot {verb} \"{name}\": {}.", trash::plain(error))
}

/// Pastes `items` into `dest`: copies them, or with `cut` moves them. Nothing
/// is replaced; a name that is taken is made "name 2". What fails is
/// reported and the rest goes on.
pub fn paste(items: &[PathBuf], cut: bool, dest: &Path) -> Report {
    let mut report = Report::default();
    for item in items {
        if cut && item.parent() == Some(dest) {
            // Moving a thing to where it is.
            report.unchanged += 1;
            continue;
        }
        let name = item.file_name().map(|n| n.to_os_string());
        let result = if cut {
            fsops::move_into(item, dest).map(|path| (path, 0))
        } else {
            fsops::copy_into(item, dest).map(|copied| (copied.path, copied.skipped))
        };
        match result {
            Ok((path, skipped)) => {
                report.done += 1;
                report.skipped += skipped;
                if path.file_name().map(|n| n.to_os_string()) != name {
                    report.renamed += 1;
                }
                report.last = Some(path);
            }
            Err(error) => {
                report
                    .errors
                    .push(error_text(if cut { "move" } else { "copy" }, item, &error))
            }
        }
    }
    report
}

/// Moves `items` to the Trash. The caller has checked each with the guard.
pub fn trash_items(trash_dir: &Path, items: &[PathBuf]) -> Report {
    let mut report = Report::default();
    let date = trash::now();
    for item in items {
        match trash::trash(trash_dir, item, &date) {
            Ok(_) => report.done += 1,
            Err(error) => report.errors.push(trash::explain(&error)),
        }
    }
    report
}

/// Puts the Trash's items called `names` back where they came from. An item
/// whose old place is outside the home folder stays in the Trash.
pub fn restore_items(guard: &Guard, trash_dir: &Path, names: &[String]) -> Report {
    let mut report = Report::default();
    let known = trash::list(trash_dir).unwrap_or_default();
    for name in names {
        let Some(item) = known.iter().find(|item| &item.name == name) else {
            report
                .errors
                .push(format!("Cannot restore \"{name}\": it is not there."));
            continue;
        };
        if let Some(original) = &item.original {
            if let Err(reason) = guard.may_restore_to(original) {
                report
                    .errors
                    .push(format!("Cannot restore \"{name}\": {reason}"));
                continue;
            }
        }
        match trash::restore(trash_dir, name) {
            Ok(path) => {
                report.done += 1;
                if item
                    .original
                    .as_ref()
                    .is_some_and(|original| *original != path)
                {
                    report.renamed += 1;
                }
                report.last = Some(path);
            }
            Err(error) => report.errors.push(format!(
                "Cannot restore \"{name}\": {}.",
                trash::plain(&error)
            )),
        }
    }
    report
}

/// Deletes everything in the Trash for good. The caller has asked first.
pub fn empty_trash(guard: &Guard, trash_dir: &Path) -> Report {
    let mut report = Report::default();
    if let Err(reason) = guard.may_empty(trash_dir) {
        report.errors.push(reason);
        return report;
    }
    match trash::empty(trash_dir) {
        Ok(count) => report.done = count,
        Err(error) => report
            .errors
            .push(format!("Cannot empty the Trash: {}.", trash::plain(&error))),
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::tests::Temp;
    use std::fs;

    fn home() -> (Temp, Guard, PathBuf) {
        let t = Temp::new();
        fs::create_dir_all(t.path().join("home/.local/share")).unwrap();
        let bin = t.path().join("home/.local/share/Trash");
        let guard = Guard::new(&t.path().join("home"), &bin).unwrap();
        (t, guard, bin)
    }

    #[test]
    fn pasting_copies_and_renames_what_is_taken() {
        let t = Temp::new();
        let a = t.file("src/a.txt", "a");
        let b = t.file("src/b.txt", "b");
        t.file("dst/a.txt", "already");
        let report = paste(&[a.clone(), b.clone()], false, &t.path().join("dst"));
        assert_eq!((report.done, report.renamed), (2, 1));
        assert!(!report.failed());
        assert_eq!(
            fs::read_to_string(t.path().join("dst/a.txt")).unwrap(),
            "already"
        );
        assert_eq!(
            fs::read_to_string(t.path().join("dst/a 2.txt")).unwrap(),
            "a"
        );
        assert!(a.exists() && b.exists());
        assert_eq!(
            report.say("Pasted"),
            "Pasted 2 items, 1 with a new name because the old one was taken."
        );
    }

    #[test]
    fn cutting_moves_and_pasting_where_it_is_does_nothing() {
        let t = Temp::new();
        let a = t.file("src/a.txt", "a");
        let report = paste(std::slice::from_ref(&a), true, &t.path().join("src"));
        assert_eq!((report.done, report.unchanged), (0, 1));
        assert_eq!(report.say("Moved"), "It is already there.");
        assert!(a.exists());
        fs::create_dir(t.path().join("dst")).unwrap();
        let report = paste(std::slice::from_ref(&a), true, &t.path().join("dst"));
        assert_eq!(report.done, 1);
        assert!(!a.exists());
        assert!(t.path().join("dst/a.txt").exists());
    }

    #[test]
    fn what_fails_is_said_and_the_rest_goes_on() {
        let t = Temp::new();
        let a = t.file("src/a.txt", "a");
        fs::create_dir(t.path().join("dst")).unwrap();
        let report = paste(
            &[t.path().join("src/missing"), a],
            false,
            &t.path().join("dst"),
        );
        assert_eq!(report.done, 1);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].starts_with("Cannot copy \"missing\":"));
        assert!(report
            .say("Pasted")
            .starts_with("Pasted 1 item. Cannot copy"));
    }

    #[test]
    fn trashing_and_restoring_round_trip() {
        let (t, guard, bin) = home();
        let a = t.file("home/Documents/a.txt", "a");
        let report = trash_items(&bin, std::slice::from_ref(&a));
        assert_eq!(report.done, 1);
        assert!(!a.exists());
        let report = restore_items(&guard, &bin, &["a.txt".to_string()]);
        assert_eq!((report.done, report.renamed), (1, 0));
        assert_eq!(fs::read_to_string(a).unwrap(), "a");
    }

    #[test]
    fn a_trashed_item_is_not_restored_over_a_new_one() {
        let (t, guard, bin) = home();
        let a = t.file("home/a.txt", "old");
        trash_items(&bin, std::slice::from_ref(&a));
        t.file("home/a.txt", "new");
        let report = restore_items(&guard, &bin, &["a.txt".to_string()]);
        assert_eq!(report.renamed, 1);
        assert_eq!(fs::read_to_string(a).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(t.path().join("home/a 2.txt")).unwrap(),
            "old"
        );
    }

    #[test]
    fn an_item_that_came_from_outside_home_is_not_restored() {
        let (t, guard, bin) = home();
        let outside = t.file("elsewhere/x", "x");
        trash::trash(&bin, &outside, "d").unwrap();
        let report = restore_items(&guard, &bin, &["x".to_string()]);
        assert_eq!(report.done, 0);
        assert!(report.errors[0].contains("only changes things inside your home folder"));
        assert!(bin.join("files/x").exists());
        assert!(!outside.exists());
    }

    #[test]
    fn a_missing_trash_item_is_said() {
        let (_t, guard, bin) = home();
        let report = restore_items(&guard, &bin, &["nope".to_string()]);
        assert!(report.errors[0].contains("not there"));
    }

    #[test]
    fn emptying_the_trash_deletes_only_what_is_in_it() {
        let (t, guard, bin) = home();
        let a = t.file("home/a.txt", "a");
        let keep = t.file("home/keep.txt", "k");
        trash_items(&bin, &[a]);
        let report = empty_trash(&guard, &bin);
        assert_eq!(report.done, 1);
        assert!(keep.exists());
        assert_eq!(fs::read_dir(bin.join("files")).unwrap().count(), 0);
        assert_eq!(empty_trash(&guard, &bin).done, 0);
    }

    #[test]
    fn a_trash_outside_home_is_not_emptied() {
        let (t, guard, _bin) = home();
        let other = t.path().join("elsewhere/Trash");
        fs::create_dir_all(other.join("files/x")).unwrap();
        let report = empty_trash(&guard, &other);
        assert!(report.failed());
        assert!(other.join("files/x").exists());
    }

    #[test]
    fn a_report_says_what_was_left_out() {
        let report = Report {
            done: 1,
            skipped: 2,
            ..Report::default()
        };
        assert_eq!(
            report.say("Pasted"),
            "Pasted 1 item. 2 items not copied: not a file or folder."
        );
    }
}
