//! What Files may change. Looking is allowed anywhere that can be read;
//! making, renaming, moving, trashing and deleting only inside the user's
//! home folder and on the drives the user has opened (the volumes mounted
//! under `/run/media/USER`), and never to the home folder itself, the
//! folders the desktop is built on, or a drive's own top folder.
//!
//! Paths are judged after links are resolved in the folder that holds them,
//! so a link in the home folder that leads to `/etc` does not make `/etc`
//! changeable. The item itself is not resolved: trashing a link removes the
//! link and nothing it points to.

use std::fs;
use std::os::unix::fs::PermissionsExt;
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

/// Where the Trash of the drive whose top folder is `volume` is: the
/// shared `.Trash/UID` when the drive has a `.Trash` made for that (a folder
/// with the sticky bit, as the freedesktop.org specification asks), and
/// `.Trash-UID` otherwise.
pub fn volume_trash_dir(volume: &Path) -> PathBuf {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let shared = volume.join(".Trash");
    if let Ok(meta) = fs::symlink_metadata(&shared) {
        if meta.is_dir() && meta.permissions().mode() & 0o1000 != 0 {
            return shared.join(uid.to_string());
        }
    }
    volume.join(format!(".Trash-{uid}"))
}

/// Whether `inside`, a path from the top of a drive, is in one of the
/// folders a drive keeps its Trash in.
fn is_volume_trash(inside: &Path) -> bool {
    inside
        .components()
        .next()
        .and_then(|first| first.as_os_str().to_str())
        .is_some_and(|first| first == ".Trash" || first.starts_with(".Trash-"))
}

#[derive(Clone)]
pub struct Guard {
    /// The home folder with links resolved.
    home: PathBuf,
    /// The Trash, which Files empties and restores from but never trashes.
    trash: PathBuf,
    /// The folder the user's drives are mounted in, `/run/media/USER`. A
    /// drive is a folder directly inside it.
    media: Option<PathBuf>,
}

impl Guard {
    /// None if the home folder cannot be found, in which case nothing can be
    /// changed at all.
    pub fn new(home: &Path, trash: &Path) -> Option<Guard> {
        Guard::with_media(home, trash, None)
    }

    /// As [`Guard::new`], and the drives mounted in `media` may be changed
    /// too.
    pub fn with_media(home: &Path, trash: &Path, media: Option<&Path>) -> Option<Guard> {
        let home = fs::canonicalize(home).ok()?;
        // The Trash may not exist yet: its parent is what can be resolved.
        let trash = match (trash.parent(), trash.file_name()) {
            (Some(parent), Some(name)) => fs::canonicalize(parent)
                .map_or_else(|_| trash.to_path_buf(), |parent| parent.join(name)),
            _ => trash.to_path_buf(),
        };
        // The folder may not be there until the first drive is mounted.
        let media =
            media.map(|media| fs::canonicalize(media).unwrap_or_else(|_| media.to_path_buf()));
        Some(Guard { home, trash, media })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The drive that holds `path`: its top folder. None for anything that
    /// is not on one of the user's drives.
    pub fn volume_of(&self, path: &Path) -> Option<PathBuf> {
        // The item itself is not resolved: a link on a drive is on the drive,
        // wherever it points.
        let resolved = self.resolve(path).ok()?;
        self.split_media(&resolved).map(|(volume, _)| volume)
    }

    /// The drive's top folder and the path from there, if `resolved` is on
    /// one of the user's drives.
    fn split_media(&self, resolved: &Path) -> Option<(PathBuf, PathBuf)> {
        let media = self.media.as_ref()?;
        let rest = resolved.strip_prefix(media).ok()?;
        let first = rest.components().next()?;
        let volume = media.join(first.as_os_str());
        let inside = rest.strip_prefix(first.as_os_str()).ok()?.to_path_buf();
        Some((volume, inside))
    }

    /// May the existing item at `path` be renamed, moved, trashed or deleted?
    pub fn may_change(&self, path: &Path) -> Result<(), String> {
        let resolved = self.resolve(path)?;
        let name = display_name(&resolved);
        if let Some((_, inside)) = self.split_media(&resolved) {
            if inside.as_os_str().is_empty() {
                return Err(format!("\"{name}\" is a drive. Eject it to take it away."));
            }
            if is_volume_trash(&inside) {
                return Err(format!(
                    "\"{name}\" belongs to the drive's Trash and is kept there."
                ));
            }
            return Ok(());
        }
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
        if let Some((_, inside)) = self.split_media(&resolved) {
            if is_volume_trash(&inside) {
                return Err("Nothing can be put in the Trash this way.".to_string());
            }
            return Ok(());
        }
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
        // A drive's own Trash is the user's to empty too: `.Trash-UID` or
        // `.Trash/UID` at the top of a drive.
        if let Some((_, inside)) = self.split_media(&resolved) {
            return if is_volume_trash(&inside) && inside.components().count() <= 2 {
                Ok(())
            } else {
                Err(self.outside())
            };
        }
        if resolved == self.trash && resolved.starts_with(&self.home) {
            Ok(())
        } else {
            Err(self.outside())
        }
    }

    fn outside(&self) -> String {
        "Files only changes things inside your home folder and on your drives.".to_string()
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

    fn with_drive() -> (Temp, Guard) {
        let t = Temp::new();
        for dir in [
            "home/.local/share",
            "media/stick/photos",
            "media/stick/.Trash-1000/files",
            "other",
        ] {
            fs::create_dir_all(t.path().join(dir)).unwrap();
        }
        t.file("media/stick/photos/a.jpg", "");
        t.file("other/b.txt", "");
        let guard = Guard::with_media(
            &t.path().join("home"),
            &t.path().join("home/.local/share/Trash"),
            Some(&t.path().join("media")),
        )
        .unwrap();
        (t, guard)
    }

    #[test]
    fn a_drive_may_be_changed_inside_but_not_its_top() {
        let (t, guard) = with_drive();
        let stick = t.path().join("media/stick");
        assert!(guard.may_change(&stick.join("photos/a.jpg")).is_ok());
        assert!(guard.may_change(&stick.join("photos")).is_ok());
        let error = guard.may_change(&stick).unwrap_err();
        assert!(error.contains("is a drive"), "{error}");
        assert!(guard.may_change(&t.path().join("media")).is_err());
        assert!(guard.may_change(&stick.join(".Trash-1000")).is_err());
        assert!(guard.may_write_in(&stick).is_ok());
        assert!(guard.may_write_in(&stick.join("photos")).is_ok());
        assert!(guard.may_write_in(&t.path().join("media")).is_err());
        assert!(guard
            .may_write_in(&stick.join(".Trash-1000/files"))
            .is_err());
        // Home still works, and the rest of the disk still does not.
        assert!(guard.may_write_in(&t.path().join("home")).is_ok());
        assert!(guard.may_write_in(&t.path().join("other")).is_err());
        assert!(guard.may_change(Path::new("/etc/passwd")).is_err());
        assert!(guard.may_write_in(Path::new("/etc")).is_err());
    }

    #[test]
    fn a_link_on_a_drive_does_not_open_what_it_points_to() {
        let (t, guard) = with_drive();
        let stick = t.path().join("media/stick");
        std::os::unix::fs::symlink(t.path().join("other"), stick.join("out")).unwrap();
        assert!(guard.may_change(&stick.join("out")).is_ok());
        assert!(guard.may_change(&stick.join("out/b.txt")).is_err());
        assert!(guard.may_write_in(&stick.join("out")).is_err());
    }

    #[test]
    fn the_drive_that_holds_a_path_is_found() {
        let (t, guard) = with_drive();
        let stick = t.path().join("media/stick");
        assert_eq!(
            guard.volume_of(&stick.join("photos/a.jpg")),
            Some(stick.clone())
        );
        // Not made yet: the folder it will be in decides.
        assert_eq!(
            guard.volume_of(&stick.join("photos/new.jpg")),
            Some(stick.clone())
        );
        assert_eq!(guard.volume_of(&stick), Some(stick));
        assert_eq!(guard.volume_of(&t.path().join("home")), None);
        assert_eq!(guard.volume_of(&t.path().join("media")), None);
    }

    #[test]
    fn only_a_drives_own_trash_may_be_emptied() {
        let (t, guard) = with_drive();
        let stick = t.path().join("media/stick");
        assert!(guard.may_empty(&stick.join(".Trash-1000")).is_ok());
        assert!(guard.may_empty(&stick.join("photos")).is_err());
        assert!(guard.may_empty(&stick).is_err());
    }

    #[test]
    fn a_drive_keeps_its_trash_in_the_place_the_specification_names() {
        let t = Temp::new();
        let stick = t.path().join("stick");
        fs::create_dir_all(&stick).unwrap();
        // SAFETY: getuid cannot fail.
        let uid = unsafe { libc::getuid() };
        assert_eq!(
            volume_trash_dir(&stick),
            stick.join(format!(".Trash-{uid}"))
        );
        // A shared .Trash is used only when it has the sticky bit.
        fs::create_dir(stick.join(".Trash")).unwrap();
        assert_eq!(
            volume_trash_dir(&stick),
            stick.join(format!(".Trash-{uid}"))
        );
        fs::set_permissions(stick.join(".Trash"), fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(
            volume_trash_dir(&stick),
            stick.join(".Trash").join(uid.to_string())
        );
    }

    #[test]
    fn a_relative_path_is_refused() {
        let (_t, guard) = setup();
        assert!(guard.may_change(Path::new("stuff/a.txt")).is_err());
    }
}
