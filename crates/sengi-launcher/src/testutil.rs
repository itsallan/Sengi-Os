//! A throwaway directory for tests that touch the file system.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        // Beside the test binary, under the build's target directory: tests
        // run scripts from here, and a guessable path in a shared /tmp could
        // be prepared by someone else. The name is short because socket
        // paths have little room.
        let base = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .expect("the test binary has a directory");
        let path = base.join(format!("slt-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temporary directory");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).expect("create parent directory");
        std::fs::write(&path, contents).expect("write test file");
        path
    }

    pub fn make_executable(&self, relative: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.0.join(relative);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("set permissions");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
