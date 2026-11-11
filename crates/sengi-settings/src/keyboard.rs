//! The Keyboard page: which layouts there are, how to switch between
//! them, and how keys repeat.
//!
//! The layouts and their variants come from the XKB database's own list,
//! `evdev.lst`. What is chosen is kept in `keyboard.*` and reaches labwc
//! as `XKB_DEFAULT_LAYOUT`, `XKB_DEFAULT_VARIANT` and
//! `XKB_DEFAULT_OPTIONS` in its environment file, and as `repeatRate` and
//! `repeatDelay` in `rc.xml`; see `derive.rs`.

use std::fs;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::app::{self, item, App, Choice, Controller};
use crate::settings::{xkb_word, Layout, MAX_LAYOUTS};
use crate::store::Value;
use crate::text;
use crate::ui::{Item, KeyboardData};

const DATABASE: &str = "/usr/share/X11/xkb/rules/evdev.lst";
pub const MAX_ENTRIES: usize = 2048;

/// The ways of switching layouts that are offered: the XKB option and
/// what to call it. Super + Space is left out; it opens the launcher.
pub const SWITCHES: [(&str, &str); 6] = [
    ("", "No shortcut"),
    ("grp:alt_shift_toggle", "Alt + Shift"),
    ("grp:ctrl_shift_toggle", "Ctrl + Shift"),
    ("grp:alt_space_toggle", "Alt + Space"),
    ("grp:caps_toggle", "Caps Lock"),
    ("grp:shifts_toggle", "Both Shift keys"),
];

/// What a second layout gets if no shortcut was chosen, so that it can
/// be reached at all.
pub const DEFAULT_SWITCH: &str = "grp:alt_shift_toggle";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub layout: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Database {
    pub layouts: Vec<Named>,
    pub variants: Vec<Variant>,
}

/// Reads `evdev.lst`: sections headed `! layout` and `! variant`, each
/// line a name, white space, and a description; a variant's description
/// starts with the layout it belongs to and a colon.
pub fn parse_database(list: &str) -> Database {
    let mut database = Database::default();
    let mut section = "";
    for line in list.lines() {
        if let Some(heading) = line.strip_prefix('!') {
            section = heading.trim();
            continue;
        }
        let Some((name, description)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        if !xkb_word(name) {
            continue;
        }
        let description = description.trim();
        match section {
            "layout" if database.layouts.len() < MAX_ENTRIES => database.layouts.push(Named {
                name: name.to_string(),
                description: text::clean(description, 48),
            }),
            "variant" if database.variants.len() < MAX_ENTRIES => {
                let Some((layout, description)) = description.split_once(':') else {
                    continue;
                };
                if xkb_word(layout.trim()) {
                    database.variants.push(Variant {
                        layout: layout.trim().to_string(),
                        name: name.to_string(),
                        description: text::clean(description, 48),
                    });
                }
            }
            _ => {}
        }
    }
    database
        .layouts
        .sort_by(|a, b| a.description.cmp(&b.description));
    database
}

impl Database {
    pub fn load() -> Database {
        let fits = fs::metadata(DATABASE).is_ok_and(|meta| meta.is_file() && meta.len() < 4 << 20);
        let list = if fits {
            fs::read_to_string(DATABASE).unwrap_or_default()
        } else {
            String::new()
        };
        parse_database(&list)
    }

    /// What to call a layout: the variant's own description if it has
    /// one, else the layout's, else its bare name.
    pub fn describe(&self, layout: &Layout) -> String {
        let variant = self
            .variants
            .iter()
            .find(|v| v.layout == layout.layout && v.name == layout.variant);
        let plain = self.layouts.iter().find(|l| l.name == layout.layout);
        match (variant, plain) {
            (Some(variant), _) if !layout.variant.is_empty() => variant.description.clone(),
            (_, Some(plain)) if layout.variant.is_empty() => plain.description.clone(),
            _ => layout.render(),
        }
    }

    pub fn variants_of(&self, layout: &str) -> Vec<&Variant> {
        self.variants
            .iter()
            .filter(|v| v.layout == layout)
            .collect()
    }
}

/// The layouts with one more, if there is room and it is not there.
pub fn with_layout(mut layouts: Vec<Layout>, added: Layout) -> Option<Vec<Layout>> {
    if layouts.len() >= MAX_LAYOUTS || layouts.contains(&added) {
        return None;
    }
    layouts.push(added);
    Some(layouts)
}

/// The layouts without the one at `index`. The last one cannot go: a
/// keyboard with no layout types nothing.
pub fn without_layout(mut layouts: Vec<Layout>, index: usize) -> Option<Vec<Layout>> {
    if layouts.len() <= 1 || index >= layouts.len() {
        return None;
    }
    layouts.remove(index);
    Some(layouts)
}

pub fn switch_words(option: &str) -> &str {
    SWITCHES
        .iter()
        .find(|(name, _)| *name == option)
        .map_or(option, |(_, words)| words)
}

pub struct KeyboardPage {
    database: std::cell::OnceCell<Database>,
}

impl KeyboardPage {
    fn database(&self) -> &Database {
        // Read the first time the page is shown: it is a few hundred
        // kilobytes that most runs of Settings never need.
        self.database.get_or_init(Database::load)
    }

    pub fn attach(app: &Rc<App>) -> Rc<KeyboardPage> {
        let page = Rc::new(KeyboardPage {
            database: std::cell::OnceCell::new(),
        });
        let data = app.window.global::<KeyboardData>();
        let with = |handler: fn(&Rc<KeyboardPage>, &Rc<App>)| {
            let (page, app) = (Rc::downgrade(&page), Rc::downgrade(app));
            move || {
                if let (Some(page), Some(app)) = (page.upgrade(), app.upgrade()) {
                    handler(&page, &app);
                }
            }
        };
        data.on_add(with(KeyboardPage::add));
        data.on_choose_switch(with(|page, app| {
            let now = app.settings.borrow().text("keyboard", "switch");
            let choices = SWITCHES
                .iter()
                .map(|(option, words)| Choice {
                    id: option.to_string(),
                    title: words.to_string(),
                    detail: String::new(),
                    current: *option == now,
                })
                .collect();
            let page = Rc::downgrade(page);
            app.choose("Switch layouts with", false, choices, move |app, option| {
                if let Some(page) = page.upgrade() {
                    page.store(app, "switch", Value::Text(option));
                }
            });
        }));
        let (weak, weak_app) = (Rc::downgrade(&page), Rc::downgrade(app));
        data.on_remove(move |id| {
            let (Some(page), Some(app)) = (weak.upgrade(), weak_app.upgrade()) else {
                return;
            };
            let layouts = app.settings.borrow().layouts();
            let index = id.parse::<usize>().unwrap_or(usize::MAX);
            if let Some(layouts) = without_layout(layouts, index) {
                page.store_layouts(&app, layouts);
            }
        });
        let (weak, weak_app) = (Rc::downgrade(&page), Rc::downgrade(app));
        data.on_repeat(move |key, value| {
            if let (Some(page), Some(app)) = (weak.upgrade(), weak_app.upgrade()) {
                page.store(&app, &key, Value::Number(i64::from(value)));
            }
        });
        page
    }

    fn store(&self, app: &Rc<App>, key: &str, value: Value) {
        if app.set("keyboard", key, value) {
            app.apply_to_session();
        }
        self.render(app);
    }

    fn store_layouts(&self, app: &Rc<App>, layouts: Vec<Layout>) {
        let several = layouts.len() > 1;
        let value = Value::List(layouts.iter().map(Layout::render).collect());
        if !app.set("keyboard", "layouts", value) {
            return;
        }
        // A second layout nobody can switch to is no use.
        if several && app.settings.borrow().text("keyboard", "switch").is_empty() {
            app.set("keyboard", "switch", Value::text(DEFAULT_SWITCH));
        }
        app.apply_to_session();
        self.render(app);
    }

    /// Pick a layout, then one of its variants, and add it.
    fn add(self: &Rc<Self>, app: &Rc<App>) {
        let choices = self
            .database()
            .layouts
            .iter()
            .map(|layout| Choice {
                id: layout.name.clone(),
                title: layout.description.clone(),
                detail: layout.name.clone(),
                current: false,
            })
            .collect();
        let page = Rc::downgrade(self);
        app.choose(
            "Add a keyboard layout",
            true,
            choices,
            move |app, layout| {
                let Some(page) = page.upgrade() else { return };
                let add = {
                    let page = Rc::downgrade(&page);
                    move |app: &Rc<App>, layout: String, variant: String| {
                        let (Some(page), Some(added)) = (
                            page.upgrade(),
                            Layout::parse(&Layout { layout, variant }.render()),
                        ) else {
                            return;
                        };
                        let layouts = app.settings.borrow().layouts();
                        if let Some(layouts) = with_layout(layouts, added) {
                            page.store_layouts(app, layouts);
                        }
                    }
                };
                let variants = page.database().variants_of(&layout);
                if variants.is_empty() {
                    add(app, layout, String::new());
                    return;
                }
                let mut choices = vec![Choice {
                    id: String::new(),
                    title: "Standard".to_string(),
                    detail: layout.clone(),
                    current: false,
                }];
                choices.extend(variants.iter().map(|variant| Choice {
                    id: variant.name.clone(),
                    title: variant.description.clone(),
                    detail: format!("{}({})", layout, variant.name),
                    current: false,
                }));
                app.choose("Which variant?", true, choices, move |app, variant| {
                    add(app, layout, variant);
                });
            },
        );
    }

    fn render(&self, app: &Rc<App>) {
        let data = app.window.global::<KeyboardData>();
        let settings = app.settings.borrow();
        let layouts = settings.layouts();
        let rows: Vec<Item> = layouts
            .iter()
            .enumerate()
            .map(|(index, layout)| Item {
                icon: "keyboard".into(),
                remove: if layouts.len() > 1 { "Remove" } else { "" }.into(),
                ..item(
                    &index.to_string(),
                    &self.database().describe(layout),
                    &layout.render(),
                )
            })
            .collect();
        data.set_can_add(layouts.len() < MAX_LAYOUTS);
        data.set_several(layouts.len() > 1);
        data.set_layouts(app::model(rows));
        data.set_switch(switch_words(&settings.text("keyboard", "switch")).into());
        data.set_delay(settings.number("keyboard", "repeat-delay") as i32);
        data.set_rate(settings.number("keyboard", "repeat-rate") as i32);
    }
}

impl Controller for KeyboardPage {
    fn show(&self, app: &Rc<App>) {
        self.render(app);
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = "\
! model
  pc105           Generic 105-key PC

! layout
  us              English (US)
  fr              French
  de              German
  bad name here
  ev$il           Not a name

! variant
  azerty          fr: French (AZERTY)
  dvorak          us: English (Dvorak)
  nodeadkeys      de: German (no dead keys)
  orphan          no colon here
  hostile         fr: Evil\u{202e}\u{7} variant

! option
  grp:alt_shift_toggle Alt+Shift
";

    fn layout(text: &str) -> Layout {
        Layout::parse(text).unwrap()
    }

    #[test]
    fn layouts_and_variants_are_read_from_their_sections() {
        let database = parse_database(LIST);
        let layouts: Vec<_> = database
            .layouts
            .iter()
            .map(|l| (l.name.as_str(), l.description.as_str()))
            .collect();
        // By description, and a model or an option is not a layout.
        assert_eq!(
            layouts,
            [
                ("us", "English (US)"),
                ("fr", "French"),
                ("de", "German"),
                ("bad", "name here")
            ]
        );
        assert_eq!(database.variants.len(), 4);
        let french: Vec<&str> = database
            .variants_of("fr")
            .iter()
            .map(|v| v.description.as_str())
            .collect();
        assert_eq!(french, ["French (AZERTY)", "Evil variant"]);
        assert!(database.variants_of("xx").is_empty());
    }

    #[test]
    fn a_list_that_is_not_one_gives_nothing() {
        assert_eq!(parse_database(""), Database::default());
        assert_eq!(
            parse_database("\0\n!\n! layout\n\n   \n"),
            Database::default()
        );
        let flood: String = (0..5000).map(|i| format!("  l{i} Layout {i}\n")).collect();
        let database = parse_database(&format!("! layout\n{flood}"));
        assert_eq!(database.layouts.len(), MAX_ENTRIES);
    }

    #[test]
    fn layouts_are_described_in_words() {
        let database = parse_database(LIST);
        assert_eq!(database.describe(&layout("us")), "English (US)");
        assert_eq!(database.describe(&layout("fr(azerty)")), "French (AZERTY)");
        // Not in the database: its own name will have to do.
        assert_eq!(database.describe(&layout("xx")), "xx");
        assert_eq!(database.describe(&layout("fr(bepo)")), "fr(bepo)");
    }

    #[test]
    fn the_real_database_has_the_usual_layouts() {
        let database = Database::load();
        // Not every build machine has it; where it is, it has these.
        if !database.layouts.is_empty() {
            assert!(database.layouts.iter().any(|l| l.name == "us"));
            assert!(database
                .variants
                .iter()
                .any(|v| v.layout == "us" && v.name == "dvorak"));
        }
    }

    #[test]
    fn adding_and_removing_layouts() {
        let two = with_layout(vec![layout("us")], layout("fr(azerty)")).unwrap();
        assert_eq!(two.len(), 2);
        // Not twice, and not more than XKB can hold.
        assert_eq!(with_layout(two.clone(), layout("us")), None);
        let full: Vec<Layout> = ["us", "fr", "de", "es"].map(layout).to_vec();
        assert_eq!(with_layout(full, layout("it")), None);

        assert_eq!(
            without_layout(two.clone(), 0),
            Some(vec![layout("fr(azerty)")])
        );
        assert_eq!(without_layout(two, 7), None);
        // The last one stays.
        assert_eq!(without_layout(vec![layout("us")], 0), None);
    }

    #[test]
    fn every_offered_shortcut_is_a_setting_the_store_takes() {
        for (option, words) in SWITCHES {
            let value = Value::text(option);
            assert!(crate::settings::check("keyboard", "switch", &value).is_ok());
            assert_eq!(switch_words(option), words);
        }
        assert_eq!(switch_words("grp:something_else"), "grp:something_else");
        assert!(SWITCHES.iter().any(|(option, _)| *option == DEFAULT_SWITCH));
    }
}
