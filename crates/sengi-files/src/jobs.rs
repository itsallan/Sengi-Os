//! The things Files does to several items at once, each returning what to
//! say about it. They are run on a thread of their own when they could take
//! a while, and have no window in them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use sengi_system::trash;

use crate::fsops::{self, Ctl};
use crate::guard::{volume_trash_dir, Guard};

/// How far a copy or move has got, written by the thread doing it and read
/// by the window, which can also ask it to stop.
#[derive(Debug, Default)]
pub struct Progress {
    pub cancel: AtomicBool,
    /// Bytes written so far, and how many there will be when it is done.
    pub bytes: AtomicU64,
    pub bytes_total: AtomicU64,
    /// Items finished, and how many were asked for.
    pub items: AtomicUsize,
    pub items_total: AtomicUsize,
}

impl Progress {
    /// "Copying 3 of 12 items, 41.2 MB of 120.0 MB".
    pub fn line(&self, doing: &str) -> String {
        let total = self.items_total.load(Ordering::Relaxed);
        let at = (self.items.load(Ordering::Relaxed) + 1).min(total.max(1));
        let mut text = format!("{doing} {at} of {}", plural(total));
        let bytes_total = self.bytes_total.load(Ordering::Relaxed);
        if bytes_total > 0 {
            text.push_str(&format!(
                ", {} of {}",
                fsops::size_text(self.bytes.load(Ordering::Relaxed).min(bytes_total)),
                fsops::size_text(bytes_total)
            ));
        }
        text
    }
}

/// What a job did, as a line to show.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Stopped by the user before it was done.
    pub cancelled: bool,
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
        if self.cancelled {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str("Cancelled.");
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
    paste_with(items, cut, dest, &Progress::default(), &|| {})
}

/// As [`paste`], writing how far it has got to `progress`, calling `tick`
/// after every piece so that the window can show it, and stopping early,
/// with what is half copied removed again, when `progress.cancel` is set.
/// Whether the disk has room for what has to be copied is checked first.
pub fn paste_with(
    items: &[PathBuf],
    cut: bool,
    dest: &Path,
    progress: &Progress,
    tick: &dyn Fn(),
) -> Report {
    let mut report = Report::default();
    // A move on one disk is a rename and writes nothing.
    let copies = |item: &PathBuf| {
        !cut || !item
            .parent()
            .is_some_and(|parent| fsops::same_disk(parent, dest))
    };
    let bytes: u64 = items
        .iter()
        .filter(|item| copies(item))
        .map(|item| fsops::tree_bytes(item))
        .sum();
    progress.items_total.store(items.len(), Ordering::Relaxed);
    progress.bytes_total.store(bytes, Ordering::Relaxed);
    if bytes > 0 {
        if let Some(free) = fsops::free_space(dest) {
            if bytes > free {
                report.errors.push(format!(
                    "There is not enough space on the disk: {} needed, {} free.",
                    fsops::size_text(bytes),
                    fsops::size_text(free)
                ));
                return report;
            }
        }
    }
    tick();
    let ctl = Ctl {
        cancel: Some(&progress.cancel),
        bytes: Some(&progress.bytes),
        tick: Some(tick),
    };
    for item in items {
        if progress.cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            break;
        }
        if cut && item.parent() == Some(dest) {
            // Moving a thing to where it is.
            report.unchanged += 1;
            progress.items.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let name = item.file_name().map(|n| n.to_os_string());
        let result = if cut {
            fsops::move_into_with(item, dest, &ctl).map(|path| (path, 0))
        } else {
            fsops::copy_into_with(item, dest, &ctl).map(|copied| (copied.path, copied.skipped))
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
            Err(error) if fsops::is_cancelled(&error) => {
                report.cancelled = true;
                break;
            }
            Err(error) => {
                report
                    .errors
                    .push(error_text(if cut { "move" } else { "copy" }, item, &error));
                // A full disk is full for the rest as well.
                if error.raw_os_error() == Some(libc::ENOSPC) {
                    break;
                }
            }
        }
        progress.items.fetch_add(1, Ordering::Relaxed);
        tick();
    }
    report
}

/// Moves `items` to the Trash: the user's own for what is in the home
/// folder, and for what is on a drive, that drive's, so that it stays on
/// the drive it came from. The caller has checked each with the guard.
pub fn trash_items(guard: &Guard, home_trash: &Path, items: &[PathBuf]) -> Report {
    let mut report = Report::default();
    let date = trash::now();
    for item in items {
        let result = match guard.volume_of(item) {
            Some(volume) => {
                trash::trash_on_volume(&volume_trash_dir(&volume), item, &date, &volume)
            }
            None => trash::trash(home_trash, item, &date),
        };
        match result {
            Ok(_) => report.done += 1,
            Err(error) => report.errors.push(trash::explain(&error)),
        }
    }
    report
}

/// One item in a Trash: its name there, and the drive it is on if it is in
/// a drive's Trash and not the user's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binned {
    pub name: String,
    pub volume: Option<PathBuf>,
}

/// What is in the Trash of the drive `volume`.
pub fn list_volume_trash(volume: &Path) -> std::io::Result<Vec<trash::Item>> {
    trash::list_on_volume(&volume_trash_dir(volume), volume)
}

/// Puts the Trash's `items` back where they came from. An item whose old
/// place is outside the home folder and the drives stays in the Trash.
pub fn restore_items(guard: &Guard, home_trash: &Path, items: &[Binned]) -> Report {
    let mut report = Report::default();
    let mut lists: HashMap<Option<PathBuf>, Vec<trash::Item>> = HashMap::new();
    for binned in items {
        let name = &binned.name;
        let known = lists.entry(binned.volume.clone()).or_insert_with(|| {
            let listed = match &binned.volume {
                Some(volume) => list_volume_trash(volume),
                None => trash::list(home_trash),
            };
            listed.unwrap_or_default()
        });
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
        let restored = match &binned.volume {
            Some(volume) => trash::restore_on_volume(&volume_trash_dir(volume), name, volume),
            None => trash::restore(home_trash, name),
        };
        match restored {
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

/// Deletes everything in the Trash for good: the user's own, and the Trash
/// of each of the mounted drives in `volumes`. The caller has asked first.
pub fn empty_trash(guard: &Guard, home_trash: &Path, volumes: &[PathBuf]) -> Report {
    let mut report = Report::default();
    let mut bins = vec![home_trash.to_path_buf()];
    // A drive that has never had anything trashed has no Trash to empty.
    bins.extend(
        volumes
            .iter()
            .map(|volume| volume_trash_dir(volume))
            .filter(|bin| bin.exists()),
    );
    for bin in &bins {
        if let Err(reason) = guard.may_empty(bin) {
            report.errors.push(reason);
            continue;
        }
        match trash::empty(bin) {
            Ok(count) => report.done += count,
            Err(error) => report
                .errors
                .push(format!("Cannot empty the Trash: {}.", trash::plain(&error))),
        }
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
        let report = trash_items(&guard, &bin, std::slice::from_ref(&a));
        assert_eq!(report.done, 1);
        assert!(!a.exists());
        let report = restore_items(&guard, &bin, &[home_item("a.txt")]);
        assert_eq!((report.done, report.renamed), (1, 0));
        assert_eq!(fs::read_to_string(a).unwrap(), "a");
    }

    #[test]
    fn a_trashed_item_is_not_restored_over_a_new_one() {
        let (t, guard, bin) = home();
        let a = t.file("home/a.txt", "old");
        trash_items(&guard, &bin, std::slice::from_ref(&a));
        t.file("home/a.txt", "new");
        let report = restore_items(&guard, &bin, &[home_item("a.txt")]);
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
        let report = restore_items(&guard, &bin, &[home_item("x")]);
        assert_eq!(report.done, 0);
        assert!(report.errors[0].contains("only changes things inside your home folder"));
        assert!(bin.join("files/x").exists());
        assert!(!outside.exists());
    }

    #[test]
    fn a_missing_trash_item_is_said() {
        let (_t, guard, bin) = home();
        let report = restore_items(&guard, &bin, &[home_item("nope")]);
        assert!(report.errors[0].contains("not there"));
    }

    #[test]
    fn emptying_the_trash_deletes_only_what_is_in_it() {
        let (t, guard, bin) = home();
        let a = t.file("home/a.txt", "a");
        let keep = t.file("home/keep.txt", "k");
        trash_items(&guard, &bin, &[a]);
        let report = empty_trash(&guard, &bin, &[]);
        assert_eq!(report.done, 1);
        assert!(keep.exists());
        assert_eq!(fs::read_dir(bin.join("files")).unwrap().count(), 0);
        assert_eq!(empty_trash(&guard, &bin, &[]).done, 0);
    }

    #[test]
    fn a_trash_outside_home_is_not_emptied() {
        let (t, guard, _bin) = home();
        let other = t.path().join("elsewhere/Trash");
        fs::create_dir_all(other.join("files/x")).unwrap();
        let report = empty_trash(&guard, &other, &[]);
        assert!(report.failed());
        assert!(other.join("files/x").exists());
    }

    fn home_item(name: &str) -> Binned {
        Binned {
            name: name.to_string(),
            volume: None,
        }
    }

    /// A home folder, and a drive mounted beside it, both just folders.
    fn with_drive() -> (Temp, Guard, PathBuf, PathBuf) {
        let t = Temp::new();
        fs::create_dir_all(t.path().join("home/.local/share")).unwrap();
        fs::create_dir_all(t.path().join("media/stick")).unwrap();
        let bin = t.path().join("home/.local/share/Trash");
        let guard =
            Guard::with_media(&t.path().join("home"), &bin, Some(&t.path().join("media"))).unwrap();
        let stick = t.path().join("media/stick");
        (t, guard, bin, stick)
    }

    #[test]
    fn copying_between_home_and_a_drive_never_replaces() {
        let (t, _guard, _bin, stick) = with_drive();
        let a = t.file("home/a.txt", "from home");
        t.file("media/stick/a.txt", "on the stick");
        let report = paste(std::slice::from_ref(&a), false, &stick);
        assert_eq!((report.done, report.renamed), (1, 1));
        assert_eq!(
            fs::read_to_string(stick.join("a.txt")).unwrap(),
            "on the stick"
        );
        assert_eq!(
            fs::read_to_string(stick.join("a 2.txt")).unwrap(),
            "from home"
        );
        // And back again: the name that is taken at home is numbered too.
        let report = paste(&[stick.join("a.txt")], false, &t.path().join("home"));
        assert_eq!((report.done, report.renamed), (1, 1));
        assert_eq!(fs::read_to_string(&a).unwrap(), "from home");
        assert_eq!(
            fs::read_to_string(t.path().join("home/a 2.txt")).unwrap(),
            "on the stick"
        );
    }

    #[test]
    fn a_move_to_another_disk_copies_then_removes() {
        // /dev/shm is another file system on most systems; where it is not,
        // there is nothing to try.
        let shm = PathBuf::from("/dev/shm");
        let t = Temp::new();
        if !shm.is_dir() || fsops::same_disk(&shm, t.path()) {
            return;
        }
        let other = shm.join(format!("sengi-files-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&other);
        if fs::create_dir_all(&other).is_err() {
            return;
        }
        let a = t.file("src/a.txt", "moving");
        t.file("src/deep/b.txt", "deeper");
        let progress = Progress::default();
        let report = paste_with(
            &[a.clone(), t.path().join("src/deep")],
            true,
            &other,
            &progress,
            &|| {},
        );
        let copied = fs::read_to_string(other.join("a.txt"));
        let deeper = fs::read_to_string(other.join("deep/b.txt"));
        let removed = !a.exists() && !t.path().join("src/deep").exists();
        let bytes = progress.bytes.load(Ordering::Relaxed);
        let _ = fs::remove_dir_all(&other);
        assert_eq!(
            (report.done, report.errors.len()),
            (2, 0),
            "{:?}",
            report.errors
        );
        assert_eq!(copied.unwrap(), "moving");
        assert_eq!(deeper.unwrap(), "deeper");
        assert!(removed);
        assert_eq!(bytes, 12);
    }

    #[test]
    fn a_paste_reports_progress() {
        let t = Temp::new();
        let a = t.file("src/a.bin", "");
        fs::write(&a, vec![0u8; 600_000]).unwrap();
        let b = t.file("src/b.bin", "0123456789");
        fs::create_dir(t.path().join("dst")).unwrap();
        let progress = Progress::default();
        let ticks = std::cell::Cell::new(0u32);
        let report = paste_with(&[a, b], false, &t.path().join("dst"), &progress, &|| {
            ticks.set(ticks.get() + 1)
        });
        assert_eq!(report.done, 2);
        assert_eq!(progress.bytes.load(Ordering::Relaxed), 600_010);
        assert_eq!(progress.bytes_total.load(Ordering::Relaxed), 600_010);
        assert_eq!(progress.items.load(Ordering::Relaxed), 2);
        assert!(ticks.get() >= 4, "{}", ticks.get());
        assert_eq!(
            progress.line("Copying"),
            "Copying 2 of 2 items, 600.0 kB of 600.0 kB"
        );
    }

    #[test]
    fn a_stopped_paste_leaves_no_half_copy() {
        let t = Temp::new();
        let a = t.file("src/a.bin", "");
        fs::write(&a, vec![0u8; 600_000]).unwrap();
        fs::create_dir(t.path().join("dst")).unwrap();
        let progress = Progress::default();
        // Stop after the first piece has been written.
        let stop = || {
            if progress.bytes.load(Ordering::Relaxed) > 0 {
                progress.cancel.store(true, Ordering::Relaxed);
            }
        };
        let report = paste_with(
            std::slice::from_ref(&a),
            false,
            &t.path().join("dst"),
            &progress,
            &stop,
        );
        assert!(report.cancelled);
        assert_eq!(report.done, 0);
        assert!(!report.failed());
        assert_eq!(report.say("Pasted"), "Cancelled.");
        assert_eq!(fs::read_dir(t.path().join("dst")).unwrap().count(), 0);
        assert!(a.exists());
    }

    #[test]
    fn what_will_not_fit_is_refused_before_anything_is_written() {
        let t = Temp::new();
        let big = t.path().join("src/huge.bin");
        fs::create_dir_all(t.path().join("src")).unwrap();
        fs::create_dir(t.path().join("dst")).unwrap();
        let made = fs::File::create(&big).and_then(|file| file.set_len(1 << 43));
        if made.is_err() {
            // A file system that will not make a sparse file this large.
            return;
        }
        let report = paste(&[big], false, &t.path().join("dst"));
        assert_eq!(report.done, 0);
        assert!(
            report.errors[0].starts_with("There is not enough space on the disk"),
            "{:?}",
            report.errors
        );
        assert_eq!(fs::read_dir(t.path().join("dst")).unwrap().count(), 0);
    }

    #[test]
    fn trash_on_a_drive_stays_on_the_drive_and_comes_back() {
        let (t, guard, bin, stick) = with_drive();
        let a = t.file("media/stick/photos/a.jpg", "pic");
        let report = trash_items(&guard, &bin, std::slice::from_ref(&a));
        assert_eq!(report.done, 1, "{:?}", report.errors);
        assert!(!a.exists());
        let drive_bin = volume_trash_dir(&stick);
        assert!(drive_bin.join("files/a.jpg").exists());
        assert!(!bin.join("files/a.jpg").exists());
        let listed = list_volume_trash(&stick).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].original.as_deref(), Some(a.as_path()));
        let item = Binned {
            name: "a.jpg".to_string(),
            volume: Some(stick),
        };
        let report = restore_items(&guard, &bin, &[item]);
        assert_eq!(report.done, 1, "{:?}", report.errors);
        assert_eq!(fs::read_to_string(&a).unwrap(), "pic");
    }

    #[test]
    fn home_trash_and_drive_trash_are_kept_apart_and_emptied_together() {
        let (t, guard, bin, stick) = with_drive();
        let at_home = t.file("home/h.txt", "h");
        let on_stick = t.file("media/stick/s.txt", "s");
        trash_items(&guard, &bin, &[at_home, on_stick]);
        assert!(bin.join("files/h.txt").exists());
        assert!(volume_trash_dir(&stick).join("files/s.txt").exists());
        let report = empty_trash(&guard, &bin, std::slice::from_ref(&stick));
        assert_eq!(
            (report.done, report.errors.len()),
            (2, 0),
            "{:?}",
            report.errors
        );
        assert_eq!(fs::read_dir(bin.join("files")).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(volume_trash_dir(&stick).join("files"))
                .unwrap()
                .count(),
            0
        );
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
