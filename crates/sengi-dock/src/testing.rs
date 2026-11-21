//! Helpers the tests of several modules share.

use std::fs;
use std::path::{Path, PathBuf};

use sengi_apps::{DesktopEntry, Env};

/// A fresh empty directory under the system temp directory.
pub fn temp_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "sengi-dock-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

pub fn entry(id: &str, name: &str, exec: &str) -> DesktopEntry {
    let mut entry = DesktopEntry::named(id, name, exec);
    entry.icon = id.into();
    entry
}

/// An environment with nothing set.
pub fn env() -> Env {
    Env::from_vars(|_| None)
}
