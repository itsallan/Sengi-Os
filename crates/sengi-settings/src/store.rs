//! The settings file as text. Reading it is sengi-config's, shared with
//! the programs that only honour a setting; this adds the one thing only
//! Settings does, which is putting a file back on disk.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub use sengi_config::store::{load, Dirs, Document, Value, FILE, MAX_BYTES};

/// Replace `path` with `contents` in one step: written to a file beside
/// it, flushed to disk, then renamed over it. A reader sees the old file
/// or the new one, never half of either, and a crash leaves the old one.
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent directory"))?;
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?
        .to_string_lossy();
    let temporary = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn writing_replaces_the_file_whole_and_leaves_nothing_behind() {
        let tmp = TempDir::new();
        let path = tmp.path().join("sengi-os/settings.toml");
        write_atomic(&path, "[a]\nk = 1\n").unwrap();
        write_atomic(&path, "[a]\nk = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[a]\nk = 2\n");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["settings.toml"]);
    }

    #[test]
    fn a_failed_write_keeps_the_old_file() {
        let tmp = TempDir::new();
        // The target is a directory, so the rename cannot succeed.
        std::fs::create_dir_all(tmp.path().join("conf/settings.toml/inner")).unwrap();
        let path = tmp.path().join("conf/settings.toml");
        assert!(write_atomic(&path, "x = 1\n").is_err());
        assert!(path.join("inner").is_dir());
        assert_eq!(
            std::fs::read_dir(tmp.path().join("conf")).unwrap().count(),
            1
        );
    }
}
