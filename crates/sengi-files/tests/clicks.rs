//! The right button and the toolbar of the real window, with pointer events
//! only: every bare part of the folder's side has the folder's menu, a row
//! has its own, and the icon buttons do what their names say.

use std::fs;
use std::path::{Path, PathBuf};

use sengi_apps::Env;
use sengi_files::app::{self, App, Start};
use sengi_files::devices::Sources;
use sengi_files::session::Place;
use sengi_files::ui::{Files, FilesWindow, ThemeSwitch};
use sengi_shots::harness::{self, BACKDROP};
use slint::platform::{PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model};

const FOLDER_MENU: [&str; 4] = ["new-folder", "new-file", "paste", "hidden"];
const ITEM_MENU: [&str; 5] = ["open", "rename", "copy", "cut", "trash"];
/// Where the sidebar ends, and a place clear of the rows' ends.
const SIDE: f32 = 200.0;
const MIDDLE: f32 = 420.0;

type Shown = (FilesWindow, std::rc::Rc<App>);

fn open(root: &Path, place: Place, size: (u32, u32)) -> Shown {
    let home = root.join("home");
    let env = Env::from_vars(|name| match name {
        "HOME" => Some(home.clone().into_os_string()),
        "XDG_DATA_HOME" => Some(home.join(".local/share").into_os_string()),
        "PATH" => Some("/nonexistent".into()),
        _ => None,
    });
    let window = FilesWindow::new().unwrap();
    window.global::<ThemeSwitch>().set_light(harness::light());
    window.show().unwrap();
    harness::resize(&harness::window(), size.0, size.1);
    let start = Start {
        env,
        place,
        notice: None,
        media: None,
        sources: Sources {
            sys_block: root.join("no-sys"),
            mountinfo: root.join("no-mountinfo"),
            udev_data: root.join("no-udev"),
            by_label: root.join("no-labels"),
        },
    };
    let app = app::attach(&window, start, true);
    harness::advance(400);
    (window, app)
}

fn folder(root: &Path, dir: &Path) -> Shown {
    open(root, Place::Folder(dir.to_path_buf()), (888, 600))
}

fn files(w: &FilesWindow) -> Files<'_> {
    w.global::<Files>()
}

fn at((x, y): (f32, f32)) -> LogicalPosition {
    LogicalPosition::new(x, y)
}

fn send(event: WindowEvent) {
    harness::window().dispatch_event(event);
}

fn move_to(spot: (f32, f32)) {
    send(WindowEvent::PointerMoved { position: at(spot) });
    harness::advance(32);
}

fn button(spot: (f32, f32), button: PointerEventButton) {
    move_to(spot);
    send(WindowEvent::PointerPressed {
        position: at(spot),
        button,
    });
    harness::advance(32);
    send(WindowEvent::PointerReleased {
        position: at(spot),
        button,
    });
    harness::advance(250);
}

fn click(spot: (f32, f32)) {
    button(spot, PointerEventButton::Left);
}

fn right(spot: (f32, f32)) {
    button(spot, PointerEventButton::Right);
}

fn type_text(text: &str) {
    for c in text.chars() {
        harness::key(&harness::window(), &c.to_string());
    }
    harness::advance(32);
}

fn enter() {
    type_text(&String::from(char::from(slint::platform::Key::Return)));
}

fn escape() {
    type_text(&String::from(char::from(slint::platform::Key::Escape)));
}

/// The middle of row `n` as the list stands when it is not scrolled.
fn row(w: &FilesWindow, n: usize) -> (f32, f32) {
    let line = w.get_row_height();
    (MIDDLE, w.get_list_top() + line * n as f32 + line / 2.0)
}

fn menu(w: &FilesWindow) -> Vec<String> {
    if !files(w).get_menu_open() {
        return Vec::new();
    }
    files(w)
        .get_menu()
        .iter()
        .filter(|row| !row.separator)
        .map(|row| row.id.to_string())
        .collect()
}

/// A right-click there shows the folder's menu; it is closed again.
fn folder_menu_at(w: &FilesWindow, spot: (f32, f32), what: &str) {
    right(spot);
    assert_eq!(menu(w), FOLDER_MENU, "{what} at {spot:?}");
    let table = files(w).get_menu();
    for id in ["new-folder", "new-file"] {
        assert!(
            table.iter().any(|row| row.id == id && row.enabled),
            "{what}: {id} is off"
        );
    }
    escape();
    assert!(!files(w).get_menu_open());
}

/// Right-clicks `spot` and clicks entry `n` of the menu that opens there
/// (the first four rows have no line between the second and third).
fn choose(w: &FilesWindow, spot: (f32, f32), n: usize) {
    right(spot);
    assert!(files(w).get_menu_open(), "no menu at {spot:?}");
    click((spot.0 + 24.0, spot.1 + 6.0 + 11.0 + 22.0 * n as f32));
    assert!(!files(w).get_menu_open());
}

/// The middle of the n-th toolbar button from the left, from 0.
fn left_tool(n: usize) -> (f32, f32) {
    (SIDE + 8.0 + 15.0 + 32.0 * n as f32, 23.0)
}

/// The same from the right.
fn right_tool(width: f32, n: usize) -> (f32, f32) {
    (width - 8.0 - 15.0 - 32.0 * n as f32, 23.0)
}

fn index_of(app: &App, name: &str) -> usize {
    app.names()
        .iter()
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("{name} is not listed: {:?}", app.names()))
}

fn scroll(spot: (f32, f32), by: f32) {
    move_to(spot);
    send(WindowEvent::PointerScrolled {
        position: at(spot),
        delta_x: 0.0,
        delta_y: by,
    });
    harness::advance(600);
}

fn picture(name: &str) {
    harness::advance(400);
    harness::draw(&harness::window(), BACKDROP).save(name);
}

fn made_up_home() -> PathBuf {
    let root = std::env::temp_dir().join(format!("sengi-files-clicks-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for dir in ["home/Documents/Reports", "home/Empty", "home/long"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for name in ["budget.ods", "notes.txt"] {
        fs::write(root.join("home/Documents").join(name), name).unwrap();
    }
    for n in 0..40 {
        fs::write(root.join(format!("home/long/file {n:02}.txt")), "").unwrap();
    }
    fs::canonicalize(root).unwrap()
}

#[test]
fn the_right_button_and_the_toolbar() {
    harness::install();
    let root = made_up_home();
    let home = root.join("home");
    bare_space(&root, &home);
    new_things(&root, &home);
    scrolled(&root, &home);
    while_renaming(&root, &home);
    the_trash(&root, &home);
    toolbar(&root, &home);
    fs::remove_dir_all(root).unwrap();
}

fn bare_space(root: &Path, home: &Path) {
    for (dir, rows) in [("Documents", 3), ("Empty", 0)] {
        let (w, app) = folder(root, &home.join(dir));
        assert_eq!(app.names().len(), rows);
        let top = w.get_list_top();
        let line = w.get_row_height();
        let below = top + line * rows as f32 + 4.0;
        let spots = [
            ((MIDDLE, below), "just below the last row"),
            ((MIDDLE, 400.0), "the middle of the bare list"),
            ((SIDE + 3.0, 400.0), "the list's left edge"),
            ((884.0, 400.0), "the list's right edge"),
            ((MIDDLE, 562.0), "the foot of the list"),
            ((MIDDLE, 585.0), "the status line"),
            ((884.0, 596.0), "the bottom right corner"),
            ((MIDDLE, top - 12.0), "the column headings"),
            ((660.0, 23.0), "the bare end of the path bar"),
            ((SIDE + 3.0, 3.0), "the top left of the toolbar"),
        ];
        for (spot, what) in spots {
            folder_menu_at(&w, spot, &format!("{dir}: {what}"));
        }
        if rows == 0 {
            // Over the words that say the folder is empty.
            folder_menu_at(&w, (544.0, (top + 568.0) / 2.0), "the empty folder's hint");
            right((MIDDLE, 300.0));
            picture("files-menu-in-empty-folder-888x600.png");
            escape();
        } else {
            // A row has its own, and the ends beside it the folder's.
            for n in 0..rows {
                right(row(&w, n));
                assert_eq!(menu(&w), ITEM_MENU, "row {n}");
                assert_eq!(app.selected_names(), [app.names()[n].clone()]);
                escape();
            }
            folder_menu_at(&w, (SIDE + 3.0, row(&w, 1).1), "left of a row");
            folder_menu_at(&w, (885.0, row(&w, 1).1), "right of a row");

            // With a menu open, a right-click moves it: no second click.
            right((MIDDLE, 420.0));
            assert_eq!(menu(&w), FOLDER_MENU);
            right(row(&w, 1));
            assert_eq!(menu(&w), ITEM_MENU);
            assert_eq!(app.selected_names(), [app.names()[1].clone()]);
            right((MIDDLE, 420.0));
            assert_eq!(menu(&w), FOLDER_MENU);
            assert!(app.selected_names().is_empty());
            // On the sidebar it only closes.
            right((100.0, 500.0));
            assert!(!files(&w).get_menu_open());

            // The pointer went to another window and the keyboard with it.
            send(WindowEvent::PointerExited);
            send(WindowEvent::WindowActiveChanged(false));
            harness::advance(200);
            send(WindowEvent::WindowActiveChanged(true));
            folder_menu_at(&w, (700.0, 430.0), "after coming back");

            // A press and release with no time between them.
            send(WindowEvent::PointerPressed {
                position: at((MIDDLE, 430.0)),
                button: PointerEventButton::Right,
            });
            send(WindowEvent::PointerReleased {
                position: at((MIDDLE, 430.0)),
                button: PointerEventButton::Right,
            });
            harness::advance(250);
            assert_eq!(menu(&w), FOLDER_MENU, "a quick click on bare space");
            escape();
            let spot = row(&w, 2);
            move_to(spot);
            send(WindowEvent::PointerPressed {
                position: at(spot),
                button: PointerEventButton::Right,
            });
            send(WindowEvent::PointerReleased {
                position: at(spot),
                button: PointerEventButton::Right,
            });
            harness::advance(250);
            assert_eq!(menu(&w), ITEM_MENU, "a quick click on a row");
            escape();
        }
    }
    // And in the small window.
    let (w, _app) = open(root, Place::Folder(home.join("Documents")), (720, 480));
    folder_menu_at(&w, (MIDDLE, 400.0), "small: bare list");
    folder_menu_at(&w, (700.0, 468.0), "small: status line");
}

fn new_things(root: &Path, home: &Path) {
    let dir = home.join("Empty");
    let (w, app) = folder(root, &dir);
    // New Folder, in an empty folder.
    choose(&w, (MIDDLE, 300.0), 0);
    assert!(dir.join("untitled folder").is_dir());
    assert_eq!(files(&w).get_renaming(), 0);
    type_text("Inside");
    enter();
    assert!(dir.join("Inside").is_dir());
    // New File, from the space below the new row.
    choose(&w, (MIDDLE, 300.0), 1);
    assert!(dir.join("untitled file").is_file());
    type_text("a.txt");
    enter();
    assert!(dir.join("a.txt").is_file());
    assert_eq!(app.names(), ["Inside", "a.txt"]);
}

fn scrolled(root: &Path, home: &Path) {
    let dir = home.join("long");
    let (w, app) = folder(root, &dir);
    let top = w.get_list_top();
    let line = w.get_row_height();
    // A full folder, part way down: a row is the row under the pointer.
    scroll((MIDDLE, 300.0), -5.0 * line);
    assert!(files(&w).get_first() >= 4, "{}", files(&w).get_first());
    right((MIDDLE, top + line * 2.5));
    assert_eq!(menu(&w), ITEM_MENU);
    assert_eq!(app.selected_names(), ["file 07.txt"]);
    picture("files-menu-scrolled-888x600.png");
    // The same when it replaces an open menu.
    right((MIDDLE + 200.0, top + line * 6.5));
    assert_eq!(app.selected_names(), ["file 11.txt"]);
    escape();
    folder_menu_at(&w, (SIDE + 3.0, 300.0), "scrolled: left of the rows");
    folder_menu_at(&w, (MIDDLE, 585.0), "scrolled: the status line");
    // At the very end there is still bare space under the last row.
    scroll((MIDDLE, 300.0), -100_000.0);
    let bottom = 600.0 - 32.0;
    folder_menu_at(&w, (MIDDLE, bottom - 8.0), "under the last of many rows");
    picture("files-scrolled-end-888x600.png");
    right((MIDDLE, bottom - line * 1.5));
    assert_eq!(menu(&w), ITEM_MENU);
    assert_eq!(app.selected_names(), ["file 39.txt"]);
    escape();
    choose(&w, (SIDE + 3.0, 300.0), 0);
    assert!(dir.join("untitled folder").is_dir());
}

fn while_renaming(root: &Path, home: &Path) {
    let dir = home.join("renames");
    fs::create_dir_all(&dir).unwrap();
    for name in ["b.txt", "c.txt"] {
        fs::write(dir.join(name), name).unwrap();
    }
    let (w, app) = folder(root, &dir);

    // A name is being typed and the bare space is right-clicked: the name is
    // kept and the menu comes.
    choose(&w, (MIDDLE, 400.0), 0);
    assert!(files(&w).get_renaming() >= 0);
    type_text("one");
    right((MIDDLE, 400.0));
    assert_eq!(menu(&w), FOLDER_MENU);
    assert!(dir.join("one").is_dir(), "{:?}", app.names());
    assert_eq!(files(&w).get_renaming(), -1);
    escape();

    // A right-click on another row: that row's menu, though the new name
    // sorts the rows differently.
    choose(&w, (MIDDLE, 400.0), 1);
    type_text("a.txt");
    let c = index_of(&app, "c.txt");
    right(row(&w, c));
    assert_eq!(menu(&w), ITEM_MENU);
    assert!(dir.join("a.txt").is_file());
    assert_eq!(app.selected_names(), ["c.txt"]);
    assert_eq!(app.names(), ["one", "a.txt", "b.txt", "c.txt"]);
    escape();

    // A left click elsewhere keeps the name too.
    choose(&w, (MIDDLE, 400.0), 1);
    type_text("d.txt");
    click((MIDDLE, 450.0));
    assert!(dir.join("d.txt").is_file());
    assert_eq!(files(&w).get_renaming(), -1);

    // A name that is taken keeps the old one and says why.
    choose(&w, (MIDDLE, 400.0), 1);
    type_text("b.txt");
    right((MIDDLE, 400.0));
    assert!(dir.join("untitled file").is_file());
    assert!(
        app.status_text().contains("already exists"),
        "{}",
        app.status_text()
    );
    assert_eq!(menu(&w), FOLDER_MENU);
    escape();
    // And the next right-click works.
    folder_menu_at(&w, (MIDDLE, 420.0), "after a refused name");
}

fn the_trash(root: &Path, home: &Path) {
    let (w, app) = open(root, Place::Trash, (888, 600));
    assert!(app.names().is_empty());
    for spot in [(MIDDLE, 300.0), (MIDDLE, 585.0), (660.0, 23.0)] {
        right(spot);
        assert_eq!(menu(&w), ["empty-trash", "hidden"], "{spot:?}");
        escape();
    }
    drop((w, app));

    let (w, app) = folder(root, &home.join("renames"));
    let b = index_of(&app, "b.txt");
    right(row(&w, b));
    // Move to Trash: the last row, under two lines of nine.
    click((MIDDLE + 24.0, row(&w, b).1 + 6.0 + 11.0 + 4.0 * 22.0 + 18.0));
    assert!(!home.join("renames/b.txt").exists());
    drop((w, app));

    let (w, app) = open(root, Place::Trash, (888, 600));
    assert_eq!(app.names(), ["b.txt"]);
    right(row(&w, 0));
    assert_eq!(menu(&w), ["restore"]);
    escape();
    right((MIDDLE, 400.0));
    assert_eq!(menu(&w), ["empty-trash", "hidden"]);
    escape();
    picture("files-trash-one-888x600.png");

    // Restore and Empty Trash are icons in the toolbar.
    click(right_tool(888.0, 1));
    assert!(files(&w).get_dialog_open(), "Empty Trash did not ask");
    files(&w).set_dialog_open(false);
    w.invoke_take_focus();
    click(row(&w, 0));
    click(right_tool(888.0, 2));
    assert!(home.join("renames/b.txt").exists());
}

fn toolbar(root: &Path, home: &Path) {
    let dir = home.join("tools");
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join(".secret"), "").unwrap();
    let (w, app) = folder(root, &dir);

    // The name comes after a moment on the button, and goes with the pointer.
    assert!(!w.get_tip_shown());
    move_to(right_tool(888.0, 1));
    harness::advance(500);
    assert!(w.get_tip_shown());
    picture("files-tooltip-888x600.png");
    move_to((MIDDLE, 300.0));
    assert!(!w.get_tip_shown());
    move_to(left_tool(2));
    harness::advance(500);
    assert!(w.get_tip_shown());
    picture("files-tooltip-up-888x600.png");

    // Up, Back, Forward.
    click(left_tool(2));
    assert!(!w.get_tip_shown());
    assert_eq!(app.place(), Place::Folder(home.to_path_buf()));
    click(left_tool(0));
    assert_eq!(app.place(), Place::Folder(dir.clone()));
    click(left_tool(1));
    assert_eq!(app.place(), Place::Folder(home.to_path_buf()));
    click(left_tool(0));

    // Hidden files, on and off.
    click(right_tool(888.0, 0));
    assert!(files(&w).get_show_hidden());
    assert!(app.names().contains(&".secret".to_string()));
    move_to((MIDDLE, 300.0));
    picture("files-hidden-on-888x600.png");
    click(right_tool(888.0, 0));
    assert!(!app.names().contains(&".secret".to_string()));

    // New File and New Folder.
    click(right_tool(888.0, 1));
    assert!(dir.join("untitled file").is_file());
    type_text("made.txt");
    enter();
    click(right_tool(888.0, 2));
    assert!(dir.join("untitled folder").is_dir());
    type_text("made");
    // A button pressed while the name is typed keeps it.
    click(right_tool(888.0, 0));
    assert!(dir.join("made").is_dir(), "{:?}", app.names());
    assert_eq!(files(&w).get_renaming(), -1);
    click(right_tool(888.0, 0));
}
