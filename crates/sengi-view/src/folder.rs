//! The pictures that sit beside the one that was opened, in the order Files
//! lists them: by name without regard to case, and numbers by their value.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// Whether the name says PNG or JPEG.
pub fn is_picture_name(path: &Path) -> bool {
    path.extension().is_some_and(|ext| {
        let ext = ext.to_string_lossy().to_ascii_lowercase();
        matches!(ext.as_str(), "png" | "jpg" | "jpeg")
    })
}

/// Names compared as people read them: "img2" before "img10".
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let run = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(d) = it.next_if(char::is_ascii_digit) {
                        digits.push(d);
                    }
                    digits.trim_start_matches('0').to_string()
                };
                let (da, db) = (run(&mut a), run(&mut b));
                let order = da.len().cmp(&db.len()).then_with(|| da.cmp(&db));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// The pictures in the folder of `path`, in order, and where `path` is among
/// them. `path` is in the list even if its name is not a picture's.
pub fn siblings(path: &Path) -> (Vec<PathBuf>, usize) {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|p| is_picture_name(p) && p.is_file())
                .collect()
        })
        .unwrap_or_default();
    if !files.iter().any(|p| p == path) {
        files.push(path.to_path_buf());
    }
    let key = |p: &PathBuf| {
        p.file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    files.sort_by(|a, b| natural(&key(a), &key(b)).then_with(|| a.cmp(b)));
    let at = files.iter().position(|p| p == path).unwrap_or(0);
    (files, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_sort_by_value_and_case_is_ignored() {
        let mut names = vec![
            "img10.png",
            "IMG2.png",
            "img1.png",
            "Beach.jpg",
            "apple.jpg",
            "img02.png",
        ];
        names.sort_by(|a, b| natural(a, b));
        assert_eq!(
            names,
            [
                "apple.jpg",
                "Beach.jpg",
                "img1.png",
                "IMG2.png",
                "img02.png",
                "img10.png"
            ]
        );
    }

    #[test]
    fn the_folder_gives_its_pictures_in_order_with_the_place_of_the_open_one() {
        let dir = std::env::temp_dir().join(format!("sengi-view-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub.png")).unwrap();
        for name in ["b.JPG", "a.png", "10.png", "2.jpeg", "notes.txt", "c.gif"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        let (files, at) = siblings(&dir.join("b.JPG"));
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        // A folder named like a picture is not one, and neither is a GIF.
        assert_eq!(names, ["2.jpeg", "10.png", "a.png", "b.JPG"]);
        assert_eq!(at, 3);
        // A file that is not in the list by name is still the one shown.
        let (files, at) = siblings(&dir.join("no-extension"));
        assert_eq!(files.len(), 5);
        assert_eq!(files[at], dir.join("no-extension"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
