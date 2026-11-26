//! The apps the Store recommends, from `featured.toml`, and what is shown
//! for a chip and a search over them.

use crate::catalog::{self, App};

const BUILTIN: &str = include_str!("../featured.toml");

/// One recommended app: its ID and the name to show without Flathub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub picks: Vec<Pick>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Featured {
    pub groups: Vec<Group>,
}

fn unquote(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text)
}

impl Featured {
    /// Reads the file's shape: `[group]` lines and `"id" = "name"` lines,
    /// with `#` comments. The first thing wrong is the error.
    pub fn parse(text: &str) -> Result<Featured, String> {
        let mut groups: Vec<Group> = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            let at = || format!("line {}", number + 1);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let name = sengi_text::line(unquote(name), 40);
                if name.is_empty() {
                    return Err(format!("{}: a group without a name", at()));
                }
                groups.push(Group {
                    name,
                    picks: Vec::new(),
                });
                continue;
            }
            let Some((id, name)) = line.split_once('=') else {
                return Err(format!("{}: not a group or an app", at()));
            };
            let (id, name) = (unquote(id), unquote(name));
            if !catalog::valid_id(id) {
                return Err(format!("{}: {id:?} is not an application ID", at()));
            }
            let Some(group) = groups.last_mut() else {
                return Err(format!("{}: an app before any group", at()));
            };
            group.picks.push(Pick {
                id: id.to_string(),
                name: sengi_text::line(name, 80),
            });
        }
        Ok(Featured { groups })
    }

    /// The list that ships. A file that does not parse is a bug the tests
    /// catch; the Store then simply has no recommendations.
    pub fn builtin() -> Featured {
        Featured::parse(BUILTIN).unwrap_or_default()
    }

    /// Whether `id` is one of the recommended apps.
    pub fn contains(&self, id: &str) -> bool {
        self.groups
            .iter()
            .any(|g| g.picks.iter().any(|p| p.id == id))
    }
}

/// What the chips under the search field choose. Chip 0 is All, chip 1 is
/// Featured, and each group after that has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Chip {
    #[default]
    All,
    Featured,
    Group(usize),
}

impl Chip {
    pub fn from_index(index: usize) -> Chip {
        match index {
            0 => Chip::All,
            1 => Chip::Featured,
            n => Chip::Group(n - 2),
        }
    }

    pub fn labels(featured: &Featured) -> Vec<String> {
        ["All", "Featured"]
            .into_iter()
            .map(str::to_string)
            .chain(featured.groups.iter().map(|g| g.name.clone()))
            .collect()
    }
}

/// One line of what the list shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// The name of a group.
    Heading(String),
    /// An app of the catalog, by its place there.
    App(usize),
    /// A recommended app with no catalog to take its summary or its icon
    /// from: Flathub has not been reached. It cannot be installed.
    Offline { id: String, name: String },
}

/// What the list shows for a chip and what was typed.
///
/// With a catalog, a recommended app Flathub does not list is left out.
/// All with something typed searches the whole catalog (at most `limit`
/// apps, best first); everything else shows recommended apps by group,
/// in the order of the file, those whose name has every word typed.
/// Without a catalog the recommended apps are shown by name alone, by the
/// same rules.
pub fn browse(
    featured: &Featured,
    catalog: &[App],
    query: &str,
    chip: Chip,
    limit: usize,
) -> Vec<Item> {
    let typed = !query.trim().is_empty();
    if chip == Chip::All && typed && !catalog.is_empty() {
        return catalog::search(catalog, query, limit)
            .into_iter()
            .map(Item::App)
            .collect();
    }
    let mut items = Vec::new();
    for (index, group) in featured.groups.iter().enumerate() {
        if matches!(chip, Chip::Group(g) if g != index) {
            continue;
        }
        let mut shown = Vec::new();
        for pick in &group.picks {
            if catalog.is_empty() {
                if !typed || catalog::name_matches(&pick.name, query) {
                    shown.push(Item::Offline {
                        id: pick.id.clone(),
                        name: pick.name.clone(),
                    });
                }
            } else if let Some(at) = catalog.iter().position(|a| a.id == pick.id) {
                if !typed || catalog[at].matches(query) {
                    shown.push(Item::App(at));
                }
            }
        }
        if !shown.is_empty() {
            items.push(Item::Heading(group.name.clone()));
            items.extend(shown);
        }
    }
    items
}

/// Where the arrow keys move to: the row `by` places from `current`
/// (1 or -1) that is not a heading, or `current` when there is none.
pub fn step(items: &[Item], current: usize, by: i32) -> usize {
    let mut at = current as i64;
    loop {
        at += i64::from(by);
        if at < 0 || at >= items.len() as i64 {
            return current;
        }
        if !matches!(items[at as usize], Item::Heading(_)) {
            return at as usize;
        }
    }
}

/// The first row that is not a heading.
pub fn first(items: &[Item]) -> usize {
    items
        .iter()
        .position(|i| !matches!(i, Item::Heading(_)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(f: &Featured) {
        let mut ids = std::collections::HashSet::new();
        let mut groups = std::collections::HashSet::new();
        for group in &f.groups {
            assert!(!group.picks.is_empty(), "{} has no apps", group.name);
            assert!(groups.insert(group.name.clone()), "{} twice", group.name);
            for pick in &group.picks {
                assert!(catalog::valid_id(&pick.id), "{}", pick.id);
                assert!(!pick.name.is_empty(), "{} has no name", pick.id);
                assert!(ids.insert(pick.id.clone()), "{} twice", pick.id);
            }
        }
    }

    #[test]
    fn the_shipped_list_is_sound() {
        let f = Featured::parse(BUILTIN).expect("featured.toml parses");
        assert!(f.groups.len() >= 8);
        valid(&f);
        assert_eq!(f, Featured::builtin());
    }

    #[test]
    fn the_welcome_suggestions_are_recommended() {
        let f = Featured::builtin();
        for id in [
            "org.mozilla.firefox",
            "org.libreoffice.LibreOffice",
            "org.videolan.VLC",
            "org.gimp.GIMP",
        ] {
            assert!(f.contains(id), "{id}");
        }
    }

    #[test]
    fn a_bad_file_is_refused() {
        assert!(Featured::parse("\"a.b\" = \"x\"").is_err());
        assert!(Featured::parse("[A]\nnot an app").is_err());
        assert!(Featured::parse("[A]\n\"nodots\" = \"x\"").is_err());
        assert!(Featured::parse("[]").is_err());
        assert!(Featured::parse("# only a comment\n")
            .unwrap()
            .groups
            .is_empty());
    }

    fn small() -> Featured {
        Featured::parse(
            "[One]\n\"a.b.One\" = \"One\"\n\"a.b.Gone\" = \"Gone\"\n\
             [Two]\n\"a.b.Two\" = \"Two\"\n[Three]\n\"a.b.Three\" = \"Three\"\n",
        )
        .unwrap()
    }

    fn apps() -> Vec<App> {
        vec![
            App::new("a.b.Two", "Two", "second"),
            App::new("a.b.One", "One", "first"),
            App::new("a.b.Three", "Three Ways", ""),
            App::new("c.d.Other", "Other One", ""),
        ]
    }

    fn shape(items: &[Item]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                Item::Heading(h) => format!("# {h}"),
                Item::App(n) => apps()[*n].name.clone(),
                Item::Offline { name, .. } => format!("offline {name}"),
            })
            .collect()
    }

    #[test]
    fn empty_search_shows_the_groups_without_the_missing() {
        let items = browse(&small(), &apps(), "", Chip::All, 40);
        assert_eq!(
            shape(&items),
            ["# One", "One", "# Two", "Two", "# Three", "Three Ways"]
        );
        assert_eq!(items, browse(&small(), &apps(), "  ", Chip::Featured, 40));
    }

    #[test]
    fn all_with_a_search_looks_at_the_whole_catalog() {
        let items = browse(&small(), &apps(), "one", Chip::All, 40);
        assert_eq!(shape(&items), ["One", "Other One"]);
        assert_eq!(browse(&small(), &apps(), "one", Chip::All, 1).len(), 1);
    }

    #[test]
    fn featured_and_groups_filter_the_recommended_apps_only() {
        let f = small();
        let items = browse(&f, &apps(), "one", Chip::Featured, 40);
        assert_eq!(shape(&items), ["# One", "One"]);
        let items = browse(&f, &apps(), "", Chip::from_index(3), 40);
        assert_eq!(shape(&items), ["# Two", "Two"]);
        assert!(browse(&f, &apps(), "one", Chip::from_index(3), 40).is_empty());
        assert!(browse(&f, &apps(), "", Chip::Group(9), 40).is_empty());
    }

    #[test]
    fn chips_are_all_featured_and_the_groups() {
        assert_eq!(
            Chip::labels(&small()),
            ["All", "Featured", "One", "Two", "Three"]
        );
        assert_eq!(Chip::from_index(0), Chip::All);
        assert_eq!(Chip::from_index(1), Chip::Featured);
        assert_eq!(Chip::from_index(2), Chip::Group(0));
    }

    #[test]
    fn without_a_catalog_the_names_are_shown_alone() {
        let f = small();
        let items = browse(&f, &[], "", Chip::All, 40);
        assert_eq!(
            shape(&items),
            [
                "# One",
                "offline One",
                "offline Gone",
                "# Two",
                "offline Two",
                "# Three",
                "offline Three"
            ]
        );
        let items = browse(&f, &[], "thr", Chip::All, 40);
        assert_eq!(shape(&items), ["# Three", "offline Three"]);
        let items = browse(&f, &[], "", Chip::Group(1), 40);
        assert_eq!(shape(&items), ["# Two", "offline Two"]);
    }

    #[test]
    fn the_keys_skip_headings() {
        let items = browse(&small(), &apps(), "", Chip::All, 40);
        assert_eq!(first(&items), 1);
        assert_eq!(step(&items, 1, 1), 3);
        assert_eq!(step(&items, 3, -1), 1);
        assert_eq!(step(&items, 1, -1), 1);
        assert_eq!(step(&items, 5, 1), 5);
        assert_eq!(first(&[]), 0);
    }
}
