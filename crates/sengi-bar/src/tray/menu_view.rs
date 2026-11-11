//! Drawing a tray item's menu: `sengi-bar --menu <output> <x>`.
//!
//! sengi-layer gives a process one kind of surface, and the bar's is 28
//! pixels tall. So the bar starts itself again in this mode for each menu,
//! the way sengi-launcher is started for the power menu. The menu arrives
//! on standard input as [`menu::encode`] writes it, and again whenever the
//! app changes it. What the user does is reported on standard output, one
//! line each: `opened <id>` when a submenu is opened and `clicked <id>`
//! when a row is chosen. Closing without choosing says nothing.
//!
//! The menu works like the session menu sengi-launcher draws: one row is
//! highlighted, the pointer or the arrow keys move the highlight, Return
//! or a click chooses, Escape closes. Right and Left go into and out of
//! submenus.

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

/// A key the menu acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    /// Into the highlighted row's submenu.
    Right,
    /// Out of the submenu the highlight is in.
    Left,
    Return,
}

impl Key {
    fn named(name: &str) -> Option<Key> {
        Some(match name {
            "up" => Key::Up,
            "down" => Key::Down,
            "right" => Key::Right,
            "left" => Key::Left,
            "return" => Key::Return,
            _ => return None,
        })
    }
}

/// What a key or the pointer led to.
#[derive(Debug, Default, PartialEq, Eq)]
struct Outcome {
    /// A submenu opened; the bar is told so the app can fill it.
    opened: Option<i32>,
    /// A row was chosen.
    chosen: Option<i32>,
    /// What is on screen needs bringing up to date.
    changed: bool,
}

#[derive(Default)]
struct State {
    tree: Vec<Entry>,
    /// The open submenus, outermost first.
    open: Vec<Open>,
    /// Where the tray icon is.
    anchor: f32,
    /// The highlighted row: which panel it is in and its id.
    selected: Option<(usize, i32)>,
    /// A submenu row the keyboard wants opened. Only the UI knows where
    /// the row is on screen, so it is asked to report the row as if the
    /// pointer had come over it.
    request: Option<(usize, i32)>,
}

/// What one panel shows, before it is handed to the UI.
#[derive(Debug, PartialEq)]
struct Panel<'a> {
    entries: &'a [Entry],
    open_id: i32,
    selected_id: i32,
    request_id: i32,
    left: f32,
    right: f32,
    top: f32,
}

/// Whether the highlight can rest on a row.
fn selectable(entry: &Entry) -> bool {
    entry.enabled && !entry.separator
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

    fn entry(&self, (panel, id): (usize, i32)) -> Option<&Entry> {
        self.level(panel)?.iter().find(|entry| entry.id == id)
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
        // A highlight or a request on a row that is gone is dropped.
        let gone = |state: &State, at: Option<(usize, i32)>| {
            at.is_some_and(|at| !state.entry(at).is_some_and(selectable))
        };
        if gone(self, self.selected) {
            self.selected = None;
        }
        if gone(self, self.request) {
            self.request = None;
        }

        // The id marked in panel `index`, or -1.
        let of = |at: Option<(usize, i32)>, index: usize| match at {
            Some((panel, id)) if panel == index => id,
            _ => -1,
        };
        let open_id = |index: usize| self.open.get(index).map_or(-1, |open| open.id);
        let mut panels = vec![Panel {
            entries: &self.tree,
            open_id: open_id(0),
            selected_id: of(self.selected, 0),
            request_id: of(self.request, 0),
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
                selected_id: of(self.selected, index + 1),
                request_id: of(self.request, index + 1),
                left: open.left,
                right: open.right,
                top: open.top,
            });
        }
        panels
    }

    /// The pointer came over a row of panel `index`, or the UI is
    /// answering a request to say where a row is.
    fn pointed(&mut self, index: usize, at: Open) -> Outcome {
        let key = (index, at.id);
        let Some((opens, rests)) = self
            .entry(key)
            .map(|entry| (entry.submenu && entry.enabled, selectable(entry)))
        else {
            return Outcome::default();
        };
        let asked = self.request.take() == Some(key);
        let mut outcome = Outcome::default();
        if rests && self.selected != Some(key) {
            self.selected = Some(key);
            outcome.changed = true;
        }
        if opens {
            if !self.open.get(index).is_some_and(|held| held.id == at.id) {
                outcome.opened = Some(at.id);
                outcome.changed = true;
                self.open.truncate(index);
                self.open.push(at);
            }
            if asked {
                // Opened from the keyboard: the highlight goes in.
                let first = self
                    .level(index + 1)
                    .and_then(|rows| rows.iter().find(|entry| selectable(entry)))
                    .map(|entry| entry.id);
                if let Some(first) = first {
                    self.selected = Some((index + 1, first));
                }
                outcome.changed = true;
            }
        } else if self.open.len() > index && rests {
            // Another row of the same panel: its submenu closes.
            self.open.truncate(index);
            outcome.changed = true;
        }
        outcome
    }

    /// Moves the highlight `step` rows within its panel, round the ends.
    fn step(&mut self, step: isize) -> Outcome {
        let panel = match self.selected {
            Some((panel, _)) => panel,
            // Nothing highlighted yet: start in the innermost panel.
            None => self.panels().len() - 1,
        };
        let Some(rows) = self.level(panel) else {
            return Outcome::default();
        };
        let stops: Vec<i32> = rows
            .iter()
            .filter(|entry| selectable(entry))
            .map(|entry| entry.id)
            .collect();
        if stops.is_empty() {
            return Outcome::default();
        }
        let held = self
            .selected
            .and_then(|(_, id)| stops.iter().position(|stop| *stop == id));
        let next = match held {
            Some(at) => (at as isize + step).rem_euclid(stops.len() as isize) as usize,
            None if step > 0 => 0,
            None => stops.len() - 1,
        };
        self.selected = Some((panel, stops[next]));
        // The highlight has left the row a submenu hung off.
        if self
            .open
            .get(panel)
            .is_some_and(|open| open.id != stops[next])
        {
            self.open.truncate(panel);
        }
        Outcome {
            changed: true,
            ..Outcome::default()
        }
    }

    fn key(&mut self, key: Key) -> Outcome {
        match key {
            Key::Down => self.step(1),
            Key::Up => self.step(-1),
            Key::Right | Key::Return => {
                let Some(at) = self.selected else {
                    return Outcome::default();
                };
                let Some(entry) = self.entry(at).filter(|entry| selectable(entry)) else {
                    return Outcome::default();
                };
                if entry.submenu {
                    self.request = Some(at);
                    Outcome {
                        changed: true,
                        ..Outcome::default()
                    }
                } else if key == Key::Return {
                    Outcome {
                        chosen: Some(at.1),
                        ..Outcome::default()
                    }
                } else {
                    Outcome::default()
                }
            }
            Key::Left => {
                let Some((panel, _)) = self.selected.filter(|(panel, _)| *panel > 0) else {
                    return Outcome::default();
                };
                // Back to the row this panel hangs off.
                let parent = self.open[panel - 1].id;
                self.open.truncate(panel - 1);
                self.selected = Some((panel - 1, parent));
                Outcome {
                    changed: true,
                    ..Outcome::default()
                }
            }
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
            on: entry.state == menu::State::On,
            mixed: entry.state == menu::State::Mixed,
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
            selected_id: panel.selected_id,
            request_id: panel.request_id,
            left: panel.left,
            right: panel.right,
            top: panel.top,
        };
        if index < model.row_count() {
            if model.row_data(index).as_ref() != Some(&shown) {
                model.set_row_data(index, shown);
            }
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
        anchor,
        ..State::default()
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
    // anywhere reaches us, and holding the keyboard.
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
        // Says what happened and brings the screen up to date. `keep` is
        // the number of panels whose rows cannot have changed.
        let act = Rc::new({
            let (model, handle) = (model.clone(), handle.clone());
            move |state: &mut State, outcome: Outcome, keep: usize| {
                if let Some(id) = outcome.opened {
                    println!("opened {id}");
                }
                if let Some(id) = outcome.chosen.filter(|id| state.can_choose(*id)) {
                    println!("clicked {id}");
                    handle.quit();
                }
                if outcome.changed {
                    sync(&model, state, keep);
                }
            }
        });
        window.on_dismiss({
            let handle = handle.clone();
            move || handle.quit()
        });
        window.on_chosen({
            let (state, act) = (state.clone(), act.clone());
            move |id| {
                let outcome = Outcome {
                    chosen: Some(id),
                    ..Outcome::default()
                };
                act(&mut state.borrow_mut(), outcome, 0);
            }
        });
        window.on_entered({
            let (state, act) = (state.clone(), act.clone());
            move |index, id, left, right, top| {
                let Ok(index) = usize::try_from(index) else {
                    return;
                };
                let at = Open {
                    id,
                    left,
                    right,
                    top,
                };
                let mut state = state.borrow_mut();
                let outcome = state.pointed(index, at);
                act(&mut state, outcome, index + 1);
            }
        });
        window.on_key({
            let (state, act) = (state.clone(), act.clone());
            move |name| {
                let Some(key) = Key::named(name.as_str()) else {
                    return;
                };
                let mut state = state.borrow_mut();
                let outcome = state.key(key);
                // Rows do not change on a key; only what is open and what
                // is highlighted.
                let panels = state.open.len() + 1;
                act(&mut state, outcome, panels);
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

    fn separator(id: i32) -> Entry {
        Entry {
            id,
            separator: true,
            ..Entry::default()
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
                separator(10),
                sub(6, vec![row(7)]),
                Entry {
                    enabled: false,
                    ..sub(8, vec![row(9)])
                },
            ],
            anchor: 900.0,
            ..State::default()
        }
    }

    fn ids(state: &mut State) -> Vec<Vec<i32>> {
        state
            .panels()
            .iter()
            .map(|panel| panel.entries.iter().map(|entry| entry.id).collect())
            .collect()
    }

    const TOP: [i32; 5] = [1, 2, 10, 6, 8];

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
        assert_eq!(panels[0].selected_id, -1);
    }

    #[test]
    fn the_pointer_moves_the_highlight() {
        let mut state = state();
        assert!(state.pointed(0, open(1)).changed);
        assert_eq!(state.panels()[0].selected_id, 1);
        // Staying on the row is not news.
        assert_eq!(state.pointed(0, open(1)), Outcome::default());
        // Separators and disabled rows do not take the highlight.
        assert_eq!(state.pointed(0, open(10)), Outcome::default());
        assert_eq!(state.pointed(0, open(8)), Outcome::default());
        assert_eq!(state.selected, Some((0, 1)));
    }

    #[test]
    fn entering_a_submenu_row_opens_it_beside_the_row() {
        let mut state = state();
        let outcome = state.pointed(0, open(2));
        assert_eq!((outcome.opened, outcome.changed), (Some(2), true));
        assert_eq!(ids(&mut state), [TOP.to_vec(), vec![3, 4]]);
        let panels = state.panels();
        assert_eq!(panels[0].open_id, 2);
        assert_eq!(
            (panels[1].left, panels[1].right, panels[1].top),
            (100.0, 260.0, 42.0)
        );
        // Opened with the pointer, the highlight stays on the row.
        assert_eq!(state.selected, Some((0, 2)));
        assert_eq!(state.pointed(0, open(2)), Outcome::default());

        // A level deeper.
        assert_eq!(state.pointed(1, open(4)).opened, Some(4));
        assert_eq!(ids(&mut state), [TOP.to_vec(), vec![3, 4], vec![5]]);
    }

    #[test]
    fn moving_to_another_row_closes_what_hung_off_the_old_one() {
        let mut state = state();
        state.pointed(0, open(2));
        state.pointed(1, open(4));
        // A plain row of the submenu closes the one below it only.
        assert!(state.pointed(1, open(3)).changed);
        assert_eq!(ids(&mut state).len(), 2);
        // Another submenu of the menu replaces the lot.
        assert_eq!(state.pointed(0, open(6)).opened, Some(6));
        assert_eq!(ids(&mut state), [TOP.to_vec(), vec![7]]);
        assert!(state.pointed(0, open(1)).changed);
        assert_eq!(ids(&mut state).len(), 1);
    }

    #[test]
    fn disabled_submenus_and_unknown_rows_do_not_open() {
        let mut state = state();
        assert_eq!(state.pointed(0, open(8)), Outcome::default());
        assert_eq!(state.pointed(0, open(77)), Outcome::default());
        // A panel that is not there.
        assert_eq!(state.pointed(3, open(5)), Outcome::default());
    }

    #[test]
    fn the_arrow_keys_move_the_highlight_past_what_cannot_be_chosen() {
        let mut state = state();
        // With nothing highlighted, Down starts at the top, Up at the end.
        assert!(state.key(Key::Down).changed);
        assert_eq!(state.selected, Some((0, 1)));
        state.key(Key::Down);
        assert_eq!(state.selected, Some((0, 2)));
        // Over the separator, and round the end past the disabled row.
        state.key(Key::Down);
        assert_eq!(state.selected, Some((0, 6)));
        state.key(Key::Down);
        assert_eq!(state.selected, Some((0, 1)));
        state.key(Key::Up);
        assert_eq!(state.selected, Some((0, 6)));

        let mut fresh = self::state();
        fresh.key(Key::Up);
        assert_eq!(fresh.selected, Some((0, 6)));
    }

    #[test]
    fn return_chooses_the_highlighted_row() {
        let mut state = state();
        // Nothing highlighted: nothing to choose.
        assert_eq!(state.key(Key::Return), Outcome::default());
        state.key(Key::Down);
        assert_eq!(state.key(Key::Return).chosen, Some(1));
        // Right on a plain row does nothing.
        assert_eq!(state.key(Key::Right), Outcome::default());
    }

    #[test]
    fn right_goes_into_a_submenu_and_left_comes_back_out() {
        let mut state = state();
        state.key(Key::Down);
        state.key(Key::Down);
        // The keyboard cannot know where the row is on screen; the UI is
        // asked, and answers as if the pointer were there.
        assert!(state.key(Key::Right).changed);
        assert_eq!(state.panels()[0].request_id, 2);
        let outcome = state.pointed(0, open(2));
        assert_eq!(outcome.opened, Some(2));
        assert_eq!(ids(&mut state), [TOP.to_vec(), vec![3, 4]]);
        // The highlight has gone in, and the request is spent.
        assert_eq!(state.selected, Some((1, 3)));
        assert_eq!(state.panels()[0].request_id, -1);

        // Return on a submenu row does the same as Right.
        state.key(Key::Down);
        assert_eq!(state.selected, Some((1, 4)));
        assert_eq!(state.key(Key::Return).chosen, None);
        state.pointed(1, open(4));
        assert_eq!(state.selected, Some((2, 5)));
        assert_eq!(state.key(Key::Return).chosen, Some(5));

        // Left closes one level at a time and lands on its row.
        state.key(Key::Left);
        assert_eq!((state.selected, state.open.len()), (Some((1, 4)), 1));
        state.key(Key::Left);
        assert_eq!((state.selected, state.open.len()), (Some((0, 2)), 0));
        assert_eq!(state.key(Key::Left), Outcome::default());
    }

    #[test]
    fn moving_the_highlight_off_a_row_closes_its_submenu() {
        let mut state = state();
        state.pointed(0, open(2));
        assert_eq!(ids(&mut state).len(), 2);
        state.key(Key::Down);
        assert_eq!(state.selected, Some((0, 6)));
        assert_eq!(ids(&mut state).len(), 1);
    }

    #[test]
    fn a_new_tree_keeps_open_what_is_still_there() {
        let mut state = state();
        state.pointed(0, open(2));
        state.pointed(1, open(4));
        // The app renames a row: everything stays open.
        state.tree[0].label = "renamed".into();
        assert_eq!(ids(&mut state).len(), 3);
        // It drops the inner submenu, and the highlight that was on it.
        state.tree[1].children.pop();
        assert_eq!(ids(&mut state), [TOP.to_vec(), vec![3]]);
        assert_eq!((state.open.len(), state.selected), (1, None));
        // And then the outer one.
        state.tree.remove(1);
        assert_eq!(ids(&mut state), [vec![1, 10, 6, 8]]);
        assert!(state.open.is_empty());
    }

    #[test]
    fn an_empty_submenu_waits_to_be_filled() {
        let mut state = state();
        state.tree[3].children.clear();
        assert_eq!(state.pointed(0, open(6)).opened, Some(6));
        // Reported as opened, but there is nothing to draw yet.
        assert_eq!(ids(&mut state).len(), 1);
        assert_eq!(state.panels()[0].open_id, 6);
        state.tree[3].children = vec![row(7)];
        assert_eq!(ids(&mut state), [TOP.to_vec(), vec![7]]);
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

    #[test]
    fn a_menu_bigger_than_a_pipe_holds_is_read_whole() {
        // The bar writes it from a thread that may wait; this end only
        // has to cope with the size.
        let long = "\u{1d11e}".repeat(crate::tray::text::MAX_TEXT);
        let many: Vec<Entry> = (0..menu::MAX_ENTRIES as i32)
            .map(|id| Entry {
                label: long.clone(),
                ..row(id + 1)
            })
            .collect();
        let wire = menu::encode(&many);
        assert!(wire.len() > 64 * 1024 && wire.len() < MAX_INPUT as usize);
        let read = read_menu(&mut wire.as_bytes()).unwrap();
        assert_eq!(read.len(), menu::MAX_ENTRIES);
    }
}
