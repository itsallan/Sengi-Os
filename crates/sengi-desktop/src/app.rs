//! Joins the desk to the Slint component. One desk serves every window, so
//! a desktop on each output shows the same icons and the same selection.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use sengi_layer::Instances;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::desk::{Desk, Row};
use crate::menu::{Choice, Request};
use crate::ui::{Desktop, DesktopItem};

/// How long a notice stays up.
const NOTICE: Duration = Duration::from_secs(4);

pub struct Shell {
    desk: RefCell<Desk>,
    windows: Instances<Desktop>,
    notice: Timer,
    /// Shows a menu, or with `None` takes away the one that is shown.
    menus: RefCell<Option<Menus>>,
}

type Menus = Box<dyn FnMut(Option<Request>) -> Result<(), String>>;

fn item(row: &Row) -> DesktopItem {
    DesktopItem {
        name: SharedString::from(row.name.as_str()),
        folder: row.folder,
        selected: row.selected,
        renaming: row.renaming,
    }
}

/// Brings one window's list up to date, touching only the rows that
/// changed: an icon being renamed must not be rebuilt under the typing.
fn update(window: &Desktop, rows: &[Row], selected: i32) {
    let model = window.get_items();
    let Some(list) = model.as_any().downcast_ref::<VecModel<DesktopItem>>() else {
        window.set_items(ModelRc::new(VecModel::from(
            rows.iter().map(item).collect::<Vec<_>>(),
        )));
        window.set_selected(selected);
        return;
    };
    for (index, row) in rows.iter().enumerate() {
        let wanted = item(row);
        if index >= list.row_count() {
            list.push(wanted);
        } else if list.row_data(index).as_ref() != Some(&wanted) {
            list.set_row_data(index, wanted);
        }
    }
    while list.row_count() > rows.len() {
        list.remove(list.row_count() - 1);
    }
    window.set_selected(selected);
}

impl Shell {
    pub fn new(desk: Desk) -> Rc<Shell> {
        Rc::new(Shell {
            desk: RefCell::new(desk),
            windows: Instances::new(),
            notice: Timer::default(),
            menus: RefCell::new(None),
        })
    }

    /// Says how menus are shown and taken away.
    pub fn set_menus(&self, menus: impl FnMut(Option<Request>) -> Result<(), String> + 'static) {
        *self.menus.borrow_mut() = Some(Box::new(menus));
    }

    fn menu(&self, request: Option<Request>) {
        let result = match self.menus.borrow_mut().as_mut() {
            Some(menus) => menus(request),
            None => Ok(()),
        };
        self.report(result);
    }

    /// A row of a menu was chosen. A menu of an icon is for the selected
    /// one, which the press that asked for it selected.
    pub fn chosen(&self, choice: Choice) {
        let result = {
            let mut desk = self.desk.borrow_mut();
            let selected = desk.selected_index();
            match choice {
                Choice::NewFolder => desk.new_folder(),
                Choice::NewFile => desk.new_file(),
                Choice::Terminal => desk.terminal(),
                Choice::Settings => desk.settings(),
                Choice::Open => desk.open(selected),
                Choice::Rename => {
                    desk.begin_rename(selected);
                    Ok(())
                }
                Choice::Trash => desk.trash(selected),
            }
        };
        self.sync();
        self.report(result);
    }

    /// Shows what the folder holds in every window.
    fn sync(&self) {
        let (rows, selected) = {
            let desk = self.desk.borrow();
            (desk.rows(), desk.selected_index())
        };
        self.windows
            .for_each(|window| update(window, &rows, selected));
    }

    /// The folder changed on disk.
    pub fn refresh(&self) {
        self.desk.borrow_mut().refresh();
        self.sync();
    }

    /// Shows a line of text for a few seconds.
    pub fn say(&self, text: &str) {
        let text = SharedString::from(text);
        self.windows
            .for_each(|window| window.set_notice(text.clone()));
        let windows = self.windows.clone();
        self.notice.start(TimerMode::SingleShot, NOTICE, move || {
            windows.for_each(|window| window.set_notice(SharedString::new()));
        });
    }

    fn report(&self, result: Result<(), String>) {
        if let Err(text) = result {
            self.say(&text);
        }
    }

    /// Wires a window to the desk. The window then shows the icons and its
    /// requests are carried out.
    pub fn attach(self: &Rc<Self>, window: &Desktop) {
        self.windows.add(window);

        window.on_select({
            let shell = self.clone();
            move |index| {
                shell.desk.borrow_mut().select(index);
                shell.sync();
            }
        });
        window.on_open_item({
            let shell = self.clone();
            move |index| {
                let result = shell.desk.borrow().open(index);
                shell.report(result);
            }
        });
        window.on_begin_rename({
            let shell = self.clone();
            move |index| {
                shell.desk.borrow_mut().begin_rename(index);
                shell.sync();
            }
        });
        window.on_commit_rename({
            let shell = self.clone();
            move |_, text| {
                let result = shell.desk.borrow_mut().commit_rename(text.as_str());
                shell.sync();
                shell.report(result);
            }
        });
        window.on_cancel_rename({
            let shell = self.clone();
            move || {
                shell.desk.borrow_mut().cancel_rename();
                shell.sync();
            }
        });
        window.on_trash_item({
            let shell = self.clone();
            move |index| {
                let result = shell.desk.borrow_mut().trash(index);
                shell.sync();
                shell.report(result);
            }
        });
        window.on_menu({
            let shell = self.clone();
            let window = window.as_weak();
            move |x, y, item| {
                let output = window
                    .upgrade()
                    .map_or_else(SharedString::new, |window| window.get_output());
                shell.menu(Some(Request {
                    output: output.into(),
                    x,
                    y,
                    item,
                }));
            }
        });
        window.on_menu_done({
            let shell = self.clone();
            move || shell.menu(None)
        });

        let (rows, selected) = {
            let desk = self.desk.borrow();
            (desk.rows(), desk.selected_index())
        };
        update(window, &rows, selected);
    }
}
