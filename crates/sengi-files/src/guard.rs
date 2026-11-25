//! What Files may change. Looking is allowed anywhere that can be read;
//! making, renaming, moving, trashing and deleting only inside the user's
//! home folder, and never to the home folder itself or the folders the
//! desktop is built on.
//!
//! Paths are judged after links are resolved in the folder that holds them,
//! so a link in the home folder that leads to `/etc` does not make `/etc`
//! changeable. The item itself is not resolved: trashing a link removes the
//! link and nothing it points to.

use std::fs;
use std::path::{Path, PathBuf};

/// Folders inside home that stay where they are. Names relative to home.
const KEPT: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Music",
    "Pictures",
    "Public",
    "Templates",
    "Videos",
    ".config",
    ".local",
    ".local/share",
];

#[derive(Clone)]
pub struct Guard {
    /// The home folder with links resolved.
    home: PathBuf,
    /// The Trash, which Files empties and restores from but never trashes.
    trash: PathBuf,
}

impl Guard {
    /// None if the home folder cannot be found, in which case nothing can be
    /// changed at all.
    pub fn new(home: &Path, trash: &Path) -> Option<Guard> {
        let home = fs::canonicalize(home).ok()?;
        // The Trash may not exist yet: its parent is what can be resolved.
        let trash = match (trash.parent(), trash.file_name()) {
            (Some(parent), Some(name)) => fs::canonicalize(parent)
                .map_or_else(|_| trash.to_path_buf(), |parent| parent.join(name)),
            _ => trash.to_path_buf(),
        };
        Some(Guard { home, trash })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// May the existing item at `path` be renamed, moved, trashed or deleted?
    pub fn may_change(&self, path: &Path) -> Result<(), String> {
        let resolved = self.resolve(path)?;
        let name = display_name(&resolved);
        if resolved == self.home || !resolved.starts_with(&self.home) {
            return Err(self.outside());
        }
        if resolved == self.trash || resolved.starts_with(&self.trash) {
            return Err(format!("\"{name}\" is in the Trash already."));
        }
        let inside = resolved.strip_prefix(&self.home).unwrap_or(&resolved);
        if KEPT.iter().any(|kept| inside == Path::new(kept)) {
            return Err(format!(
                "\"{name}\" is part of your home folder and cannot be moved, renamed or deleted."
            ));
        }
        Ok(())
    }

    /// May things be made or put in the folder `dir`?
    pub fn may_write_in(&self, dir: &Path) -> Result<(), String> {
        let resolved = fs::canonicalize(dir)
            .map_err(|error| format!("Cannot use that folder: {}.", plain(&error)))?;
        if !resolved.starts_with(&self.home) {
            return Err(self.outside());
        }
        if resolved == self.trash || resolved.starts_with(&self.trash) {
            return Err("Nothing can be put in the Trash this way.".to_string());
        }
        Ok(())
    }

    /// May the item in the Trash called `name` be restored to `to`?
    /// Putting it back is a write, so the same rule as for any.
    pub fn may_restore_to(&self, to: &Path) -> Result<(), String> {
        // The folder may be gone; the nearest one that is there decides.
        let mut at = to.parent();
        while let Some(dir) = at {
            if dir.exists() {
                return self.may_write_in(dir);
            }
            at = dir.parent();
        }
        Err(self.outside())
    }

    /// Whether the Trash folder is one Files may empty: the user's own,
    /// inside home. A Trash anywhere else is left alone.
    pub fn may_empty(&self, trash: &Path) -> Result<(), String> {
        let resolved = match (trash.parent(), trash.file_name()) {
            (Some(parent), Some(name)) => fs::canonicalize(parent)
                .map(|parent| parent.join(name))
                .map_err(|_| self.outside())?,
            _ => return Err(self.outside()),
        };
        if resolved == self.trash && resolved.starts_with(&self.home) {
            Ok(())
        } else {
            Err(self.outside())
        }
    }

    fn outside(&self) -> String {
        "Files only changes things inside your home folder.".to_string()
    }

    /// The path with links resolved in the folder that holds it.
    fn resolve(&self, path: &Path) -> Result<PathBuf, String> {
        if !path.is_absolute() {
            return Err("That is not a full path.".to_string());
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            // The root.
            return Err(self.outside());
        };
        fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .map_err(|error| format!("Cannot find it: {}.", plain(&error)))
    }
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "/".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn plain(error: &std::io::Error) -> String {
    sengi_system::trash::plain(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::tests::Temp;

    fn setup() -> (Temp, Guard) {
        let t = Temp::new();
        for dir in [
            "home/Documents",
            "home/Desktop",
            "home/.local/share",
            "home/stuff",
            "other",
        ] {
            fs::create_dir_all(t.path().join(dir)).unwrap();
        }
        t.file("home/stuff/a.txt", "");
        t.file("other/b.txt", "");
        let guard = Guard::new(
            &t.path().join("home"),
            &t.path().join("home/.local/share/Trash"),
        )
        .unwrap();
        (t, guard)
    }

    #[test]
    fn things_inside_home_may_be_changed() {
        let (t, guard) = setup();
        assert!(guard.may_change(&t.path().join("home/stuff/a.txt")).is_ok());
        assert!(guard.may_change(&t.path().join("home/stuff")).is_ok());
        assert!(guard
            .may_change(&t.path().join("home/Documents/new"))
            .is_ok());
    }

    #[test]
    fn the_home_folder_and_what_is_outside_it_may_not() {
        let (t, guard) = setup();
        for path in [
            t.path().join("home"),
            t.path().join("other/b.txt"),
            t.path().join("other"),
            "/".into(),
            "/etc".into(),
            "/etc/passwd".into(),
            "/usr/bin".into(),
            "/home".into(),
            "/tmp".into(),
        ] {
            assert!(guard.may_change(&path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn the_folders_the_desktop_is_built_on_stay() {
        let (t, guard) = setup();
        for kept in ["Documents", "Desktop", ".local", ".local/share"] {
            let error = guard
                .may_change(&t.path().join("home").join(kept))
                .unwrap_err();
            assert!(error.contains("cannot be moved"), "{kept}: {error}");
        }
    }

    #[test]
    fn a_link_out_of_home_does_not_open_what_it_points_to() {
        let (t, guard) = setup();
        std::os::unix::fs::symlink(t.path().join("other"), t.path().join("home/stuff/out"))
            .unwrap();
        // The link itself is home's to trash; what is inside it is not.
        assert!(guard.may_change(&t.path().join("home/stuff/out")).is_ok());
        assert!(guard
            .may_change(&t.path().join("home/stuff/out/b.txt"))
            .is_err());
        assert!(guard
            .may_write_in(&t.path().join("home/stuff/out"))
            .is_err());
    }

    #[test]
    fn dot_dot_does_not_climb_out() {
        let (t, guard) = setup();
        assert!(guard
            .may_change(&t.path().join("home/stuff/../../other/b.txt"))
            .is_err());
        assert!(guard
            .may_write_in(&t.path().join("home/stuff/../../other"))
            .is_err());
    }

    #[test]
    fn writing_is_for_folders_inside_home_only() {
        let (t, guard) = setup();
        assert!(guard.may_write_in(&t.path().join("home")).is_ok());
        assert!(guard.may_write_in(&t.path().join("home/stuff")).is_ok());
        assert!(guard.may_write_in(&t.path().join("other")).is_err());
        assert!(guard.may_write_in(Path::new("/etc")).is_err());
        assert!(guard.may_write_in(&t.path().join("home/missing")).is_err());
    }

    #[test]
    fn the_trash_is_not_trashed_or_pasted_into() {
        let (t, guard) = setup();
        let trash = t.path().join("home/.local/share/Trash");
        fs::create_dir_all(trash.join("files")).unwrap();
        assert!(guard.may_change(&trash).is_err());
        assert!(guard.may_change(&trash.join("files")).is_err());
        assert!(guard.may_write_in(&trash.join("files")).is_err());
    }

    #[test]
    fn only_the_users_own_trash_may_be_emptied() {
        let (t, guard) = setup();
        let own = t.path().join("home/.local/share/Trash");
        assert!(guard.may_empty(&own).is_ok());
        assert!(guard.may_empty(&t.path().join("other/Trash")).is_err());
        assert!(guard.may_empty(Path::new("/")).is_err());
        assert!(guard.may_empty(Path::new("/tmp")).is_err());
    }

    #[test]
    fn restoring_goes_where_writing_may() {
        let (t, guard) = setup();
        assert!(guard
            .may_restore_to(&t.path().join("home/stuff/a.txt"))
            .is_ok());
        // A folder that has gone: the nearest one that is there decides.
        assert!(guard
            .may_restore_to(&t.path().join("home/gone/deeper/a.txt"))
            .is_ok());
        assert!(guard.may_restore_to(&t.path().join("other/b.txt")).is_err());
        assert!(guard.may_restore_to(Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn a_relative_path_is_refused() {
        let (_t, guard) = setup();
        assert!(guard.may_change(Path::new("stuff/a.txt")).is_err());
    }
}
