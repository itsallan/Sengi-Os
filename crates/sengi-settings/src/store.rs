//! The settings file as text. Reading it is sengi-config's, shared with
//! the programs that only honour a setting; this adds the one thing only
//! Settings does, which is putting a file back on disk.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub use sengi_config::store::{load, Dirs, Document, Value, FILE, MAX_BYTES};

/// Numbers the temporary files of this process, so that two writers never
/// share one.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// How many names to try before giving up on finding a free one.
const NAME_TRIES: usize = 64;

/// A new, empty file in `dir` that nobody else has: the name carries the
/// process and a counter, and the file is only ever created, never opened
/// if it is already there. A name left behind by a process that died is
/// passed over.
fn create_beside(dir: &Path, name: &str) -> io::Result<(fs::File, PathBuf)> {
    for _ in 0..NAME_TRIES {
        let temporary = dir.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let created = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary);
        match created {
            Ok(file) => return Ok((file, temporary)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("no free name for a temporary file"))
}

/// Replace `path` with `contents` in one step: written to a file beside
/// it, flushed to disk, then renamed over it. A reader sees the old file
/// or the new one, never half of either, and a crash leaves the old one.
/// Any number of threads and processes may do this to one path at once;
/// the file is then the whole of what one of them wrote.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("no parent directory"))?;
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?
        .to_string_lossy();
    let (mut file, temporary) = create_beside(dir, &name)?;
    let written = (|| {
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    match written {
        Ok(()) => {
            // The new name is only safe from a power cut once the
            // directory is on disk too. The file is in place either way,
            // so failing here is not a failed write.
            let _ = fs::File::open(dir).and_then(|dir| dir.sync_all());
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
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

    /// What writer `who` writes: long enough to take several writes to
    /// the disk, and different in length from every other writer's.
    fn body(who: usize) -> String {
        format!("[writer]\nwho = {who}\n{}", "pad = 1\n".repeat(4000 + who))
    }

    #[test]
    fn writers_at_the_same_time_never_leave_part_of_a_file() {
        const WRITERS: usize = 8;
        const ROUNDS: usize = 40;
        let tmp = TempDir::new();
        let path = tmp.path().join("labwc/rc.xml");
        write_atomic(&path, &body(0)).unwrap();
        let bodies: Vec<String> = (0..WRITERS).map(body).collect();
        let whole = |text: &str| bodies.iter().any(|body| body == text);

        std::thread::scope(|scope| {
            let writers: Vec<_> = (0..WRITERS)
                .map(|who| {
                    let (path, body) = (&path, &bodies[who]);
                    scope.spawn(move || {
                        for _ in 0..ROUNDS {
                            write_atomic(path, body).unwrap();
                        }
                    })
                })
                .collect();
            // Read all the while: every reading is one writer's whole file.
            while writers.iter().any(|writer| !writer.is_finished()) {
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(whole(&text), "read {} bytes of a file", text.len());
            }
        });
        assert!(whole(&std::fs::read_to_string(&path).unwrap()));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn a_temporary_file_left_by_someone_else_is_stepped_over() {
        let tmp = TempDir::new();
        let path = tmp.path().join("settings.toml");
        // The names the next few writes would have used are all taken.
        let next = NEXT.load(Ordering::Relaxed);
        for number in next..next + 8 {
            tmp.write(
                &format!(".settings.toml.{}.{number}.tmp", std::process::id()),
                "stale",
            );
        }
        write_atomic(&path, "k = 1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "k = 1\n");
    }
}
