//! The folder shown by the Open and Save As dialog.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// The most rows the list is given. A folder with more is searched by typing
/// the name.
pub const MAX_ROWS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub folder: bool,
}

/// What a folder holds: folders first, then files, each by name without
/// regard to case. Names that start with a dot are left out. The second
/// value is how many entries did not fit.
pub fn list(folder: &Path) -> io::Result<(Vec<Entry>, usize)> {
    let mut entries: Vec<Entry> = fs::read_dir(folder)?
        .flatten()
        .filter_map(|item| {
            // A name that is not UTF-8 cannot be typed back, so it is not shown.
            let name = item.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            // A link counts as what it leads to.
            let folder = item.path().is_dir();
            Some(Entry { name, folder })
        })
        .collect();
    entries.sort_by_cached_key(|e| (!e.folder, e.name.to_lowercase(), e.name.clone()));
    let left_out = entries.len().saturating_sub(MAX_ROWS);
    entries.truncate(MAX_ROWS);
    Ok((entries, left_out))
}

/// Where a path typed in the field leads: an absolute path as it is, `~` and
/// `~/` from the home folder, anything else from the folder shown.
pub fn resolve(typed: &str, folder: &Path, home: &Path) -> PathBuf {
    let typed = typed.trim();
    let joined = if typed == "~" {
        home.to_path_buf()
    } else if let Some(rest) = typed.strip_prefix("~/") {
        home.join(rest)
    } else {
        folder.join(typed)
    };
    tidy(&joined)
}

/// `path` without `.` and `..` steps, worked out from the words alone.
fn tidy(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// A folder as it is shown: `~` for the home folder.
pub fn shown(folder: &Path, home: &Path) -> String {
    match folder.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => folder.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_come_first_and_hidden_names_are_left_out() {
        let dir = std::env::temp_dir().join(format!("sengi-edit-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("zeta")).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        for name in ["b.txt", "A.txt", ".profile"] {
            fs::write(dir.join(name), "x").unwrap();
        }
        let (entries, left_out) = list(&dir).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zeta", "A.txt", "b.txt"]);
        assert!(entries[0].folder && !entries[1].folder);
        assert_eq!(left_out, 0);
        for i in 0..MAX_ROWS {
            fs::write(dir.join(format!("n{i}")), "x").unwrap();
        }
        let (entries, left_out) = list(&dir).unwrap();
        assert_eq!((entries.len(), left_out), (MAX_ROWS, 3));
        assert!(list(&dir.join("missing")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_typed_path_finds_its_place() {
        let home = Path::new("/home/a");
        let folder = Path::new("/home/a/docs");
        let to = |typed| resolve(typed, folder, home);
        assert_eq!(to(" notes.txt "), Path::new("/home/a/docs/notes.txt"));
        assert_eq!(to("~"), home);
        assert_eq!(to("~/x/y.md"), Path::new("/home/a/x/y.md"));
        assert_eq!(to("/tmp/z"), Path::new("/tmp/z"));
        assert_eq!(to(".."), home);
        assert_eq!(to("./../y"), Path::new("/home/a/y"));
        assert_eq!(to("/.."), Path::new("/"));
    }

    #[test]
    fn the_home_folder_is_shown_as_a_tilde() {
        let home = Path::new("/home/a");
        assert_eq!(shown(home, home), "~");
        assert_eq!(shown(Path::new("/home/a/x/y"), home), "~/x/y");
        assert_eq!(shown(Path::new("/etc"), home), "/etc");
    }
}
