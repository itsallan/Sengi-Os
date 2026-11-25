//! The real Files window, drawn without a display and driven with the
//! keyboard, over a made-up home folder. It checks what the keys do to the
//! files, and writes pictures of the window when SENGI_RENDER_DIR is set.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sengi_apps::Env;
use sengi_files::app::{self, App, Start};
use sengi_files::session::Place;
use sengi_files::ui::{Files, FilesWindow};
use sengi_shots::harness::{self, BACKDROP};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model};

struct Home {
    root: PathBuf,
    home: PathBuf,
}

impl Home {
    fn new() -> Home {
        let root = std::env::temp_dir().join(format!("sengi-files-window-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        for dir in [
            "Desktop",
            "Documents/Reports",
            "Documents/archive",
            "Downloads",
            "Pictures",
            ".local/share",
        ] {
            fs::create_dir_all(home.join(dir)).unwrap();
        }
        let write = |path: &str, text: &str| fs::write(home.join(path), text).unwrap();
        write("Documents/notes.txt", "notes");
        write("Documents/Budget.ods", "budget");
        write("Documents/readme.md", "readme");
        write("Documents/.hidden", "secret");
        write("Documents/Reports/q1.txt", "q1");
        write("Downloads/setup.tar.gz", "archive bytes");
        write("Pictures/holiday.png", "png");
        write("Pictures/Portrait.jpg", "jpg");
        let root = fs::canonicalize(root).unwrap();
        let home = root.join("home");
        Home { root, home }
    }

    fn env(&self, path: &str) -> Env {
        let home = self.home.clone().into_os_string();
        let data = self.home.join(".local/share").into_os_string();
        let path = OsString::from(path);
        Env::from_vars(move |name| match name {
            "HOME" => Some(home.clone()),
            "XDG_DATA_HOME" => Some(data.clone()),
            "PATH" => Some(path.clone()),
            _ => None,
        })
    }

    fn trash(&self) -> PathBuf {
        self.home.join(".local/share/Trash")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn open_window(home: &Home, place: &Path, size: (u32, u32)) -> (FilesWindow, std::rc::Rc<App>) {
    let window = FilesWindow::new().unwrap();
    window.show().unwrap();
    harness::resize(&harness::window(), size.0, size.1);
    let start = Start {
        env: home.env("/nonexistent"),
        place: Place::Folder(place.to_path_buf()),
        notice: None,
    };
    let app = app::attach(&window, start, true);
    harness::advance(400);
    (window, app)
}

fn press(key: Key) {
    type_text(&String::from(char::from(key)));
}

fn type_text(text: &str) {
    let window = harness::window();
    for c in text.chars() {
        harness::key(&window, &c.to_string());
    }
    harness::advance(32);
}

/// A key with modifiers held: they are pressed first and let go after.
fn chord(mods: &[Key], text: &str) {
    let window = harness::window();
    for key in mods {
        window.dispatch_event(WindowEvent::KeyPressed {
            text: char::from(*key).to_string().into(),
        });
    }
    harness::key(&window, text);
    for key in mods.iter().rev() {
        window.dispatch_event(WindowEvent::KeyReleased {
            text: char::from(*key).to_string().into(),
        });
    }
    harness::advance(32);
}

/// A button went down at (x, y) and came up again at (x2, y2), with the
/// pointer going through the points between when they differ.
fn press_release(button: PointerEventButton, from: (f32, f32), to: (f32, f32)) {
    let window = harness::window();
    let at = |(x, y): (f32, f32)| LogicalPosition::new(x, y);
    window.dispatch_event(WindowEvent::PointerMoved { position: at(from) });
    harness::advance(16);
    window.dispatch_event(WindowEvent::PointerPressed {
        position: at(from),
        button,
    });
    harness::advance(16);
    for step in 1..=5 {
        let t = step as f32 / 5.0;
        let mid = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        window.dispatch_event(WindowEvent::PointerMoved { position: at(mid) });
        harness::advance(16);
    }
    window.dispatch_event(WindowEvent::PointerReleased {
        position: at(to),
        button,
    });
    harness::advance(32);
}

fn right_click(at: (f32, f32)) {
    press_release(PointerEventButton::Right, at, at);
}

/// The middle of row `n` of the list in a window of the usual layout:
/// 200 wide places, a bar of buttons 51 high, rows 34 high.
fn row_at(n: usize) -> (f32, f32) {
    (500.0, 51.0 + 34.0 * n as f32 + 17.0)
}

/// The middle of place `n` in the sidebar.
fn place_at(n: usize) -> (f32, f32) {
    (100.0, 8.0 + 36.0 * n as f32 + 17.0)
}

fn menu_ids(w: &FilesWindow) -> Vec<String> {
    files(w)
        .get_menu()
        .iter()
        .filter(|row| !row.separator)
        .map(|row| row.id.to_string())
        .collect()
}

fn picture(name: &str) {
    harness::advance(400);
    harness::draw(&harness::window(), BACKDROP).save(name);
}

fn files(w: &FilesWindow) -> Files<'_> {
    w.global::<Files>()
}

fn names(app: &App) -> Vec<String> {
    app.names()
}

#[test]
fn files_window() {
    harness::install();
    let home = Home::new();

    // The folder, at the two sizes the window is looked at.
    let documents = home.home.join("Documents");
    let (w, app) = open_window(&home, &documents, (888, 600));
    assert_eq!(
        names(&app),
        ["archive", "Reports", "Budget.ods", "notes.txt", "readme.md"]
    );
    assert_eq!(files(&w).get_total(), 5);
    assert_eq!(files(&w).get_rows().row_count(), 5);
    assert!(
        app.status_text().starts_with("5 items"),
        "{}",
        app.status_text()
    );
    press(Key::DownArrow);
    press(Key::DownArrow);
    press(Key::DownArrow);
    assert_eq!(app.selected_names(), ["Budget.ods"]);
    picture("files-folder-888x600.png");
    harness::resize(&harness::window(), 720, 480);
    picture("files-folder-720x480.png");
    harness::resize(&harness::window(), 888, 600);

    // Shift extends the selection, Ctrl+A takes all.
    chord(&[Key::Shift], &String::from(char::from(Key::DownArrow)));
    assert_eq!(app.selected_names(), ["Budget.ods", "notes.txt"]);
    chord(&[Key::Control], "a");
    assert_eq!(app.selected_names().len(), 5);
    press(Key::Escape);
    assert!(app.selected_names().is_empty());

    // Hidden files come with Ctrl+H.
    chord(&[Key::Control], "h");
    assert!(names(&app).contains(&".hidden".to_string()));
    assert!(files(&w).get_show_hidden());
    chord(&[Key::Control], "h");
    assert!(!names(&app).contains(&".hidden".to_string()));

    // Return on a file with no xdg-open says so.
    app.select_named("notes.txt");
    press(Key::Return);
    assert!(
        app.status_text().contains("xdg-open is not installed"),
        "{}",
        app.status_text()
    );
    assert!(files(&w).get_notice_is_error());
    picture("files-notice-888x600.png");

    // A new folder starts out being renamed, with its name selected.
    chord(&[Key::Control, Key::Shift], "N");
    assert!(documents.join("untitled folder").is_dir());
    assert!(files(&w).get_renaming() >= 0);
    picture("files-renaming-888x600.png");
    type_text("Projects");
    press(Key::Return);
    assert!(documents.join("Projects").is_dir());
    assert!(!documents.join("untitled folder").exists());
    assert_eq!(app.selected_names(), ["Projects"]);
    assert_eq!(files(&w).get_renaming(), -1);

    // F2 renames; a name that is taken is refused and the field stays.
    app.select_named("readme.md");
    press(Key::F2);
    assert!(files(&w).get_renaming() >= 0);
    type_text("notes.txt");
    press(Key::Return);
    assert!(
        app.status_text().contains("already exists"),
        "{}",
        app.status_text()
    );
    assert!(files(&w).get_renaming() >= 0);
    assert!(documents.join("readme.md").exists());
    press(Key::Escape);
    assert_eq!(files(&w).get_renaming(), -1);
    press(Key::F2);
    type_text("README.md");
    press(Key::Return);
    assert!(documents.join("README.md").exists());
    assert!(!documents.join("readme.md").exists());

    // Copy and paste into a folder, twice: the second gets another name.
    app.select_named("notes.txt");
    chord(&[Key::Control], "c");
    app.select_named("archive");
    press(Key::Return);
    assert_eq!(app.place(), Place::Folder(documents.join("archive")));
    chord(&[Key::Control], "v");
    chord(&[Key::Control], "v");
    assert_eq!(names(&app), ["notes 2.txt", "notes.txt"]);
    assert_eq!(
        fs::read_to_string(documents.join("archive/notes 2.txt")).unwrap(),
        "notes"
    );
    assert!(documents.join("notes.txt").exists());
    assert!(
        app.status_text().contains("Pasted"),
        "{}",
        app.status_text()
    );

    // Cut and paste moves.
    app.select_named("notes 2.txt");
    chord(&[Key::Control], "x");
    chord(&[Key::Alt], &String::from(char::from(Key::UpArrow)));
    assert_eq!(app.place(), Place::Folder(documents.clone()));
    chord(&[Key::Control], "v");
    assert!(documents.join("notes 2.txt").exists());
    assert!(!documents.join("archive/notes 2.txt").exists());

    // Delete goes to the Trash, with a record.
    app.select_named("notes 2.txt");
    press(Key::Delete);
    assert!(!documents.join("notes 2.txt").exists());
    assert!(home.trash().join("files/notes 2.txt").exists());
    let record = fs::read_to_string(home.trash().join("info/notes 2.txt.trashinfo")).unwrap();
    assert!(record.contains(&format!(
        "Path={}/Documents/notes%202.txt",
        home.home.display()
    )));
    assert!(
        app.status_text().contains("to the Trash"),
        "{}",
        app.status_text()
    );

    // The Trash lists it, restores it, and empties on request.
    app.select_named("Projects");
    press(Key::Delete);
    files(&w).invoke_go_place(files(&w).get_places().row_count() as i32 - 1);
    assert_eq!(app.place(), Place::Trash);
    assert_eq!(names(&app), ["Projects", "notes 2.txt"]);
    picture("files-trash-888x600.png");
    harness::resize(&harness::window(), 720, 480);
    picture("files-trash-720x480.png");
    harness::resize(&harness::window(), 888, 600);
    app.select_named("notes 2.txt");
    files(&w).invoke_restore();
    assert!(documents.join("notes 2.txt").exists());
    assert_eq!(names(&app), ["Projects"]);
    files(&w).invoke_empty_trash();
    assert!(files(&w).get_dialog_open());
    picture("files-empty-trash-888x600.png");
    files(&w).invoke_dialog_cancelled();
    w.invoke_take_focus();
    assert!(home.trash().join("files/Projects").exists());
    files(&w).invoke_empty_trash();
    files(&w).invoke_dialog_confirmed();
    w.invoke_take_focus();
    assert!(!home.trash().join("files/Projects").exists());
    assert!(names(&app).is_empty());
    assert!(documents.join("notes.txt").exists());
    picture("files-trash-empty-888x600.png");

    // Backspace and Alt+Left walk the way back.
    press(Key::Backspace);
    assert_eq!(app.place(), Place::Folder(home.home.clone()));
    chord(&[Key::Alt], &String::from(char::from(Key::LeftArrow)));
    assert_eq!(app.place(), Place::Trash);
    drop(w);
    drop(app);

    // Away from home nothing is changed: not trashed, not renamed, not
    // pasted into.
    let elsewhere = home.root.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("keep.txt"), "keep").unwrap();
    let (w, app) = open_window(&home, &elsewhere, (888, 600));
    press(Key::DownArrow);
    press(Key::Delete);
    assert!(elsewhere.join("keep.txt").exists());
    assert!(
        app.status_text()
            .contains("only changes things inside your home folder"),
        "{}",
        app.status_text()
    );
    press(Key::F2);
    assert_eq!(files(&w).get_renaming(), -1);
    chord(&[Key::Control], "x");
    chord(&[Key::Control, Key::Shift], "N");
    assert!(!elsewhere.join("untitled folder").exists());
    picture("files-refused-888x600.png");
    drop(w);
    drop(app);

    // A folder of ten thousand entries is read whole and only a screenful
    // is given to the window.
    let big = home.home.join("big");
    fs::create_dir_all(&big).unwrap();
    for n in 0..10_000 {
        fs::write(big.join(format!("file {n:05}.txt")), "").unwrap();
    }
    let started = Instant::now();
    let (w, app) = open_window(&home, &big, (888, 600));
    assert_eq!(names(&app).len(), 10_000);
    assert_eq!(files(&w).get_total(), 10_000);
    assert!(files(&w).get_rows().row_count() < 40);
    assert!(
        app.status_text().starts_with("10000 items"),
        "{}",
        app.status_text()
    );
    press(Key::End);
    assert_eq!(app.selected_names(), ["file 09999.txt"]);
    assert!(files(&w).get_first() > 9_900);
    assert!(files(&w).get_rows().row_count() < 40);
    picture("files-big-888x600.png");
    assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
    drop(w);
    drop(app);

    // An empty folder, and one that cannot be read.
    let empty = home.home.join("Desktop");
    let (w, _app) = open_window(&home, &empty, (888, 600));
    assert_eq!(files(&w).get_hint().as_str(), "This folder is empty.");
    picture("files-empty-888x600.png");
    drop(w);
    drop(_app);

    polish(&home);
}

fn row_of(app: &App, name: &str) -> usize {
    names(app)
        .iter()
        .position(|n| n == name)
        .unwrap_or_else(|| panic!("{name} is not listed: {:?}", names(app)))
}

/// The right-click menu, drag and drop, the folder changing under the
/// window, natural order, the way forward, and the glyphs.
fn polish(home: &Home) {
    let play = home.home.join("play");
    fs::create_dir_all(play.join("alpha")).unwrap();
    fs::create_dir_all(play.join("Zed")).unwrap();
    for name in ["file10.txt", "file2.txt", "File1.txt", "note.txt"] {
        fs::write(play.join(name), name).unwrap();
    }
    let (w, app) = open_window(home, &play, (888, 600));

    // Numbers sort as numbers, folders first, case ignored.
    assert_eq!(
        names(&app),
        [
            "alpha",
            "Zed",
            "File1.txt",
            "file2.txt",
            "file10.txt",
            "note.txt"
        ]
    );

    // The glyphs the places and buttons ask for are in the icon folder, and
    // each place names one.
    let icons = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/icons/phosphor/bold");
    let places = files(&w).get_places();
    assert_eq!(places.row_count(), 6);
    for place in places.iter() {
        let file = icons.join(format!("{}-bold.svg", place.icon));
        assert!(file.exists(), "{} has no glyph: {file:?}", place.name);
    }
    for glyph in [
        "arrow-left",
        "arrow-up",
        "folder-plus",
        "arrow-counter-clockwise",
    ] {
        assert!(icons.join(format!("{glyph}-bold.svg")).exists(), "{glyph}");
    }
    picture("files-icons-888x600.png");

    // Right-click on an item selects it and offers its menu.
    right_click(row_at(3));
    assert_eq!(app.selected_names(), ["file2.txt"]);
    assert!(files(&w).get_menu_open());
    assert_eq!(menu_ids(&w), ["open", "rename", "copy", "cut", "trash"]);
    picture("files-menu-item-888x600.png");
    // A click elsewhere only closes it.
    press_release(PointerEventButton::Left, row_at(9), row_at(9));
    assert!(!files(&w).get_menu_open());
    assert_eq!(app.selected_names(), ["file2.txt"]);
    // So does Escape.
    right_click(row_at(3));
    assert!(files(&w).get_menu_open());
    press(Key::Escape);
    assert!(!files(&w).get_menu_open());

    // Copy from the menu, paste from the empty space's menu.
    right_click(row_at(3));
    files(&w).invoke_menu_chosen("copy".into());
    assert!(files(&w).get_can_paste());
    right_click(row_at(12));
    assert!(app.selected_names().is_empty());
    assert_eq!(menu_ids(&w), ["new-folder", "new-file", "paste", "hidden"]);
    assert!(files(&w)
        .get_menu()
        .iter()
        .all(|row| row.enabled || row.separator));
    picture("files-menu-empty-888x600.png");
    files(&w).invoke_menu_chosen("paste".into());
    assert!(play.join("file2 2.txt").exists());
    assert!(names(&app).contains(&"file2 2.txt".to_string()));

    // The Show Hidden switch in the menu.
    fs::write(play.join(".dot"), "").unwrap();
    app::folder_changed(&app);
    assert!(!names(&app).contains(&".dot".to_string()));
    right_click(row_at(12));
    files(&w).invoke_menu_chosen("hidden".into());
    assert!(files(&w).get_show_hidden());
    assert!(names(&app).contains(&".dot".to_string()));
    right_click(row_at(12));
    assert!(files(&w).get_menu().iter().any(|row| row.checked));
    files(&w).invoke_menu_chosen("hidden".into());
    assert!(!names(&app).contains(&".dot".to_string()));

    // A real click on New Folder in the menu: the menu opens at the pointer.
    let spot = row_at(12);
    right_click(spot);
    press_release(
        PointerEventButton::Left,
        (spot.0 + 20.0, spot.1 + 6.0 + 11.0),
        (spot.0 + 20.0, spot.1 + 6.0 + 11.0),
    );
    assert!(play.join("untitled folder").is_dir());
    assert!(files(&w).get_renaming() >= 0);
    type_text("Made");
    press(Key::Return);
    assert!(play.join("Made").is_dir());

    // Rename and trash from an item's menu.
    right_click(row_at(row_of(&app, "note.txt")));
    files(&w).invoke_menu_chosen("rename".into());
    assert!(files(&w).get_renaming() >= 0);
    press(Key::Escape);
    right_click(row_at(row_of(&app, "note.txt")));
    files(&w).invoke_menu_chosen("trash".into());
    assert!(!play.join("note.txt").exists());
    assert!(home.trash().join("files/note.txt").exists());

    // The folder changes under the window: the list follows, the selection
    // stays.
    app.select_named("file10.txt");
    fs::write(play.join("brand new.txt"), "").unwrap();
    fs::remove_file(play.join("file2 2.txt")).unwrap();
    app::folder_changed(&app);
    assert!(names(&app).contains(&"brand new.txt".to_string()));
    assert!(!names(&app).contains(&"file2 2.txt".to_string()));
    assert_eq!(app.selected_names(), ["file10.txt"]);
    // While a name is being typed the list is left alone until it is done.
    app.select_named("brand new.txt");
    press(Key::F2);
    fs::write(play.join("while typing.txt"), "").unwrap();
    app::folder_changed(&app);
    assert!(!names(&app).contains(&"while typing.txt".to_string()));
    assert!(files(&w).get_renaming() >= 0);
    press(Key::Escape);
    assert!(names(&app).contains(&"while typing.txt".to_string()));
    assert_eq!(app.selected_names(), ["brand new.txt"]);

    // Drag a file onto a folder row.
    let from = row_of(&app, "file10.txt");
    let onto = row_of(&app, "alpha");
    press_release(PointerEventButton::Left, row_at(from), row_at(onto));
    assert!(play.join("alpha/file10.txt").exists());
    assert!(!play.join("file10.txt").exists());
    assert!(!names(&app).contains(&"file10.txt".to_string()));
    // A plain click on a row, with no drag, still selects it.
    press_release(
        PointerEventButton::Left,
        row_at(row_of(&app, "Zed")),
        row_at(row_of(&app, "Zed")),
    );
    assert_eq!(app.selected_names(), ["Zed"]);
    // Dropping on a file does nothing.
    let file = row_of(&app, "File1.txt");
    press_release(PointerEventButton::Left, row_at(file), row_at(file + 1));
    assert!(play.join("File1.txt").exists());

    // Drag onto a place in the sidebar, with a look at it half way.
    let window = harness::window();
    let file = row_of(&app, "File1.txt");
    let start = row_at(file);
    let target = place_at(2);
    window.dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(start.0, start.1),
    });
    window.dispatch_event(WindowEvent::PointerPressed {
        position: LogicalPosition::new(start.0, start.1),
        button: PointerEventButton::Left,
    });
    for step in 1..=6 {
        let t = step as f32 / 6.0;
        window.dispatch_event(WindowEvent::PointerMoved {
            position: LogicalPosition::new(
                start.0 + (target.0 - start.0) * t,
                start.1 + (target.1 - start.1) * t,
            ),
        });
        harness::advance(16);
    }
    picture("files-dragging-888x600.png");
    window.dispatch_event(WindowEvent::PointerReleased {
        position: LogicalPosition::new(target.0, target.1),
        button: PointerEventButton::Left,
    });
    harness::advance(32);
    assert!(home.home.join("Documents/File1.txt").exists());
    assert!(!play.join("File1.txt").exists());
    assert!(app.status_text().contains("Moved"), "{}", app.status_text());

    // Back, forward, and the keys for them.
    files(&w).invoke_go_place(2);
    assert_eq!(app.place(), Place::Folder(home.home.join("Documents")));
    chord(&[Key::Alt], &String::from(char::from(Key::LeftArrow)));
    assert_eq!(app.place(), Place::Folder(play.clone()));
    chord(&[Key::Alt], &String::from(char::from(Key::RightArrow)));
    assert_eq!(app.place(), Place::Folder(home.home.join("Documents")));
    chord(&[Key::Alt], &String::from(char::from(Key::RightArrow)));
    assert_eq!(app.place(), Place::Folder(home.home.join("Documents")));
    picture("files-documents-888x600.png");
    harness::resize(&harness::window(), 720, 480);
    picture("files-documents-720x480.png");
    drop(w);
    drop(app);

    // A folder that cannot be moved is not moved by dragging it either.
    let (w, app) = open_window(home, &home.home, (888, 600));
    let from = row_of(&app, "Documents");
    let onto = row_of(&app, "Downloads");
    press_release(PointerEventButton::Left, row_at(from), row_at(onto));
    assert!(home.home.join("Documents").is_dir());
    assert!(!home.home.join("Downloads/Documents").exists());
    assert!(
        app.status_text().contains("cannot be moved"),
        "{}",
        app.status_text()
    );
    assert!(files(&w).get_notice_is_error());
    picture("files-drag-refused-888x600.png");
}
