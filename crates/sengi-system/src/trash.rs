//! The user's Trash, as the freedesktop.org Trash specification has it:
//! an item goes into `files/` of the trash folder and a record of where it
//! came from into `info/`. Only the user's own trash on the same file
//! system is used. If an item cannot be moved there it is not deleted: the
//! caller says so.
//!
//! Moving in is shared by the desktop and by Files; listing, restoring and
//! emptying are Files'. Nothing here decides what may be trashed, which is
//! for the program that asks.

use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Percent-encodes a path for a `.trashinfo` record: everything but
/// letters, digits, `-_.~` and the slash.
fn encode(path: &Path) -> String {
    let mut out = String::new();
    for &byte in path.as_os_str().as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The inverse of [`encode`]. A `%` that is not followed by two hex digits
/// is kept as it is.
pub fn decode(text: &str) -> PathBuf {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[at + 1]), hex(bytes[at + 2])) {
                out.push((high * 16 + low) as u8);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// The local time as `YYYY-MM-DDThh:mm:ss`, the form the record wants.
pub fn now() -> String {
    // SAFETY: time and localtime_r are given valid pointers, and tm is plain
    // data that localtime_r fills in.
    let tm = unsafe {
        let seconds = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&seconds, &mut tm);
        tm
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

/// Moves `path` into `trash`, recording `date` as the time of deletion.
/// Returns the name it has in the Trash. Nothing is replaced or deleted: a
/// move that fails leaves the item where it was.
pub fn trash(trash: &Path, path: &Path, date: &str) -> io::Result<String> {
    trash_in(trash, path, date, None)
}

/// As [`trash`], for a trash folder at the top of another volume (a USB
/// stick, say): the record's `Path=` is then relative to `topdir`, the top
/// of that volume, as the specification asks, so the item can be found again
/// wherever the volume is mounted next time.
pub fn trash_on_volume(trash: &Path, path: &Path, date: &str, topdir: &Path) -> io::Result<String> {
    trash_in(trash, path, date, Some(topdir))
}

fn trash_in(trash: &Path, path: &Path, date: &str, topdir: Option<&Path>) -> io::Result<String> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no name"))?
        .to_string();
    let files = trash.join("files");
    let info = trash.join("info");
    private_dir(&files)?;
    private_dir(&info)?;

    let recorded = topdir
        .and_then(|top| path.strip_prefix(top).ok())
        .unwrap_or(&path);
    let record = format!(
        "[Trash Info]\nPath={}\nDeletionDate={date}\n",
        encode(recorded)
    );
    // Writing the record with create_new is what claims the name, so two
    // trashers cannot pick the same one.
    for n in 1u32.. {
        let candidate = if n == 1 {
            name.clone()
        } else {
            format!("{name}.{n}")
        };
        let record_path = info.join(format!("{candidate}.trashinfo"));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&record_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let target = files.join(&candidate);
        if fs::symlink_metadata(&target).is_ok() {
            // A file with no record: leave it alone and try the next name.
            drop(file);
            let _ = fs::remove_file(&record_path);
            continue;
        }
        let moved = file
            .write_all(record.as_bytes())
            .and_then(|()| file.flush())
            .and_then(|()| fs::rename(&path, &target));
        if let Err(error) = moved {
            let _ = fs::remove_file(&record_path);
            return Err(error);
        }
        return Ok(candidate);
    }
    unreachable!("the counter does not run out")
}

/// What to tell the user when the move failed.
pub fn explain(error: &io::Error) -> String {
    // EXDEV: the Trash is on another disk than the item.
    if error.raw_os_error() == Some(libc::EXDEV) {
        "The Trash is on another disk, so this cannot be moved there.".to_string()
    } else {
        format!("Cannot move to the Trash: {}.", plain(error))
    }
}

/// The text of an error without the " (os error N)" Rust appends, which is
/// no use to the reader.
pub fn plain(error: &io::Error) -> String {
    // The ones a person meets with a USB stick or a full disk, said so that
    // they know what to do.
    match error.raw_os_error() {
        Some(libc::ENOSPC) => return "There is no space left on the disk".to_string(),
        Some(libc::EDQUOT) => return "The space allowed on the disk is used up".to_string(),
        Some(libc::EROFS) => return "The disk is read-only".to_string(),
        Some(libc::EIO) => return "The disk gave an error, it may have been unplugged".to_string(),
        Some(libc::ENODEV | libc::ENXIO) => {
            return "The disk is not there any more, it may have been unplugged".to_string()
        }
        _ => {}
    }
    let text = error.to_string();
    text.split(" (os error").next().unwrap_or(&text).to_string()
}

/// `name` for n = 1, then "name 2", "name 3" and so on, with the number put
/// before the extension: "photo.jpg", "photo 2.jpg". A leading dot is not an
/// extension, and ".tar.gz" and its kin count as one.
pub fn numbered_name(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_string();
    }
    let (stem, extension) = split_extension(name);
    format!("{stem} {n}{extension}")
}

fn split_extension(name: &str) -> (&str, &str) {
    let Some(dot) = name
        .rfind('.')
        .filter(|&dot| dot > 0 && dot + 1 < name.len())
    else {
        return (name, "");
    };
    let (stem, extension) = name.split_at(dot);
    if let Some(inner) = stem.strip_suffix(".tar") {
        if !inner.is_empty() {
            return (inner, &name[inner.len()..]);
        }
    }
    (stem, extension)
}

/// One item in the Trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// What it is called in `files/`.
    pub name: String,
    /// Where it was before, if the record says and says an absolute path.
    pub original: Option<PathBuf>,
    /// When it was trashed, as the record has it.
    pub date: String,
    pub folder: bool,
}

/// `topdir` is the top of the volume a per-volume trash belongs to: a
/// relative `Path=` is taken from there. Without it only an absolute one is
/// believed.
fn read_record(info: &Path, name: &str, topdir: Option<&Path>) -> (Option<PathBuf>, String) {
    let Ok(text) = fs::read_to_string(info.join(format!("{name}.trashinfo"))) else {
        return (None, String::new());
    };
    let mut original = None;
    let mut date = String::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Path=") {
            let path = decode(value.trim());
            original = if path.is_absolute() {
                Some(path)
            } else {
                topdir.map(|top| top.join(path))
            };
        } else if let Some(value) = line.strip_prefix("DeletionDate=") {
            date = value.trim().to_string();
        }
    }
    (original, date)
}

/// What is in the Trash. A Trash that does not exist yet is an empty one.
pub fn list(trash: &Path) -> io::Result<Vec<Item>> {
    list_in(trash, None)
}

/// As [`list`] for the trash of a volume whose top is `topdir`.
pub fn list_on_volume(trash: &Path, topdir: &Path) -> io::Result<Vec<Item>> {
    list_in(trash, Some(topdir))
}

fn list_in(trash: &Path, topdir: Option<&Path>) -> io::Result<Vec<Item>> {
    let entries = match fs::read_dir(trash.join("files")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let info = trash.join("info");
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let (original, date) = read_record(&info, &name, topdir);
        // A link to a folder is not a folder here: it is the link that is in
        // the Trash.
        let folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
        items.push(Item {
            name,
            original,
            date,
            folder,
        });
    }
    Ok(items)
}

/// A name that is a single plain component of `files/`.
fn plain_name(name: &str) -> io::Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a name"));
    }
    Ok(())
}

/// Puts the item back where it came from, and returns where it went. Folders
/// that are gone are made again. If something is there now, the item gets a
/// numbered name next to it; nothing is replaced.
pub fn restore(trash: &Path, name: &str) -> io::Result<PathBuf> {
    restore_in(trash, name, None)
}

/// As [`restore`] for the trash of a volume whose top is `topdir`.
pub fn restore_on_volume(trash: &Path, name: &str, topdir: &Path) -> io::Result<PathBuf> {
    restore_in(trash, name, Some(topdir))
}

fn restore_in(trash: &Path, name: &str, topdir: Option<&Path>) -> io::Result<PathBuf> {
    plain_name(name)?;
    let source = trash.join("files").join(name);
    fs::symlink_metadata(&source)?;
    let (original, _) = read_record(&trash.join("info"), name, topdir);
    let original = original.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "there is no record of where it came from",
        )
    })?;
    let parent = original
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no place to put it"))?;
    let file_name = original
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no name"))?
        .to_string();
    fs::create_dir_all(parent)?;
    let target = (1u32..)
        .map(|n| parent.join(numbered_name(&file_name, n)))
        .find(|candidate| fs::symlink_metadata(candidate).is_err())
        .expect("the counter does not run out");
    fs::rename(&source, &target)?;
    let _ = fs::remove_file(trash.join("info").join(format!("{name}.trashinfo")));
    Ok(target)
}

/// Deletes one item of the Trash for good, with its record.
pub fn purge(trash: &Path, name: &str) -> io::Result<()> {
    plain_name(name)?;
    let source = trash.join("files").join(name);
    // remove_dir_all does not follow links out of the folder.
    if fs::symlink_metadata(&source)?.is_dir() {
        fs::remove_dir_all(&source)?;
    } else {
        fs::remove_file(&source)?;
    }
    match fs::remove_file(trash.join("info").join(format!("{name}.trashinfo"))) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Deletes everything in the Trash for good. Returns how many items went; an
/// item that cannot be deleted stays, and the first such error is returned
/// after the rest are done.
pub fn empty(trash: &Path) -> io::Result<usize> {
    let mut gone = 0;
    let mut first_error = None;
    for item in list(trash)? {
        match purge(trash, &item.name) {
            Ok(()) => gone += 1,
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    // Records that name nothing are of no use to anyone.
    if let Ok(records) = fs::read_dir(trash.join("info")) {
        for record in records.flatten() {
            let path = record.path();
            if let Some(stem) = path.file_stem() {
                if fs::symlink_metadata(trash.join("files").join(stem)).is_err() {
                    let _ = fs::remove_file(&path);
                }
            }
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(gone),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn the_record_names_the_old_place_and_the_time() {
        let root = TempDir::new();
        let desktop = root.path().join("Desktop");
        fs::create_dir_all(desktop.join("my folder")).unwrap();
        fs::write(desktop.join("my folder/inside"), "x").unwrap();
        let bin = root.path().join("Trash");

        let name = trash(&bin, &desktop.join("my folder"), "2026-11-25T10:20:30").unwrap();
        assert_eq!(name, "my folder");
        assert!(!desktop.join("my folder").exists());
        assert!(bin.join("files/my folder/inside").is_file());
        let record = fs::read_to_string(bin.join("info/my folder.trashinfo")).unwrap();
        assert_eq!(
            record,
            format!(
                "[Trash Info]\nPath={}/Desktop/my%20folder\nDeletionDate=2026-11-25T10:20:30\n",
                root.path().display()
            )
        );
    }

    #[test]
    fn the_same_name_twice_gets_a_number() {
        let root = TempDir::new();
        let desktop = root.path().join("Desktop");
        fs::create_dir_all(&desktop).unwrap();
        let bin = root.path().join("Trash");
        for expected in ["a.txt", "a.txt.2", "a.txt.3"] {
            fs::write(desktop.join("a.txt"), expected).unwrap();
            assert_eq!(trash(&bin, &desktop.join("a.txt"), "d").unwrap(), expected);
        }
        assert_eq!(
            fs::read_to_string(bin.join("files/a.txt.2")).unwrap(),
            "a.txt.2"
        );
        assert!(bin.join("info/a.txt.3.trashinfo").is_file());
    }

    #[test]
    fn a_failed_move_leaves_the_item_and_no_record() {
        let root = TempDir::new();
        let bin = root.path().join("Trash");
        let error = trash(&bin, &root.path().join("missing"), "d").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(fs::read_dir(bin.join("info")).unwrap().count(), 0);
    }

    #[test]
    fn a_volume_trash_records_a_path_from_the_top_of_the_volume() {
        let root = TempDir::new();
        let top = root.path().join("stick");
        fs::create_dir_all(&top).unwrap();
        fs::write(top.join("a b.txt"), "x").unwrap();
        let bin = top.join(".Trash-1000");
        trash_on_volume(&bin, &top.join("a b.txt"), "d", &top).unwrap();
        let record = fs::read_to_string(bin.join("info/a b.txt.trashinfo")).unwrap();
        assert!(record.contains("Path=a%20b.txt\n"), "{record}");
        let items = list_on_volume(&bin, &top).unwrap();
        assert_eq!(
            items[0].original.as_deref(),
            Some(top.join("a b.txt").as_path())
        );
        // Without the top of the volume a relative record is not believed.
        assert_eq!(list(&bin).unwrap()[0].original, None);
        let back = restore_on_volume(&bin, "a b.txt", &top).unwrap();
        assert_eq!(back, top.join("a b.txt"));
        assert!(back.exists());
    }

    #[test]
    fn a_full_or_read_only_disk_is_said_plainly() {
        let full = io::Error::from_raw_os_error(libc::ENOSPC);
        assert_eq!(plain(&full), "There is no space left on the disk");
        let locked = io::Error::from_raw_os_error(libc::EROFS);
        assert_eq!(plain(&locked), "The disk is read-only");
    }

    #[test]
    fn odd_characters_are_encoded_and_decoded() {
        assert_eq!(encode(Path::new("/h/a b/é#")), "/h/a%20b/%C3%A9%23");
        assert_eq!(decode("/h/a%20b/%C3%A9%23"), Path::new("/h/a b/é#"));
        assert_eq!(decode("/h/100%"), Path::new("/h/100%"));
        assert_eq!(decode("/h/%zz%4"), Path::new("/h/%zz%4"));
    }

    #[test]
    fn the_date_is_the_spec_form() {
        let shape = now();
        let bytes = shape.as_bytes();
        assert_eq!(bytes.len(), 19);
        assert_eq!((bytes[4], bytes[7], bytes[10]), (b'-', b'-', b'T'));
        assert_eq!((bytes[13], bytes[16]), (b':', b':'));
    }

    #[test]
    fn numbers_go_before_the_extension() {
        assert_eq!(numbered_name("photo.jpg", 1), "photo.jpg");
        assert_eq!(numbered_name("photo.jpg", 2), "photo 2.jpg");
        assert_eq!(numbered_name("folder", 3), "folder 3");
        assert_eq!(numbered_name(".bashrc", 2), ".bashrc 2");
        assert_eq!(numbered_name("trailing.", 2), "trailing. 2");
        assert_eq!(numbered_name("a.tar.gz", 2), "a 2.tar.gz");
        assert_eq!(numbered_name(".tar.gz", 2), ".tar 2.gz");
    }

    #[test]
    fn what_was_trashed_is_listed_and_put_back() {
        let root = TempDir::new();
        let docs = root.path().join("Documents");
        fs::create_dir_all(docs.join("sub")).unwrap();
        fs::write(docs.join("a.txt"), "a").unwrap();
        let bin = root.path().join("Trash");
        assert!(list(&bin).unwrap().is_empty());
        trash(&bin, &docs.join("a.txt"), "2026-01-02T03:04:05").unwrap();
        trash(&bin, &docs.join("sub"), "2026-01-02T03:04:06").unwrap();

        let mut items = list(&bin).unwrap();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "a.txt");
        assert_eq!(
            items[0].original.as_deref(),
            Some(docs.join("a.txt").as_path())
        );
        assert_eq!(items[0].date, "2026-01-02T03:04:05");
        assert!(!items[0].folder && items[1].folder);

        assert_eq!(restore(&bin, "a.txt").unwrap(), docs.join("a.txt"));
        assert_eq!(fs::read_to_string(docs.join("a.txt")).unwrap(), "a");
        assert!(!bin.join("info/a.txt.trashinfo").exists());
        assert_eq!(list(&bin).unwrap().len(), 1);
    }

    #[test]
    fn restoring_never_replaces_and_remakes_folders() {
        let root = TempDir::new();
        let docs = root.path().join("Documents/deep");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("a.txt"), "old").unwrap();
        let bin = root.path().join("Trash");
        trash(&bin, &docs.join("a.txt"), "d").unwrap();
        fs::write(docs.join("a.txt"), "new").unwrap();
        let back = restore(&bin, "a.txt").unwrap();
        assert_eq!(back, docs.join("a 2.txt"));
        assert_eq!(fs::read_to_string(docs.join("a.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(&back).unwrap(), "old");

        fs::write(docs.join("b"), "b").unwrap();
        trash(&bin, &docs.join("b"), "d").unwrap();
        fs::remove_dir_all(root.path().join("Documents")).unwrap();
        let back = restore(&bin, "b").unwrap();
        assert_eq!(back, docs.join("b"));
    }

    #[test]
    fn restoring_without_a_record_says_so_and_keeps_the_item() {
        let root = TempDir::new();
        let bin = root.path().join("Trash");
        fs::create_dir_all(bin.join("files")).unwrap();
        fs::write(bin.join("files/orphan"), "x").unwrap();
        let error = restore(&bin, "orphan").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(bin.join("files/orphan").exists());
        assert!(restore(&bin, "../orphan").is_err());
        assert!(restore(&bin, "missing").is_err());
    }

    #[test]
    fn emptying_deletes_items_records_and_strays() {
        let root = TempDir::new();
        let home = root.path().join("h");
        fs::create_dir_all(home.join("d/inner")).unwrap();
        fs::write(home.join("d/inner/x"), "x").unwrap();
        fs::write(home.join("f"), "f").unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, "keep").unwrap();
        std::os::unix::fs::symlink(&outside, home.join("d/link")).unwrap();
        let bin = root.path().join("Trash");
        trash(&bin, &home.join("d"), "d").unwrap();
        trash(&bin, &home.join("f"), "d").unwrap();
        fs::write(bin.join("info/stray.trashinfo"), "").unwrap();

        assert_eq!(empty(&bin).unwrap(), 2);
        assert_eq!(fs::read_dir(bin.join("files")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(bin.join("info")).unwrap().count(), 0);
        assert_eq!(fs::read_to_string(outside).unwrap(), "keep");
        assert_eq!(empty(&bin).unwrap(), 0);
    }
}
