//! What the desktop knows: the folders and files, which one is selected and
//! which one is being renamed. Nothing here draws or touches a window, so
//! the rules can be tested alone.

use std::path::{Path, PathBuf};

use sengi_apps::Env;

use crate::files::{self, Entry};
use crate::{open, trash};

pub struct Desk {
    dir: PathBuf,
    env: Env,
    entries: Vec<Entry>,
    /// Selection and rename are kept by name, so a change in the folder
    /// that moves the icons about does not move them to another one.
    selected: Option<String>,
    renaming: Option<String>,
    /// Where the Trash is. Settable for tests.
    trash: PathBuf,
}

/// An icon as it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub folder: bool,
    pub selected: bool,
    pub renaming: bool,
}

impl Desk {
    pub fn new(dir: PathBuf, env: Env) -> Desk {
        let trash = trash::trash_dir(&env);
        let mut desk = Desk {
            dir,
            env,
            entries: Vec::new(),
            selected: None,
            renaming: None,
            trash,
        };
        desk.refresh();
        desk
    }

    #[cfg(test)]
    pub fn with_trash(mut self, trash: PathBuf) -> Desk {
        self.trash = trash;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reads the folder again. A selection or rename of something that is
    /// gone ends.
    pub fn refresh(&mut self) {
        self.entries = files::list(&self.dir).unwrap_or_default();
        let there = |name: &Option<String>| {
            name.as_ref()
                .is_some_and(|name| self.entries.iter().any(|e| &e.name == name))
        };
        if !there(&self.selected) {
            self.selected = None;
        }
        if !there(&self.renaming) {
            self.renaming = None;
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        self.entries
            .iter()
            .map(|entry| Row {
                name: entry.name.clone(),
                folder: entry.folder,
                selected: self.selected.as_deref() == Some(entry.name.as_str()),
                renaming: self.renaming.as_deref() == Some(entry.name.as_str()),
            })
            .collect()
    }

    /// The index of the selected icon, or -1.
    pub fn selected_index(&self) -> i32 {
        self.entries
            .iter()
            .position(|e| self.selected.as_deref() == Some(e.name.as_str()))
            .map_or(-1, |index| index as i32)
    }

    fn entry(&self, index: i32) -> Option<&Entry> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    /// Selects the icon at `index`; anything else selects nothing.
    pub fn select(&mut self, index: i32) {
        self.selected = self.entry(index).map(|e| e.name.clone());
    }

    /// Makes "untitled folder" and puts it in rename.
    pub fn new_folder(&mut self) -> Result<(), String> {
        let name = files::create_folder(&self.dir)
            .map_err(|error| format!("Cannot make a folder: {error}"))?;
        self.refresh();
        self.selected = Some(name.clone());
        self.renaming = Some(name);
        Ok(())
    }

    /// Makes an empty "untitled file" and puts it in rename.
    pub fn new_file(&mut self) -> Result<(), String> {
        let name = files::create_file(&self.dir)
            .map_err(|error| format!("Cannot make a file: {error}"))?;
        self.refresh();
        self.selected = Some(name.clone());
        self.renaming = Some(name);
        Ok(())
    }

    pub fn begin_rename(&mut self, index: i32) {
        if let Some(entry) = self.entry(index) {
            let name = entry.name.clone();
            self.selected = Some(name.clone());
            self.renaming = Some(name);
        }
    }

    pub fn cancel_rename(&mut self) {
        self.renaming = None;
    }

    pub fn is_renaming(&self) -> bool {
        self.renaming.is_some()
    }

    /// Ends the rename with `text` as the new name. Does nothing if no
    /// rename is going on, which is how the second of two ways to end it
    /// (Return, then the field losing the keyboard) is ignored. On failure
    /// the old name stays.
    pub fn commit_rename(&mut self, text: &str) -> Result<(), String> {
        let Some(old) = self.renaming.take() else {
            return Ok(());
        };
        let result = files::rename(&self.dir, &old, text);
        if let Ok(new) = &result {
            if self.selected.as_deref() == Some(old.as_str()) {
                self.selected = Some(new.clone());
            }
        }
        self.refresh();
        result.map(|_| ()).map_err(|error| error.to_string())
    }

    /// Moves the icon at `index` to the Trash. Never deletes: if the move
    /// cannot be made the item stays and the reason comes back.
    pub fn trash(&mut self, index: i32) -> Result<(), String> {
        let Some(entry) = self.entry(index) else {
            return Ok(());
        };
        let path = self.dir.join(&entry.name);
        let result = trash::trash(&self.trash, &path, &trash::now());
        self.refresh();
        result.map(|_| ()).map_err(|error| trash::explain(&error))
    }

    /// Opens the icon at `index`.
    pub fn open(&self, index: i32) -> Result<(), String> {
        let Some(entry) = self.entry(index) else {
            return Ok(());
        };
        open::open(&self.env, &self.dir.join(&entry.name), entry.folder)
    }

    pub fn terminal(&self) -> Result<(), String> {
        open::terminal(&self.env, &self.dir)
    }

    pub fn settings(&self) -> Result<(), String> {
        open::settings(&self.env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;
    use std::fs;

    fn desk(tag: &str) -> (Desk, PathBuf) {
        let root = temp_dir(tag);
        let dir = root.join("Desktop");
        fs::create_dir_all(&dir).unwrap();
        let env = Env::from_vars(|name| (name == "PATH").then(|| "/nonexistent".into()));
        (Desk::new(dir, env).with_trash(root.join("Trash")), root)
    }

    fn names(desk: &Desk) -> Vec<String> {
        desk.rows().into_iter().map(|row| row.name).collect()
    }

    #[test]
    fn a_new_folder_is_selected_and_in_rename() {
        let (mut desk, root) = desk("new");
        desk.new_folder().unwrap();
        let rows = desk.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "untitled folder");
        assert!(rows[0].folder && rows[0].selected && rows[0].renaming);
        desk.commit_rename("Photos").unwrap();
        assert_eq!(names(&desk), ["Photos"]);
        let rows = desk.rows();
        assert!(rows[0].selected && !rows[0].renaming);
        desk.new_folder().unwrap();
        assert!(names(&desk).contains(&"untitled folder".to_string()));
        desk.commit_rename("untitled folder").unwrap();
        desk.new_folder().unwrap();
        assert!(names(&desk).contains(&"untitled folder 2".to_string()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_new_file_is_empty_selected_and_in_rename() {
        let (mut desk, root) = desk("new-file");
        desk.new_file().unwrap();
        desk.new_file().unwrap();
        let rows = desk.rows();
        let listed: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(listed, ["untitled file", "untitled file 2"]);
        assert!(rows.iter().all(|r| !r.folder));
        // Only the last one is being named.
        assert!(!rows[0].renaming && rows[1].renaming && rows[1].selected);
        desk.commit_rename("notes.txt").unwrap();
        assert_eq!(names(&desk), ["notes.txt", "untitled file"]);
        assert!(desk.dir().join("notes.txt").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ending_a_rename_twice_does_nothing_the_second_time() {
        let (mut desk, root) = desk("twice");
        desk.new_folder().unwrap();
        desk.commit_rename("one").unwrap();
        desk.commit_rename("two").unwrap();
        assert_eq!(names(&desk), ["one"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_refused_name_keeps_the_old_one_and_says_why() {
        let (mut desk, root) = desk("refused");
        desk.new_folder().unwrap();
        desk.commit_rename("a").unwrap();
        desk.new_folder().unwrap();
        let error = desk.commit_rename("a").unwrap_err();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(names(&desk), ["a", "untitled folder"]);
        assert!(!desk.is_renaming());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelling_keeps_the_name_it_had() {
        let (mut desk, root) = desk("cancel");
        desk.new_folder().unwrap();
        desk.cancel_rename();
        assert_eq!(names(&desk), ["untitled folder"]);
        assert!(!desk.rows()[0].renaming);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selection_follows_the_name_when_the_folder_changes() {
        let (mut desk, root) = desk("follow");
        let dir = desk.dir().to_path_buf();
        fs::create_dir(dir.join("b")).unwrap();
        fs::create_dir(dir.join("c")).unwrap();
        desk.refresh();
        desk.select(1);
        assert_eq!(desk.selected_index(), 1);
        // Something sorts in front of it.
        fs::create_dir(dir.join("a")).unwrap();
        desk.refresh();
        assert_eq!(desk.selected_index(), 2);
        assert!(desk.rows()[2].selected);
        // It goes away.
        fs::remove_dir(dir.join("c")).unwrap();
        desk.refresh();
        assert_eq!(desk.selected_index(), -1);
        desk.select(0);
        desk.select(-1);
        assert_eq!(desk.selected_index(), -1);
        desk.select(99);
        assert_eq!(desk.selected_index(), -1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_trash_takes_the_icon_and_nothing_is_deleted() {
        let (mut desk, root) = desk("trash");
        desk.new_folder().unwrap();
        desk.commit_rename("old stuff").unwrap();
        desk.trash(0).unwrap();
        assert!(names(&desk).is_empty());
        assert!(root.join("Trash/files/old stuff").is_dir());
        assert!(root.join("Trash/info/old stuff.trashinfo").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_trash_that_cannot_be_used_leaves_the_icon() {
        let (mut desk, root) = desk("trash-bad");
        desk.new_folder().unwrap();
        // A file where the Trash's folder should be.
        fs::write(root.join("Trash"), "").unwrap();
        let error = desk.trash(0).unwrap_err();
        assert!(error.starts_with("Cannot move to the Trash"), "{error}");
        assert_eq!(names(&desk), ["untitled folder"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opening_without_a_file_manager_says_so() {
        let (mut desk, root) = desk("open");
        desk.new_folder().unwrap();
        assert_eq!(desk.open(0), Err(open::NO_FILE_MANAGER.to_string()));
        fs::remove_dir_all(root).unwrap();
    }
}
