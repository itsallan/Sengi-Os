//! The Desktop folder as a list of names, and the few things done to it:
//! making a folder, renaming.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The name a new folder starts with.
pub const NEW_FOLDER: &str = "untitled folder";

/// The name a new file starts with.
pub const NEW_FILE: &str = "untitled file";

/// More than this many entries are not shown. A folder of thousands of files
/// would otherwise cost memory for icons nobody can see.
const MAX_ENTRIES: usize = 400;

/// The longest name a file system takes, in bytes.
const MAX_NAME: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub folder: bool,
}

/// `~/Desktop`.
pub fn desktop_dir(home: &Path) -> PathBuf {
    home.join("Desktop")
}

/// Makes the folder if it is not there yet, as the first run does.
pub fn ensure(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// What is in `dir`, folders first and each group by name without regard to
/// case. Names that start with a dot are hidden, as everywhere.
pub fn list(dir: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let Ok(name) = item.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        // A link to a folder is a folder. A broken link is a file.
        let folder = fs::metadata(item.path()).is_ok_and(|meta| meta.is_dir());
        entries.push(Entry { name, folder });
    }
    entries.sort_by(|a, b| {
        b.folder
            .cmp(&a.folder)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    entries.truncate(MAX_ENTRIES);
    Ok(entries)
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// The first free name of the form "untitled folder", "untitled folder 2",
/// "untitled folder 3".
pub fn new_folder_name(dir: &Path) -> String {
    free_name(dir, NEW_FOLDER)
}

/// The same for "untitled file".
pub fn new_file_name(dir: &Path) -> String {
    free_name(dir, NEW_FILE)
}

fn free_name(dir: &Path, base: &str) -> String {
    (1u32..)
        .map(|n| numbered(base, n))
        .find(|name| !exists(&dir.join(name)))
        .unwrap_or_else(|| base.to_string())
}

fn numbered(base: &str, n: u32) -> String {
    if n == 1 {
        base.to_string()
    } else {
        format!("{base} {n}")
    }
}

/// Makes a folder with the first free "untitled folder" name and returns it.
/// Creating is what claims the name, so two makers cannot get the same one.
pub fn create_folder(dir: &Path) -> io::Result<String> {
    claim(dir, NEW_FOLDER, |path| fs::create_dir(path))
}

/// Makes an empty file with the first free "untitled file" name. Nothing
/// that is already there is touched: the file is made only if its name is new.
pub fn create_file(dir: &Path) -> io::Result<String> {
    claim(dir, NEW_FILE, |path| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(drop)
    })
}

fn claim(dir: &Path, base: &str, make: impl Fn(&Path) -> io::Result<()>) -> io::Result<String> {
    for n in 1u32.. {
        let name = numbered(base, n);
        match make(&dir.join(&name)) {
            Ok(()) => return Ok(name),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("the counter does not run out")
}

#[derive(Debug)]
pub enum RenameError {
    /// A name cannot hold a slash, be "." or "..", or be this long.
    Invalid,
    /// Something is already called that.
    Exists(String),
    Io(io::Error),
}

impl fmt::Display for RenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => write!(f, "That name cannot be used."),
            Self::Exists(name) => write!(f, "\"{name}\" already exists."),
            Self::Io(error) => write!(f, "Cannot rename: {error}"),
        }
    }
}

/// Renames `old` to `new` inside `dir` and returns the name it now has.
/// Spaces around the new name are dropped. An empty name, or the same one,
/// changes nothing. Nothing is ever replaced.
pub fn rename(dir: &Path, old: &str, new: &str) -> Result<String, RenameError> {
    let new = new.trim();
    if new.is_empty() || new == old {
        return Ok(old.to_string());
    }
    if new == "." || new == ".." || new.contains('/') || new.contains('\0') || new.len() > MAX_NAME
    {
        return Err(RenameError::Invalid);
    }
    let target = dir.join(new);
    if exists(&target) {
        return Err(RenameError::Exists(new.to_string()));
    }
    fs::rename(dir.join(old), &target).map_err(RenameError::Io)?;
    Ok(new.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn the_desktop_is_in_the_home_folder() {
        assert_eq!(
            desktop_dir(Path::new("/home/ada")),
            Path::new("/home/ada/Desktop")
        );
    }

    #[test]
    fn the_first_run_makes_the_folder() {
        let dir = temp_dir("ensure");
        let desktop = dir.join("Desktop");
        ensure(&desktop).unwrap();
        ensure(&desktop).unwrap();
        assert!(desktop.is_dir());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folders_come_first_then_files_by_name_and_hidden_ones_are_left_out() {
        let dir = temp_dir("list");
        fs::create_dir(dir.join("zebra")).unwrap();
        fs::create_dir(dir.join("Apple")).unwrap();
        fs::write(dir.join("banana.txt"), "").unwrap();
        fs::write(dir.join("Cherry.txt"), "").unwrap();
        fs::write(dir.join(".hidden"), "").unwrap();
        let entries = list(&dir).unwrap();
        assert_eq!(
            names(&entries),
            ["Apple", "zebra", "banana.txt", "Cherry.txt"]
        );
        assert!(entries[0].folder && entries[1].folder);
        assert!(!entries[2].folder && !entries[3].folder);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_link_to_a_folder_is_a_folder_and_a_broken_link_is_a_file() {
        let dir = temp_dir("links");
        fs::create_dir(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();
        std::os::unix::fs::symlink(dir.join("gone"), dir.join("dangling")).unwrap();
        let entries = list(&dir).unwrap();
        let folder = |name: &str| entries.iter().find(|e| e.name == name).unwrap().folder;
        assert!(folder("alias"));
        assert!(folder("real"));
        assert!(!folder("dangling"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn listing_stops_at_a_limit() {
        let dir = temp_dir("many");
        for n in 0..MAX_ENTRIES + 20 {
            fs::write(dir.join(format!("f{n}")), "").unwrap();
        }
        assert_eq!(list(&dir).unwrap().len(), MAX_ENTRIES);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_folders_are_numbered_from_two() {
        let dir = temp_dir("names");
        assert_eq!(new_folder_name(&dir), "untitled folder");
        assert_eq!(create_folder(&dir).unwrap(), "untitled folder");
        assert_eq!(new_folder_name(&dir), "untitled folder 2");
        assert_eq!(create_folder(&dir).unwrap(), "untitled folder 2");
        assert_eq!(create_folder(&dir).unwrap(), "untitled folder 3");
        // A gap is not refilled before the next number is free.
        fs::remove_dir(dir.join("untitled folder 2")).unwrap();
        assert_eq!(new_folder_name(&dir), "untitled folder 2");
        // A file in the way counts as well.
        fs::write(dir.join("untitled folder 2"), "").unwrap();
        assert_eq!(new_folder_name(&dir), "untitled folder 4");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn new_files_are_numbered_from_two_and_nothing_is_overwritten() {
        let dir = temp_dir("files");
        assert_eq!(new_file_name(&dir), "untitled file");
        assert_eq!(create_file(&dir).unwrap(), "untitled file");
        assert!(dir.join("untitled file").is_file());
        assert_eq!(fs::metadata(dir.join("untitled file")).unwrap().len(), 0);
        assert_eq!(create_file(&dir).unwrap(), "untitled file 2");
        assert_eq!(create_file(&dir).unwrap(), "untitled file 3");
        // What is in the way, a file with content, a folder, a dangling link,
        // is skipped and left as it is.
        fs::write(dir.join("untitled file 4"), "keep").unwrap();
        fs::create_dir(dir.join("untitled file 5")).unwrap();
        std::os::unix::fs::symlink(dir.join("gone"), dir.join("untitled file 6")).unwrap();
        assert_eq!(create_file(&dir).unwrap(), "untitled file 7");
        assert_eq!(
            fs::read_to_string(dir.join("untitled file 4")).unwrap(),
            "keep"
        );
        assert!(!dir.join("gone").exists());
        // Files and folders count separately.
        assert_eq!(create_folder(&dir).unwrap(), "untitled folder");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn renaming_moves_the_folder() {
        let dir = temp_dir("rename");
        fs::create_dir(dir.join("old")).unwrap();
        assert_eq!(rename(&dir, "old", "  new name ").unwrap(), "new name");
        assert!(dir.join("new name").is_dir());
        assert!(!dir.join("old").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_empty_or_unchanged_name_is_left_alone() {
        let dir = temp_dir("same");
        fs::create_dir(dir.join("a")).unwrap();
        assert_eq!(rename(&dir, "a", "").unwrap(), "a");
        assert_eq!(rename(&dir, "a", "   ").unwrap(), "a");
        assert_eq!(rename(&dir, "a", "a").unwrap(), "a");
        assert!(dir.join("a").is_dir());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn renaming_never_replaces_or_escapes() {
        let dir = temp_dir("refuse");
        fs::create_dir(dir.join("a")).unwrap();
        fs::write(dir.join("b"), "keep").unwrap();
        assert!(matches!(
            rename(&dir, "a", "b"),
            Err(RenameError::Exists(_))
        ));
        assert_eq!(fs::read_to_string(dir.join("b")).unwrap(), "keep");
        for bad in ["x/y", "..", ".", "a\0b", &"n".repeat(300)] {
            assert!(matches!(rename(&dir, "a", bad), Err(RenameError::Invalid)));
        }
        assert!(dir.join("a").is_dir());
        fs::remove_dir_all(dir).unwrap();
    }
}
