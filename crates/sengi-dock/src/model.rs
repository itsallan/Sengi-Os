//! What the dock shows and what a click does. No I/O happens here apart from
//! re-reading the desktop entries when a window from an unknown app appears.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use crate::desktop::{Apps, Entry};
use crate::icons;
use crate::matching;
use crate::windows::Window;

/// Key of the search button. No desktop-file ID can be empty.
pub const LAUNCHER_KEY: &str = "";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The search button. Always first.
    Launcher,
    /// Listed in the config. Always present, in config order.
    Pinned,
    /// Has windows but is not pinned. Shown after the pinned apps while it runs.
    Running,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    /// Drawn by the UI from the design tokens.
    Search,
    /// Drawn by the UI from the design tokens.
    Terminal,
    /// An `Icon=` value: a theme icon name or an absolute path. May be empty.
    Named(String),
}

/// Apps whose icon we draw ourselves rather than load from their package.
const OWN_ICONS: &[(&str, Icon)] = &[("foot", Icon::Terminal), ("footclient", Icon::Terminal)];

/// Key prefix for windows that have no app_id. Each gets an item of its own,
/// since nothing says they belong together. An app_id cannot collide with
/// it: keys made from app_ids are trimmed, so they never start with a space.
const NAMELESS: &str = " window:";

/// The part of a window the dock cares about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowRef {
    pub id: u32,
    pub activated: bool,
    pub minimized: bool,
    pub focus_seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    /// Stable identity: the lowercased desktop-file ID, or the app_id for a
    /// window that matches no installed app.
    pub key: String,
    pub name: String,
    pub icon: Icon,
    /// Shown on the fallback tile when the icon cannot be loaded.
    pub letter: String,
    /// Whether clicking can start the app: it has a desktop entry.
    pub launchable: bool,
    /// The app's windows, oldest first.
    pub windows: Vec<WindowRef>,
}

impl Item {
    pub fn running(&self) -> bool {
        !self.windows.is_empty()
    }

    pub fn focused(&self) -> bool {
        self.windows.iter().any(|w| w.activated)
    }

    pub fn window_count(&self) -> usize {
        self.windows.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Primary,
    /// A middle click, or a click with Ctrl or Shift held.
    Secondary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    OpenLauncher,
    /// Start the app with this key.
    Launch(String),
    /// Focus this window, restoring it first if it is minimised.
    Activate(u32),
    Minimize(u32),
    Nothing,
}

/// Decides what a click on a dock item does.
///
/// A primary click starts an app that is not running and brings forward one
/// that is: its most recently used window. On the app that already has
/// focus it minimises the window, or, when the app has several windows,
/// moves to the one used longest ago, so repeated clicks walk through them
/// all. A secondary click always starts a new instance.
pub fn on_click(item: &Item, button: Button) -> Action {
    if item.kind == Kind::Launcher {
        return Action::OpenLauncher;
    }
    let launch = || {
        if item.launchable {
            Action::Launch(item.key.clone())
        } else {
            Action::Nothing
        }
    };
    if button == Button::Secondary {
        return launch();
    }

    let Some(focused) = item.windows.iter().find(|w| w.activated) else {
        return match item.windows.iter().max_by_key(|w| (w.focus_seq, w.id)) {
            Some(recent) => Action::Activate(recent.id),
            None => launch(),
        };
    };
    match item
        .windows
        .iter()
        .filter(|w| w.id != focused.id)
        .min_by_key(|w| (w.focus_seq, w.id))
    {
        Some(next) => Action::Activate(next.id),
        None => Action::Minimize(focused.id),
    }
}

/// How long a started app is given to show a window before another click
/// may start it again.
pub const LAUNCH_WAIT: Duration = Duration::from_secs(5);

/// The same for the search launcher, which never shows up as a window: long
/// enough to swallow a double click, short enough not to be noticed.
pub const LAUNCHER_WAIT: Duration = Duration::from_millis(500);

/// Launches that have been started and have not yet produced a window.
///
/// An app takes a second or more to map its first window, and until then
/// its item still looks idle, so a double click, or an impatient second
/// click, would start it twice.
#[derive(Default)]
pub struct Launches {
    started: HashMap<String, Instant>,
}

impl Launches {
    fn wait(key: &str) -> Duration {
        if key == LAUNCHER_KEY {
            LAUNCHER_WAIT
        } else {
            LAUNCH_WAIT
        }
    }

    /// True while the item with this key is waiting for its window.
    pub fn pending(&self, key: &str, now: Instant) -> bool {
        self.started
            .get(key)
            .is_some_and(|&at| now.saturating_duration_since(at) < Self::wait(key))
    }

    /// Records a launch. False means one is already under way and this
    /// click should do nothing.
    pub fn begin(&mut self, key: &str, now: Instant) -> bool {
        if self.pending(key, now) {
            return false;
        }
        self.started.insert(key.to_string(), now);
        true
    }

    /// Forgets a launch that failed, so the next click can try again.
    pub fn cancel(&mut self, key: &str) {
        self.started.remove(key);
    }

    /// Forgets launches whose app now has a window, or whose time is up.
    pub fn settle(&mut self, items: &[Item], now: Instant) {
        self.started.retain(|key, at| {
            let running = items.iter().any(|item| item.key == *key && item.running());
            !running && now.saturating_duration_since(*at) < Self::wait(key)
        });
    }
}

/// The windows that stand on their own, oldest first, each carrying the
/// focus of the dialogs that belong to it.
fn fold_children(windows: &[Window]) -> Vec<Window> {
    let root_of = |window: &Window| {
        let mut root = window;
        // Parents nest only a few deep; the limit guards against a loop.
        for _ in 0..8 {
            match root
                .parent
                .and_then(|parent| windows.iter().find(|w| w.id == parent))
            {
                Some(parent) => root = parent,
                None => break,
            }
        }
        root.id
    };

    let mut roots: Vec<Window> = windows
        .iter()
        .filter(|w| root_of(w) == w.id)
        .cloned()
        .collect();
    roots.sort_by_key(|w| w.id);
    for child in windows {
        let root = root_of(child);
        if root == child.id {
            continue;
        }
        if let Some(root) = roots.iter_mut().find(|w| w.id == root) {
            root.activated |= child.activated;
            root.focus_seq = root.focus_seq.max(child.focus_seq);
        }
    }
    roots
}

/// The dock's state: installed apps, pins, and how app_ids map onto them.
pub struct Dock {
    data_dirs: Vec<PathBuf>,
    apps: Apps,
    pinned: Vec<String>,
    /// app_id to item key, remembered so matching runs once per app.
    keys: HashMap<String, String>,
    /// When each applications directory was last changed, as of the last
    /// time the entries were read.
    stamps: Vec<Option<SystemTime>>,
    /// The newest window seen so far.
    newest_window: u32,
    /// Counts re-reads of the entries.
    generation: u64,
}

/// When each data directory's `applications` was last modified. Installing
/// or removing an app adds or removes a file there, which changes it.
fn stamps(data_dirs: &[PathBuf]) -> Vec<Option<SystemTime>> {
    data_dirs
        .iter()
        .map(|dir| {
            std::fs::metadata(dir.join("applications"))
                .and_then(|meta| meta.modified())
                .ok()
        })
        .collect()
}

impl Dock {
    /// `pinned` holds desktop-file IDs from the config. `data_dirs` is where
    /// desktop entries are re-read from when apps have been installed or
    /// removed while the dock runs.
    pub fn new(apps: Apps, pinned: Vec<String>, data_dirs: Vec<PathBuf>) -> Self {
        Self {
            stamps: stamps(&data_dirs),
            data_dirs,
            apps,
            pinned,
            keys: HashMap::new(),
            newest_window: 0,
            generation: 0,
        }
    }

    /// Goes up each time the desktop entries are re-read. Names and icons
    /// remembered from before may then be out of date.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Re-reads the desktop entries if an applications directory has changed
    /// since they were last read.
    fn reload_if_changed(&mut self) {
        let now = stamps(&self.data_dirs);
        if now != self.stamps {
            self.stamps = now;
            self.apps = Apps::load(&self.data_dirs);
            self.keys.clear();
            self.generation += 1;
        }
    }

    pub fn load(pinned: Vec<String>, data_dirs: Vec<PathBuf>) -> Self {
        Self::new(Apps::load(&data_dirs), pinned, data_dirs)
    }

    /// The desktop entry behind an item key.
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.apps.get(key)
    }

    fn key_for(&mut self, app_id: &str) -> String {
        if let Some(key) = self.keys.get(app_id) {
            return key.clone();
        }
        let found = matching::find(app_id, &self.apps).map(|e| e.id.to_lowercase());
        let key = found.unwrap_or_else(|| app_id.trim().to_lowercase());
        self.keys.insert(app_id.to_string(), key.clone());
        key
    }

    fn app_item(&self, kind: Kind, key: String, windows: Vec<WindowRef>) -> Item {
        let (name, icon, launchable) = match self.apps.get(&key) {
            Some(entry) => {
                let own = OWN_ICONS
                    .iter()
                    .find(|(id, _)| key == *id)
                    .map(|(_, icon)| icon.clone());
                (
                    entry.name.clone(),
                    own.unwrap_or_else(|| Icon::Named(entry.icon.clone())),
                    entry.exec.is_some(),
                )
            }
            // No entry: the app_id is the best name there is, and sometimes
            // an icon is installed under it.
            None => (key.clone(), Icon::Named(key.clone()), false),
        };
        Item {
            kind,
            letter: icons::initial(&name),
            key,
            name,
            icon,
            launchable,
            windows,
        }
    }

    /// The dock's items for the given windows: the search button, the pinned
    /// apps in config order, then other running apps in the order they
    /// first opened a window. Every window lands on exactly one item.
    ///
    /// Dialogs, file choosers and other windows with a parent are not
    /// counted: they stand for the window they belong to, which takes over
    /// their focus. So an app with one window and an open dialog still has
    /// one window, and a portal's file chooser does not get a tile.
    pub fn items(&mut self, windows: &[Window]) -> Vec<Item> {
        // A window opening is the moment a newly installed app matters, and
        // the only time the directories are looked at: never on a timer.
        let newest = windows.iter().map(|w| w.id).max().unwrap_or(0);
        if newest > self.newest_window {
            self.newest_window = newest;
            self.reload_if_changed();
        }
        let windows = fold_children(windows);
        // Group windows by app, remembering the order apps first appear in.
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, Vec<&Window>> = HashMap::new();
        for window in &windows {
            let key = if window.app_id.trim().is_empty() {
                format!("{NAMELESS}{}", window.id)
            } else {
                self.key_for(&window.app_id)
            };
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(window);
        }
        let refs = |list: &[&Window]| -> Vec<WindowRef> {
            list.iter()
                .map(|w| WindowRef {
                    id: w.id,
                    activated: w.activated,
                    minimized: w.minimized,
                    focus_seq: w.focus_seq,
                })
                .collect()
        };

        let mut items = vec![Item {
            kind: Kind::Launcher,
            key: LAUNCHER_KEY.to_string(),
            name: "Search".to_string(),
            icon: Icon::Search,
            letter: String::new(),
            launchable: true,
            windows: Vec::new(),
        }];

        let mut used: HashSet<String> = HashSet::new();
        for id in &self.pinned {
            // A pin for an app that is not installed is left out.
            let Some(entry) = self.apps.get(id) else {
                continue;
            };
            let key = entry.id.to_lowercase();
            if !used.insert(key.clone()) {
                continue;
            }
            let windows = groups.get(&key).map(|g| refs(g)).unwrap_or_default();
            items.push(self.app_item(Kind::Pinned, key, windows));
        }

        for key in order {
            if used.contains(&key) {
                continue;
            }
            let group = &groups[&key];
            if key.starts_with(NAMELESS) {
                // A window with no app_id goes by its title.
                let name = match group[0].title.trim() {
                    "" => "Window".to_string(),
                    title => title.to_string(),
                };
                items.push(Item {
                    kind: Kind::Running,
                    key,
                    letter: icons::initial(&name),
                    name,
                    icon: Icon::Named(String::new()),
                    launchable: false,
                    windows: refs(group),
                });
            } else {
                items.push(self.app_item(Kind::Running, key, refs(group)));
            }
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::testing::{entry, temp_dir, write};

    fn apps() -> Apps {
        Apps::from_entries(vec![
            entry("firefox-esr", "Firefox ESR", "firefox-esr %u"),
            entry("foot", "Foot", "foot"),
            entry("org.gnome.Nautilus", "Files", "nautilus %U"),
            entry("vim", "Vim", "vim %F"),
        ])
    }

    fn dock(pinned: &[&str]) -> Dock {
        Dock::new(
            apps(),
            pinned.iter().map(|s| s.to_string()).collect(),
            Vec::new(),
        )
    }

    fn window(id: u32, app_id: &str) -> Window {
        Window {
            id,
            app_id: app_id.into(),
            title: format!("{app_id} window"),
            ..Window::default()
        }
    }

    fn focused(id: u32, app_id: &str, seq: u64) -> Window {
        Window {
            activated: true,
            focus_seq: seq,
            ..window(id, app_id)
        }
    }

    fn keys(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.key.as_str()).collect()
    }

    #[test]
    fn with_no_windows_the_dock_holds_search_and_the_pins() {
        let items = dock(&["foot", "firefox-esr"]).items(&[]);
        assert_eq!(keys(&items), ["", "foot", "firefox-esr"]);
        assert_eq!(items[0].kind, Kind::Launcher);
        assert_eq!(items[0].icon, Icon::Search);
        assert_eq!(items[1].kind, Kind::Pinned);
        assert_eq!(items[1].name, "Foot");
        assert!(items.iter().all(|i| !i.running() && !i.focused()));
    }

    #[test]
    fn a_terminal_window_marks_the_pinned_terminal_instead_of_adding_an_icon() {
        // The bug in the Waybar dock: this used to give two terminal icons.
        let items = dock(&["foot"]).items(&[focused(1, "foot", 1)]);
        assert_eq!(keys(&items), ["", "foot"]);
        assert_eq!(items[1].kind, Kind::Pinned);
        assert!(items[1].running());
        assert!(items[1].focused());
        assert_eq!(items[1].window_count(), 1);
    }

    #[test]
    fn our_own_icons_are_used_for_the_terminal() {
        let items = dock(&["foot", "vim"]).items(&[]);
        assert_eq!(items[1].icon, Icon::Terminal);
        assert_eq!(items[2].icon, Icon::Named("vim".into()));
        assert_eq!(items[2].letter, "V");
    }

    #[test]
    fn windows_of_one_app_are_grouped() {
        let items = dock(&["foot"]).items(&[
            window(1, "foot"),
            window(2, "firefox-esr"),
            focused(3, "Foot", 4),
            window(4, "firefox-esr"),
        ]);
        assert_eq!(keys(&items), ["", "foot", "firefox-esr"]);
        assert_eq!(items[1].window_count(), 2);
        assert!(items[1].focused());
        assert_eq!(items[2].kind, Kind::Running);
        assert_eq!(items[2].window_count(), 2);
        assert!(!items[2].focused());
        assert_eq!(
            items[2].windows.iter().map(|w| w.id).collect::<Vec<_>>(),
            [2, 4]
        );
    }

    #[test]
    fn running_apps_follow_the_pins_in_the_order_they_opened() {
        let mut dock = dock(&["firefox-esr", "foot"]);
        // Given out of order on purpose.
        let items = dock.items(&[
            window(7, "vim"),
            window(3, "org.gnome.Nautilus"),
            window(5, "foot"),
        ]);
        assert_eq!(
            keys(&items),
            ["", "firefox-esr", "foot", "org.gnome.nautilus", "vim"]
        );
        assert_eq!(items[3].name, "Files");

        // Closing one leaves the others where they were.
        let items = dock.items(&[window(7, "vim"), window(5, "foot")]);
        assert_eq!(keys(&items), ["", "firefox-esr", "foot", "vim"]);
    }

    #[test]
    fn loosely_named_windows_still_find_their_app() {
        let items = dock(&["org.gnome.Nautilus.desktop"]).items(&[window(1, "nautilus")]);
        assert_eq!(keys(&items), ["", "org.gnome.nautilus"]);
        assert!(items[1].running());
    }

    #[test]
    fn pins_for_missing_apps_and_repeated_pins_are_skipped() {
        let items = dock(&["gone", "foot", "Foot.desktop"]).items(&[]);
        assert_eq!(keys(&items), ["", "foot"]);
    }

    #[test]
    fn a_window_from_no_known_app_gets_an_item_named_after_its_app_id() {
        let items = dock(&["foot"]).items(&[window(1, "XTerm"), window(2, "xterm")]);
        assert_eq!(keys(&items), ["", "foot", "xterm"]);
        let xterm = &items[2];
        assert_eq!(xterm.name, "xterm");
        assert_eq!(xterm.letter, "X");
        assert_eq!(xterm.icon, Icon::Named("xterm".into()));
        assert!(!xterm.launchable);
        assert_eq!(xterm.window_count(), 2);
    }

    #[test]
    fn windows_without_an_app_id_each_get_an_item_named_by_title() {
        let mut a = window(1, "");
        a.title = "Untitled drawing".into();
        let mut b = window(2, " ");
        b.title = String::new();
        let items = dock(&[]).items(&[a, b]);
        assert_eq!(keys(&items), ["", " window:1", " window:2"]);
        assert_eq!(items[1].name, "Untitled drawing");
        assert_eq!(items[1].letter, "U");
        assert_eq!(items[2].name, "Window");
        assert!(!items[1].launchable);
    }

    #[test]
    fn an_app_installed_later_is_picked_up_when_its_window_appears() {
        let dir = temp_dir("late");
        let mut dock = Dock::load(vec!["foot".into()], vec![dir.clone()]);
        assert_eq!(keys(&dock.items(&[])), [""]);

        write(
            &dir.join("applications/org.example.Paint.desktop"),
            "[Desktop Entry]\nType=Application\nName=Paint\nExec=paint\nIcon=paint\n",
        );
        let items = dock.items(&[window(1, "paint")]);
        assert_eq!(keys(&items), ["", "org.example.paint"]);
        assert_eq!(items[1].name, "Paint");
        assert!(items[1].launchable);
        assert_eq!(dock.entry("org.example.paint").unwrap().name, "Paint");
        assert_eq!(dock.generation(), 1);
    }

    fn touch(dir: &std::path::Path, seconds: u64) {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        std::fs::File::open(dir)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn an_app_installed_after_its_window_was_first_seen_is_still_picked_up() {
        let dir = temp_dir("later");
        let apps_dir = dir.join("applications");
        write(
            &apps_dir.join("foot.desktop"),
            "[Desktop Entry]\nType=Application\nName=Foot\nExec=foot\n",
        );
        touch(&apps_dir, 1_000);
        let mut dock = Dock::load(vec!["foot".into(), "paint".into()], vec![dir.clone()]);

        // Seen once with nothing installed for it: a plain tile.
        let items = dock.items(&[window(1, "paint")]);
        assert_eq!(keys(&items), ["", "foot", "paint"]);
        assert!(!items[2].launchable);
        assert_eq!(dock.generation(), 0);

        write(
            &apps_dir.join("paint.desktop"),
            "[Desktop Entry]\nType=Application\nName=Paint\nExec=paint\nIcon=paint\n",
        );
        touch(&apps_dir, 2_000);

        // Nothing is re-read until a window opens.
        let items = dock.items(&[window(1, "paint")]);
        assert_eq!(items[2].name, "paint");
        assert_eq!(dock.generation(), 0);

        // Any new window will do. The pin for it now resolves too.
        let items = dock.items(&[window(1, "paint"), window(2, "foot")]);
        assert_eq!(keys(&items), ["", "foot", "paint"]);
        assert_eq!(items[2].kind, Kind::Pinned);
        assert_eq!(items[2].name, "Paint");
        assert!(items[2].launchable);
        assert_eq!(dock.generation(), 1);

        // An unchanged directory is not read again.
        dock.items(&[window(1, "paint"), window(2, "foot"), window(3, "foot")]);
        assert_eq!(dock.generation(), 1);
    }

    #[test]
    fn a_removed_app_loses_its_pin_at_the_next_window() {
        let dir = temp_dir("removed");
        let apps_dir = dir.join("applications");
        write(
            &apps_dir.join("foot.desktop"),
            "[Desktop Entry]\nType=Application\nName=Foot\nExec=foot\n",
        );
        touch(&apps_dir, 1_000);
        let mut dock = Dock::load(vec!["foot".into()], vec![dir]);
        assert_eq!(keys(&dock.items(&[])), ["", "foot"]);

        std::fs::remove_file(apps_dir.join("foot.desktop")).unwrap();
        touch(&apps_dir, 2_000);
        assert_eq!(keys(&dock.items(&[window(1, "vim")])), ["", "vim"]);
    }

    #[test]
    fn a_dialog_is_part_of_its_window() {
        let mut dialog = focused(2, "foot", 5);
        dialog.parent = Some(1);
        let items = dock(&["foot"]).items(&[window(1, "foot"), dialog]);
        assert_eq!(keys(&items), ["", "foot"]);
        let foot = &items[1];
        assert_eq!(foot.window_count(), 1);
        assert_eq!(foot.windows[0].id, 1);
        // The dialog has the focus, so the app does.
        assert!(foot.focused());
        // One window: a click minimises it rather than cycling to the dialog.
        assert_eq!(on_click(foot, Button::Primary), Action::Minimize(1));
    }

    #[test]
    fn a_file_chooser_from_another_process_gets_no_tile() {
        // The portal's chooser has its own app_id but belongs to the app.
        let mut chooser = focused(2, "xdg-desktop-portal-gtk", 3);
        chooser.parent = Some(1);
        let mut nested = window(3, "xdg-desktop-portal-gtk");
        nested.parent = Some(2);
        let items = dock(&["foot"]).items(&[window(1, "firefox-esr"), chooser, nested]);
        assert_eq!(keys(&items), ["", "foot", "firefox-esr"]);
        assert_eq!(items[2].window_count(), 1);
        assert!(items[2].focused());
    }

    #[test]
    fn a_window_whose_parent_is_unknown_stands_alone() {
        let mut orphan = window(2, "vim");
        orphan.parent = Some(99);
        let mut own = window(3, "vim");
        own.parent = Some(3);
        let items = dock(&[]).items(&[orphan, own]);
        assert_eq!(keys(&items), ["", "vim"]);
        assert_eq!(items[1].window_count(), 2);
    }

    #[test]
    fn the_same_windows_give_the_same_items() {
        let mut dock = dock(&["foot"]);
        let windows = [focused(1, "foot", 1), window(2, "vim")];
        assert_eq!(dock.items(&windows), dock.items(&windows));
    }

    fn item(windows: Vec<WindowRef>) -> Item {
        Item {
            kind: Kind::Pinned,
            key: "foot".into(),
            name: "Foot".into(),
            icon: Icon::Terminal,
            letter: "F".into(),
            launchable: true,
            windows,
        }
    }

    fn win(id: u32, activated: bool, focus_seq: u64) -> WindowRef {
        WindowRef {
            id,
            activated,
            minimized: false,
            focus_seq,
        }
    }

    #[test]
    fn the_search_button_opens_the_launcher() {
        let items = dock(&[]).items(&[]);
        assert_eq!(on_click(&items[0], Button::Primary), Action::OpenLauncher);
        assert_eq!(on_click(&items[0], Button::Secondary), Action::OpenLauncher);
    }

    #[test]
    fn clicking_an_app_that_is_not_running_starts_it() {
        assert_eq!(
            on_click(&item(vec![]), Button::Primary),
            Action::Launch("foot".into())
        );
    }

    #[test]
    fn clicking_a_running_app_focuses_its_most_recent_window() {
        let windows = vec![win(1, false, 5), win(2, false, 9), win(3, false, 2)];
        assert_eq!(
            on_click(&item(windows), Button::Primary),
            Action::Activate(2)
        );
        // Never focused yet: the newest window.
        let fresh = vec![win(4, false, 0), win(6, false, 0)];
        assert_eq!(on_click(&item(fresh), Button::Primary), Action::Activate(6));
    }

    #[test]
    fn a_minimised_window_is_brought_back_by_a_click() {
        let mut hidden = win(1, false, 3);
        hidden.minimized = true;
        assert_eq!(
            on_click(&item(vec![hidden]), Button::Primary),
            Action::Activate(1)
        );
    }

    #[test]
    fn clicking_the_focused_app_minimises_its_only_window() {
        assert_eq!(
            on_click(&item(vec![win(1, true, 4)]), Button::Primary),
            Action::Minimize(1)
        );
    }

    #[test]
    fn clicking_the_focused_app_walks_through_its_windows() {
        // Focus history: 1, then 2, then 3 (focused now).
        let mut windows = vec![win(1, false, 1), win(2, false, 2), win(3, true, 3)];
        let mut visited = Vec::new();
        for seq in 4..10 {
            let Action::Activate(next) = on_click(&item(windows.clone()), Button::Primary) else {
                panic!("expected a window to be focused");
            };
            visited.push(next);
            for w in &mut windows {
                w.activated = w.id == next;
                if w.activated {
                    w.focus_seq = seq;
                }
            }
        }
        assert_eq!(visited, [1, 2, 3, 1, 2, 3]);
    }

    #[test]
    fn a_secondary_click_starts_another_instance() {
        assert_eq!(
            on_click(&item(vec![win(1, true, 1)]), Button::Secondary),
            Action::Launch("foot".into())
        );
        assert_eq!(
            on_click(&item(vec![]), Button::Secondary),
            Action::Launch("foot".into())
        );
    }

    #[test]
    fn a_second_click_while_an_app_is_starting_is_ignored() {
        let mut launches = Launches::default();
        let start = Instant::now();
        assert!(launches.begin("foot", start));
        assert!(launches.pending("foot", start));
        // The double click, and an impatient one a little later.
        assert!(!launches.begin("foot", start + Duration::from_millis(150)));
        assert!(!launches.begin("foot", start + Duration::from_secs(3)));
        // Another app is not held up.
        assert!(launches.begin("web", start + Duration::from_millis(200)));
    }

    #[test]
    fn a_launch_is_over_once_the_window_appears() {
        let mut launches = Launches::default();
        let start = Instant::now();
        launches.begin("foot", start);
        let soon = start + Duration::from_secs(1);

        launches.settle(&[item(vec![])], soon);
        assert!(launches.pending("foot", soon));

        launches.settle(&[item(vec![win(1, true, 1)])], soon);
        assert!(!launches.pending("foot", soon));
        // The window was closed again straight away: a click starts it.
        assert!(launches.begin("foot", soon));
    }

    #[test]
    fn an_app_that_never_shows_a_window_can_be_started_again_later() {
        let mut launches = Launches::default();
        let start = Instant::now();
        launches.begin("foot", start);
        let late = start + LAUNCH_WAIT;
        assert!(!launches.pending("foot", late));
        launches.settle(&[item(vec![])], late);
        assert!(launches.begin("foot", late));
    }

    #[test]
    fn a_failed_launch_does_not_block_the_next_try() {
        let mut launches = Launches::default();
        let start = Instant::now();
        launches.begin("foot", start);
        launches.cancel("foot");
        assert!(launches.begin("foot", start));
    }

    #[test]
    fn the_launcher_only_swallows_a_double_click() {
        let mut launches = Launches::default();
        let start = Instant::now();
        assert!(launches.begin(LAUNCHER_KEY, start));
        assert!(!launches.begin(LAUNCHER_KEY, start + Duration::from_millis(200)));
        assert!(launches.begin(LAUNCHER_KEY, start + LAUNCHER_WAIT));
    }

    #[test]
    fn an_app_without_a_desktop_entry_cannot_be_started() {
        let mut unknown = item(vec![win(1, false, 1)]);
        unknown.kind = Kind::Running;
        unknown.launchable = false;
        assert_eq!(on_click(&unknown, Button::Secondary), Action::Nothing);
        assert_eq!(on_click(&unknown, Button::Primary), Action::Activate(1));
    }
}
