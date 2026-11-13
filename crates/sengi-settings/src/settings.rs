//! The settings, as Settings holds them: what sengi-config reads from the
//! two files, and the writing of the user's one.
//!
//! What the keys are, their defaults and limits, and which file wins are
//! all in sengi-config, where the other programs find them too.

use std::fs;
use std::ops::Deref;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub use sengi_config::settings::{
    check, xkb_word, zone_is_plain, Default, Layout, KEYS, MAX_IDLE_SECONDS, MAX_LAYOUTS,
};

use crate::store::{self, Dirs, Document, Value};

/// One key and what it should become: section, key, value.
pub type Change = (String, String, Value);

/// What tells one version of a file from the next without reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    size: u64,
    inode: u64,
}

/// The stamp of the file at `path`; `None` when there is none.
fn stamp(path: Option<&Path>) -> Option<Stamp> {
    let meta = fs::metadata(path?).ok()?;
    Some(Stamp {
        modified: meta.modified().ok(),
        size: meta.len(),
        inode: meta.ino(),
    })
}

/// The settings in effect. Reading goes straight through to
/// [`sengi_config::Settings`].
#[derive(Debug, Clone, std::default::Default)]
pub struct Settings {
    read: sengi_config::Settings,
    /// The user's file as it was when it was last read or written here.
    seen: Option<Stamp>,
}

impl Deref for Settings {
    type Target = sengi_config::Settings;

    fn deref(&self) -> &sengi_config::Settings {
        &self.read
    }
}

impl Settings {
    /// Read both files. The warnings are for standard error: unreadable
    /// lines, keys this version does not know, values out of range.
    pub fn load(dirs: &Dirs) -> (Settings, Vec<String>) {
        // Stamped before it is read: a change in between shows up as a
        // change later, where the other order would hide it.
        let seen = stamp(dirs.user(store::FILE).as_deref());
        let (read, warnings) = sengi_config::Settings::load(dirs);
        (Settings { read, seen }, warnings)
    }

    pub fn from_documents(
        system: Document,
        user: Document,
        user_path: Option<PathBuf>,
    ) -> Settings {
        let seen = stamp(user_path.as_deref());
        let read = sengi_config::Settings::from_documents(system, user, user_path);
        Settings { read, seen }
    }

    /// The file belongs to its owner, who may have edited it since it was
    /// read here. Writing back the copy from then would undo that edit
    /// without a word, so a file that is no longer the one that was read
    /// is read again first. True if it was read again.
    pub fn catch_up(&mut self) -> bool {
        let Some(path) = self.user_path().map(PathBuf::from) else {
            return false;
        };
        let now = stamp(Some(&path));
        if now == self.seen {
            return false;
        }
        *self.read.user_mut() = store::load(&path).0;
        self.seen = now;
        true
    }

    /// Change one key here and not in the file: for a copy that shows
    /// what is on its way to the file through [`Settings::change`].
    pub fn hold(&mut self, change: &Change) {
        let (section, key, value) = change;
        self.read.user_mut().set(section, key, value.clone());
    }

    /// Change one key in the user's file: see [`Settings::change`].
    pub fn set(&mut self, section: &str, key: &str, value: Value) -> Result<(), String> {
        self.change(&[(section.to_string(), key.to_string(), value)])
    }

    /// Change keys in the user's file and write the file: the file as
    /// it is now, with those keys changed. This is the only place the
    /// file is written. On failure the values in memory are put back,
    /// so what is held stays what is stored.
    pub fn change(&mut self, changes: &[Change]) -> Result<(), String> {
        let mut checked = Vec::with_capacity(changes.len());
        for (section, key, value) in changes {
            let value =
                check(section, key, value).map_err(|why| format!("{section}.{key} {why}"))?;
            checked.push((section, key, value));
        }
        self.catch_up();
        let before = self.user().clone();
        for (section, key, value) in checked {
            if self.get(section, key).as_ref() != Some(&value) {
                self.read.user_mut().set(section, key, value);
            }
        }
        if *self.user() == before {
            return Ok(());
        }
        let Some(path) = self.user_path().map(PathBuf::from) else {
            *self.read.user_mut() = before;
            return Err("there is no home directory to keep settings in".to_string());
        };
        match store::write_atomic(&path, &self.user().render()) {
            Ok(()) => {
                self.seen = stamp(Some(&path));
                Ok(())
            }
            Err(error) => {
                *self.read.user_mut() = before;
                Err(format!("could not write {}: {error}", path.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn settings(system: &str, user: &str) -> Settings {
        Settings::from_documents(Document::parse(system).0, Document::parse(user).0, None)
    }

    #[test]
    fn an_edit_made_by_hand_meanwhile_is_not_written_over() {
        let tmp = TempDir::new();
        let file = "home/sengi-os/settings.toml";
        let path = tmp.path().join(file);
        tmp.write(file, "[power]\nlock-after = 600\n");
        let dirs = Dirs::from(
            None,
            Some(&tmp.path().join("home").display().to_string()),
            Some("/nonexistent"),
        );
        let (mut s, _) = Settings::load(&dirs);

        // While Settings is open the owner adds a key and a comment.
        tmp.write(
            file,
            "# edited by hand\n[power]\nlock-after = 600\n[mouse]\nspeed = 40\n",
        );
        s.set("clock", "format", Value::text("12h")).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# edited by hand\n[power]\nlock-after = 600\n[mouse]\nspeed = 40\n\n[clock]\nformat = \"12h\"\n"
        );
        // What was read again is also what is in effect here now.
        assert_eq!(s.number("mouse", "speed"), 40);

        // They change the very key that is then changed here: the later
        // change is the one made here, the rest of their edit stands.
        tmp.write(
            file,
            "[power]\nlock-after = 900  # mine\nblank-after = 60\n",
        );
        s.set("power", "lock-after", Value::Number(120)).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[power]\nlock-after = 120  # mine\nblank-after = 60\n"
        );

        // They already set what is being set here: nothing to write.
        tmp.write(file, "[power]\nlock-after = 30\n# untouched\n");
        s.set("power", "lock-after", Value::Number(30)).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[power]\nlock-after = 30\n# untouched\n"
        );

        // They delete the file: what is written is the one key, not a
        // copy of what the file used to say.
        fs::remove_file(&path).unwrap();
        s.set("clock", "format", Value::text("12h")).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[clock]\nformat = \"12h\"\n"
        );

        // And with nobody else writing, each change builds on the last.
        s.set("power", "blank-after", Value::Number(0)).unwrap();
        s.set("clock", "format", Value::text("24h")).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "[clock]\nformat = \"24h\"\n\n[power]\nblank-after = 0\n"
        );
    }

    #[test]
    fn a_change_is_written_and_unknown_keys_survive_it() {
        let tmp = TempDir::new();
        tmp.write("etc/sengi-os/settings.toml", "[power]\nlock-after = 600\n");
        tmp.write(
            "home/sengi-os/settings.toml",
            "# mine\n[power]\nfuture = \"keep me\"\n",
        );
        let dirs = Dirs::from(
            None,
            Some(&tmp.path().join("home").display().to_string()),
            Some(&tmp.path().join("etc").display().to_string()),
        );
        let (mut s, warnings) = Settings::load(&dirs);
        assert_eq!(warnings.len(), 1);
        assert_eq!(s.number("power", "lock-after"), 600);
        s.set("power", "lock-after", Value::Number(120)).unwrap();
        s.set("clock", "format", Value::text("12h")).unwrap();
        let written =
            std::fs::read_to_string(tmp.path().join("home/sengi-os/settings.toml")).unwrap();
        assert_eq!(
            written,
            "# mine\n[power]\nfuture = \"keep me\"\nlock-after = 120\n\n[clock]\nformat = \"12h\"\n"
        );
        // The system file is never touched.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("etc/sengi-os/settings.toml")).unwrap(),
            "[power]\nlock-after = 600\n"
        );
        let (again, _) = Settings::load(&dirs);
        assert_eq!(again.number("power", "lock-after"), 120);
        assert_eq!(again.text("clock", "format"), "12h");
    }

    #[test]
    fn a_refused_or_failed_change_leaves_things_as_they_were() {
        let tmp = TempDir::new();
        let dirs = Dirs::from(
            None,
            Some(&tmp.path().display().to_string()),
            Some("/nonexistent"),
        );
        let (mut s, _) = Settings::load(&dirs);
        assert!(s.set("power", "lock-after", Value::Number(-1)).is_err());
        assert!(s.set("power", "nonsense", Value::Number(1)).is_err());
        assert!(!tmp.path().join("sengi-os").exists());
        // Setting the value already in effect writes nothing.
        s.set("power", "lock-after", Value::Number(300)).unwrap();
        assert!(!tmp.path().join("sengi-os").exists());
        // A directory where the file should be: the write fails.
        std::fs::create_dir_all(tmp.path().join("sengi-os/settings.toml")).unwrap();
        assert!(s.set("power", "lock-after", Value::Number(60)).is_err());
        assert_eq!(s.number("power", "lock-after"), 300);
        // Nowhere to write at all.
        let mut homeless = settings("", "");
        assert!(homeless
            .set("power", "lock-after", Value::Number(60))
            .is_err());
        assert_eq!(homeless.number("power", "lock-after"), 300);
    }

    #[test]
    fn several_changes_are_one_write_on_top_of_the_file_as_it_is() {
        let tmp = TempDir::new();
        let file = "home/sengi-os/settings.toml";
        let path = tmp.path().join(file);
        tmp.write(file, "[power]\nlock-after = 600\n");
        let dirs = Dirs::from(
            None,
            Some(&tmp.path().join("home").display().to_string()),
            Some("/nonexistent"),
        );
        let (mut s, _) = Settings::load(&dirs);
        tmp.write(file, "# by hand\n[power]\nlock-after = 600\n");
        let change =
            |section: &str, key: &str, value| (section.to_string(), key.to_string(), value);
        s.change(&[
            change("power", "lock-after", Value::Number(60)),
            change("power", "lock-after", Value::Number(120)),
            change("clock", "format", Value::text("12h")),
        ])
        .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# by hand\n[power]\nlock-after = 120\n\n[clock]\nformat = \"12h\"\n"
        );
        // One bad change and none of them is made.
        let refused = s.change(&[
            change("power", "blank-after", Value::Number(60)),
            change("power", "lock-after", Value::Number(-1)),
        ]);
        assert!(refused.is_err());
        assert_eq!(s.number("power", "blank-after"), 300);
        assert!(!fs::read_to_string(&path).unwrap().contains("blank-after"));

        // A copy that only holds a change shows it and writes nothing.
        let (mut shown, _) = Settings::load(&dirs);
        shown.hold(&change("power", "lock-after", Value::Number(30)));
        assert_eq!(shown.number("power", "lock-after"), 30);
        assert!(!shown.catch_up());
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains("lock-after = 120"));
    }
}
