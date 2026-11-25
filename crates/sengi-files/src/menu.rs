//! What the right-click menu offers, from what is selected and where the
//! window is. Each row has an id that the window hands back when it is
//! chosen; rows that cannot be used now are still shown, but dimmed.

use crate::session::Session;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: &'static str,
    pub label: &'static str,
    pub enabled: bool,
    /// A line between groups; it has no id.
    pub separator: bool,
    /// A switch that is on.
    pub checked: bool,
}

fn row(id: &'static str, label: &'static str, enabled: bool) -> Row {
    Row {
        id,
        label,
        enabled,
        separator: false,
        checked: false,
    }
}

fn line() -> Row {
    Row {
        id: "",
        label: "",
        enabled: false,
        separator: true,
        checked: false,
    }
}

/// The menu for a click on an item (the item is selected by then) or, with
/// `on_item` false, on the empty space.
pub fn rows(session: &Session, on_item: bool) -> Vec<Row> {
    let one = session.only_selected().is_some();
    let some = !session.selected.is_empty();
    if session.in_trash() {
        return if on_item {
            vec![row("restore", "Restore", some)]
        } else {
            vec![
                row("empty-trash", "Empty Trash", !session.entries.is_empty()),
                line(),
                Row {
                    checked: session.show_hidden,
                    ..row("hidden", "Show Hidden", true)
                },
            ]
        };
    }
    if on_item {
        let rename = one
            && session
                .only_selected()
                .is_some_and(|at| session.can_rename(at).is_ok());
        let changeable = some && session.changeable_selection().is_ok();
        vec![
            row("open", "Open", one),
            row("rename", "Rename", rename),
            line(),
            row("copy", "Copy", some),
            row("cut", "Cut", changeable),
            line(),
            row("trash", "Move to Trash", changeable),
        ]
    } else {
        let here = session.can_change_here().is_ok();
        vec![
            row("new-folder", "New Folder", here),
            row("new-file", "New File", here),
            line(),
            row("paste", "Paste", here && session.clipboard.is_some()),
            line(),
            Row {
                checked: session.show_hidden,
                ..row("hidden", "Show Hidden", true)
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::tests::Temp;
    use crate::fsops::Entry;
    use crate::session::{Clipboard, Place};

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            folder: false,
            link: false,
            size: 0,
            modified: 0,
            original: None,
            deleted: String::new(),
        }
    }

    fn ids(rows: &[Row]) -> Vec<&'static str> {
        rows.iter().filter(|r| !r.separator).map(|r| r.id).collect()
    }

    fn enabled(rows: &[Row], id: &str) -> bool {
        rows.iter().find(|r| r.id == id).unwrap().enabled
    }

    fn home_session(t: &Temp) -> Session {
        let home = t.path().join("home");
        std::fs::create_dir_all(home.join("stuff")).unwrap();
        let mut s = Session::new(
            home.clone(),
            home.join(".local/share/Trash"),
            Place::Folder(home.join("stuff")),
        );
        s.entries = vec![entry("a"), entry("b")];
        s
    }

    #[test]
    fn an_item_offers_open_rename_copy_cut_and_trash() {
        let t = Temp::new();
        let mut s = home_session(&t);
        s.click(0, false, false);
        let rows = rows(&s, true);
        assert_eq!(ids(&rows), ["open", "rename", "copy", "cut", "trash"]);
        assert!(rows.iter().all(|r| r.enabled || r.separator));
        // Two items: no Open and no Rename, the rest still work.
        s.click(1, true, false);
        let rows = super::rows(&s, true);
        assert!(!enabled(&rows, "open") && !enabled(&rows, "rename"));
        assert!(enabled(&rows, "copy") && enabled(&rows, "trash"));
    }

    #[test]
    fn away_from_home_an_item_can_be_copied_and_nothing_else_changed() {
        let t = Temp::new();
        let mut s = home_session(&t);
        s.place = Place::Folder(t.path().to_path_buf());
        s.click(0, false, false);
        let rows = rows(&s, true);
        assert!(enabled(&rows, "copy") && enabled(&rows, "open"));
        assert!(!enabled(&rows, "cut") && !enabled(&rows, "trash") && !enabled(&rows, "rename"));
        let empty = super::rows(&s, false);
        assert!(!enabled(&empty, "new-folder") && !enabled(&empty, "paste"));
    }

    #[test]
    fn empty_space_offers_new_paste_and_the_hidden_switch() {
        let t = Temp::new();
        let mut s = home_session(&t);
        let rows = rows(&s, false);
        assert_eq!(ids(&rows), ["new-folder", "new-file", "paste", "hidden"]);
        assert!(enabled(&rows, "new-folder"));
        assert!(!enabled(&rows, "paste"));
        s.clipboard = Some(Clipboard {
            items: vec!["/x".into()],
            cut: false,
        });
        s.show_hidden = true;
        let rows = super::rows(&s, false);
        assert!(enabled(&rows, "paste"));
        assert!(rows.iter().find(|r| r.id == "hidden").unwrap().checked);
    }

    #[test]
    fn the_trash_offers_restore_and_emptying() {
        let t = Temp::new();
        let mut s = home_session(&t);
        s.place = Place::Trash;
        s.click(0, false, false);
        let rows = rows(&s, true);
        assert_eq!(ids(&rows), ["restore"]);
        assert!(enabled(&rows, "restore"));
        let empty = super::rows(&s, false);
        assert_eq!(ids(&empty), ["empty-trash", "hidden"]);
        s.entries.clear();
        assert!(!enabled(&super::rows(&s, false), "empty-trash"));
    }
}
