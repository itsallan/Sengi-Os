//! Reading and writing the file: what may be opened as text, and a save
//! that cannot leave half a file behind.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// The most that is opened. More than this is a log or a database, not
/// something to edit in a window that keeps the whole text in memory.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// Why a file is not opened for editing.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    TooLarge(u64),
    NotText,
    NotAFile,
    Unreadable(String),
}

impl Refusal {
    /// What the window says in place of the text.
    pub fn say(&self, name: &str) -> String {
        match self {
            Refusal::TooLarge(size) => format!(
                "\"{name}\" is {} and Text Editor opens files up to {}. It is not opened, so nothing in it can be changed by mistake.",
                megabytes(*size),
                megabytes(MAX_BYTES),
            ),
            Refusal::NotText => format!(
                "\"{name}\" is not a plain text file: it is not valid UTF-8, or it has NUL bytes. Text Editor only opens UTF-8 text."
            ),
            Refusal::NotAFile => format!("\"{name}\" is not a file."),
            Refusal::Unreadable(why) => format!("\"{name}\" cannot be read: {why}"),
        }
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// The text of `path`, or why it is not opened.
pub fn load(path: &Path) -> Result<String, Refusal> {
    let meta = fs::metadata(path).map_err(|error| Refusal::Unreadable(why(&error)))?;
    if !meta.is_file() {
        return Err(Refusal::NotAFile);
    }
    if meta.len() > MAX_BYTES {
        return Err(Refusal::TooLarge(meta.len()));
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(path)
        .and_then(|file| file.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| Refusal::Unreadable(why(&error)))?;
    // The file may have grown since it was measured.
    if bytes.len() as u64 > MAX_BYTES {
        return Err(Refusal::TooLarge(bytes.len() as u64));
    }
    if bytes.contains(&0) {
        return Err(Refusal::NotText);
    }
    String::from_utf8(bytes).map_err(|_| Refusal::NotText)
}

fn why(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "it does not exist".to_string(),
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => error.to_string(),
    }
}

/// Writes `text` to `path`: to a file beside it first and then over it, so a
/// failure leaves the old file as it was. A link is followed, not replaced,
/// and the mode of an existing file is kept.
pub fn save(path: &Path, text: &str) -> Result<(), String> {
    let target = match fs::canonicalize(path) {
        Ok(real) => real,
        Err(error) if error.kind() == io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => return Err(why(&error)),
    };
    let dir = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let temp: PathBuf = dir.join(format!(".{name}.sengi-edit-{}", std::process::id()));
    let result = (|| -> io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        if let Ok(meta) = fs::metadata(&target) {
            fs::set_permissions(&temp, meta.permissions())?;
        }
        fs::rename(&temp, &target)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(why(&error));
    }
    Ok(())
}

/// Where a name typed for a new file goes: an absolute path as it is,
/// `~/` from the home folder, anything else in the home folder too.
pub fn place_named(typed: &str, home: &Path) -> PathBuf {
    let typed = typed.trim();
    if let Some(rest) = typed.strip_prefix("~/") {
        home.join(rest)
    } else if typed.starts_with('/') {
        PathBuf::from(typed)
    } else {
        home.join(typed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Dir(PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let path =
                std::env::temp_dir().join(format!("sengi-edit-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Dir(path)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn text_is_loaded_and_saved_as_it_is() {
        let d = Dir::new("roundtrip");
        let text = "one\r\ntwo \u{e9}\n\nlast line, no newline";
        let path = d.file("a.txt", text.as_bytes());
        assert_eq!(load(&path).unwrap(), text);
        save(&path, "changed\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "changed\n");
        // Nothing is left beside it.
        assert_eq!(fs::read_dir(&d.0).unwrap().count(), 1);
    }

    #[test]
    fn a_new_file_is_made_and_the_mode_of_an_old_one_kept() {
        let d = Dir::new("mode");
        let fresh = d.0.join("new.txt");
        save(&fresh, "x").unwrap();
        assert_eq!(fs::read_to_string(&fresh).unwrap(), "x");
        let script = d.file("run.sh", b"echo\n");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o750)).unwrap();
        save(&script, "echo 2\n").unwrap();
        assert_eq!(
            fs::metadata(&script).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[test]
    fn a_link_is_followed_not_replaced() {
        let d = Dir::new("link");
        let real = d.file("real.txt", b"old");
        let link = d.0.join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        save(&link, "new").unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
    }

    #[test]
    fn a_save_that_fails_says_why() {
        let d = Dir::new("fail");
        let error = save(&d.0.join("no/such/dir/a.txt"), "x").unwrap_err();
        assert!(!error.is_empty());
    }

    #[test]
    fn binary_files_are_refused() {
        let d = Dir::new("binary");
        assert_eq!(
            load(&d.file("bad.bin", &[0xff, 0xfe, b'a'])),
            Err(Refusal::NotText)
        );
        assert_eq!(load(&d.file("nul", b"a\0b")), Err(Refusal::NotText));
        assert_eq!(load(&d.0), Err(Refusal::NotAFile));
        assert!(matches!(
            load(&d.0.join("missing")),
            Err(Refusal::Unreadable(_))
        ));
    }

    #[test]
    fn a_file_over_the_limit_is_refused_and_one_at_it_is_not() {
        let d = Dir::new("big");
        let at = d.file("at.txt", &vec![b'a'; MAX_BYTES as usize]);
        assert_eq!(load(&at).unwrap().len() as u64, MAX_BYTES);
        let over = d.file("over.txt", &vec![b'a'; MAX_BYTES as usize + 1]);
        assert_eq!(load(&over), Err(Refusal::TooLarge(MAX_BYTES + 1)));
        let notice = Refusal::TooLarge(MAX_BYTES + 1).say("over.txt");
        assert!(notice.contains("5.0 MB"), "{notice}");
    }

    #[test]
    fn a_typed_name_finds_its_place() {
        let home = Path::new("/home/a");
        assert_eq!(
            place_named(" notes.txt ", home),
            Path::new("/home/a/notes.txt")
        );
        assert_eq!(place_named("~/x/y.md", home), Path::new("/home/a/x/y.md"));
        assert_eq!(place_named("/tmp/z", home), Path::new("/tmp/z"));
    }
}
