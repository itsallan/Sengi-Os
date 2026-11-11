//! Which application a window belongs to.
//!
//! Wayland windows carry an app_id that is supposed to be the desktop-file
//! ID, and often is not quite. The rules here are tried from the most to the
//! least certain, so a window lands on its app's dock icon rather than
//! getting a second one.

use crate::desktop::DesktopEntry;
use crate::exec;
use crate::index::AppIndex;

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

/// The last part of a reverse-DNS name: `org.mozilla.Firefox` gives `Firefox`.
fn last_component(id: &str) -> Option<&str> {
    let (_, last) = id.rsplit_once('.')?;
    Some(last).filter(|s| !s.is_empty())
}

/// Entries shown in menus are preferred over hidden helpers such as
/// `foot-server` when a loose rule matches several.
fn pick(apps: &AppIndex, matches: impl Fn(&DesktopEntry) -> bool) -> Option<&DesktopEntry> {
    let mut hidden = None;
    for entry in apps.entries().iter().filter(|e| matches(e)) {
        if !entry.no_display {
            return Some(entry);
        }
        hidden.get_or_insert(entry);
    }
    hidden
}

/// For the loose rules: the entry only if it is the single one that matches.
/// Two apps that both fit are a coin toss, and a window on the wrong icon is
/// worse than a window with an icon of its own. Hidden helpers only count
/// when no visible entry matches.
fn only(apps: &AppIndex, matches: impl Fn(&DesktopEntry) -> bool) -> Option<&DesktopEntry> {
    let found: Vec<&DesktopEntry> = apps.entries().iter().filter(|e| matches(e)).collect();
    let visible: Vec<&DesktopEntry> = found.iter().copied().filter(|e| !e.no_display).collect();
    match (visible.as_slice(), found.as_slice()) {
        ([entry], _) => Some(entry),
        ([], [entry]) => Some(entry),
        _ => None,
    }
}

/// Programs that run something else. An Exec line that starts with one says
/// nothing about which app it is: `python3 /opt/x/main.py` is not "python3".
const WRAPPERS: &[&str] = &[
    "sh",
    "bash",
    "dash",
    "zsh",
    "fish",
    "env",
    "exec",
    "sudo",
    "pkexec",
    "nice",
    "ionice",
    "python",
    "perl",
    "ruby",
    "node",
    "nodejs",
    "deno",
    "bun",
    "lua",
    "php",
    "java",
    "mono",
    "dotnet",
    "gjs",
    "electron",
    "flatpak",
    "snap",
    "wine",
    "wine64",
    "proton",
    "steam",
    "gtk-launch",
    "xdg-open",
    "dbus-launch",
    "dbus-run-session",
    "systemd-run",
    "sengi-run",
    "foot",
    "footclient",
    "xterm",
];

/// True for `python3`, `python3.13`, `electron34`, `java` and the like.
fn is_wrapper(program: &str) -> bool {
    let program = program.to_lowercase();
    let stem = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-');
    WRAPPERS.contains(&stem)
}

/// Finds the desktop entry for a window's app_id.
pub fn find<'a>(app_id: &str, apps: &'a AppIndex) -> Option<&'a DesktopEntry> {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        return None;
    }

    // 1. The app_id is the desktop-file ID.
    if let Some(entry) = apps.get(app_id) {
        return Some(entry);
    }

    // 2. The entry names the app_id as its window class.
    if let Some(entry) = pick(apps, |e| {
        e.wm_class
            .as_deref()
            .is_some_and(|class| same(class, app_id))
    }) {
        return Some(entry);
    }

    // 3. The app_id is the name of the program the entry starts, unless
    //    that program is only a way of starting something else.
    if !is_wrapper(app_id) {
        if let Some(entry) = only(apps, |e| {
            exec::program_name(&e.exec).is_some_and(|program| same(&program, app_id))
        }) {
            return Some(entry);
        }
    }

    // 4. One side is a reverse-DNS name and the other only its last part:
    //    app_id `org.gnome.Nautilus` with `nautilus.desktop`, or app_id
    //    `firefox` with `org.mozilla.firefox.desktop`. Two reverse-DNS names
    //    that merely end in the same word are different apps.
    match last_component(app_id) {
        Some(short) => apps
            .get(short)
            .filter(|entry| !entry.app_id().contains('.')),
        None => only(apps, |e| {
            last_component(e.app_id()).is_some_and(|last| same(last, app_id))
        }),
    }
}

/// The key to group windows under: the matched entry's ID, or the
/// app_id itself for a window that belongs to no installed app.
pub fn app_key(app_id: &str, apps: &AppIndex) -> String {
    match find(app_id, apps) {
        Some(entry) => entry.app_id().to_lowercase(),
        None => app_id.trim().to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::entry;

    fn apps() -> AppIndex {
        let mut server = entry("foot-server", "Foot Server", "foot --server");
        server.no_display = true;
        let mut code = entry("code-oss", "Code", "/usr/bin/code-oss --unity-launch %F");
        code.wm_class = Some("Code - OSS".into());
        let mut gimp = entry("gimp", "GIMP", "gimp-3.0 %U");
        gimp.wm_class = Some("gimp-3.0".into());
        AppIndex::from_entries(vec![
            code,
            entry(
                "firefox-esr",
                "Firefox ESR",
                "/usr/lib/firefox-esr/firefox-esr %u",
            ),
            entry("foot", "Foot", "foot"),
            server,
            entry("footclient", "Foot Client", "footclient"),
            gimp,
            entry("org.gnome.Nautilus", "Files", "nautilus --new-window %U"),
            entry(
                "org.mozilla.Thunderbird",
                "Thunderbird",
                "thunderbird-bin %u",
            ),
            entry(
                "signal-desktop",
                "Signal",
                "env LANG=C /opt/Signal/signal-desktop %U",
            ),
            entry("vim", "Vim", "vim %F"),
        ])
    }

    fn id(app_id: &str) -> Option<String> {
        find(app_id, &apps()).map(|e| e.app_id().to_string())
    }

    #[test]
    fn the_terminal_window_matches_the_pinned_terminal() {
        // The bug this module exists for.
        assert_eq!(id("foot").as_deref(), Some("foot"));
        assert_eq!(app_key("foot", &apps()), "foot");
    }

    #[test]
    fn desktop_ids_match_whatever_the_case() {
        assert_eq!(id("Foot").as_deref(), Some("foot"));
        assert_eq!(
            id("org.gnome.nautilus").as_deref(),
            Some("org.gnome.Nautilus")
        );
        assert_eq!(id("firefox-esr.desktop").as_deref(), Some("firefox-esr"));
    }

    #[test]
    fn the_window_class_is_tried_second() {
        assert_eq!(id("code - oss").as_deref(), Some("code-oss"));
        assert_eq!(id("gimp-3.0").as_deref(), Some("gimp"));
    }

    #[test]
    fn the_program_name_is_tried_third() {
        assert_eq!(
            id("thunderbird-bin").as_deref(),
            Some("org.mozilla.Thunderbird")
        );
        assert_eq!(id("nautilus").as_deref(), Some("org.gnome.Nautilus"));
    }

    #[test]
    fn the_id_wins_over_another_entry_that_runs_the_same_program() {
        // foot-server also runs `foot`, and sorts first.
        assert_eq!(id("foot").as_deref(), Some("foot"));
        assert_eq!(id("footclient").as_deref(), Some("footclient"));
    }

    #[test]
    fn reverse_dns_names_match_on_their_last_part() {
        assert_eq!(id("org.vim.Vim").as_deref(), Some("vim"));
        assert_eq!(
            id("thunderbird").as_deref(),
            Some("org.mozilla.Thunderbird")
        );
    }

    #[test]
    fn two_reverse_dns_names_ending_alike_are_different_apps() {
        assert_eq!(id("io.example.Thunderbird"), None);
        let apps = AppIndex::from_entries(vec![
            entry(
                "io.elementary.calculator",
                "Calculator",
                "io.elementary.calculator",
            ),
            entry("org.xfce.Settings", "Settings", "xfce4-settings-manager"),
        ]);
        assert!(find("org.gnome.Calculator", &apps).is_none());
        assert!(find("org.gnome.Settings", &apps).is_none());
        assert_eq!(
            app_key("org.gnome.Calculator", &apps),
            "org.gnome.calculator"
        );
    }

    #[test]
    fn a_short_app_id_shared_by_two_reverse_dns_entries_matches_neither() {
        let apps = AppIndex::from_entries(vec![
            entry(
                "io.elementary.terminal",
                "Terminal",
                "io.elementary.terminal",
            ),
            entry("org.gnome.Terminal", "Terminal", "gnome-terminal"),
        ]);
        assert!(find("terminal", &apps).is_none());
        // With only one of them installed there is no doubt.
        let apps = AppIndex::from_entries(vec![entry(
            "org.gnome.Terminal",
            "Terminal",
            "gnome-terminal",
        )]);
        assert_eq!(
            find("terminal", &apps).unwrap().app_id(),
            "org.gnome.Terminal"
        );
    }

    #[test]
    fn interpreters_and_launchers_are_not_program_names() {
        let apps = AppIndex::from_entries(vec![
            entry("backup", "Backup", "python3 /opt/backup/main.py"),
            entry("chat", "Chat", "electron34 /usr/lib/chat/app.asar"),
            entry("game", "Game", "steam steam://rungameid/70"),
            entry("notes", "Notes", "flatpak run org.example.Notes"),
            entry("old", "Old", "wine /opt/old/old.exe"),
            entry("report", "Report", "java -jar /opt/report.jar"),
            entry("script", "Script", "sh -c \"exec /opt/script/run\""),
        ]);
        for app_id in [
            "python3",
            "Python3.13",
            "electron",
            "electron34",
            "steam",
            "flatpak",
            "wine",
            "java",
            "sh",
        ] {
            assert!(find(app_id, &apps).is_none(), "{app_id} matched an entry");
        }
        assert!(is_wrapper("python3.13"));
        assert!(!is_wrapper("python-editor"));
        assert!(!is_wrapper("shotwell"));
    }

    #[test]
    fn a_wrapper_with_an_entry_of_its_own_still_matches_by_id() {
        let apps = AppIndex::from_entries(vec![
            entry("game", "Game", "steam steam://rungameid/70"),
            entry("steam", "Steam", "/usr/bin/steam %U"),
        ]);
        assert_eq!(find("steam", &apps).unwrap().app_id(), "steam");
    }

    #[test]
    fn a_program_started_by_two_entries_matches_neither() {
        let apps = AppIndex::from_entries(vec![
            entry("editor-a", "Editor", "edit --profile a"),
            entry("editor-b", "Editor (B)", "edit --profile b"),
        ]);
        assert!(find("edit", &apps).is_none());
    }

    #[test]
    fn env_wrappers_are_seen_through() {
        let mut list = apps().entries().to_vec();
        list.retain(|e| e.app_id() != "signal-desktop");
        list.push(entry(
            "signal",
            "Signal",
            "env LANG=C /opt/Signal/signal-desktop %U",
        ));
        let apps = AppIndex::from_entries(list);
        assert_eq!(find("signal-desktop", &apps).unwrap().app_id(), "signal");
    }

    #[test]
    fn unknown_and_empty_app_ids_match_nothing() {
        assert_eq!(id("xterm"), None);
        assert_eq!(id(""), None);
        assert_eq!(id("  "), None);
        assert_eq!(app_key("XTerm", &apps()), "xterm");
    }

    #[test]
    fn visible_entries_are_preferred_over_hidden_ones() {
        let mut helper = entry("a-helper", "Helper", "tool --daemon");
        helper.no_display = true;
        let apps = AppIndex::from_entries(vec![helper, entry("z-tool", "Tool", "tool")]);
        assert_eq!(find("tool", &apps).unwrap().app_id(), "z-tool");
    }
}
