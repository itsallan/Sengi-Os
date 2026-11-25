//! Joins the desk to the Slint component. One desk serves every window, so
//! a desktop on each output shows the same icons and the same selection.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use sengi_layer::Instances;
use slint::{Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::desk::{Desk, Row};
use crate::ui::{Desktop, DesktopItem};

/// How long a notice stays up.
const NOTICE: Duration = Duration::from_secs(4);

pub struct Shell {
    desk: RefCell<Desk>,
    windows: Instances<Desktop>,
    notice: Timer,
}

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
        })
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
        window.on_new_folder({
            let shell = self.clone();
            move || {
                let result = shell.desk.borrow_mut().new_folder();
                shell.sync();
                shell.report(result);
            }
        });
        window.on_new_file({
            let shell = self.clone();
            move || {
                let result = shell.desk.borrow_mut().new_file();
                shell.sync();
                shell.report(result);
            }
        });
        window.on_open_terminal({
            let shell = self.clone();
            move || {
                let result = shell.desk.borrow().terminal();
                shell.report(result);
            }
        });
        window.on_open_settings({
            let shell = self.clone();
            move || {
                let result = shell.desk.borrow().settings();
                shell.report(result);
            }
        });

        let (rows, selected) = {
            let desk = self.desk.borrow();
            (desk.rows(), desk.selected_index())
        };
        update(window, &rows, selected);
    }
}
