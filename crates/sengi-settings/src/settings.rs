//! The settings, as Settings holds them: what sengi-config reads from the
//! two files, and the writing of the user's one.
//!
//! What the keys are, their defaults and limits, and which file wins are
//! all in sengi-config, where the other programs find them too.

use std::ops::Deref;
use std::path::PathBuf;

pub use sengi_config::settings::{
    check, xkb_word, zone_is_plain, Default, Layout, KEYS, MAX_IDLE_SECONDS, MAX_LAYOUTS,
};

use crate::store::{self, Dirs, Document, Value};

/// The settings in effect. Reading goes straight through to
/// [`sengi_config::Settings`].
#[derive(Debug, Clone, std::default::Default)]
pub struct Settings(sengi_config::Settings);

impl Deref for Settings {
    type Target = sengi_config::Settings;

    fn deref(&self) -> &sengi_config::Settings {
        &self.0
    }
}

impl Settings {
    /// Read both files. The warnings are for standard error: unreadable
    /// lines, keys this version does not know, values out of range.
    pub fn load(dirs: &Dirs) -> (Settings, Vec<String>) {
        let (settings, warnings) = sengi_config::Settings::load(dirs);
        (Settings(settings), warnings)
    }

    pub fn from_documents(
        system: Document,
        user: Document,
        user_path: Option<PathBuf>,
    ) -> Settings {
        Settings(sengi_config::Settings::from_documents(
            system, user, user_path,
        ))
    }

    /// Change one key in the user's file and write the file. On failure
    /// the value in memory is put back, so what is shown stays what is
    /// stored.
    pub fn set(&mut self, section: &str, key: &str, value: Value) -> Result<(), String> {
        let value = check(section, key, &value).map_err(|why| format!("{section}.{key} {why}"))?;
        if self.get(section, key).as_ref() == Some(&value) {
            return Ok(());
        }
        let path = self
            .user_path()
            .map(PathBuf::from)
            .ok_or("there is no home directory to keep settings in")?;
        let before = self.user().clone();
        self.0.user_mut().set(section, key, value);
        store::write_atomic(&path, &self.user().render()).map_err(|error| {
            *self.0.user_mut() = before;
            format!("could not write {}: {error}", path.display())
        })
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
}
