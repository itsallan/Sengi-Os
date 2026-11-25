//! The notification centre with its history in groups, the dock while a
//! tile is dragged, and the launcher's right-click menu. The pointer and
//! the keyboard are driven through the real components, so what the
//! callbacks report is what the programs get.

use std::cell::RefCell;
use std::rc::Rc;

use sengi_shots::harness::{self, BACKDROP};
use sengi_shots::{dock, launcher, notify};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{
    ComponentHandle, Image, LogicalPosition, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel,
};

fn list<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn swatch(size: u32, rgb: [u8; 3]) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(size, size);
    for pixel in buffer.make_mut_slice() {
        *pixel = Rgba8Pixel {
            r: rgb[0],
            g: rgb[1],
            b: rgb[2],
            a: 255,
        };
    }
    Image::from_rgba8(buffer)
}

fn at(x: f32, y: f32) -> LogicalPosition {
    LogicalPosition::new(x, y)
}

fn press(
    win: &slint::platform::software_renderer::MinimalSoftwareWindow,
    x: f32,
    y: f32,
    button: PointerEventButton,
) {
    win.dispatch_event(WindowEvent::PointerMoved { position: at(x, y) });
    win.dispatch_event(WindowEvent::PointerPressed {
        position: at(x, y),
        button,
    });
}

fn release(
    win: &slint::platform::software_renderer::MinimalSoftwareWindow,
    x: f32,
    y: f32,
    button: PointerEventButton,
) {
    win.dispatch_event(WindowEvent::PointerReleased {
        position: at(x, y),
        button,
    });
}

// The notification centre ---------------------------------------------------

fn line(key: i32, time: &str, summary: &str, body: &str, critical: bool) -> notify::HistoryItem {
    notify::HistoryItem {
        key,
        time: time.into(),
        summary: summary.into(),
        body: body.into(),
        critical,
        ..Default::default()
    }
}

fn heading(app: &str, count: i32) -> notify::HistoryItem {
    notify::HistoryItem {
        key: -1,
        header: true,
        count,
        group: app.into(),
        app: app.into(),
        ..Default::default()
    }
}

fn grouped() -> Vec<notify::HistoryItem> {
    vec![
        heading("Mail", 3),
        line(
            1,
            "now",
            "Lunch on Friday?",
            "Sam: are you free around one?",
            false,
        ),
        line(
            2,
            "2 min ago",
            "Your invoice is ready",
            "Invoice 1042 from Acme is attached.",
            false,
        ),
        line(3, "yesterday", "Welcome to Sengi OS", "", false),
        heading("Calendar", 1),
        line(4, "1 hour ago", "Stand-up in 5 minutes", "Room 4", false),
        heading("Battery", 1),
        line(
            5,
            "3 hours ago",
            "Battery low",
            "9% left. Plug in the charger.",
            true,
        ),
    ]
}

#[test]
fn the_centre_groups_by_app_and_clears_a_group() {
    harness::install();
    let w = notify::NotifyWindow::new().unwrap();
    w.set_history(list(grouped()));
    w.set_centre_open(true);
    w.set_centre_focus(-1);
    let cleared = Rc::new(RefCell::new(Vec::<String>::new()));
    let dismissed = Rc::new(RefCell::new(Vec::<i32>::new()));
    w.on_group_cleared({
        let cleared = cleared.clone();
        move |app| cleared.borrow_mut().push(app.to_string())
    });
    w.on_history_dismissed({
        let dismissed = dismissed.clone();
        move |key| dismissed.borrow_mut().push(key)
    });
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 1366, 768);
    harness::advance(400);
    harness::draw(&win, BACKDROP).save("notify-centre-grouped-1366x768.png");

    // The keyboard is on the first heading: Delete clears that group, and
    // Return does too.
    w.invoke_take_keyboard();
    w.set_centre_focus(2);
    harness::key(&win, "\u{7f}");
    w.set_centre_focus(6);
    harness::key(&win, "\n");
    // And on a line it removes that line only.
    w.set_centre_focus(3);
    harness::key(&win, "\u{7f}");
    assert_eq!(*cleared.borrow(), ["Mail", "Calendar"]);
    assert_eq!(*dismissed.borrow(), [1]);

    // The picture with the keyboard on a heading.
    w.set_centre_focus(6);
    harness::advance(100);
    harness::draw(&win, BACKDROP).save("notify-centre-grouped-focus-1366x768.png");
}

// The dock --------------------------------------------------------------------

struct Events {
    clicked: Vec<i32>,
    reordered: Vec<(i32, i32)>,
    removed: Vec<i32>,
    drags: Vec<bool>,
}

fn dock_with_pins() -> (dock::Dock, Rc<RefCell<Events>>) {
    let w = dock::Dock::new().unwrap();
    let tile = |name: &str, k: usize, pinned: bool| dock::DockItem {
        name: name.into(),
        icon: dock::DockIcon::Image,
        image: swatch(
            40,
            [[200, 80, 60], [60, 160, 90], [70, 100, 200], [220, 180, 60]][k % 4],
        ),
        present: true,
        pinned,
        running: k == 1,
        ..Default::default()
    };
    w.set_items(list(vec![
        dock::DockItem {
            name: "Search".into(),
            icon: dock::DockIcon::Search,
            present: true,
            ..Default::default()
        },
        tile("Files", 0, true),
        tile("Notes", 1, true),
        tile("Mail", 2, true),
        tile("Terminal", 3, true),
        // Running but not kept in the dock.
        tile("Calculator", 4, false),
    ]));
    let events = Rc::new(RefCell::new(Events {
        clicked: Vec::new(),
        reordered: Vec::new(),
        removed: Vec::new(),
        drags: Vec::new(),
    }));
    w.on_clicked({
        let events = events.clone();
        move |row| events.borrow_mut().clicked.push(row)
    });
    w.on_reordered({
        let events = events.clone();
        move |from, to| events.borrow_mut().reordered.push((from, to))
    });
    w.on_removed({
        let events = events.clone();
        move |row| events.borrow_mut().removed.push(row)
    });
    w.on_drag_changed({
        let events = events.clone();
        move |on| events.borrow_mut().drags.push(on)
    });
    (w, events)
}

/// Where the middle of row `row` is in a window `width` wide, and the
/// height of the middle of the panel.
fn centre_of(w: &dock::Dock, width: f32, row: usize) -> (f32, f32) {
    let left = (width - w.get_panel_width()) / 2.0 + w.get_side_padding();
    let x = left + w.get_slot_width() * (row as f32 + 0.5);
    let y = w.get_dock_height() - w.get_panel_height() / 2.0;
    (x, y)
}

fn drag(
    win: &slint::platform::software_renderer::MinimalSoftwareWindow,
    from: (f32, f32),
    to: (f32, f32),
) {
    press(win, from.0, from.1, PointerEventButton::Left);
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        let (x, y) = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        win.dispatch_event(WindowEvent::PointerMoved { position: at(x, y) });
        harness::advance(16);
    }
}

#[test]
fn a_pinned_tile_can_be_dragged_to_another_place_or_out_of_the_dock() {
    harness::install();
    let (w, events) = dock_with_pins();
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 1366, w.get_dock_height().ceil() as u32);
    harness::advance(400);
    let width = 1366.0;

    // A click that does not move is a click and nothing else.
    let files = centre_of(&w, width, 1);
    press(&win, files.0, files.1, PointerEventButton::Left);
    release(&win, files.0, files.1, PointerEventButton::Left);
    harness::advance(50);
    assert_eq!(events.borrow().clicked, [1]);
    assert!(events.borrow().drags.is_empty());

    // Dragging Files (row 1) over Mail (row 3) shows where it will land.
    let mail = centre_of(&w, width, 3);
    drag(&win, files, mail);
    assert!(w.get_dragging(), "the drag began");
    assert_eq!(w.get_drag_from(), 1);
    harness::draw(&win, BACKDROP).save("dock-drag-reorder-1366.png");
    release(&win, mail.0, mail.1, PointerEventButton::Left);
    harness::advance(50);
    {
        let events = events.borrow();
        assert_eq!(events.reordered, [(1, 3)]);
        assert_eq!(events.clicked, [1], "letting go of a drag is no click");
        assert_eq!(events.drags, [true, false]);
        assert!(events.removed.is_empty());
    }
    assert!(!w.get_dragging());

    // Dragging a tile up and out of the dock removes it, and says so.
    let notes = centre_of(&w, width, 2);
    let above = (notes.0, w.get_dock_height() - w.get_panel_height() - 36.0);
    drag(&win, notes, above);
    harness::draw(&win, BACKDROP).save("dock-drag-remove-1366.png");
    release(&win, above.0, above.1, PointerEventButton::Left);
    harness::advance(50);
    assert_eq!(events.borrow().removed, [2]);
    assert_eq!(events.borrow().reordered, [(1, 3)]);

    // The search button and the running-only app are no places to drop.
    // Going past either end puts the tile at the end of the pinned ones.
    let terminal = centre_of(&w, width, 4);
    let search = centre_of(&w, width, 0);
    drag(&win, terminal, search);
    release(&win, search.0, search.1, PointerEventButton::Left);
    harness::advance(50);
    let files = centre_of(&w, width, 1);
    let calculator = centre_of(&w, width, 5);
    drag(&win, files, calculator);
    release(&win, calculator.0, calculator.1, PointerEventButton::Left);
    harness::advance(50);
    // Never row 0, never row 5.
    assert_eq!(events.borrow().reordered, [(1, 3), (4, 1), (1, 4)]);
}

#[test]
fn only_pinned_tiles_can_be_dragged() {
    harness::install();
    let (w, events) = dock_with_pins();
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 1366, w.get_dock_height().ceil() as u32);
    harness::advance(400);
    let search = centre_of(&w, 1366.0, 0);
    let target = centre_of(&w, 1366.0, 3);
    drag(&win, search, target);
    assert!(!w.get_dragging());
    release(&win, target.0, target.1, PointerEventButton::Left);
    let calculator = centre_of(&w, 1366.0, 5);
    drag(&win, calculator, target);
    assert!(!w.get_dragging());
    release(&win, target.0, target.1, PointerEventButton::Left);
    assert!(events.borrow().reordered.is_empty());
    assert!(events.borrow().removed.is_empty());
}

// The launcher --------------------------------------------------------------------

#[test]
fn a_right_click_on_an_app_opens_a_menu_and_a_left_click_does_not() {
    harness::install();
    let rows: Vec<launcher::Row> = (0..24)
        .map(|n| launcher::Row {
            title: format!("Application {n}").into(),
            icon: swatch(48, [[200, 80, 60], [60, 160, 90], [70, 100, 200]][n % 3]),
            has_icon: n % 4 != 0,
            letter: "A".into(),
            ..Default::default()
        })
        .collect();
    let w = launcher::LauncherWindow::new().unwrap();
    w.set_grid_capable(true);
    w.set_grid(true);
    w.set_icons(true);
    w.set_rows(list(rows));
    let asked = Rc::new(RefCell::new(Vec::<i32>::new()));
    let chosen = Rc::new(RefCell::new(Vec::<i32>::new()));
    let clicked = Rc::new(RefCell::new(Vec::<i32>::new()));
    // What the program does: fill in the rows of the menu.
    w.on_context_requested({
        let (weak, asked) = (w.as_weak(), asked.clone());
        move |row| {
            asked.borrow_mut().push(row);
            let w = weak.upgrade().unwrap();
            w.set_menu_rows(list(vec!["Open".into(), "Keep in Dock".into()]));
            w.set_menu_row(row);
        }
    });
    w.on_menu_chosen({
        let chosen = chosen.clone();
        move |row| chosen.borrow_mut().push(row)
    });
    w.on_row_clicked({
        let clicked = clicked.clone();
        move |row| clicked.borrow_mut().push(row)
    });
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 1280, 720);
    harness::advance(400);

    // The panel is 640 wide, in the middle, and its grid starts 57 below
    // its top; the panel is 465 tall, in the middle. Cells are 104 by 100.
    let cell = |index: f32| {
        (
            320.0 + 8.0 + 104.0 * (index % 6.0) + 52.0,
            127.5 + 57.0 + 100.0 * (index / 6.0).floor() + 50.0,
        )
    };
    let second = cell(1.0);
    press(&win, second.0, second.1, PointerEventButton::Right);
    release(&win, second.0, second.1, PointerEventButton::Right);
    harness::advance(100);
    assert_eq!(*asked.borrow(), [1]);
    assert!(clicked.borrow().is_empty(), "a right click is not a pick");
    assert_eq!(w.get_menu_row(), 1);
    harness::draw(&win, BACKDROP).save("launcher-menu-1280x720.png");

    // A left click on the second row of the menu chooses it. Rows are 22
    // high with 6 around them.
    let row = (second.0 + 40.0, second.1 + 6.0 + 22.0 * 1.5);
    press(&win, row.0, row.1, PointerEventButton::Left);
    release(&win, row.0, row.1, PointerEventButton::Left);
    harness::advance(50);
    assert_eq!(*chosen.borrow(), [1]);
    assert!(clicked.borrow().is_empty());

    // A left click elsewhere only closes the menu.
    w.set_menu_row(-1);
    let other = cell(8.0);
    press(&win, other.0, other.1, PointerEventButton::Right);
    release(&win, other.0, other.1, PointerEventButton::Right);
    harness::advance(50);
    assert_eq!(w.get_menu_row(), 8);
    let away = cell(20.0);
    press(&win, away.0, away.1, PointerEventButton::Left);
    release(&win, away.0, away.1, PointerEventButton::Left);
    harness::advance(50);
    assert_eq!(w.get_menu_row(), -1, "the click closed the menu");
    assert!(clicked.borrow().is_empty(), "and picked nothing");
}
