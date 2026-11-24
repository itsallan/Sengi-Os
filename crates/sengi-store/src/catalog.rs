//! The list of apps and the matching of a name typed against it.

use std::collections::HashSet;

/// One app of the remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    pub id: String,
    pub name: String,
    pub summary: String,
    lower: String,
}

impl App {
    pub fn new(id: &str, name: &str, summary: &str) -> Self {
        let name = sengi_text::line(name, 80);
        Self {
            id: id.to_string(),
            lower: name.to_lowercase(),
            name,
            summary: sengi_text::line(summary, 200),
        }
    }
}

/// Whether `id` is an application ID as Flatpak has them: dotted names
/// of letters, digits, underscores and dashes. It is passed to a command
/// and used in a file name, so nothing else is let through.
pub fn valid_id(id: &str) -> bool {
    id.len() <= 255
        && id.split('.').count() >= 2
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        && !id.starts_with('-')
}

/// Flathub lists the pieces that belong to an app as apps of their own:
/// the base it builds on, its translations, its debug symbols.
fn is_part(id: &str) -> bool {
    ["BaseApp", "Locale", "Debug", "Sources"]
        .iter()
        .any(|suffix| id.ends_with(&format!(".{suffix}")))
}

/// Reads the output of `flatpak remote-ls --columns=application,name,description`:
/// one app a line, the columns separated by tabs. A line that is not that
/// is skipped.
pub fn parse_listing(text: &str) -> Vec<App> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for line in text.lines() {
        let mut columns = line.split('\t');
        let id = columns.next().unwrap_or("").trim();
        let name = columns.next().unwrap_or("").trim();
        let summary = columns.next().unwrap_or("").trim();
        if !valid_id(id) || name.is_empty() || is_part(id) || !seen.insert(id.to_string()) {
            continue;
        }
        apps.push(App::new(id, name, summary));
    }
    apps
}

/// Reads the output of `flatpak list --columns=application`.
pub fn parse_installed(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|id| valid_id(id))
        .map(str::to_string)
        .collect()
}

/// How well a name fits what was typed. Lower is better.
fn rank(name: &str, words: &[String]) -> Option<u8> {
    if words.iter().any(|word| !name.contains(word.as_str())) {
        return None;
    }
    let whole = words.join(" ");
    Some(if name == whole {
        0
    } else if name.starts_with(&whole) {
        1
    } else if name
        .split(|c: char| !c.is_alphanumeric())
        .any(|part| part.starts_with(words[0].as_str()))
    {
        2
    } else {
        3
    })
}

/// The apps whose name has every word of `query` in it, without regard to
/// case, best first, at most `limit` of them. Only the name is looked at,
/// not the summary. A query with nothing in it finds nothing.
pub fn search(apps: &[App], query: &str, limit: usize) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(u8, usize, usize)> = apps
        .iter()
        .enumerate()
        .filter_map(|(index, app)| {
            rank(&app.lower, &words).map(|rank| (rank, app.lower.chars().count(), index))
        })
        .collect();
    found.sort_by(|a, b| {
        (a.0, a.1, &apps[a.2].lower, &apps[a.2].id).cmp(&(
            b.0,
            b.1,
            &apps[b.2].lower,
            &apps[b.2].id,
        ))
    });
    found.truncate(limit);
    found.into_iter().map(|(_, _, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "org.mozilla.firefox\tFirefox\tFast, Private & Safe Web Browser\n\
        org.mozilla.firefox.BaseApp\tfirefox application base\tbase\n\
        dev.qwery.AddWater\tAdd Water\tKeep Firefox in fashion\n\
        org.gimp.GIMP\tGNU Image Manipulation Program\tHigh-end image creation\n\
        org.gimp.GIMP\tGNU Image Manipulation Program\tagain\n\
        org.gnome.Calculator\tCalculator\t\n\
        \n\
        not an app line\n\
        -bad.id\tNope\tx\n\
        org.kde.kate\t\tno name\n";

    #[test]
    fn listing_is_parsed() {
        let apps = parse_listing(LISTING);
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "org.mozilla.firefox",
                "dev.qwery.AddWater",
                "org.gimp.GIMP",
                "org.gnome.Calculator"
            ]
        );
        assert_eq!(apps[0].summary, "Fast, Private & Safe Web Browser");
        assert_eq!(apps[3].summary, "");
    }

    #[test]
    fn installed_is_parsed() {
        let set = parse_installed("org.gimp.GIMP\n\n org.gnome.Calculator \nnonsense\n");
        assert_eq!(set.len(), 2);
        assert!(set.contains("org.gimp.GIMP"));
    }

    #[test]
    fn ids_are_checked() {
        assert!(valid_id("org.mozilla.firefox"));
        assert!(valid_id("io.github.some-one.App_2"));
        assert!(!valid_id("firefox"));
        assert!(!valid_id("../../etc.passwd"));
        assert!(!valid_id("org..x"));
        assert!(!valid_id("--user.x"));
        assert!(!valid_id("org.x y"));
    }

    fn names(apps: &[App], found: &[usize]) -> Vec<String> {
        found.iter().map(|&i| apps[i].name.clone()).collect()
    }

    #[test]
    fn name_is_matched_in_any_case() {
        let apps = parse_listing(LISTING);
        assert_eq!(names(&apps, &search(&apps, "FIREFOX", 10)), ["Firefox"]);
        assert_eq!(names(&apps, &search(&apps, "irefo", 10)), ["Firefox"]);
    }

    #[test]
    fn summary_is_not_searched() {
        let apps = parse_listing(LISTING);
        // "Add Water" has Firefox in its summary only.
        let found = names(&apps, &search(&apps, "firefox", 10));
        assert!(!found.contains(&"Add Water".to_string()));
        assert!(search(&apps, "browser", 10).is_empty());
    }

    #[test]
    fn words_may_come_in_any_order() {
        let apps = parse_listing(LISTING);
        assert_eq!(names(&apps, &search(&apps, "water add", 10)), ["Add Water"]);
        assert!(search(&apps, "add fire", 10).is_empty());
    }

    #[test]
    fn better_matches_come_first() {
        let apps = vec![
            App::new("a.b.Inkscape", "Inkscape Extras", ""),
            App::new("a.b.Scape", "Escape Room", ""),
            App::new("a.b.Ink", "Ink", ""),
            App::new("a.b.Pink", "Pink Tool", ""),
        ];
        let found = names(&apps, &search(&apps, "ink", 10));
        assert_eq!(found, ["Ink", "Inkscape Extras", "Pink Tool"]);
    }

    #[test]
    fn nothing_typed_finds_nothing_and_the_limit_holds() {
        let apps = parse_listing(LISTING);
        assert!(search(&apps, "   ", 10).is_empty());
        assert_eq!(search(&apps, "a", 2).len(), 2);
    }
}
