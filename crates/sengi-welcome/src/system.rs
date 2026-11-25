//! What the welcome reads from the machine and the commands it runs.
//!
//! Settings is not linked in: a choice is made by running
//! `sengi-settings --set`, which writes the setting and has the session take
//! it up exactly as the Settings window does, so there is one place that
//! knows how.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use sengi_config::settings::{xkb_word, Layout};

pub const NET_DIR: &str = "/sys/class/net";
pub const XKB_LIST: &str = "/usr/share/X11/xkb/rules/evdev.lst";
pub const WALLPAPER_DIR: &str = "/usr/share/backgrounds/sengi";
const MAX_LAYOUTS_LISTED: usize = 512;

/// The apps suggested on the Apps step: what is shown and what the Store is
/// asked to search for.
pub const SUGGESTIONS: [(&str, &str); 4] = [
    ("Web browser", "firefox"),
    ("Office", "libreoffice"),
    ("Video player", "vlc"),
    ("Image editor", "gimp"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Net {
    Wired,
    Wifi,
    Offline,
}

/// One entry of `/sys/class/net`.
pub struct Interface {
    pub up: bool,
    pub wireless: bool,
    pub physical: bool,
}

/// A wired link wins over Wi-Fi when both are up.
pub fn net_of(interfaces: &[Interface]) -> Net {
    let live = || interfaces.iter().filter(|i| i.up && i.physical);
    if live().any(|i| !i.wireless) {
        Net::Wired
    } else if live().any(|i| i.wireless) {
        Net::Wifi
    } else {
        Net::Offline
    }
}

pub fn read_net(root: &Path) -> Net {
    let Ok(entries) = fs::read_dir(root) else {
        return Net::Offline;
    };
    let interfaces: Vec<Interface> = entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            let state = fs::read_to_string(path.join("operstate")).unwrap_or_default();
            Interface {
                up: state.trim() == "up",
                wireless: path.join("wireless").exists() || path.join("phy80211").exists(),
                physical: path.join("device").exists(),
            }
        })
        .collect();
    net_of(&interfaces)
}

/// A keyboard layout to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What `keyboard.layouts` is given: `de`.
    pub id: String,
    pub name: String,
}

/// The layouts of the XKB database list (`evdev.lst`, the section headed
/// `! layout`), by name; the same source Settings reads. Variants are left
/// to Settings.
pub fn parse_layouts(list: &str) -> Vec<Entry> {
    let mut found = Vec::new();
    let mut inside = false;
    for line in list.lines() {
        if let Some(heading) = line.strip_prefix('!') {
            inside = heading.trim() == "layout";
            continue;
        }
        if !inside || found.len() >= MAX_LAYOUTS_LISTED {
            continue;
        }
        if let Some((id, name)) = line.trim().split_once(char::is_whitespace) {
            let name: String = name.trim().chars().filter(|c| !c.is_control()).collect();
            if xkb_word(id) && !name.is_empty() {
                found.push(Entry {
                    id: id.to_string(),
                    name,
                });
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// What is offered when the database cannot be read.
pub fn fallback_layouts() -> Vec<Entry> {
    [
        ("us", "English (US)"),
        ("gb", "English (UK)"),
        ("de", "German"),
        ("fr", "French"),
        ("es", "Spanish"),
        ("it", "Italian"),
        ("pt", "Portuguese"),
        ("ru", "Russian"),
    ]
    .iter()
    .map(|(id, name)| Entry {
        id: id.to_string(),
        name: name.to_string(),
    })
    .collect()
}

pub fn load_layouts(path: &Path) -> Vec<Entry> {
    let fits = fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() < 4 << 20);
    let found = if fits {
        parse_layouts(&fs::read_to_string(path).unwrap_or_default())
    } else {
        Vec::new()
    };
    if found.is_empty() {
        fallback_layouts()
    } else {
        found
    }
}

/// What is listed before anything is typed: the layouts most people want.
const COMMON: [&str; 8] = ["us", "gb", "de", "fr", "es", "it", "pt", "ru"];

/// The layouts that contain `filter`, in the name or the short name, as
/// far as `limit`. With nothing typed, the common ones.
pub fn filter_layouts<'a>(all: &'a [Entry], filter: &str, limit: usize) -> Vec<&'a Entry> {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        let common: Vec<&Entry> = COMMON
            .iter()
            .filter_map(|id| all.iter().find(|e| e.id == *id))
            .take(limit)
            .collect();
        if !common.is_empty() {
            return common;
        }
    }
    all.iter()
        .filter(|e| {
            filter.is_empty()
                || e.name.to_lowercase().contains(&filter)
                || e.id.to_lowercase() == filter
        })
        .take(limit)
        .collect()
}

/// The layouts with `chosen` first and the others after it, once each.
pub fn with_first(current: &[Layout], chosen: &str) -> Vec<String> {
    let mut list = vec![chosen.to_string()];
    for layout in current {
        let text = layout.render();
        if !list.contains(&text) {
            list.push(text);
        }
    }
    list.truncate(sengi_config::settings::MAX_LAYOUTS);
    list
}

/// A built-in wallpaper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wallpaper {
    pub title: String,
    pub path: PathBuf,
}

/// `01-hills.png` is "Hills".
pub fn title_of(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let name = match stem.split_once('-') {
        Some((number, rest)) if number.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => stem,
    };
    let mut chars: Vec<char> = name.replace(['-', '_'], " ").chars().collect();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

/// The shipped pictures in `dir`, in the order of their names.
pub fn wallpapers(dir: &Path) -> Vec<Wallpaper> {
    let mut files: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".png"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| Wallpaper {
            title: title_of(&file),
            path: dir.join(&file),
        })
        .collect()
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| word.to_string()).collect()
}

pub fn theme_command(light: bool) -> Vec<String> {
    words(&[
        "sengi-settings",
        "--set",
        "appearance.theme",
        if light { "light" } else { "dark" },
    ])
}

/// `None` is the plain desktop colour.
pub fn wallpaper_command(path: Option<&Path>) -> Vec<String> {
    let value = path.map_or_else(|| "solid".to_string(), |p| p.display().to_string());
    let mut command = words(&["sengi-settings", "--set", "appearance.wallpaper"]);
    command.push(value);
    command
}

pub fn layouts_command(layouts: &[String]) -> Vec<String> {
    let mut command = words(&["sengi-settings", "--set", "keyboard.layouts"]);
    command.push(layouts.join(","));
    command
}

/// Settings, at `page` unless that is empty.
pub fn settings_command(page: &str) -> Vec<String> {
    if page.is_empty() {
        words(&["sengi-settings"])
    } else {
        words(&["sengi-settings", page])
    }
}

pub fn store_command(search: &str) -> Vec<String> {
    if search.is_empty() {
        words(&["sengi-store"])
    } else {
        let mut command = words(&["sengi-store", "--search"]);
        command.push(search.to_string());
        command
    }
}

pub fn files_command() -> Vec<String> {
    words(&["sengi-files"])
}

/// How commands are run, so that the tests can watch instead.
pub trait Runner {
    /// A change of settings: run in turn, in the background, in the order
    /// given, so that one does not race the next.
    fn change(&self, command: Vec<String>);
    /// A program for the user to look at: started and left alone.
    fn open(&self, command: Vec<String>);
}

/// The real thing.
pub struct Process {
    queue: mpsc::Sender<Vec<String>>,
}

impl Process {
    pub fn new() -> Process {
        let (queue, work) = mpsc::channel::<Vec<String>>();
        let _ = std::thread::Builder::new()
            .name("welcome-settings".to_string())
            .spawn(move || {
                for command in work {
                    let _ = Command::new(&command[0])
                        .args(&command[1..])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            });
        Process { queue }
    }
}

impl Default for Process {
    fn default() -> Self {
        Process::new()
    }
}

impl Runner for Process {
    fn change(&self, command: Vec<String>) {
        let _ = self.queue.send(command);
    }

    fn open(&self, command: Vec<String>) {
        // In its own scope under app.slice, as the launcher starts apps;
        // directly if sengi-run is not there.
        let started = Command::new("sengi-run")
            .args(&command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .or_else(|_| {
                Command::new(&command[0])
                    .args(&command[1..])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            });
        if let Ok(mut child) = started {
            // Reaped when it ends, so that it is no zombie meanwhile.
            let _ = std::thread::Builder::new()
                .name("welcome-child".to_string())
                .spawn(move || {
                    let _ = child.wait();
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "! model\n  pc105  Generic 105-key PC\n\n! layout\n  us              English (US)\n  de              German\n  fr              French\n  bad;id          Nope\n\n! variant\n  nodeadkeys      de: German (no dead keys)\n";

    fn iface(up: bool, wireless: bool, physical: bool) -> Interface {
        Interface {
            up,
            wireless,
            physical,
        }
    }

    #[test]
    fn a_cable_wins_over_wifi_and_virtual_links_do_not_count() {
        assert_eq!(
            net_of(&[iface(true, true, true), iface(true, false, true)]),
            Net::Wired
        );
        assert_eq!(net_of(&[iface(true, true, true)]), Net::Wifi);
        assert_eq!(net_of(&[iface(false, true, true)]), Net::Offline);
        assert_eq!(net_of(&[iface(true, false, false)]), Net::Offline);
        assert_eq!(net_of(&[]), Net::Offline);
    }

    #[test]
    fn layouts_come_from_their_section_by_name() {
        let found = parse_layouts(LIST);
        let ids: Vec<&str> = found.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["us", "fr", "de"]);
        assert_eq!(found[0].name, "English (US)");
        assert!(parse_layouts("").is_empty());
        assert!(!fallback_layouts().is_empty());
    }

    #[test]
    fn the_filter_matches_names_and_exact_short_names() {
        let all = parse_layouts(LIST);
        assert_eq!(filter_layouts(&all, "ger", 5)[0].id, "de");
        assert_eq!(filter_layouts(&all, "FR", 5)[0].id, "fr");
        // Nothing typed: the common ones that exist, in their order.
        let shown: Vec<&str> = filter_layouts(&all, "", 5)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(shown, ["us", "de", "fr"]);
        assert!(filter_layouts(&all, "zzz", 5).is_empty());
    }

    #[test]
    fn the_chosen_layout_goes_first_and_is_not_repeated() {
        let current = vec![
            Layout::parse("us").unwrap(),
            Layout::parse("fr(azerty)").unwrap(),
        ];
        assert_eq!(with_first(&current, "de"), ["de", "us", "fr(azerty)"]);
        assert_eq!(with_first(&current, "fr(azerty)"), ["fr(azerty)", "us"]);
    }

    #[test]
    fn wallpaper_titles_drop_the_number() {
        assert_eq!(title_of("01-hills.png"), "Hills");
        assert_eq!(title_of("deep_blue.png"), "Deep blue");
    }

    #[test]
    fn the_commands_are_settings_changes() {
        assert_eq!(
            theme_command(true),
            ["sengi-settings", "--set", "appearance.theme", "light"]
        );
        assert_eq!(theme_command(false)[3], "dark");
        assert_eq!(wallpaper_command(None)[3], "solid");
        assert_eq!(
            wallpaper_command(Some(Path::new("/usr/share/backgrounds/sengi/01-a.png")))[3],
            "/usr/share/backgrounds/sengi/01-a.png"
        );
        assert_eq!(layouts_command(&["de".into(), "us".into()])[3], "de,us");
        assert_eq!(settings_command("network"), ["sengi-settings", "network"]);
        assert_eq!(store_command("gimp"), ["sengi-store", "--search", "gimp"]);
        assert_eq!(store_command(""), ["sengi-store"]);
    }

    #[test]
    fn every_suggestion_is_a_plain_word() {
        for (label, search) in SUGGESTIONS {
            assert!(!label.is_empty());
            assert!(search.chars().all(|c| c.is_ascii_lowercase()));
        }
    }
}
