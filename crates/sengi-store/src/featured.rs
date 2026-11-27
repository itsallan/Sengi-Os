//! The apps the Store recommends, from `featured.toml`, and what each of
//! its pages shows.

use crate::catalog::{self, App};

const BUILTIN: &str = include_str!("../featured.toml");

/// The glyph of a group that names none.
const GLYPH: &str = "storefront";

/// One recommended app: its ID and the name to show without Flathub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pick {
    pub id: String,
    pub name: String,
}

/// A category: its name, the Phosphor Bold icon of its tile and its apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub icon: String,
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
    /// Reads the file's shape: `[group]` lines, an `icon = "name"` line
    /// under each, and `"id" = "name"` lines, with `#` comments. The first
    /// thing wrong is the error.
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
                    icon: GLYPH.to_string(),
                    picks: Vec::new(),
                });
                continue;
            }
            let Some((id, name)) = line.split_once('=') else {
                return Err(format!("{}: not a group or an app", at()));
            };
            let (id, name) = (unquote(id), unquote(name));
            let Some(group) = groups.last_mut() else {
                return Err(format!("{}: an app before any group", at()));
            };
            if id == "icon" {
                group.icon = sengi_text::line(name, 40);
                continue;
            }
            if !catalog::valid_id(id) {
                return Err(format!("{}: {id:?} is not an application ID", at()));
            }
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
        self.group_of(id).is_some()
    }

    /// The first group that recommends `id`.
    pub fn group_of(&self, id: &str) -> Option<&Group> {
        self.groups
            .iter()
            .find(|g| g.picks.iter().any(|p| p.id == id))
    }
}

/// The page under the search field. What is typed in the field goes before
/// it: a search shows its results whichever page it was typed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Page {
    /// The categories and the featured apps.
    #[default]
    Start,
    /// The apps of one category, by its place in the list.
    Category(usize),
}

/// One card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// An app of the catalog, by its place there.
    App(usize),
    /// A recommended app with no catalog to take its summary from: Flathub
    /// has not been reached. It cannot be installed.
    Offline { id: String, name: String },
}

fn card(pick: &Pick, catalog: &[App]) -> Option<Item> {
    if catalog.is_empty() {
        return Some(Item::Offline {
            id: pick.id.clone(),
            name: pick.name.clone(),
        });
    }
    catalog.iter().position(|a| a.id == pick.id).map(Item::App)
}

/// The cards shown for a page and what was typed.
///
/// A search looks at every app of the catalog (at most `limit`, best
/// first), or at the names of the recommended apps when there is none.
/// With nothing typed the start page features the first app of each
/// category that Flathub has, and a category shows its own. An app Flathub
/// does not list is left out, so nothing is offered that cannot be had.
pub fn browse(
    featured: &Featured,
    catalog: &[App],
    query: &str,
    page: Page,
    limit: usize,
) -> Vec<Item> {
    if !query.trim().is_empty() {
        if !catalog.is_empty() {
            return catalog::search(catalog, query, limit)
                .into_iter()
                .map(Item::App)
                .collect();
        }
        return featured
            .groups
            .iter()
            .flat_map(|g| &g.picks)
            .filter(|p| catalog::name_matches(&p.name, query))
            .filter_map(|p| card(p, catalog))
            .take(limit)
            .collect();
    }
    match page {
        Page::Start => featured
            .groups
            .iter()
            .filter_map(|g| g.picks.iter().find_map(|p| card(p, catalog)))
            .collect(),
        Page::Category(index) => featured.groups.get(index).map_or_else(Vec::new, |g| {
            g.picks.iter().filter_map(|p| card(p, catalog)).collect()
        }),
    }
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
    fn every_category_has_a_glyph_the_interface_can_draw() {
        let icons =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/icons/phosphor/bold");
        for group in &Featured::builtin().groups {
            assert_ne!(group.icon, GLYPH, "{} names no icon", group.name);
            let file = icons.join(format!("{}-bold.svg", group.icon));
            assert!(file.is_file(), "{} has no {}", group.name, group.icon);
        }
    }

    #[test]
    fn the_welcome_suggestions_are_featured_first() {
        let f = Featured::builtin();
        let first: Vec<&str> = f.groups.iter().map(|g| g.picks[0].id.as_str()).collect();
        for id in [
            "org.mozilla.firefox",
            "org.libreoffice.LibreOffice",
            "org.videolan.VLC",
            "org.gimp.GIMP",
        ] {
            assert!(first.contains(&id), "{id}");
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
            "[One]\nicon = \"globe\"\n\"a.b.Gone\" = \"Gone\"\n\"a.b.One\" = \"One\"\n\
             [Two]\n\"a.b.Two\" = \"Two\"\n\"a.b.Three\" = \"Three\"\n\
             [Nothing]\n\"a.b.Missing\" = \"Missing\"\n",
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
                Item::App(n) => apps()[*n].name.clone(),
                Item::Offline { name, .. } => format!("offline {name}"),
            })
            .collect()
    }

    #[test]
    fn icons_belong_to_their_group() {
        let f = small();
        assert_eq!(f.groups[0].icon, "globe");
        assert_eq!(f.groups[1].icon, GLYPH);
        assert_eq!(f.group_of("a.b.Three").unwrap().name, "Two");
        assert!(f.group_of("c.d.Other").is_none());
    }

    #[test]
    fn the_start_page_features_the_first_app_flathub_has_of_each() {
        let items = browse(&small(), &apps(), "  ", Page::Start, 40);
        assert_eq!(shape(&items), ["One", "Two"]);
    }

    #[test]
    fn a_category_shows_its_apps_that_flathub_has() {
        let items = browse(&small(), &apps(), "", Page::Category(1), 40);
        assert_eq!(shape(&items), ["Two", "Three Ways"]);
        assert!(browse(&small(), &apps(), "", Page::Category(2), 40).is_empty());
        assert!(browse(&small(), &apps(), "", Page::Category(9), 40).is_empty());
    }

    #[test]
    fn a_search_looks_at_the_whole_catalog_from_any_page() {
        for page in [Page::Start, Page::Category(1)] {
            let items = browse(&small(), &apps(), "one", page, 40);
            assert_eq!(shape(&items), ["One", "Other One"]);
        }
        assert_eq!(browse(&small(), &apps(), "one", Page::Start, 1).len(), 1);
    }

    #[test]
    fn without_a_catalog_the_names_are_shown_alone() {
        let f = small();
        let start = browse(&f, &[], "", Page::Start, 40);
        assert_eq!(
            shape(&start),
            ["offline Gone", "offline Two", "offline Missing"]
        );
        let found = browse(&f, &[], "thr", Page::Start, 40);
        assert_eq!(shape(&found), ["offline Three"]);
        let category = browse(&f, &[], "", Page::Category(1), 40);
        assert_eq!(shape(&category), ["offline Two", "offline Three"]);
    }
}
