//! A throwaway directory for tests.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub struct TempDir(PathBuf);

static NEXT: AtomicU32 = AtomicU32::new(0);

impl TempDir {
    pub fn new() -> TempDir {
        let dir = std::env::temp_dir().join(format!(
            "sengi-install-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }

    pub fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    pub fn mkdir(&self, path: &str) {
        std::fs::create_dir_all(self.0.join(path)).unwrap();
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.0.join(path)).unwrap()
    }

    pub fn exists(&self, path: &str) -> bool {
        self.0.join(path).symlink_metadata().is_ok()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
