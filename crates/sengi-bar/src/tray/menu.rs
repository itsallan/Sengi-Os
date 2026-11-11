//! A tray item's context menu, as exported through `com.canonical.dbusmenu`.
//!
//! `GetLayout` answers with a tree of `(id, properties, children)`. This
//! turns it into the few things a menu row needs, with limits on how deep
//! and how long it may be, and carries the result to and from the helper
//! that draws it as lines of text.

use zbus::zvariant::Value;

use super::text;

/// Levels of menu we keep: the menu itself and three levels of submenu.
pub const MAX_DEPTH: usize = 4;

/// Rows we keep across all levels. More than fits on a screen already.
pub const MAX_ENTRIES: usize = 200;

/// The properties asked for in `GetLayout`, so an app does not send icon
/// bitmaps for every row.
pub const PROPERTIES: [&str; 8] = [
    "label",
    "enabled",
    "visible",
    "type",
    "toggle-type",
    "toggle-state",
    "children-display",
    "icon-name",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Toggle {
    #[default]
    None,
    Check,
    Radio,
}

/// One row of a menu.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub id: i32,
    pub label: String,
    pub enabled: bool,
    pub separator: bool,
    pub toggle: Toggle,
    /// Whether a check or radio row is switched on.
    pub on: bool,
    /// Opens a submenu. The children may arrive only once it is opened.
    pub submenu: bool,
    /// Kept for whoever draws menu icons; nothing does yet.
    pub icon: String,
    pub children: Vec<Entry>,
}

fn inner<'a>(mut value: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(boxed) = value {
        value = boxed;
    }
    value
}

fn string<'a>(value: &'a Value<'a>) -> Option<&'a str> {
    match inner(value) {
        Value::Str(text) => Some(text.as_str()),
        _ => None,
    }
}

fn boolean(value: &Value<'_>) -> Option<bool> {
    match inner(value) {
        Value::Bool(flag) => Some(*flag),
        _ => None,
    }
}

/// The rows of the menu in a `GetLayout` reply's layout, which is the root
/// node; the root itself is not a row.
pub fn parse(layout: &Value<'_>) -> Vec<Entry> {
    let mut budget = MAX_ENTRIES;
    children(layout, 1, &mut budget)
}

fn children(node: &Value<'_>, depth: usize, budget: &mut usize) -> Vec<Entry> {
    let Value::Structure(node) = inner(node) else {
        return Vec::new();
    };
    let Some(Value::Array(list)) = node.fields().get(2).map(inner) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for child in list.iter() {
        if *budget == 0 {
            break;
        }
        if let Some(entry) = entry(child, depth, budget) {
            entries.push(entry);
        }
    }
    tidy(entries)
}

fn entry(node: &Value<'_>, depth: usize, budget: &mut usize) -> Option<Entry> {
    let Value::Structure(fields) = inner(node) else {
        return None;
    };
    let fields = fields.fields();
    let Value::I32(id) = inner(fields.first()?) else {
        return None;
    };
    let mut entry = Entry {
        id: *id,
        enabled: true,
        ..Entry::default()
    };
    let mut visible = true;
    if let Some(Value::Dict(properties)) = fields.get(1).map(inner) {
        for (key, value) in properties.iter() {
            match string(key).unwrap_or_default() {
                "label" => entry.label = text::label(string(value).unwrap_or_default()),
                "enabled" => entry.enabled = boolean(value).unwrap_or(true),
                "visible" => visible = boolean(value).unwrap_or(true),
                "type" => entry.separator = string(value) == Some("separator"),
                "toggle-type" => {
                    entry.toggle = match string(value) {
                        Some("checkmark") => Toggle::Check,
                        Some("radio") => Toggle::Radio,
                        _ => Toggle::None,
                    }
                }
                "toggle-state" => entry.on = matches!(inner(value), Value::I32(1)),
                "children-display" => entry.submenu = string(value) == Some("submenu"),
                "icon-name" => {
                    entry.icon = text::clean(string(value).unwrap_or_default(), text::MAX_TEXT)
                }
                _ => {}
            }
        }
    }
    if !visible {
        return None;
    }
    *budget -= 1;
    if entry.separator {
        // A separator is a line and nothing else.
        return Some(Entry {
            id: entry.id,
            separator: true,
            ..Entry::default()
        });
    }
    if entry.submenu && depth < MAX_DEPTH {
        entry.children = children(node, depth + 1, budget);
    } else if entry.submenu {
        // Too deep to open: shown, but it leads nowhere.
        entry.submenu = false;
        entry.enabled = false;
    }
    Some(entry)
}

/// Drops separators that would open or close a menu or sit next to another.
fn tidy(entries: Vec<Entry>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.separator && out.last().is_none_or(|last| last.separator) {
            continue;
        }
        out.push(entry);
    }
    if out.last().is_some_and(|last| last.separator) {
        out.pop();
    }
    out
}

/// Finds a row anywhere in the tree.
pub fn find(entries: &[Entry], id: i32) -> Option<&Entry> {
    entries.iter().find_map(|entry| {
        if entry.id == id && !entry.separator {
            Some(entry)
        } else {
            find(&entry.children, id)
        }
    })
}

/// The line that ends a menu on the wire.
pub const END: &str = ".";

/// Writes a menu as one line per row, `depth TAB id TAB flags TAB label`,
/// then [`END`]. Labels are single lines already.
pub fn encode(entries: &[Entry]) -> String {
    fn walk(entries: &[Entry], depth: usize, out: &mut String) {
        for entry in entries {
            let mut flags = String::new();
            for (set, flag) in [
                (entry.enabled, 'e'),
                (entry.separator, 's'),
                (entry.submenu, 'm'),
                (entry.toggle == Toggle::Check, 'c'),
                (entry.toggle == Toggle::Radio, 'r'),
                (entry.on, 'x'),
            ] {
                if set {
                    flags.push(flag);
                }
            }
            out.push_str(&format!(
                "{depth}\t{}\t{flags}\t{}\n",
                entry.id, entry.label
            ));
            walk(&entry.children, depth + 1, out);
        }
    }
    let mut out = String::new();
    walk(entries, 0, &mut out);
    out.push_str(END);
    out.push('\n');
    out
}

/// Reads back what [`encode`] wrote, given the lines before [`END`]. Lines
/// that make no sense are skipped, and the same limits apply as when
/// parsing.
pub fn decode<'a>(lines: impl IntoIterator<Item = &'a str>) -> Vec<Entry> {
    // The rows whose children are being read, outermost first.
    let mut open: Vec<Entry> = Vec::new();
    let mut top: Vec<Entry> = Vec::new();

    fn close(open: &mut Vec<Entry>, top: &mut Vec<Entry>, depth: usize) {
        while open.len() > depth {
            let done = open.pop().expect("checked by the loop");
            match open.last_mut() {
                Some(parent) => parent.children.push(done),
                None => top.push(done),
            }
        }
    }

    for line in lines.into_iter().take(MAX_ENTRIES) {
        let mut parts = line.splitn(4, '\t');
        let (Some(depth), Some(id), Some(flags), Some(label)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(depth), Ok(id)) = (depth.parse::<usize>(), id.parse::<i32>()) else {
            continue;
        };
        if depth >= MAX_DEPTH || depth > open.len() {
            continue;
        }
        close(&mut open, &mut top, depth);
        open.push(Entry {
            id,
            label: text::clean(label, text::MAX_TEXT),
            enabled: flags.contains('e'),
            separator: flags.contains('s'),
            submenu: flags.contains('m'),
            toggle: if flags.contains('c') {
                Toggle::Check
            } else if flags.contains('r') {
                Toggle::Radio
            } else {
                Toggle::None
            },
            on: flags.contains('x'),
            icon: String::new(),
            children: Vec::new(),
        });
    }
    close(&mut open, &mut top, 0);
    top
}

#[cfg(test)]
pub mod testing {
    use std::collections::HashMap;

    use zbus::zvariant::{Structure, Value};

    /// A layout node as `GetLayout` returns it.
    pub fn node(
        id: i32,
        properties: &[(&str, Value<'static>)],
        children: Vec<Value<'static>>,
    ) -> Value<'static> {
        let properties: HashMap<String, Value<'static>> = properties
            .iter()
            .map(|(key, value)| (key.to_string(), value.try_clone().unwrap()))
            .collect();
        Value::from(Structure::from((id, properties, children)))
    }

    pub fn item(id: i32, label: &str) -> Value<'static> {
        node(id, &[("label", Value::from(label.to_string()))], Vec::new())
    }

    pub fn separator(id: i32) -> Value<'static> {
        node(id, &[("type", Value::from("separator"))], Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn labels(entries: &[Entry]) -> Vec<&str> {
        entries
            .iter()
            .map(|entry| {
                if entry.separator {
                    "-"
                } else {
                    entry.label.as_str()
                }
            })
            .collect()
    }

    #[test]
    fn a_flat_menu_becomes_rows() {
        let layout = node(0, &[], vec![item(1, "Open"), item(2, "Quit")]);
        let menu = parse(&layout);
        assert_eq!(labels(&menu), ["Open", "Quit"]);
        assert_eq!(menu[1].id, 2);
        assert!(menu[0].enabled && !menu[0].separator && !menu[0].submenu);
        assert_eq!(menu[0].toggle, Toggle::None);
    }

    #[test]
    fn mnemonics_and_junk_are_cleaned_out_of_labels() {
        let layout = node(
            0,
            &[],
            vec![item(1, "_Preferences"), item(2, "a__b\n\u{202e}c")],
        );
        assert_eq!(labels(&parse(&layout)), ["Preferences", "a_b c"]);
    }

    #[test]
    fn disabled_and_hidden_rows() {
        let layout = node(
            0,
            &[],
            vec![
                node(
                    1,
                    &[
                        ("label", Value::from("Greyed")),
                        ("enabled", Value::from(false)),
                    ],
                    Vec::new(),
                ),
                node(
                    2,
                    &[
                        ("label", Value::from("Gone")),
                        ("visible", Value::from(false)),
                    ],
                    Vec::new(),
                ),
                item(3, "Here"),
            ],
        );
        let menu = parse(&layout);
        assert_eq!(labels(&menu), ["Greyed", "Here"]);
        assert!(!menu[0].enabled);
        assert!(menu[1].enabled);
    }

    #[test]
    fn check_and_radio_rows_carry_their_state() {
        let toggle = |id, kind: &'static str, state: i32| {
            node(
                id,
                &[
                    ("label", Value::from("Row")),
                    ("toggle-type", Value::from(kind)),
                    ("toggle-state", Value::from(state)),
                ],
                Vec::new(),
            )
        };
        let layout = node(
            0,
            &[],
            vec![
                toggle(1, "checkmark", 1),
                toggle(2, "checkmark", 0),
                toggle(3, "radio", 1),
                // Indeterminate reads as off.
                toggle(4, "radio", -1),
                toggle(5, "", 1),
            ],
        );
        let menu = parse(&layout);
        let state: Vec<_> = menu.iter().map(|entry| (entry.toggle, entry.on)).collect();
        assert_eq!(
            state,
            [
                (Toggle::Check, true),
                (Toggle::Check, false),
                (Toggle::Radio, true),
                (Toggle::Radio, false),
                (Toggle::None, true),
            ]
        );
    }

    #[test]
    fn separators_are_kept_only_between_rows() {
        let layout = node(
            0,
            &[],
            vec![
                separator(1),
                item(2, "One"),
                separator(3),
                separator(4),
                item(5, "Two"),
                separator(6),
            ],
        );
        assert_eq!(labels(&parse(&layout)), ["One", "-", "Two"]);
    }

    fn submenu(id: i32, label: &'static str, children: Vec<Value<'static>>) -> Value<'static> {
        node(
            id,
            &[
                ("label", Value::from(label)),
                ("children-display", Value::from("submenu")),
            ],
            children,
        )
    }

    #[test]
    fn submenus_nest() {
        let layout = node(
            0,
            &[],
            vec![
                item(1, "Top"),
                submenu(
                    2,
                    "More",
                    vec![
                        item(3, "Inner"),
                        submenu(4, "Deeper", vec![item(5, "Leaf")]),
                    ],
                ),
            ],
        );
        let menu = parse(&layout);
        assert!(menu[1].submenu);
        assert_eq!(labels(&menu[1].children), ["Inner", "Deeper"]);
        assert_eq!(labels(&menu[1].children[1].children), ["Leaf"]);
        assert_eq!(find(&menu, 5).unwrap().label, "Leaf");
        assert_eq!(find(&menu, 2).unwrap().label, "More");
        assert!(find(&menu, 99).is_none());
    }

    #[test]
    fn children_without_the_submenu_property_are_not_a_submenu() {
        let layout = node(
            0,
            &[],
            vec![node(
                1,
                &[("label", Value::from("Plain"))],
                vec![item(2, "x")],
            )],
        );
        let menu = parse(&layout);
        assert!(!menu[0].submenu && menu[0].children.is_empty());
    }

    #[test]
    fn nesting_stops_at_the_depth_limit() {
        let mut layout = item(1000, "bottom");
        for level in (0..20).rev() {
            layout = submenu(level, "level", vec![layout]);
        }
        let mut level = parse(&layout);
        let mut depth = 0;
        while let Some(first) = level.first() {
            depth += 1;
            if first.children.is_empty() {
                // The last level we keep cannot be opened.
                assert!(!first.submenu && !first.enabled);
            }
            level = first.children.clone();
        }
        assert_eq!(depth, MAX_DEPTH);
    }

    #[test]
    fn an_endless_menu_is_cut_at_the_entry_limit() {
        let many: Vec<_> = (1..=10_000).map(|id| item(id, "row")).collect();
        assert_eq!(parse(&node(0, &[], many)).len(), MAX_ENTRIES);

        // The limit covers all levels together.
        let wide = |base: i32| (0..150).map(|i| item(base + i, "row")).collect::<Vec<_>>();
        let layout = node(
            0,
            &[],
            vec![submenu(1, "a", wide(1000)), submenu(2, "b", wide(2000))],
        );
        let menu = parse(&layout);
        let total: usize = menu.iter().map(|entry| 1 + entry.children.len()).sum();
        assert_eq!(total, MAX_ENTRIES);
    }

    #[test]
    fn layouts_of_the_wrong_shape_give_an_empty_menu() {
        assert!(parse(&Value::from("nonsense")).is_empty());
        assert!(parse(&Value::from(7i32)).is_empty());
        // Rows that are not structures, or have no numeric id, are skipped.
        let layout = node(0, &[], vec![Value::from("row"), item(1, "Real")]);
        assert_eq!(labels(&parse(&layout)), ["Real"]);
        // Properties of the wrong type fall back to their defaults.
        let odd = node(
            1,
            &[
                ("label", Value::from(5i32)),
                ("enabled", Value::from("no")),
                ("toggle-state", Value::from("on")),
            ],
            Vec::new(),
        );
        let menu = parse(&node(0, &[], vec![odd]));
        assert_eq!(menu[0].label, "");
        assert!(menu[0].enabled && !menu[0].on);
    }

    #[test]
    fn a_menu_survives_the_trip_to_the_helper() {
        let layout = node(
            0,
            &[],
            vec![
                item(1, "Open"),
                separator(2),
                node(
                    3,
                    &[
                        ("label", Value::from("Sync")),
                        ("toggle-type", Value::from("checkmark")),
                        ("toggle-state", Value::from(1i32)),
                        ("enabled", Value::from(false)),
                    ],
                    Vec::new(),
                ),
                submenu(
                    4,
                    "More",
                    vec![
                        node(
                            5,
                            &[
                                ("label", Value::from("Radio")),
                                ("toggle-type", Value::from("radio")),
                            ],
                            Vec::new(),
                        ),
                        submenu(6, "Deeper", vec![item(7, "Leaf")]),
                    ],
                ),
                item(8, "Quit"),
            ],
        );
        let menu = parse(&layout);
        let wire = encode(&menu);
        assert!(wire.ends_with(".\n"));
        let lines: Vec<&str> = wire.lines().take_while(|line| *line != END).collect();
        assert_eq!(decode(lines), menu);
    }

    #[test]
    fn broken_lines_from_the_wire_are_skipped() {
        let lines = [
            "0\t1\te\tOne",
            "not a row",
            "x\t2\te\tBad depth",
            "0\tx\te\tBad id",
            // Claims a parent that is not there.
            "3\t4\te\tOrphan",
            "9\t5\te\tToo deep",
            "0\t6\tem\tSub",
            "1\t7\te\tChild\twith a tab",
        ];
        let menu = decode(lines);
        assert_eq!(labels(&menu), ["One", "Sub"]);
        assert_eq!(labels(&menu[1].children), ["Child with a tab"]);
        assert!(decode(std::iter::repeat_n("0\t1\te\tx", 100_000)).len() <= MAX_ENTRIES);
    }
}
