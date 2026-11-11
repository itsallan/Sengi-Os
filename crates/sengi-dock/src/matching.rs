//! Which application a window belongs to.
//!
//! Wayland windows carry an app_id that is supposed to be the desktop-file
//! ID, and often is not quite. The rules here are tried from the most to the
//! least certain, so a window lands on its app's dock icon rather than
//! getting a second one.

use crate::desktop::{Apps, Entry};
use crate::exec;

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
fn pick(apps: &Apps, matches: impl Fn(&Entry) -> bool) -> Option<&Entry> {
    let mut hidden = None;
    for entry in apps.entries().iter().filter(|e| matches(e)) {
        if !entry.no_display {
            return Some(entry);
        }
        hidden.get_or_insert(entry);
    }
    hidden
}

/// Finds the desktop entry for a window's app_id.
pub fn find<'a>(app_id: &str, apps: &'a Apps) -> Option<&'a Entry> {
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

    // 3. The app_id is the name of the program the entry starts.
    if let Some(entry) = pick(apps, |e| {
        e.exec
            .as_deref()
            .and_then(exec::program_name)
            .is_some_and(|program| same(&program, app_id))
    }) {
        return Some(entry);
    }

    // 4. One side is a reverse-DNS name and the other only its last part:
    //    app_id `org.gnome.Nautilus` with `nautilus.desktop`, or app_id
    //    `firefox` with `org.mozilla.firefox.desktop`.
    let short = last_component(app_id);
    if let Some(short) = short {
        if let Some(entry) = apps.get(short) {
            return Some(entry);
        }
    }
    pick(apps, |e| {
        last_component(&e.id).is_some_and(|last| same(last, short.unwrap_or(app_id)))
    })
}

/// The key the dock groups windows under: the matched entry's ID, or the
/// app_id itself for a window that belongs to no installed app.
pub fn app_key(app_id: &str, apps: &Apps) -> String {
    match find(app_id, apps) {
        Some(entry) => entry.id.to_lowercase(),
        None => app_id.trim().to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::testing::entry;

    fn apps() -> Apps {
        let mut server = entry("foot-server", "Foot Server", "foot --server");
        server.no_display = true;
        let mut code = entry("code-oss", "Code", "/usr/bin/code-oss --unity-launch %F");
        code.wm_class = Some("Code - OSS".into());
        let mut gimp = entry("gimp", "GIMP", "gimp-3.0 %U");
        gimp.wm_class = Some("gimp-3.0".into());
        Apps::from_entries(vec![
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
        find(app_id, &apps()).map(|e| e.id.clone())
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
        assert_eq!(
            id("io.example.Thunderbird").as_deref(),
            Some("org.mozilla.Thunderbird")
        );
    }

    #[test]
    fn env_wrappers_are_seen_through() {
        let mut list = apps().entries().to_vec();
        list.retain(|e| e.id != "signal-desktop");
        list.push(entry(
            "signal",
            "Signal",
            "env LANG=C /opt/Signal/signal-desktop %U",
        ));
        let apps = Apps::from_entries(list);
        assert_eq!(find("signal-desktop", &apps).unwrap().id, "signal");
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
        let apps = Apps::from_entries(vec![helper, entry("z-tool", "Tool", "tool")]);
        assert_eq!(find("tool", &apps).unwrap().id, "z-tool");
    }
}
