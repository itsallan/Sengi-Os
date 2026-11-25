//! What the window is showing and what is selected, with no window: the
//! place, its entries, the selection, the clipboard, the way back. The
//! window reads this and the keys and clicks change it, so all of it can be
//! tested without drawing anything.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::fsops::Entry;
use crate::guard::Guard;

/// Where the window is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    Folder(PathBuf),
    Trash,
}

/// Files that were copied or cut, to be pasted. Only inside this program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clipboard {
    pub items: Vec<PathBuf>,
    pub cut: bool,
}

/// One step of the path bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    pub place: Place,
}

/// The longest path bar shown; a longer path starts with a step that is
/// the folder just before the first one shown.
const MAX_CRUMBS: usize = 4;

pub struct Session {
    pub home: PathBuf,
    pub trash: PathBuf,
    /// None when the home folder cannot be found: nothing can then be
    /// changed.
    pub guard: Option<Guard>,
    /// Where the user's drives are mounted, `/run/media/USER`.
    pub media: Option<PathBuf>,
    pub place: Place,
    back: Vec<Place>,
    forward: Vec<Place>,
    pub entries: Vec<Entry>,
    pub selected: BTreeSet<usize>,
    /// The entry the keyboard is on, and where a range selection starts.
    pub cursor: usize,
    anchor: usize,
    pub show_hidden: bool,
    pub clipboard: Option<Clipboard>,
    /// Names not shown because they are not text.
    pub skipped: usize,
    /// Why the place could not be read, in place of its entries.
    pub problem: Option<String>,
    pub loading: Option<usize>,
    /// Counts up with every load, so that the answer of an older one is
    /// known and thrown away.
    pub generation: u64,
}

impl Session {
    pub fn new(home: PathBuf, trash: PathBuf, place: Place) -> Session {
        let guard = Guard::new(&home, &trash);
        Session {
            home,
            trash,
            guard,
            media: None,
            place,
            back: Vec::new(),
            forward: Vec::new(),
            entries: Vec::new(),
            selected: BTreeSet::new(),
            cursor: 0,
            anchor: 0,
            show_hidden: false,
            clipboard: None,
            skipped: 0,
            problem: None,
            loading: None,
            generation: 0,
        }
    }

    /// Lets the drives mounted in `media` be changed too, and shows their
    /// paths from the drive's name.
    pub fn with_media(mut self, media: Option<PathBuf>) -> Session {
        self.guard = Guard::with_media(&self.home, &self.trash, media.as_deref());
        self.media = media;
        self
    }

    /// Whether `dir` is a drive's top folder.
    pub fn is_volume_root(&self, dir: &Path) -> bool {
        self.media
            .as_deref()
            .is_some_and(|media| dir.parent() == Some(media))
    }

    pub fn in_trash(&self) -> bool {
        self.place == Place::Trash
    }

    /// The folder being shown, if it is one.
    pub fn dir(&self) -> Option<&Path> {
        match &self.place {
            Place::Folder(dir) => Some(dir),
            Place::Trash => None,
        }
    }

    // Moving between places.

    /// Goes to `place`. Going where it already is only reloads.
    pub fn go(&mut self, place: Place) {
        if place != self.place {
            let old = std::mem::replace(&mut self.place, place);
            self.back.push(old);
            // A long walk does not need a long memory.
            if self.back.len() > 100 {
                self.back.remove(0);
            }
            // A new way ahead forgets the old one.
            self.forward.clear();
        }
        self.clear_view();
    }

    pub fn go_back(&mut self) -> bool {
        let Some(place) = self.back.pop() else {
            return false;
        };
        let here = std::mem::replace(&mut self.place, place);
        self.forward.push(here);
        self.clear_view();
        true
    }

    pub fn go_forward(&mut self) -> bool {
        let Some(place) = self.forward.pop() else {
            return false;
        };
        let here = std::mem::replace(&mut self.place, place);
        self.back.push(here);
        self.clear_view();
        true
    }

    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// The place above this one: a folder's parent, and for the Trash, home.
    pub fn parent_place(&self) -> Option<Place> {
        match &self.place {
            // Above a drive is not the folder all drives are in, but home.
            Place::Folder(dir) if self.is_volume_root(dir) => {
                Some(Place::Folder(self.home.clone()))
            }
            Place::Folder(dir) => dir.parent().map(|p| Place::Folder(p.to_path_buf())),
            Place::Trash => Some(Place::Folder(self.home.clone())),
        }
    }

    pub fn up(&mut self) -> bool {
        match self.parent_place() {
            Some(place) => {
                self.go(place);
                true
            }
            None => false,
        }
    }

    fn clear_view(&mut self) {
        self.entries.clear();
        self.selected.clear();
        self.cursor = 0;
        self.anchor = 0;
        self.problem = None;
        self.skipped = 0;
    }

    /// Starts a load: returns the number its answer must carry.
    pub fn begin_load(&mut self) -> u64 {
        self.generation += 1;
        self.loading = Some(0);
        self.generation
    }

    /// Takes an answer, if it is the latest. `keep` are names to select again.
    pub fn finish_load(
        &mut self,
        generation: u64,
        result: Result<(Vec<Entry>, usize), String>,
        keep: &[String],
    ) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = None;
        match result {
            Ok((entries, skipped)) => {
                self.entries = entries;
                self.skipped = skipped;
                self.problem = None;
            }
            Err(problem) => {
                self.entries.clear();
                self.skipped = 0;
                self.problem = Some(problem);
            }
        }
        self.selected.clear();
        self.cursor = 0;
        self.anchor = 0;
        for name in keep {
            if let Some(at) = self.entries.iter().position(|e| &e.name == name) {
                self.selected.insert(at);
                self.cursor = at;
                self.anchor = at;
            }
        }
        true
    }

    // Selecting.

    /// A click on entry `at`. Ctrl toggles it, Shift extends from the last
    /// one clicked, and neither makes it the only one.
    pub fn click(&mut self, at: usize, ctrl: bool, shift: bool) {
        if at >= self.entries.len() {
            return;
        }
        if shift {
            let (from, to) = (self.anchor.min(at), self.anchor.max(at));
            if !ctrl {
                self.selected.clear();
            }
            self.selected.extend(from..=to);
        } else if ctrl {
            if !self.selected.remove(&at) {
                self.selected.insert(at);
            }
            self.anchor = at;
        } else {
            self.selected.clear();
            self.selected.insert(at);
            self.anchor = at;
        }
        self.cursor = at;
    }

    /// Moves the cursor by `by` entries, selecting it, or with `extend`
    /// stretching the selection from where it started.
    pub fn step(&mut self, by: i32, extend: bool) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() - 1;
        let at = if self.selected.is_empty() && !extend && by.abs() == 1 {
            // Nothing chosen yet: the first arrow lands on an end.
            if by < 0 {
                last
            } else {
                0
            }
        } else {
            (self.cursor as i64 + i64::from(by)).clamp(0, last as i64) as usize
        };
        if extend {
            let (from, to) = (self.anchor.min(at), self.anchor.max(at));
            self.selected = (from..=to).collect();
            self.cursor = at;
        } else {
            self.click(at, false, false);
        }
    }

    pub fn select_all(&mut self) {
        self.selected = (0..self.entries.len()).collect();
    }

    pub fn select_none(&mut self) {
        self.selected.clear();
    }

    /// Selects exactly the entry called `name`, if it is there.
    pub fn select_name(&mut self, name: &str) -> Option<usize> {
        let at = self.entries.iter().position(|e| e.name == name)?;
        self.click(at, false, false);
        Some(at)
    }

    pub fn selected_entries(&self) -> Vec<&Entry> {
        self.selected
            .iter()
            .filter_map(|&at| self.entries.get(at))
            .collect()
    }

    pub fn path_of(&self, at: usize) -> Option<PathBuf> {
        let entry = self.entries.get(at)?;
        Some(self.dir()?.join(&entry.name))
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected
            .iter()
            .filter_map(|&at| self.path_of(at))
            .collect()
    }

    /// The one selected entry, when exactly one is.
    pub fn only_selected(&self) -> Option<usize> {
        match self.selected.len() {
            1 => self.selected.iter().next().copied(),
            _ => None,
        }
    }

    // Asking what may be done. Each answer is what to do or what to say.

    fn guard(&self) -> Result<&Guard, String> {
        self.guard.as_ref().ok_or_else(|| {
            "Files cannot find your home folder, so it cannot change anything.".to_string()
        })
    }

    pub fn can_change_here(&self) -> Result<PathBuf, String> {
        let dir = self
            .dir()
            .ok_or_else(|| "Open a folder first.".to_string())?;
        self.guard()?.may_write_in(dir)?;
        Ok(dir.to_path_buf())
    }

    /// Paths the selection would trash or cut: all of them may be, or none.
    pub fn changeable_selection(&self) -> Result<Vec<PathBuf>, String> {
        if self.selected.is_empty() {
            return Err("Nothing is selected.".to_string());
        }
        let guard = self.guard()?;
        let paths = self.selected_paths();
        for path in &paths {
            guard.may_change(path)?;
        }
        Ok(paths)
    }

    pub fn can_rename(&self, at: usize) -> Result<(), String> {
        let path = self.path_of(at).ok_or("Nothing to rename.")?;
        self.guard()?.may_change(&path)
    }

    pub fn copy(&mut self) -> Result<usize, String> {
        if self.in_trash() {
            return Err("Restore things from the Trash before copying them.".to_string());
        }
        if self.selected.is_empty() {
            return Err("Nothing is selected.".to_string());
        }
        let items = self.selected_paths();
        let count = items.len();
        self.clipboard = Some(Clipboard { items, cut: false });
        Ok(count)
    }

    pub fn cut(&mut self) -> Result<usize, String> {
        if self.in_trash() {
            return Err("Restore things from the Trash before moving them.".to_string());
        }
        let items = self.changeable_selection()?;
        let count = items.len();
        self.clipboard = Some(Clipboard { items, cut: true });
        Ok(count)
    }

    /// What a paste would do: the clipboard and the folder to put it in.
    pub fn paste_plan(&self) -> Result<(Clipboard, PathBuf), String> {
        let clipboard = self
            .clipboard
            .clone()
            .ok_or_else(|| "There is nothing to paste.".to_string())?;
        let dest = self.can_change_here()?;
        Ok((clipboard, dest))
    }

    /// What dropping the selection on the folder `dest` would do: the items
    /// to move. Same rules as cut and paste: only what may be changed, only
    /// into a folder inside home, never into itself, never where it is.
    pub fn drop_plan(&self, dest: &Path) -> Result<Vec<PathBuf>, String> {
        if self.in_trash() {
            return Err("Restore things from the Trash before moving them.".to_string());
        }
        let items = self.changeable_selection()?;
        self.guard()?.may_write_in(dest)?;
        let dest_real = std::fs::canonicalize(dest).unwrap_or_else(|_| dest.to_path_buf());
        for item in &items {
            let item_real = std::fs::canonicalize(item).unwrap_or_else(|_| item.clone());
            if dest_real == item_real || dest_real.starts_with(&item_real) {
                return Err("A folder cannot be moved into itself.".to_string());
            }
        }
        if items.iter().all(|item| item.parent() == Some(dest)) {
            return Err("It is already in that folder.".to_string());
        }
        Ok(items)
    }

    pub fn guard_ref(&self) -> Option<&Guard> {
        self.guard.as_ref()
    }

    // What the path bar and the status line say.

    pub fn crumbs(&self) -> Vec<Crumb> {
        let mut crumbs = match &self.place {
            Place::Trash => {
                return vec![Crumb {
                    label: "Trash".to_string(),
                    place: Place::Trash,
                }]
            }
            Place::Folder(dir) => {
                let mut crumbs = Vec::new();
                // On a drive the path starts at the drive, by its name.
                let on_drive = self.media.as_ref().and_then(|media| {
                    let rest = dir.strip_prefix(media).ok()?;
                    let first = rest.components().next()?;
                    let top = media.join(first.as_os_str());
                    let below = rest.strip_prefix(first.as_os_str()).ok()?.to_path_buf();
                    Some((top, below))
                });
                let (start, rest) = match (on_drive, dir.strip_prefix(&self.home)) {
                    (Some((top, below)), _) => (top, below),
                    (None, Ok(rest)) => (self.home.clone(), rest.to_path_buf()),
                    (None, Err(_)) => (
                        PathBuf::from("/"),
                        dir.strip_prefix("/").unwrap_or(dir).to_path_buf(),
                    ),
                };
                let label = if start == self.home {
                    "Home".to_string()
                } else if self.is_volume_root(&start) {
                    start
                        .file_name()
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
                } else {
                    "/".to_string()
                };
                crumbs.push(Crumb {
                    label,
                    place: Place::Folder(start.clone()),
                });
                let mut at = start;
                for part in rest.components() {
                    at.push(part);
                    crumbs.push(Crumb {
                        label: part.as_os_str().to_string_lossy().into_owned(),
                        place: Place::Folder(at.clone()),
                    });
                }
                crumbs
            }
        };
        if crumbs.len() > MAX_CRUMBS {
            let cut = crumbs.len() - (MAX_CRUMBS - 1);
            let before = crumbs[cut - 1].place.clone();
            crumbs.drain(..cut);
            crumbs.insert(
                0,
                Crumb {
                    label: "\u{2026}".to_string(),
                    place: before,
                },
            );
        }
        crumbs
    }

    pub fn count_text(&self) -> String {
        let count = self.entries.len();
        let things = if count == 1 { "item" } else { "items" };
        let mut text = format!("{count} {things}");
        if !self.selected.is_empty() {
            text.push_str(&format!(", {} selected", self.selected.len()));
        }
        text
    }
}

/// The glyph of a place in the sidebar, by the name of its file in
/// ui/icons/phosphor/bold. A place with none is shown as words only.
pub fn place_icon(name: &str) -> &'static str {
    match name {
        "Home" => "house",
        "Desktop" => "monitor",
        "Documents" => "folder-open",
        "Downloads" => "download-simple",
        "Pictures" => "image",
        "Trash" => "trash",
        _ => "",
    }
}

/// The sidebar: a name and where it goes. Only folders that exist.
pub fn places(home: &Path) -> Vec<(String, Place)> {
    let mut places = vec![("Home".to_string(), Place::Folder(home.to_path_buf()))];
    for name in ["Desktop", "Documents", "Downloads", "Pictures"] {
        let dir = home.join(name);
        if dir.is_dir() {
            places.push((name.to_string(), Place::Folder(dir)));
        }
    }
    places.push(("Trash".to_string(), Place::Trash));
    places
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::tests::Temp;

    fn entry(name: &str, folder: bool) -> Entry {
        Entry {
            name: name.to_string(),
            folder,
            link: false,
            size: 0,
            modified: 0,
            original: None,
            deleted: String::new(),
            volume: None,
        }
    }

    fn session(names: &[&str]) -> Session {
        let mut s = Session::new(
            "/h".into(),
            "/h/.local/share/Trash".into(),
            Place::Folder("/h/x".into()),
        );
        s.entries = names.iter().map(|n| entry(n, false)).collect();
        s
    }

    #[test]
    fn a_click_selects_one_and_ctrl_adds_and_removes() {
        let mut s = session(&["a", "b", "c", "d"]);
        s.click(1, false, false);
        assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), [1]);
        s.click(3, true, false);
        assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), [1, 3]);
        s.click(1, true, false);
        assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), [3]);
        s.click(0, false, false);
        assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn shift_extends_from_the_last_plain_click() {
        let mut s = session(&["a", "b", "c", "d", "e"]);
        s.click(1, false, false);
        s.click(3, false, true);
        assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), [1, 2, 3]);
        s.click(0, false, true);
        assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn arrows_move_the_selection_and_stop_at_the_ends() {
        let mut s = session(&["a", "b", "c"]);
        s.step(1, false);
        assert_eq!(s.cursor, 0);
        s.step(1, false);
        s.step(1, false);
        s.step(1, false);
        assert_eq!(s.cursor, 2);
        assert_eq!(s.selected.len(), 1);
        s.step(-1, true);
        s.step(-1, true);
        assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), [0, 1, 2]);
        let mut s = session(&["a", "b", "c"]);
        s.step(-1, false);
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn a_click_past_the_end_does_nothing() {
        let mut s = session(&["a"]);
        s.click(5, false, false);
        assert!(s.selected.is_empty());
    }

    #[test]
    fn back_and_up_walk_the_places() {
        let mut s = Session::new("/h".into(), "/h/t".into(), Place::Folder("/h".into()));
        assert!(!s.can_back());
        s.go(Place::Folder("/h/a".into()));
        s.go(Place::Folder("/h/a/b".into()));
        assert!(s.up());
        assert_eq!(s.place, Place::Folder("/h/a".into()));
        assert!(s.go_back());
        assert_eq!(s.place, Place::Folder("/h/a/b".into()));
        assert!(s.go_back());
        assert!(s.go_back());
        assert_eq!(s.place, Place::Folder("/h".into()));
        assert!(!s.go_back());
        s.go(Place::Trash);
        assert!(s.up());
        assert_eq!(s.place, Place::Folder("/h".into()));
        s.go(Place::Folder("/".into()));
        assert!(!s.up());
    }

    #[test]
    fn forward_undoes_back_until_a_new_place_is_chosen() {
        let mut s = Session::new("/h".into(), "/h/t".into(), Place::Folder("/h".into()));
        assert!(!s.can_forward() && !s.go_forward());
        s.go(Place::Folder("/h/a".into()));
        s.go(Place::Folder("/h/b".into()));
        assert!(s.go_back() && s.go_back());
        assert_eq!(s.place, Place::Folder("/h".into()));
        assert!(s.can_forward());
        assert!(s.go_forward());
        assert_eq!(s.place, Place::Folder("/h/a".into()));
        assert!(s.can_back());
        // Somewhere new ends the way forward.
        s.go(Place::Trash);
        assert!(!s.can_forward());
        assert!(s.go_back());
        assert_eq!(s.place, Place::Folder("/h/a".into()));
        assert!(s.go_forward());
        assert_eq!(s.place, Place::Trash);
    }

    #[test]
    fn going_where_it_already_is_does_not_grow_the_way_back() {
        let mut s = Session::new("/h".into(), "/h/t".into(), Place::Folder("/h".into()));
        s.go(Place::Folder("/h".into()));
        assert!(!s.can_back());
    }

    #[test]
    fn an_old_answer_is_thrown_away() {
        let mut s = session(&[]);
        let first = s.begin_load();
        let second = s.begin_load();
        assert!(!s.finish_load(first, Ok((vec![entry("old", false)], 0)), &[]));
        assert!(s.entries.is_empty() && s.loading.is_some());
        assert!(s.finish_load(second, Ok((vec![entry("new", false)], 2)), &[]));
        assert_eq!(s.entries[0].name, "new");
        assert_eq!(s.skipped, 2);
        assert!(s.loading.is_none());
    }

    #[test]
    fn a_reload_selects_what_was_asked_for_again() {
        let mut s = session(&[]);
        let n = s.begin_load();
        let entries = vec![entry("a", false), entry("b", false), entry("c", false)];
        s.finish_load(n, Ok((entries, 0)), &["c".to_string(), "gone".to_string()]);
        assert_eq!(s.selected.iter().copied().collect::<Vec<_>>(), [2]);
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn a_folder_that_cannot_be_read_says_why() {
        let mut s = session(&["a"]);
        let n = s.begin_load();
        s.finish_load(n, Err("Permission denied".into()), &[]);
        assert!(s.entries.is_empty());
        assert_eq!(s.problem.as_deref(), Some("Permission denied"));
    }

    #[test]
    fn the_path_bar_starts_at_home_or_the_root() {
        let mut s = Session::new(
            "/home/ada".into(),
            "/t".into(),
            Place::Folder("/home/ada/Documents/work".into()),
        );
        let labels = |s: &Session| s.crumbs().into_iter().map(|c| c.label).collect::<Vec<_>>();
        assert_eq!(labels(&s), ["Home", "Documents", "work"]);
        s.place = Place::Folder("/home/ada".into());
        assert_eq!(labels(&s), ["Home"]);
        s.place = Place::Folder("/usr/share".into());
        assert_eq!(labels(&s), ["/", "usr", "share"]);
        s.place = Place::Folder("/".into());
        assert_eq!(labels(&s), ["/"]);
        s.place = Place::Trash;
        assert_eq!(labels(&s), ["Trash"]);
    }

    #[test]
    fn a_long_path_is_shortened_from_the_left() {
        let s = Session::new(
            "/home/ada".into(),
            "/t".into(),
            Place::Folder("/home/ada/a/b/c/d/e".into()),
        );
        let crumbs = s.crumbs();
        let labels: Vec<_> = crumbs.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["\u{2026}", "c", "d", "e"]);
        // The first step goes to the folder just before the first one shown.
        assert_eq!(crumbs[0].place, Place::Folder("/home/ada/a/b".into()));
        assert_eq!(crumbs[3].place, Place::Folder("/home/ada/a/b/c/d/e".into()));
    }

    #[test]
    fn counts_are_said_in_the_singular_too() {
        let mut s = session(&["a"]);
        assert_eq!(s.count_text(), "1 item");
        s.entries.push(entry("b", false));
        s.click(0, false, false);
        assert_eq!(s.count_text(), "2 items, 1 selected");
    }

    #[test]
    fn the_sidebar_has_only_folders_that_exist() {
        let t = Temp::new();
        std::fs::create_dir(t.path().join("Documents")).unwrap();
        std::fs::create_dir(t.path().join("Pictures")).unwrap();
        let names: Vec<_> = places(t.path()).into_iter().map(|p| p.0).collect();
        assert_eq!(names, ["Home", "Documents", "Pictures", "Trash"]);
    }

    #[test]
    fn copy_and_cut_remember_the_selection() {
        let t = Temp::new();
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join("stuff")).unwrap();
        t.file("home/stuff/a", "");
        t.file("home/stuff/b", "");
        let mut s = Session::new(
            home.clone(),
            home.join(".local/share/Trash"),
            Place::Folder(home.join("stuff")),
        );
        s.entries = vec![entry("a", false), entry("b", false)];
        assert!(s.copy().is_err());
        s.click(0, false, false);
        assert_eq!(s.copy(), Ok(1));
        assert_eq!(
            s.clipboard,
            Some(Clipboard {
                items: vec![home.join("stuff/a")],
                cut: false
            })
        );
        s.click(1, false, false);
        assert_eq!(s.cut(), Ok(1));
        assert!(s.clipboard.as_ref().unwrap().cut);
        let (clipboard, dest) = s.paste_plan().unwrap();
        assert_eq!(clipboard.items, [home.join("stuff/b")]);
        assert_eq!(dest, home.join("stuff"));
    }

    #[test]
    fn nothing_outside_home_can_be_cut_trashed_or_pasted_into() {
        let t = Temp::new();
        let home = t.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        t.file("elsewhere/f", "");
        let mut s = Session::new(
            home.clone(),
            home.join(".local/share/Trash"),
            Place::Folder(t.path().join("elsewhere")),
        );
        s.entries = vec![entry("f", false)];
        s.click(0, false, false);
        assert!(s.changeable_selection().is_err());
        assert!(s.cut().is_err());
        assert!(s.can_rename(0).is_err());
        // Copying out of it is only reading.
        assert_eq!(s.copy(), Ok(1));
        assert!(s.paste_plan().is_err());
    }

    #[test]
    fn a_drop_follows_the_rules_of_a_move() {
        let t = Temp::new();
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join("Documents/inner")).unwrap();
        std::fs::create_dir_all(home.join("proj/sub")).unwrap();
        std::fs::create_dir_all(t.path().join("outside")).unwrap();
        t.file("home/proj/a.txt", "");
        let mut s = Session::new(
            home.clone(),
            home.join(".local/share/Trash"),
            Place::Folder(home.clone()),
        );
        s.entries = vec![entry("Documents", true), entry("proj", true)];
        // A protected folder cannot be dragged anywhere.
        s.click(0, false, false);
        assert!(s.drop_plan(&home.join("proj")).is_err());
        // A folder goes into another, not into itself or what it holds.
        s.click(1, false, false);
        assert_eq!(
            s.drop_plan(&home.join("Documents")),
            Ok(vec![home.join("proj")])
        );
        assert!(s.drop_plan(&home.join("proj")).is_err());
        assert!(s.drop_plan(&home.join("proj/sub")).is_err());
        // Where it already is, and out of home, are refused.
        assert!(s.drop_plan(&home).is_err());
        assert!(s.drop_plan(&t.path().join("outside")).is_err());
        s.place = Place::Trash;
        assert!(s.drop_plan(&home.join("Documents")).is_err());
    }

    #[test]
    fn a_drive_is_where_the_path_bar_starts_and_home_is_above_it() {
        let mut s = Session::new(
            "/h".into(),
            "/h/.local/share/Trash".into(),
            Place::Folder("/run/media/u/My Stick/photos/2026".into()),
        )
        .with_media(Some("/run/media/u".into()));
        let crumbs = s.crumbs();
        let labels: Vec<&str> = crumbs.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["My Stick", "photos", "2026"]);
        assert_eq!(
            crumbs[0].place,
            Place::Folder("/run/media/u/My Stick".into())
        );
        s.place = Place::Folder("/run/media/u/My Stick".into());
        assert_eq!(s.parent_place(), Some(Place::Folder("/h".into())));
        s.place = Place::Folder("/run/media/u/My Stick/photos".into());
        assert_eq!(
            s.parent_place(),
            Some(Place::Folder("/run/media/u/My Stick".into()))
        );
    }

    #[test]
    fn nothing_can_be_changed_without_a_home_folder() {
        let mut s = Session::new(
            "/no/such/home".into(),
            "/no/t".into(),
            Place::Folder("/tmp".into()),
        );
        s.entries = vec![entry("f", false)];
        s.click(0, false, false);
        assert!(s.guard.is_none());
        assert!(s.changeable_selection().is_err());
        assert!(s.can_change_here().is_err());
    }
}
