//! Drawing a tray item's menu: `sengi-bar --menu <output> <x>`.
//!
//! sengi-layer gives a process one kind of surface, and the bar's is 28
//! pixels tall. So the bar starts itself again in this mode for each menu,
//! the way sengi-launcher is started for the power menu. The menu arrives
//! on standard input as [`menu::encode`] writes it, and again whenever the
//! app changes it. What the user does is reported on standard output, one
//! line each: `opened <id>` when a submenu is opened and `clicked <id>`
//! when a row is chosen. Closing without choosing says nothing.

use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read};
use std::process::ExitCode;
use std::rc::Rc;

use sengi_layer::{Anchor, App, Config, KeyboardMode, Layer, Outputs};
use slint::{Model, ModelRc, VecModel};

use super::menu::{self, Entry, Toggle};
use crate::{MenuPanel, MenuRow, TrayMenu};

/// First argument of the process that draws a menu.
pub const FLAG: &str = "--menu";

/// Most the helper will read from the bar over its whole life.
const MAX_INPUT: u64 = 4 * 1024 * 1024;

/// What the helper tells the bar.
#[derive(Debug, PartialEq, Eq)]
pub enum Report {
    Opened(i32),
    Clicked(i32),
}

pub fn parse_report(line: &str) -> Option<Report> {
    let (what, id) = line.split_once(' ')?;
    let id = id.parse().ok()?;
    match what {
        "opened" => Some(Report::Opened(id)),
        "clicked" => Some(Report::Clicked(id)),
        _ => None,
    }
}

/// An open submenu: the row it belongs to and where that row is on screen.
#[derive(Clone, Debug, PartialEq)]
struct Open {
    id: i32,
    left: f32,
    right: f32,
    top: f32,
}

#[derive(Default)]
struct State {
    tree: Vec<Entry>,
    /// The open submenus, outermost first.
    open: Vec<Open>,
    /// Where the tray icon is.
    anchor: f32,
}

/// What one panel shows, before it is handed to the UI.
#[derive(Debug, PartialEq)]
struct Panel<'a> {
    entries: &'a [Entry],
    open_id: i32,
    left: f32,
    right: f32,
    top: f32,
}

impl State {
    /// The rows of panel `index`: the menu for 0, then each open submenu.
    fn level(&self, index: usize) -> Option<&[Entry]> {
        let mut level = self.tree.as_slice();
        for open in self.open.get(..index)? {
            level = &level.iter().find(|entry| entry.id == open.id)?.children;
        }
        Some(level)
    }

    /// The panels to draw. Submenus that are gone from the tree are
    /// closed; one that is still empty waits for the app to fill it.
    fn panels(&mut self) -> Vec<Panel<'_>> {
        let mut depth = 0;
        let mut level = self.tree.as_slice();
        for open in &self.open {
            let Some(entry) = level.iter().find(|e| e.id == open.id && e.submenu) else {
                break;
            };
            level = &entry.children;
            depth += 1;
        }
        self.open.truncate(depth);

        let open_id = |index: usize| self.open.get(index).map_or(-1, |open| open.id);
        let mut panels = vec![Panel {
            entries: &self.tree,
            open_id: open_id(0),
            left: self.anchor,
            right: self.anchor,
            top: 0.0,
        }];
        for (index, open) in self.open.iter().enumerate() {
            let entries = self.level(index + 1).unwrap_or_default();
            if entries.is_empty() {
                break;
            }
            panels.push(Panel {
                entries,
                open_id: open_id(index + 1),
                left: open.left,
                right: open.right,
                top: open.top,
            });
        }
        panels
    }

    /// The pointer came over a row of panel `index`. Returns the id to
    /// report as opened, if a submenu opened, and whether anything changed.
    fn entered(&mut self, index: usize, open: Open) -> (Option<i32>, bool) {
        let Some((opens, separator)) = self
            .level(index)
            .and_then(|level| level.iter().find(|entry| entry.id == open.id))
            .map(|entry| (entry.submenu && entry.enabled, entry.separator))
        else {
            return (None, false);
        };
        if opens {
            if self.open.get(index).is_some_and(|held| held.id == open.id) {
                return (None, false);
            }
            let id = open.id;
            self.open.truncate(index);
            self.open.push(open);
            (Some(id), true)
        } else if self.open.len() > index && !separator {
            // Another row of the same panel: its submenu closes.
            self.open.truncate(index);
            (None, true)
        } else {
            (None, false)
        }
    }

    /// Whether choosing this row means anything.
    fn can_choose(&self, id: i32) -> bool {
        menu::find(&self.tree, id).is_some_and(|entry| entry.enabled && !entry.submenu)
    }
}

fn rows(entries: &[Entry]) -> ModelRc<MenuRow> {
    let rows: Vec<MenuRow> = entries
        .iter()
        .map(|entry| MenuRow {
            id: entry.id,
            label: entry.label.as_str().into(),
            enabled: entry.enabled,
            separator: entry.separator,
            submenu: entry.submenu,
            check: entry.toggle == Toggle::Check,
            radio: entry.toggle == Toggle::Radio,
            on: entry.on,
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// Brings the UI's panels in line with the state. Panels before `keep`
/// show the same rows as before and keep their row models, so the rows on
/// screen, and the pointer's place among them, are left alone.
fn sync(model: &VecModel<MenuPanel>, state: &mut State, keep: usize) {
    let panels = state.panels();
    for (index, panel) in panels.iter().enumerate() {
        let held = model.row_data(index).filter(|_| index < keep);
        let shown = MenuPanel {
            rows: held.map_or_else(|| rows(panel.entries), |held| held.rows),
            marks: panel
                .entries
                .iter()
                .any(|entry| entry.toggle != Toggle::None),
            open_id: panel.open_id,
            left: panel.left,
            right: panel.right,
            top: panel.top,
        };
        if index < model.row_count() {
            model.set_row_data(index, shown);
        } else {
            model.push(shown);
        }
    }
    while model.row_count() > panels.len() {
        model.remove(model.row_count() - 1);
    }
}

/// Reads one menu. None at the end of input or when it has gone wrong.
fn read_menu(input: &mut impl BufRead) -> Option<Vec<Entry>> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end_matches('\n');
        if line == menu::END {
            return Some(menu::decode(lines.iter().map(String::as_str)));
        }
        if lines.len() < menu::MAX_ENTRIES {
            lines.push(line.to_string());
        }
    }
}

/// The helper's `main`. Returns None when the arguments are not a menu
/// request.
pub fn run(args: &[String]) -> Option<ExitCode> {
    let [flag, output, x] = args else {
        return None;
    };
    if flag != FLAG {
        return None;
    }
    let anchor = x.parse::<i32>().unwrap_or(0).max(0) as f32;
    Some(match show(output, anchor) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-bar: menu: {error}");
            ExitCode::FAILURE
        }
    })
}

fn show(output: &str, anchor: f32) -> Result<(), sengi_layer::Error> {
    let mut input = BufReader::new(std::io::stdin().take(MAX_INPUT));
    // Nothing is put on screen until there is a menu to put there.
    let Some(tree) = read_menu(&mut input) else {
        return Ok(());
    };
    let state = Rc::new(RefCell::new(State {
        tree,
        open: Vec::new(),
        anchor,
    }));
    let model = Rc::new(VecModel::<MenuPanel>::default());
    sync(&model, &mut state.borrow_mut(), 0);

    let app = App::new()?;
    let handle = app.handle();

    // Later versions of the menu, and the bar closing it by closing the
    // pipe, arrive from a thread that does nothing but read.
    let updates = handle.channel({
        let (state, model, handle) = (state.clone(), model.clone(), handle.clone());
        move |update: Option<Vec<Entry>>| match update {
            Some(tree) => {
                let mut state = state.borrow_mut();
                state.tree = tree;
                sync(&model, &mut state, 0);
            }
            None => handle.quit(),
        }
    })?;
    std::thread::spawn(move || loop {
        let update = read_menu(&mut input);
        let ended = update.is_none();
        if updates.send(update).is_err() || ended {
            return;
        }
    });

    // Like the launcher: over everything, covering the output so a click
    // anywhere reaches us, and holding the keyboard for Escape.
    let config = Config::new("sengi-bar-menu")
        .layer(Layer::Overlay)
        .anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT)
        .size(0, 0)
        .exclusive_zone(-1)
        .keyboard(KeyboardMode::Exclusive)
        .outputs(if output.is_empty() {
            Outputs::Auto
        } else {
            Outputs::Named(output.to_string())
        });

    app.run(config, move |context| {
        let window = TrayMenu::new()?;
        window.set_panels(ModelRc::from(model.clone()));

        let handle = context.handle();
        window.on_dismiss({
            let handle = handle.clone();
            move || handle.quit()
        });
        window.on_chosen({
            let (state, handle) = (state.clone(), handle.clone());
            move |id| {
                if state.borrow().can_choose(id) {
                    println!("clicked {id}");
                    handle.quit();
                }
            }
        });
        window.on_entered({
            let (state, model) = (state.clone(), model.clone());
            move |index, id, left, right, top| {
                let Ok(index) = usize::try_from(index) else {
                    return;
                };
                let open = Open {
                    id,
                    left,
                    right,
                    top,
                };
                let mut state = state.borrow_mut();
                let (opened, changed) = state.entered(index, open);
                if let Some(id) = opened {
                    println!("opened {id}");
                }
                if changed {
                    sync(&model, &mut state, index + 1);
                }
            }
        });
        // The keyboard going elsewhere means something else has opened.
        context.surface().on_focus_changed(move |focused| {
            if !focused {
                handle.quit();
            }
        });
        Ok(window)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i32) -> Entry {
        Entry {
            id,
            label: format!("row {id}"),
            enabled: true,
            ..Entry::default()
        }
    }

    fn sub(id: i32, children: Vec<Entry>) -> Entry {
        Entry {
            submenu: true,
            children,
            ..row(id)
        }
    }

    fn open(id: i32) -> Open {
        Open {
            id,
            left: 100.0,
            right: 260.0,
            top: 40.0 + id as f32,
        }
    }

    fn state() -> State {
        State {
            tree: vec![
                row(1),
                sub(2, vec![row(3), sub(4, vec![row(5)])]),
                sub(6, vec![row(7)]),
                Entry {
                    enabled: false,
                    ..sub(8, vec![row(9)])
                },
            ],
            open: Vec::new(),
            anchor: 900.0,
        }
    }

    fn ids(state: &mut State) -> Vec<Vec<i32>> {
        state
            .panels()
            .iter()
            .map(|panel| panel.entries.iter().map(|entry| entry.id).collect())
            .collect()
    }

    #[test]
    fn reports_parse_back() {
        assert_eq!(parse_report("clicked 12"), Some(Report::Clicked(12)));
        assert_eq!(parse_report("opened 3"), Some(Report::Opened(3)));
        assert_eq!(parse_report("clicked"), None);
        assert_eq!(parse_report("clicked twelve"), None);
        assert_eq!(parse_report("deleted 1"), None);
        assert_eq!(parse_report(""), None);
    }

    #[test]
    fn a_closed_menu_is_one_panel_under_the_icon() {
        let mut state = state();
        let panels = state.panels();
        assert_eq!(panels.len(), 1);
        assert_eq!((panels[0].left, panels[0].open_id), (900.0, -1));
    }

    #[test]
    fn entering_a_submenu_row_opens_it_beside_the_row() {
        let mut state = state();
        assert_eq!(state.entered(0, open(2)), (Some(2), true));
        assert_eq!(ids(&mut state), [vec![1, 2, 6, 8], vec![3, 4]]);
        let panels = state.panels();
        assert_eq!(panels[0].open_id, 2);
        assert_eq!(
            (panels[1].left, panels[1].right, panels[1].top),
            (100.0, 260.0, 42.0)
        );
        // Staying on the row is not news.
        assert_eq!(state.entered(0, open(2)), (None, false));

        // A level deeper.
        assert_eq!(state.entered(1, open(4)), (Some(4), true));
        assert_eq!(ids(&mut state), [vec![1, 2, 6, 8], vec![3, 4], vec![5]]);
    }

    #[test]
    fn moving_to_another_row_closes_what_hung_off_the_old_one() {
        let mut state = state();
        state.entered(0, open(2));
        state.entered(1, open(4));
        // A plain row of the submenu closes the one below it only.
        assert_eq!(state.entered(1, open(3)), (None, true));
        assert_eq!(ids(&mut state).len(), 2);
        // Another submenu of the menu replaces the lot.
        assert_eq!(state.entered(0, open(6)), (Some(6), true));
        assert_eq!(ids(&mut state), [vec![1, 2, 6, 8], vec![7]]);
        assert_eq!(state.entered(0, open(1)), (None, true));
        assert_eq!(ids(&mut state).len(), 1);
        // With nothing open, plain rows change nothing.
        assert_eq!(state.entered(0, open(1)), (None, false));
    }

    #[test]
    fn disabled_submenus_and_unknown_rows_do_not_open() {
        let mut state = state();
        assert_eq!(state.entered(0, open(8)), (None, false));
        assert_eq!(state.entered(0, open(77)), (None, false));
        // A panel that is not there.
        assert_eq!(state.entered(3, open(5)), (None, false));
    }

    #[test]
    fn a_new_tree_keeps_open_what_is_still_there() {
        let mut state = state();
        state.entered(0, open(2));
        state.entered(1, open(4));
        // The app renames a row: everything stays open.
        state.tree[0].label = "renamed".into();
        assert_eq!(ids(&mut state).len(), 3);
        // It drops the inner submenu.
        state.tree[1].children.pop();
        assert_eq!(ids(&mut state), [vec![1, 2, 6, 8], vec![3]]);
        assert_eq!(state.open.len(), 1);
        // And then the outer one.
        state.tree.remove(1);
        assert_eq!(ids(&mut state), [vec![1, 6, 8]]);
        assert!(state.open.is_empty());
    }

    #[test]
    fn an_empty_submenu_waits_to_be_filled() {
        let mut state = state();
        state.tree[2].children.clear();
        assert_eq!(state.entered(0, open(6)), (Some(6), true));
        // Reported as opened, but there is nothing to draw yet.
        assert_eq!(ids(&mut state).len(), 1);
        assert_eq!(state.panels()[0].open_id, 6);
        state.tree[2].children = vec![row(7)];
        assert_eq!(ids(&mut state), [vec![1, 2, 6, 8], vec![7]]);
    }

    #[test]
    fn only_enabled_plain_rows_can_be_chosen() {
        let mut state = state();
        state.tree[0].enabled = false;
        assert!(!state.can_choose(1));
        assert!(!state.can_choose(2));
        assert!(state.can_choose(3));
        assert!(state.can_choose(5));
        assert!(!state.can_choose(77));
    }

    #[test]
    fn menus_are_read_up_to_the_end_marker() {
        let wire = menu::encode(&[row(1), sub(2, vec![row(3)])]);
        let twice = format!("{wire}{wire}");
        let mut input = twice.as_bytes();
        let first = read_menu(&mut input).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[1].children[0].id, 3);
        assert_eq!(read_menu(&mut input).unwrap(), first);
        assert_eq!(read_menu(&mut input), None);
        // Cut off before the marker: not a menu.
        assert_eq!(read_menu(&mut "0\t1\te\thalf".as_bytes()), None);
    }
}
