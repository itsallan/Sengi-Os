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
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model};

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
}
