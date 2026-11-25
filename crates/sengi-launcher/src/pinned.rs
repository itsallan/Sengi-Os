//! The dock's pinned apps, as the launcher's right-click menu changes them.
//! It reads and writes the same `sengi-os/dock.toml` the dock does, through
//! sengi-apps, and never talks to the dock: the dock notices the file
//! changed.

use std::path::PathBuf;

use sengi_apps::pins;
use sengi_apps::{AppIndex, DesktopEntry};

use crate::ui::Pins;

pub struct DockPins {
    apps: AppIndex,
    /// The app id of each candidate, by candidate index.
    ids: Vec<String>,
    /// The files the list may come from, most important first.
    read: Vec<PathBuf>,
    /// The user's file. None without a home folder.
    write: Option<PathBuf>,
}

impl DockPins {
    /// `entries` are the applications the candidates stand for, in the same
    /// order.
    pub fn new(entries: &[DesktopEntry], read: Vec<PathBuf>, write: Option<PathBuf>) -> Self {
        DockPins {
            apps: AppIndex::from_entries(entries.to_vec()),
            ids: entries
                .iter()
                .map(|entry| entry.app_id().to_string())
                .collect(),
            read,
            write,
        }
    }

    /// For the process's own environment.
    pub fn from_process(entries: &[DesktopEntry]) -> Self {
        Self::new(entries, pins::paths(), pins::user_path())
    }
}

impl Pins for DockPins {
    fn is_pinned(&self, index: usize) -> Option<bool> {
        let id = self.ids.get(index)?;
        // Read again every time: the dock may have changed the file since.
        Some(pins::is_pinned(&pins::pinned(&self.read), &self.apps, id))
    }

    fn set(&self, index: usize, pinned: bool) -> Result<(), String> {
        let id = self.ids.get(index).ok_or("That application is gone")?;
        let Some(path) = &self.write else {
            return Err("There is no home folder to keep the dock in".to_string());
        };
        let mut list = pins::pinned(&self.read);
        let changed = if pinned {
            pins::pin(&mut list, &self.apps, id)
        } else {
            pins::unpin(&mut list, &self.apps, id)
        };
        if !changed {
            return Ok(());
        }
        pins::save(path, &list).map_err(|error| format!("Cannot save the dock: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn entries() -> Vec<DesktopEntry> {
        vec![
            DesktopEntry::named("foot", "Terminal", "foot"),
            DesktopEntry::named("web", "Web", "web"),
            DesktopEntry::named("org.example.Mail", "Mail", "mail"),
        ]
    }

    fn pins_in(dir: &TempDir) -> DockPins {
        let user = dir.path().join("user/sengi-os/dock.toml");
        let system = dir.path().join("system/sengi-os/dock.toml");
        DockPins::new(&entries(), vec![user.clone(), system], Some(user))
    }

    fn saved(dir: &TempDir) -> Vec<String> {
        let path = dir.path().join("user/sengi-os/dock.toml");
        pins::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn keeping_an_app_adds_it_after_the_others_in_the_users_file() {
        let dir = TempDir::new();
        dir.write("system/sengi-os/dock.toml", "pinned = [\"foot\"]\n");
        let pins = pins_in(&dir);
        assert_eq!(pins.is_pinned(0), Some(true));
        assert_eq!(pins.is_pinned(1), Some(false));
        pins.set(1, true).unwrap();
        pins.set(2, true).unwrap();
        // The system file is left alone; the user's now holds the list.
        assert_eq!(saved(&dir), ["foot", "web", "org.example.mail"]);
        assert_eq!(pins.is_pinned(2), Some(true));
    }

    #[test]
    fn removing_an_app_takes_it_out_and_keeps_the_rest() {
        let dir = TempDir::new();
        dir.write(
            "user/sengi-os/dock.toml",
            "# mine\npinned = [\"foot\", \"web\"]\nlater = 1\n",
        );
        let pins = pins_in(&dir);
        pins.set(0, false).unwrap();
        assert_eq!(saved(&dir), ["web"]);
        let text = std::fs::read_to_string(dir.path().join("user/sengi-os/dock.toml")).unwrap();
        assert!(text.starts_with("# mine\n") && text.contains("later = 1"));
        assert_eq!(pins.is_pinned(0), Some(false));
        // Doing it again changes nothing and is not an error.
        pins.set(0, false).unwrap();
    }

    #[test]
    fn without_a_home_folder_nothing_is_written() {
        let dir = TempDir::new();
        let user = dir.path().join("user/dock.toml");
        let pins = DockPins::new(&entries(), vec![user], None);
        assert!(pins.set(1, true).is_err());
        assert_eq!(pins.is_pinned(9), None);
    }
}
