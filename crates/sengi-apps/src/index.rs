//! Every installed application, looked up by desktop file ID.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::desktop::{self, strip_suffix, DesktopEntry};
use crate::env::Env;

/// Every installed application, with the override order already applied.
/// Unlike a menu this includes `NoDisplay` entries and the like: a window
/// can belong to an application no menu lists.
#[derive(Clone, Debug, Default)]
pub struct AppIndex {
    entries: Vec<DesktopEntry>,
    by_id: HashMap<String, usize>,
}

impl AppIndex {
    /// Reads `applications/` under each data directory. A file in an earlier
    /// directory replaces one with the same ID in a later directory.
    pub fn load(data_dirs: &[PathBuf], env: &Env) -> Self {
        let dirs: Vec<PathBuf> = data_dirs
            .iter()
            .map(|dir| dir.join("applications"))
            .collect();
        // Lookups ignore case, so of two IDs that differ only in case the
        // one from the more important directory is kept.
        let mut seen = HashSet::new();
        let mut entries: Vec<DesktopEntry> = desktop::scan_all(&dirs, env)
            .into_iter()
            .filter(|entry| seen.insert(entry.app_id().to_lowercase()))
            .collect();
        entries.sort_by(|a, b| a.app_id().cmp(b.app_id()));
        Self::from_entries(entries)
    }

    /// An index of exactly these entries, in this order.
    pub fn from_entries(entries: Vec<DesktopEntry>) -> Self {
        let by_id = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.app_id().to_lowercase(), index))
            .collect();
        Self { entries, by_id }
    }

    /// Looks an entry up by desktop-file ID, with or without `.desktop`,
    /// ignoring case. The ID is tried as given first, because some IDs end
    /// in `.desktop` themselves: Telegram's is `org.telegram.desktop`.
    pub fn get(&self, id: &str) -> Option<&DesktopEntry> {
        let id = id.to_lowercase();
        self.by_id
            .get(&id)
            .or_else(|| self.by_id.get(strip_suffix(&id)))
            .map(|&index| &self.entries[index])
    }

    /// All entries, sorted by ID.
    pub fn entries(&self) -> &[DesktopEntry] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::ffi::OsString;

    const FOOT: &str = "[Desktop Entry]\nType=Application\nExec=foot\nIcon=foot\n\
        Terminal=false\nName=Foot\nName[de]=Fuss\nGenericName=Terminal\n";

    fn env() -> Env {
        Env::from_vars(|name| (name == "HOME").then(|| OsString::from("/nonexistent")))
    }

    fn load(dirs: &[PathBuf]) -> AppIndex {
        AppIndex::load(dirs, &env())
    }

    #[test]
    fn the_user_directory_overrides_the_system_one() {
        let tmp = TempDir::new();
        let (user, system) = (tmp.path().join("user"), tmp.path().join("system"));
        tmp.write(
            "user/applications/foot.desktop",
            "[Desktop Entry]\nType=Application\nName=My terminal\nExec=foot -f mono\n",
        );
        tmp.write("system/applications/foot.desktop", FOOT);
        tmp.write(
            "system/applications/web.desktop",
            "[Desktop Entry]\nType=Application\nName=Web\nExec=web\n",
        );

        let apps = load(&[user.clone(), system]);
        assert_eq!(apps.entries().len(), 2);
        let foot = apps.get("foot").unwrap();
        assert_eq!(foot.name, "My terminal");
        assert_eq!(foot.path, user.join("applications/foot.desktop"));
        assert_eq!(apps.get("web.desktop").unwrap().name, "Web");
    }

    #[test]
    fn a_hidden_user_file_removes_the_system_app() {
        let tmp = TempDir::new();
        tmp.write(
            "user/applications/foot.desktop",
            "[Desktop Entry]\nHidden=true\n",
        );
        tmp.write("system/applications/foot.desktop", FOOT);
        let apps = load(&[tmp.path().join("user"), tmp.path().join("system")]);
        assert!(apps.get("foot").is_none());
    }

    #[test]
    fn entries_menus_leave_out_are_still_indexed() {
        let tmp = TempDir::new();
        tmp.write(
            "applications/server.desktop",
            "[Desktop Entry]\nType=Application\nName=Mail\nNoDisplay=true\n\
             Terminal=true\nStartupWMClass=mail-client\n",
        );
        tmp.write(
            "applications/elsewhere.desktop",
            "[Desktop Entry]\nType=Application\nName=Elsewhere\nExec=elsewhere\n\
             OnlyShowIn=GNOME;\nTryExec=/no/such/program\n",
        );
        let apps = load(&[tmp.path().to_path_buf()]);
        let server = apps.get("server").unwrap();
        assert_eq!(server.name, "Mail");
        assert!(server.terminal);
        assert!(server.no_display);
        assert_eq!(server.wm_class.as_deref(), Some("mail-client"));
        assert_eq!(server.exec, "");
        let elsewhere = apps.get("elsewhere").unwrap();
        assert!(!elsewhere.no_display);
        assert_eq!(elsewhere.wm_class, None);
    }

    #[test]
    fn links_and_nameless_files_are_not_apps() {
        let tmp = TempDir::new();
        tmp.write(
            "applications/site.desktop",
            "[Desktop Entry]\nType=Link\nName=Site\nURL=x\n",
        );
        tmp.write(
            "applications/nameless.desktop",
            "[Desktop Entry]\nType=Application\n",
        );
        tmp.write("applications/empty.desktop", "");
        tmp.write(
            "applications/gone.desktop",
            "[Desktop Entry]\nType=Application\nName=Gone\nHidden=true\n",
        );
        assert!(load(&[tmp.path().to_path_buf()]).entries().is_empty());
    }

    #[test]
    fn an_id_that_ends_in_desktop_is_found_both_ways() {
        let tmp = TempDir::new();
        tmp.write(
            "applications/org.telegram.desktop.desktop",
            "[Desktop Entry]\nType=Application\nName=Telegram\nExec=telegram-desktop\n",
        );
        tmp.write(
            "applications/org.telegram.desktop",
            "[Desktop Entry]\nType=Application\nName=Other\nExec=other\n",
        );
        let apps = load(&[tmp.path().to_path_buf()]);
        assert_eq!(apps.get("org.telegram.desktop").unwrap().name, "Telegram");
        assert_eq!(
            apps.get("org.telegram.desktop.desktop").unwrap().name,
            "Telegram"
        );
        assert_eq!(apps.get("Org.Telegram.Desktop").unwrap().name, "Telegram");
        assert_eq!(apps.get("org.telegram").unwrap().name, "Other");
    }

    #[test]
    fn ids_ignore_case_and_flatten_subdirectories() {
        let tmp = TempDir::new();
        tmp.write(
            "applications/kde/Okular.desktop",
            "[Desktop Entry]\nType=Application\nName=Okular\nExec=okular\n",
        );
        tmp.write("applications/notes.txt", "not a desktop file");
        let apps = load(&[tmp.path().to_path_buf(), PathBuf::from("/nonexistent")]);
        assert_eq!(apps.entries().len(), 1);
        assert_eq!(apps.get("KDE-okular").unwrap().app_id(), "kde-Okular");
    }

    #[test]
    fn of_two_ids_that_differ_in_case_the_earlier_directory_wins() {
        let tmp = TempDir::new();
        tmp.write(
            "user/applications/Foot.desktop",
            "[Desktop Entry]\nType=Application\nName=Mine\nExec=foot\n",
        );
        tmp.write("system/applications/foot.desktop", FOOT);
        let apps = load(&[tmp.path().join("user"), tmp.path().join("system")]);
        assert_eq!(apps.entries().len(), 1);
        assert_eq!(apps.get("foot").unwrap().name, "Mine");
    }
}
