//! What Files does to the file system, one function at a time, with no
//! window in sight: reading a folder, naming, copying, moving.
//!
//! Nothing here replaces or overwrites. A name that is taken is not used:
//! the item gets the next free "name 2", "name 3". Links are copied as
//! links and never followed out of the folder being copied. Whether a
//! thing may be touched at all is for [`crate::guard`]; these functions do
//! what they are told.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sengi_system::trash::numbered_name;

/// The name a new folder starts with.
pub const NEW_FOLDER: &str = "untitled folder";

/// The longest name a file system takes, in bytes.
const MAX_NAME: usize = 255;

/// One thing in a folder, or in the Trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// A folder, or a link to one.
    pub folder: bool,
    pub link: bool,
    pub size: u64,
    /// Seconds since 1970, or 0 when not known.
    pub modified: i64,
    /// In the Trash: where it was, and when it was trashed.
    pub original: Option<PathBuf>,
    pub deleted: String,
}

/// What reading a folder found.
#[derive(Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// Names that are not text and so are not shown.
    pub skipped: usize,
}

/// Folders first, then each group by name without regard to case, and by
/// the exact name when those are the same.
pub fn sort(entries: &mut [Entry]) {
    entries
        .sort_by_cached_key(|entry| (!entry.folder, entry.name.to_lowercase(), entry.name.clone()));
}

/// Reads `dir`. Names that start with a dot are left out unless `hidden`.
/// `progress` is told how many entries have been read so far, every few
/// hundred of them; `cancel` ends the read early with an empty answer.
pub fn read(
    dir: &Path,
    hidden: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize),
) -> io::Result<Listing> {
    let mut listing = Listing::default();
    let mut seen = 0usize;
    for item in fs::read_dir(dir)? {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Listing::default());
        }
        seen += 1;
        if seen.is_multiple_of(500) {
            progress(seen);
        }
        let Ok(item) = item else { continue };
        let Ok(name) = item.file_name().into_string() else {
            listing.skipped += 1;
            continue;
        };
        if !hidden && name.starts_with('.') {
            continue;
        }
        let meta = item.metadata().ok();
        let link = meta
            .as_ref()
            .is_some_and(|meta| meta.file_type().is_symlink());
        // A link to a folder is a folder. A broken link is a file.
        let folder = if link {
            fs::metadata(item.path()).is_ok_and(|meta| meta.is_dir())
        } else {
            meta.as_ref().is_some_and(|meta| meta.is_dir())
        };
        listing.entries.push(Entry {
            name,
            folder,
            link,
            size: meta
                .as_ref()
                .filter(|_| !folder)
                .map_or(0, |meta| meta.len()),
            modified: meta.as_ref().map_or(0, MetadataExt::mtime),
            original: None,
            deleted: String::new(),
        });
    }
    sort(&mut listing.entries);
    Ok(listing)
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Makes a folder with the first free "untitled folder" name and returns it.
/// Creating is what claims the name, so two makers cannot get the same one.
pub fn create_folder(dir: &Path) -> io::Result<String> {
    for n in 1u32.. {
        let name = if n == 1 {
            NEW_FOLDER.to_string()
        } else {
            format!("{NEW_FOLDER} {n}")
        };
        match fs::create_dir(dir.join(&name)) {
            Ok(()) => return Ok(name),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!("the counter does not run out")
}

#[derive(Debug)]
pub enum RenameError {
    /// A name cannot hold a slash, be "." or "..", or be this long.
    Invalid,
    /// Something is already called that.
    Exists(String),
    Io(io::Error),
}

impl fmt::Display for RenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => write!(f, "That name cannot be used."),
            Self::Exists(name) => write!(f, "\"{name}\" already exists."),
            Self::Io(error) => write!(f, "Cannot rename: {}", sengi_system::trash::plain(error)),
        }
    }
}

/// Renames `old` to `new` inside `dir` and returns the name it now has.
/// Spaces around the new name are dropped. An empty name, or the same one,
/// changes nothing. Nothing is ever replaced.
pub fn rename(dir: &Path, old: &str, new: &str) -> Result<String, RenameError> {
    let new = new.trim();
    if new.is_empty() || new == old {
        return Ok(old.to_string());
    }
    if new == "." || new == ".." || new.contains('/') || new.contains('\0') || new.len() > MAX_NAME
    {
        return Err(RenameError::Invalid);
    }
    let target = dir.join(new);
    if exists(&target) {
        return Err(RenameError::Exists(new.to_string()));
    }
    fs::rename(dir.join(old), &target).map_err(RenameError::Io)?;
    Ok(new.to_string())
}

/// Whether `inner` is `outer` or somewhere under it, once links are
/// resolved. A path that cannot be resolved is taken to be inside, which
/// is the cautious answer for the callers.
fn is_within(inner: &Path, outer: &Path) -> bool {
    match (fs::canonicalize(inner), fs::canonicalize(outer)) {
        (Ok(inner), Ok(outer)) => inner.starts_with(outer),
        _ => true,
    }
}

fn name_of(path: &Path) -> io::Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "it has no name"))
}

/// What a copy left out because it is not a file, a folder or a link.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Copied {
    pub path: PathBuf,
    pub skipped: usize,
}

/// Copies `src` into the folder `dest_dir` under a name that is free there,
/// and returns where it went. A folder is copied with everything in it; a
/// link is made again as a link. If anything fails, what was made is
/// removed again and `src` is untouched.
pub fn copy_into(src: &Path, dest_dir: &Path) -> io::Result<Copied> {
    let name = name_of(src)?;
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() && is_within(dest_dir, src) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a folder cannot be copied into itself",
        ));
    }
    let mut skipped = 0;
    for n in 1u32.. {
        let target = dest_dir.join(numbered_name(&name, n));
        let made = if meta.is_dir() {
            match fs::create_dir(&target) {
                Ok(()) => Some(copy_folder_contents(src, &target, &mut skipped)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => return Err(error),
            }
        } else {
            copy_leaf(src, &target, &meta, &mut skipped)
        };
        match made {
            None => continue,
            Some(Ok(())) => {
                if meta.is_dir() {
                    // The folder's own mode last, in case it is read-only.
                    let _ = fs::set_permissions(&target, meta.permissions());
                }
                return Ok(Copied {
                    path: target,
                    skipped,
                });
            }
            Some(Err(error)) => {
                // What was made here is ours, and half of it is no use.
                if meta.is_dir() {
                    let _ = fs::remove_dir_all(&target);
                } else {
                    let _ = fs::remove_file(&target);
                }
                return Err(error);
            }
        }
    }
    unreachable!("the counter does not run out")
}

/// Copies one thing that is not a folder to `target`, which must not
/// exist. None when it does.
fn copy_leaf(
    src: &Path,
    target: &Path,
    meta: &fs::Metadata,
    skipped: &mut usize,
) -> Option<io::Result<()>> {
    let kind = meta.file_type();
    if kind.is_symlink() {
        let result = fs::read_link(src).and_then(|to| std::os::unix::fs::symlink(to, target));
        return match result {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
            other => Some(other),
        };
    }
    if !kind.is_file() {
        // A socket, a pipe or a device: nothing to copy.
        *skipped += 1;
        return Some(Ok(()));
    }
    let mut from = match fs::File::open(src) {
        Ok(file) => file,
        Err(error) => return Some(Err(error)),
    };
    let mut to = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(meta.permissions().mode() & 0o777)
        .open(target)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return None,
        Err(error) => return Some(Err(error)),
    };
    Some(io::copy(&mut from, &mut to).map(|_| ()))
}

fn copy_folder_contents(src: &Path, target: &Path, skipped: &mut usize) -> io::Result<()> {
    for item in fs::read_dir(src)? {
        let item = item?;
        let from = item.path();
        let to = target.join(item.file_name());
        // The type of the thing itself: a link is not followed.
        let meta = fs::symlink_metadata(&from)?;
        if meta.is_dir() {
            fs::create_dir(&to)?;
            copy_folder_contents(&from, &to, skipped)?;
            fs::set_permissions(&to, meta.permissions())?;
        } else if let Some(result) = copy_leaf(&from, &to, &meta, skipped) {
            result?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the copy was disturbed",
            ));
        }
    }
    Ok(())
}

/// Moves `src` into the folder `dest_dir` under a name that is free there,
/// and returns where it went. Across disks it is copied and then removed,
/// and the original is only removed when the whole copy succeeded.
pub fn move_into(src: &Path, dest_dir: &Path) -> io::Result<PathBuf> {
    let name = name_of(src)?;
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() && is_within(dest_dir, src) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a folder cannot be moved into itself",
        ));
    }
    let target = (1u32..)
        .map(|n| dest_dir.join(numbered_name(&name, n)))
        .find(|candidate| !exists(candidate))
        .expect("the counter does not run out");
    match fs::rename(src, &target) {
        Ok(()) => Ok(target),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            let copied = copy_into(src, dest_dir)?;
            let removed = if meta.is_dir() {
                fs::remove_dir_all(src)
            } else {
                fs::remove_file(src)
            };
            // The copy is kept even if the original cannot be removed: better
            // two than none.
            removed.map(|()| copied.path)
        }
        Err(error) => Err(error),
    }
}

/// Free space for the user on the file system that holds `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs gets a valid C string and a struct it fills in.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        stat
    };
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// A size as people read it: "812 B", "12.3 kB", "4.1 MB".
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A time in seconds since 1970 as "2026-11-25 10:20" in local time, or
/// nothing when it is not known.
pub fn date_text(seconds: i64) -> String {
    if seconds == 0 {
        return String::new();
    }
    let seconds = seconds as libc::time_t;
    // SAFETY: localtime_r gets valid pointers, and tm is plain data.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return String::new();
        }
        tm
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// A fresh empty folder, removed again when it is dropped.
    pub struct Temp(pub PathBuf);

    static NEXT: AtomicU32 = AtomicU32::new(0);

    impl Default for Temp {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Temp {
        pub fn new() -> Temp {
            let dir = std::env::temp_dir().join(format!(
                "sengi-files-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            // The path as the system spells it, so comparisons with
            // canonical paths hold.
            Temp(fs::canonicalize(dir).unwrap())
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        pub fn file(&self, path: &str, text: &str) -> PathBuf {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing.entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn list(dir: &Path, hidden: bool) -> Listing {
        read(dir, hidden, &AtomicBool::new(false), &mut |_| {}).unwrap()
    }

    #[test]
    fn folders_come_first_and_names_sort_without_case() {
        let t = Temp::new();
        t.file("banana.txt", "");
        t.file("Apple.txt", "");
        t.file("cherry", "");
        t.file("Zeta/inner", "");
        t.file("alpha/inner", "");
        let listing = list(t.path(), false);
        assert_eq!(
            names(&listing),
            ["alpha", "Zeta", "Apple.txt", "banana.txt", "cherry"]
        );
        assert!(listing.entries[0].folder && !listing.entries[2].folder);
    }

    #[test]
    fn names_that_differ_only_in_case_keep_a_fixed_order() {
        let t = Temp::new();
        t.file("a", "");
        t.file("A", "");
        assert_eq!(names(&list(t.path(), false)), ["A", "a"]);
    }

    #[test]
    fn hidden_files_are_left_out_unless_asked_for() {
        let t = Temp::new();
        t.file(".secret", "");
        t.file("shown", "");
        assert_eq!(names(&list(t.path(), false)), ["shown"]);
        assert_eq!(names(&list(t.path(), true)), [".secret", "shown"]);
    }

    #[test]
    fn a_link_to_a_folder_is_a_folder_and_a_broken_one_a_file() {
        let t = Temp::new();
        fs::create_dir(t.path().join("real")).unwrap();
        std::os::unix::fs::symlink(t.path().join("real"), t.path().join("to-real")).unwrap();
        std::os::unix::fs::symlink(t.path().join("gone"), t.path().join("broken")).unwrap();
        let listing = list(t.path(), false);
        let by = |name: &str| listing.entries.iter().find(|e| e.name == name).unwrap();
        assert!(by("to-real").folder && by("to-real").link);
        assert!(!by("broken").folder && by("broken").link);
    }

    #[test]
    fn sizes_and_dates_are_read() {
        let t = Temp::new();
        t.file("five", "12345");
        let entry = list(t.path(), false).entries.remove(0);
        assert_eq!(entry.size, 5);
        assert!(entry.modified > 1_700_000_000);
        assert_eq!(date_text(entry.modified).len(), 16);
    }

    #[test]
    fn a_big_folder_is_read_whole_and_reports_progress() {
        let t = Temp::new();
        for n in 0..10_000 {
            fs::write(t.path().join(format!("f{n:05}")), "").unwrap();
        }
        let mut reports = Vec::new();
        let listing = read(t.path(), false, &AtomicBool::new(false), &mut |n| {
            reports.push(n)
        })
        .unwrap();
        assert_eq!(listing.entries.len(), 10_000);
        assert_eq!(listing.entries[0].name, "f00000");
        assert_eq!(reports.len(), 20);
        assert_eq!(reports[0], 500);
    }

    #[test]
    fn a_cancelled_read_gives_nothing() {
        let t = Temp::new();
        t.file("a", "");
        let listing = read(t.path(), false, &AtomicBool::new(true), &mut |_| {}).unwrap();
        assert!(listing.entries.is_empty());
    }

    #[test]
    fn a_folder_that_is_not_there_is_an_error() {
        let t = Temp::new();
        let error = read(
            &t.path().join("nope"),
            false,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn new_folders_get_free_names() {
        let t = Temp::new();
        assert_eq!(create_folder(t.path()).unwrap(), "untitled folder");
        assert_eq!(create_folder(t.path()).unwrap(), "untitled folder 2");
        fs::remove_dir(t.path().join("untitled folder")).unwrap();
        assert_eq!(create_folder(t.path()).unwrap(), "untitled folder");
    }

    #[test]
    fn rename_changes_the_name_and_never_replaces() {
        let t = Temp::new();
        t.file("a.txt", "a");
        t.file("b.txt", "b");
        assert_eq!(rename(t.path(), "a.txt", " c.txt ").unwrap(), "c.txt");
        assert!(t.path().join("c.txt").is_file());
        assert!(matches!(
            rename(t.path(), "c.txt", "b.txt"),
            Err(RenameError::Exists(_))
        ));
        assert_eq!(fs::read_to_string(t.path().join("b.txt")).unwrap(), "b");
        // Changing only the case of a name that is not taken works.
        assert_eq!(rename(t.path(), "b.txt", "B.txt").unwrap(), "B.txt");
    }

    #[test]
    fn rename_refuses_names_that_are_paths_or_empty() {
        let t = Temp::new();
        t.file("a", "");
        for bad in ["..", ".", "x/y", "../x", "a\0b", &"n".repeat(256)] {
            assert!(
                matches!(rename(t.path(), "a", bad), Err(RenameError::Invalid)),
                "{bad:?}"
            );
        }
        assert_eq!(rename(t.path(), "a", "   ").unwrap(), "a");
        assert_eq!(rename(t.path(), "a", "a").unwrap(), "a");
        assert!(t.path().join("a").exists());
    }

    #[test]
    fn a_file_is_copied_under_a_free_name() {
        let t = Temp::new();
        let src = t.file("one/photo.jpg", "pixels");
        fs::create_dir(t.path().join("two")).unwrap();
        let first = copy_into(&src, &t.path().join("two")).unwrap();
        assert_eq!(first.path, t.path().join("two/photo.jpg"));
        let second = copy_into(&src, &t.path().join("two")).unwrap();
        assert_eq!(second.path, t.path().join("two/photo 2.jpg"));
        let third = copy_into(&src, &t.path().join("two")).unwrap();
        assert_eq!(third.path, t.path().join("two/photo 3.jpg"));
        assert_eq!(fs::read_to_string(second.path).unwrap(), "pixels");
        // The original is where it was.
        assert_eq!(fs::read_to_string(src).unwrap(), "pixels");
    }

    #[test]
    fn copying_into_the_same_folder_makes_a_numbered_copy() {
        let t = Temp::new();
        let src = t.file("a.txt", "x");
        let copy = copy_into(&src, t.path()).unwrap();
        assert_eq!(copy.path, t.path().join("a 2.txt"));
    }

    #[test]
    fn a_folder_is_copied_with_everything_in_it() {
        let t = Temp::new();
        t.file("src/a", "a");
        t.file("src/deep/er/b", "b");
        fs::create_dir(t.path().join("src/empty")).unwrap();
        fs::create_dir(t.path().join("dst")).unwrap();
        let copy = copy_into(&t.path().join("src"), &t.path().join("dst")).unwrap();
        assert_eq!(copy.path, t.path().join("dst/src"));
        assert_eq!(fs::read_to_string(copy.path.join("a")).unwrap(), "a");
        assert_eq!(
            fs::read_to_string(copy.path.join("deep/er/b")).unwrap(),
            "b"
        );
        assert!(copy.path.join("empty").is_dir());
        let again = copy_into(&t.path().join("src"), &t.path().join("dst")).unwrap();
        assert_eq!(again.path, t.path().join("dst/src 2"));
    }

    #[test]
    fn links_are_copied_as_links_and_not_followed() {
        let t = Temp::new();
        let secret = t.file("outside/secret", "do not copy");
        fs::create_dir_all(t.path().join("src")).unwrap();
        std::os::unix::fs::symlink(t.path().join("outside"), t.path().join("src/dir-link"))
            .unwrap();
        std::os::unix::fs::symlink(&secret, t.path().join("src/file-link")).unwrap();
        // A link that points back at the folder being copied.
        std::os::unix::fs::symlink(t.path().join("src"), t.path().join("src/loop")).unwrap();
        fs::create_dir(t.path().join("dst")).unwrap();
        let copy = copy_into(&t.path().join("src"), &t.path().join("dst")).unwrap();
        for name in ["dir-link", "file-link", "loop"] {
            let made = fs::symlink_metadata(copy.path.join(name)).unwrap();
            assert!(made.file_type().is_symlink(), "{name}");
        }
        assert_eq!(
            fs::read_link(copy.path.join("dir-link")).unwrap(),
            t.path().join("outside")
        );
        assert_eq!(fs::read_to_string(secret).unwrap(), "do not copy");
    }

    #[test]
    fn a_link_given_to_copy_is_copied_as_a_link() {
        let t = Temp::new();
        let target = t.file("target", "t");
        std::os::unix::fs::symlink(&target, t.path().join("link")).unwrap();
        fs::create_dir(t.path().join("dst")).unwrap();
        let copy = copy_into(&t.path().join("link"), &t.path().join("dst")).unwrap();
        assert!(fs::symlink_metadata(copy.path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn a_folder_is_not_copied_or_moved_into_itself() {
        let t = Temp::new();
        t.file("a/b/c", "");
        let a = t.path().join("a");
        for into in [a.clone(), a.join("b")] {
            assert!(copy_into(&a, &into).is_err());
            assert!(move_into(&a, &into).is_err());
        }
        assert!(a.join("b/c").exists());
        assert!(!a.join("a").exists());
    }

    #[test]
    fn a_failed_copy_leaves_nothing_behind() {
        let t = Temp::new();
        t.file("src/ok", "ok");
        let hidden = t.file("src/unreadable", "no");
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::File::open(&hidden).is_ok() {
            // Running as root: the mode means nothing.
            return;
        }
        fs::create_dir(t.path().join("dst")).unwrap();
        assert!(copy_into(&t.path().join("src"), &t.path().join("dst")).is_err());
        assert_eq!(fs::read_dir(t.path().join("dst")).unwrap().count(), 0);
        assert!(t.path().join("src/ok").exists());
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn modes_are_kept() {
        let t = Temp::new();
        let script = t.file("run.sh", "#!/bin/sh\n");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(t.path().join("dst")).unwrap();
        let copy = copy_into(&script, &t.path().join("dst")).unwrap();
        let mode = fs::metadata(copy.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn a_move_takes_the_item_and_never_replaces() {
        let t = Temp::new();
        let src = t.file("one/a.txt", "from one");
        t.file("two/a.txt", "already there");
        let moved = move_into(&src, &t.path().join("two")).unwrap();
        assert_eq!(moved, t.path().join("two/a 2.txt"));
        assert!(!src.exists());
        assert_eq!(
            fs::read_to_string(t.path().join("two/a.txt")).unwrap(),
            "already there"
        );
        assert_eq!(fs::read_to_string(moved).unwrap(), "from one");
    }

    #[test]
    fn a_move_of_a_folder_keeps_what_is_in_it() {
        let t = Temp::new();
        t.file("src/deep/x", "x");
        fs::create_dir(t.path().join("dst")).unwrap();
        let moved = move_into(&t.path().join("src"), &t.path().join("dst")).unwrap();
        assert_eq!(fs::read_to_string(moved.join("deep/x")).unwrap(), "x");
        assert!(!t.path().join("src").exists());
    }

    #[test]
    fn sizes_read_like_a_person_would() {
        assert_eq!(size_text(0), "0 B");
        assert_eq!(size_text(999), "999 B");
        assert_eq!(size_text(1000), "1.0 kB");
        assert_eq!(size_text(12_345), "12.3 kB");
        assert_eq!(size_text(4_100_000), "4.1 MB");
        assert_eq!(size_text(999_999), "1.0 MB");
        assert_eq!(size_text(3_000_000_000), "3.0 GB");
    }

    #[test]
    fn there_is_free_space_on_the_temp_disk() {
        assert!(free_space(&std::env::temp_dir()).is_some());
        assert!(free_space(Path::new("/definitely/not/here")).is_none());
    }
}
