//! Where Flatpak keeps the icons of the apps of a remote: it unpacks them
//! with the appstream data, one folder a size, one PNG an app.

use std::fs;
use std::path::{Path, PathBuf};

use crate::catalog::valid_id;

/// Sizes in the order they are wanted: the app list draws 40 px.
const SIZES: [&str; 2] = ["64x64", "128x128"];

/// The icon file of `id` in the user's Flathub appstream data under
/// `data_home` (normally `~/.local/share`), if there is one.
pub fn find(data_home: &Path, id: &str) -> Option<PathBuf> {
    if !valid_id(id) {
        return None;
    }
    let remote = data_home.join("flatpak/appstream/flathub");
    let mut arches: Vec<PathBuf> = fs::read_dir(remote)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    arches.sort();
    for arch in arches {
        for size in SIZES {
            let path = arch
                .join("active/icons")
                .join(size)
                .join(format!("{id}.png"));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn the_icon_is_found_under_the_active_appstream() {
        let dir = std::env::temp_dir().join(format!("sengi-store-icons-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let base = dir.join("flatpak/appstream/flathub/x86_64/active/icons");
        touch(&base.join("128x128/a.b.Big.png"));
        touch(&base.join("128x128/a.b.Both.png"));
        touch(&base.join("64x64/a.b.Both.png"));
        assert_eq!(
            find(&dir, "a.b.Big"),
            Some(base.join("128x128/a.b.Big.png"))
        );
        assert_eq!(
            find(&dir, "a.b.Both"),
            Some(base.join("64x64/a.b.Both.png"))
        );
        assert_eq!(find(&dir, "a.b.None"), None);
        assert_eq!(find(&dir, "../../x.y"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_appstream_means_no_icons() {
        assert_eq!(find(Path::new("/nonexistent"), "a.b.C"), None);
    }
}
