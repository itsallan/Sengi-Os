//! Moving to the Trash, as the freedesktop.org Trash specification has it:
//! the item goes into `files/` of the user's trash folder and a record of
//! where it came from into `info/`. Only the user's own trash on the same
//! file system is used. If the item cannot be moved there, it is not
//! deleted: the caller says so.

use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use sengi_apps::Env;

/// `$XDG_DATA_HOME/Trash`, which is `~/.local/share/Trash` unless set.
pub fn trash_dir(env: &Env) -> PathBuf {
    env.data_home.join("Trash")
}

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
    format_date(&tm)
}

fn format_date(tm: &libc::tm) -> String {
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

    let record = format!(
        "[Trash Info]\nPath={}\nDeletionDate={date}\n",
        encode(&path)
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
    // EXDEV: the Trash is on another disk than the Desktop.
    if error.raw_os_error() == Some(libc::EXDEV) {
        "The Trash is on another disk, so this cannot be moved there.".to_string()
    } else {
        let text = error.to_string();
        // Rust appends " (os error N)", which is no use to the reader.
        let text = text.split(" (os error").next().unwrap_or(&text);
        format!("Cannot move to the Trash: {text}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;

    #[test]
    fn the_record_names_the_old_place_and_the_time() {
        let root = temp_dir("trash");
        let desktop = root.join("Desktop");
        fs::create_dir_all(desktop.join("my folder")).unwrap();
        fs::write(desktop.join("my folder/inside"), "x").unwrap();
        let bin = root.join("Trash");

        let name = trash(&bin, &desktop.join("my folder"), "2026-11-25T10:20:30").unwrap();
        assert_eq!(name, "my folder");
        assert!(!desktop.join("my folder").exists());
        assert!(bin.join("files/my folder/inside").is_file());
        let record = fs::read_to_string(bin.join("info/my folder.trashinfo")).unwrap();
        assert_eq!(
            record,
            format!(
                "[Trash Info]\nPath={}/Desktop/my%20folder\nDeletionDate=2026-11-25T10:20:30\n",
                root.display()
            )
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_same_name_twice_gets_a_number() {
        let root = temp_dir("trash-twice");
        let desktop = root.join("Desktop");
        fs::create_dir_all(&desktop).unwrap();
        let bin = root.join("Trash");
        for expected in ["a.txt", "a.txt.2", "a.txt.3"] {
            fs::write(desktop.join("a.txt"), expected).unwrap();
            assert_eq!(trash(&bin, &desktop.join("a.txt"), "d").unwrap(), expected);
        }
        assert_eq!(
            fs::read_to_string(bin.join("files/a.txt.2")).unwrap(),
            "a.txt.2"
        );
        assert!(bin.join("info/a.txt.3.trashinfo").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_failed_move_leaves_the_item_and_no_record() {
        let root = temp_dir("trash-fail");
        let bin = root.join("Trash");
        let error = trash(&bin, &root.join("missing"), "d").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(fs::read_dir(bin.join("info")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn odd_characters_are_encoded() {
        assert_eq!(encode(Path::new("/h/a b/é#")), "/h/a%20b/%C3%A9%23");
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
    fn the_trash_is_under_the_data_home() {
        let env = Env::from_vars(|name| match name {
            "HOME" => Some("/home/ada".into()),
            _ => None,
        });
        assert_eq!(trash_dir(&env), Path::new("/home/ada/.local/share/Trash"));
    }
}
