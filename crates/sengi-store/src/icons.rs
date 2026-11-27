//! The icons of the apps of Flathub. Flatpak unpacks them with the
//! appstream data, one folder a size, one PNG an app. They come with that
//! data and not before it, so what was missing is looked for again when the
//! data changes. Nothing of any app is shipped with the system.
//!
//! A picture is usually named after the app's ID. For some it is named
//! after the app's entry in the appstream data instead
//! (`org.kde.okular.desktop.png` for `org.kde.okular`), and only the data
//! itself says which.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sengi_apps::icons::Pixels;

use crate::catalog::valid_id;

/// Sizes in the order they are wanted: the cards draw 48 px, more on a
/// scaled screen, and shrinking the larger picture looks better.
const SIZES: [&str; 2] = ["128x128", "64x64"];

/// How often a file that is there but would not decode is tried again, for
/// a decoder that ran out of time on a busy machine.
const TRIES: u8 = 3;

/// The most of one appstream file that is read; Flathub's is about 50 MB.
const MAX_APPSTREAM: u64 = 256 * 1024 * 1024;

/// The longest piece between two `>` that is looked at.
const MAX_PIECE: u64 = 1024 * 1024;

/// App IDs and the names of their icon files, where the two differ.
pub type Aliases = HashMap<String, String>;

fn arches(data_home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(data_home.join("flatpak/appstream/flathub")) else {
        return Vec::new();
    };
    let mut arches: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    arches.sort();
    arches
}

/// Which appstream data Flatpak has unpacked for the user's Flathub: it
/// changes when new data arrives, and is `None` when there is none.
pub fn stamp(data_home: &Path) -> Option<String> {
    let found: Vec<String> = arches(data_home)
        .iter()
        .filter_map(|arch| {
            let active = arch.join("active");
            if !active.exists() {
                return None;
            }
            // `active` is a link to a folder named after the commit.
            let to = fs::read_link(&active).unwrap_or_else(|_| active.clone());
            Some(format!("{}={}", arch.display(), to.display()))
        })
        .collect();
    (!found.is_empty()).then(|| found.join(";"))
}

/// Whether Flatpak has unpacked appstream data for the user's Flathub: the
/// names and icons the list is worth showing with.
pub fn has_appstream(data_home: &Path) -> bool {
    stamp(data_home).is_some()
}

/// The icon files of `id` in the user's Flathub appstream data under
/// `data_home` (normally `~/.local/share`), the one wanted most first:
/// those named after the ID, and else the one `aliases` names.
pub fn find(data_home: &Path, id: &str, aliases: &Aliases) -> Vec<PathBuf> {
    if !valid_id(id) {
        return Vec::new();
    }
    let arches = arches(data_home);
    let named = |file: &str| -> Vec<PathBuf> {
        let mut found = Vec::new();
        for arch in &arches {
            for size in SIZES {
                let path = arch.join("active/icons").join(size).join(file);
                if path.is_file() {
                    found.push(path);
                }
            }
        }
        found
    };
    let direct = named(&format!("{id}.png"));
    match aliases.get(id) {
        Some(file) if direct.is_empty() => named(file),
        _ => direct,
    }
}

/// A file name and nothing that leads elsewhere.
fn plain_png(name: &str) -> bool {
    name.len() <= 255 && name.ends_with(".png") && !name.starts_with('.') && !name.contains('/')
}

/// The text of an element, given the piece that ends in its closing tag.
fn text_of(piece: &str) -> &str {
    piece.split('<').next().unwrap_or("").trim()
}

/// What the next piece of an appstream file holds the text of.
#[derive(Clone, Copy)]
enum Next {
    Other,
    Bundle,
    Icon,
}

/// Reads which apps of an appstream file have their cached icon under
/// another name than their ID. Only the tags needed are looked at, one at a
/// time, so the file is never held whole.
fn read_aliases(source: impl Read, found: &mut Aliases) {
    let mut reader = BufReader::new(source.take(MAX_APPSTREAM));
    let mut next = Next::Other;
    let (mut id, mut icon) = (None::<String>, None::<String>);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match (&mut reader).take(MAX_PIECE).read_until(b'>', &mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let piece = String::from_utf8_lossy(&bytes);
        match next {
            Next::Bundle => {
                // app/<id>/<arch>/<branch>
                let mut parts = text_of(&piece).split('/');
                if parts.next() == Some("app") {
                    id = parts.next().filter(|id| valid_id(id)).map(str::to_string);
                }
            }
            Next::Icon => {
                let name = text_of(&piece);
                if plain_png(name) {
                    icon = Some(name.to_string());
                }
            }
            Next::Other => {}
        }
        next = Next::Other;
        let Some(tag) = piece.rfind('<').map(|at| &piece[at..]) else {
            continue;
        };
        if tag.starts_with("<bundle") {
            next = Next::Bundle;
        } else if tag.starts_with("<icon") && tag.contains("\"cached\"") && !tag.ends_with("/>") {
            next = Next::Icon;
        } else if tag.starts_with("</component>") {
            if let (Some(id), Some(icon)) = (id.take(), icon.take()) {
                if icon.strip_suffix(".png") != Some(id.as_str()) {
                    found.insert(id, icon);
                }
            }
            (id, icon) = (None, None);
        }
    }
}

/// The aliases of the appstream data there is now.
pub fn aliases(data_home: &Path) -> Aliases {
    let mut found = Aliases::new();
    for arch in arches(data_home) {
        if let Ok(file) = fs::File::open(arch.join("active/appstream.xml")) {
            read_aliases(file, &mut found);
        }
    }
    found
}

enum Entry {
    Have(Pixels),
    /// None to be had from the data with this stamp, after so many tries.
    Missing {
        stamp: String,
        tries: u8,
    },
}

/// What is known of each app's icon.
#[derive(Default)]
pub struct Cache {
    entries: HashMap<String, Entry>,
    /// The aliases, and the stamp of the data they were read from.
    aliases: Option<(String, Arc<Aliases>)>,
}

impl Cache {
    /// The aliases of the data `stamp`, if they have been read.
    pub fn aliases(&self, stamp: &str) -> Option<Arc<Aliases>> {
        self.aliases
            .as_ref()
            .filter(|(at, _)| at == stamp)
            .map(|(_, aliases)| aliases.clone())
    }

    pub fn set_aliases(&mut self, stamp: &str, aliases: Arc<Aliases>) {
        self.aliases = Some((stamp.to_string(), aliases));
    }

    pub fn get(&self, id: &str) -> Option<&Pixels> {
        match self.entries.get(id) {
            Some(Entry::Have(pixels)) => Some(pixels),
            _ => None,
        }
    }

    /// Whether the icon of `id` is worth looking for in the data `stamp`.
    pub fn wanted(&self, id: &str, stamp: &str) -> bool {
        match self.entries.get(id) {
            None => true,
            Some(Entry::Have(_)) => false,
            Some(Entry::Missing { stamp: at, tries }) => at != stamp || *tries < TRIES,
        }
    }

    /// Notes what came of looking in the data `stamp`. `there` says a file
    /// was found, so a failure may be the moment's and not the file's.
    pub fn note(&mut self, id: &str, stamp: &str, pixels: Option<Pixels>, there: bool) {
        let entry = match pixels {
            Some(pixels) => Entry::Have(pixels),
            None => {
                let before = match self.entries.get(id) {
                    Some(Entry::Missing { stamp: at, tries }) if at == stamp => *tries,
                    _ => 0,
                };
                Entry::Missing {
                    stamp: stamp.to_string(),
                    tries: if there { before + 1 } else { TRIES },
                }
            }
        };
        self.entries.insert(id.to_string(), entry);
    }
}

#[cfg(test)]
pub mod testing {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};

    /// Lays out appstream data as Flatpak does: a folder named after the
    /// commit, `active` linking to it, and the icons by size.
    pub fn appstream(data_home: &Path, commit: &str, icons: &[(&str, &str)]) -> PathBuf {
        let arch = data_home.join("flatpak/appstream/flathub/x86_64");
        let folder = arch.join(commit);
        for (size, id) in icons {
            let dir = folder.join("icons").join(size);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("{id}.png")), b"png").unwrap();
        }
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("appstream.xml.gz"), b"x").unwrap();
        let active = arch.join("active");
        let _ = fs::remove_file(&active);
        symlink(commit, active).unwrap();
        folder
    }
}

#[cfg(test)]
mod tests {
    use super::testing::appstream;
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sengi-store-icons-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn pixels() -> Pixels {
        Pixels {
            size: 1,
            data: vec![1, 2, 3, 4],
        }
    }

    #[test]
    fn the_icons_are_found_under_the_active_appstream_larger_first() {
        let dir = temp("find");
        appstream(
            &dir,
            "abc",
            &[
                ("128x128", "a.b.Both"),
                ("64x64", "a.b.Both"),
                ("64x64", "a.b.Small"),
            ],
        );
        let active = dir.join("flatpak/appstream/flathub/x86_64/active/icons");
        let none = Aliases::new();
        assert_eq!(
            find(&dir, "a.b.Both", &none),
            [
                active.join("128x128/a.b.Both.png"),
                active.join("64x64/a.b.Both.png")
            ]
        );
        assert_eq!(
            find(&dir, "a.b.Small", &none),
            [active.join("64x64/a.b.Small.png")]
        );
        assert!(find(&dir, "a.b.None", &none).is_empty());
        assert!(find(&dir, "../../x.y", &none).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// In the shape of Flathub's data: an app whose pictures are named after
    /// its entry, one named after its ID, and one that points out of the
    /// folder.
    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<components version="0.8" origin="flatpak">
  <component type="desktop-application">
    <id>org.kde.okular.desktop</id>
    <name>Okular</name>
    <icon type="cached" width="64" height="64">org.kde.okular.desktop.png</icon>
    <icon type="remote" width="128" height="128">https://example.org/other.png</icon>
    <icon type="stock">okular</icon>
    <bundle type="flatpak" runtime="x">app/org.kde.okular/x86_64/stable</bundle>
  </component>
  <component type="desktop"><id>org.videolan.VLC</id><bundle type="flatpak">app/org.videolan.VLC/x86_64/stable</bundle><icon type="cached" width="64">org.videolan.VLC.png</icon></component>
  <component><id>a.b.Bad</id><icon type="cached">../../secret.png</icon>
    <bundle type="flatpak">app/a.b.Bad/x86_64/stable</bundle></component>
</components>"#;

    #[test]
    fn a_picture_named_after_the_entry_is_found_through_the_data() {
        let dir = temp("alias");
        let folder = appstream(&dir, "abc", &[("128x128", "org.kde.okular.desktop")]);
        fs::write(folder.join("appstream.xml"), XML).unwrap();
        let names = aliases(&dir);
        assert_eq!(names.len(), 1, "{names:?}");
        assert!(find(&dir, "org.kde.okular", &Aliases::new()).is_empty());
        let found = find(&dir, "org.kde.okular", &names);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("128x128/org.kde.okular.desktop.png"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_data_has_a_new_stamp() {
        let dir = temp("stamp");
        assert_eq!(stamp(&dir), None);
        assert!(!has_appstream(&dir));
        appstream(&dir, "one", &[]);
        let one = stamp(&dir).unwrap();
        assert!(has_appstream(&dir));
        appstream(&dir, "two", &[]);
        assert_ne!(stamp(&dir).unwrap(), one);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_was_missing_is_looked_for_again_in_new_data() {
        let mut cache = Cache::default();
        assert!(cache.wanted("a.b", "one"));
        cache.note("a.b", "one", None, false);
        assert!(!cache.wanted("a.b", "one"));
        assert!(cache.wanted("a.b", "two"));
        cache.note("a.b", "two", Some(pixels()), true);
        assert!(!cache.wanted("a.b", "three"));
        assert_eq!(cache.get("a.b"), Some(&pixels()));
    }

    #[test]
    fn a_file_that_would_not_decode_is_tried_a_few_times() {
        let mut cache = Cache::default();
        for _ in 0..TRIES {
            assert!(cache.wanted("a.b", "one"));
            cache.note("a.b", "one", None, true);
        }
        assert!(!cache.wanted("a.b", "one"));
        assert_eq!(cache.get("a.b"), None);
    }
}
