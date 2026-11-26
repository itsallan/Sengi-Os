//! The real editor window, drawn without a display and driven with the
//! keyboard. It checks what the keys do to the text and the file, and writes
//! pictures of the window when SENGI_RENDER_DIR is set.

use std::fs;
use std::path::{Path, PathBuf};

use sengi_edit::app::{self, App, Start};
use sengi_edit::doc::MAX_BYTES;
use sengi_edit::ui::{Edit, EditWindow, ThemeSwitch};
use sengi_shots::harness::{self, BACKDROP};
use slint::platform::{Key, WindowEvent};
use slint::ComponentHandle;

struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        let path = std::env::temp_dir().join(format!("sengi-edit-window-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Dir(fs::canonicalize(path).unwrap())
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open_window(dir: &Dir, path: Option<&Path>) -> (EditWindow, std::rc::Rc<App>) {
    let window = EditWindow::new().unwrap();
    sengi_shots::apply_look!(window.global::<ThemeSwitch>());
    window.show().unwrap();
    harness::resize(&harness::window(), 888, 600);
    let app = app::attach(
        &window,
        Start {
            home: dir.0.clone(),
            path: path.map(Path::to_path_buf),
        },
    );
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

const NOTES: &str = "Sengi OS notes\n\nA short file to open, change and save again.\nThe second line is here to be found: needle.\n\nA long line follows so that the wrapping can be seen at the size of the window: it goes on and on and on, past the right edge, with more words than fit in one line of this window, and then some.\n\nEnd of file.\n";

#[test]
fn editor_window() {
    harness::install();
    let dir = Dir::new();
    let notes = dir.file("notes.txt", NOTES.as_bytes());

    // The text, as it is on disk.
    let (w, app) = open_window(&dir, Some(&notes));
    assert_eq!(app.text().unwrap(), NOTES);
    assert!(!app.is_dirty());
    assert_eq!(app.status_text(), "Line 1, column 1");
    assert_eq!(
        w.global::<Edit>().get_title(),
        "notes.txt \u{2014} Text Editor"
    );
    picture("edit-text-888x600.png");

    // Typing changes it and marks it; undo and redo are the toolkit's.
    type_text("Hi ");
    assert_eq!(&app.text().unwrap()[..17], "Hi Sengi OS notes");
    assert!(app.is_dirty());
    assert!(w.global::<Edit>().get_title().starts_with("* notes.txt"));
    assert_eq!(app.status_text(), "Line 1, column 4");
    chord(&[Key::Control], "z");
    assert_eq!(app.text().unwrap(), NOTES, "undo");
    assert!(!app.is_dirty());
    chord(&[Key::Control, Key::Shift], "Z");
    assert!(app.text().unwrap().starts_with("Hi Sengi"), "redo");
    assert!(app.is_dirty());

    // Ctrl+S writes it and clears the mark.
    chord(&[Key::Control], "s");
    assert_eq!(fs::read_to_string(&notes).unwrap(), format!("Hi {NOTES}"));
    assert!(!app.is_dirty());
    assert!(
        app.status_text().starts_with("Saved"),
        "{}",
        app.status_text()
    );
    picture("edit-saved-888x600.png");

    // Ctrl+F finds without regard to case and selects what it found; the
    // same key again and Return go on to the next, round to the first.
    chord(&[Key::Control], "f");
    assert!(w.global::<Edit>().get_find_open());
    type_text("NEEDLE");
    assert_eq!(w.global::<Edit>().get_find_result(), "1 of 1");
    let at = app.text().unwrap().find("needle").unwrap() as i32;
    assert_eq!(w.global::<Edit>().get_anchor(), at);
    assert_eq!(w.global::<Edit>().get_cursor(), at + 6);
    picture("edit-find-888x600.png");
    press(Key::Return);
    assert_eq!(w.global::<Edit>().get_anchor(), at);
    type_text("x");
    assert_eq!(w.global::<Edit>().get_find_result(), "No match");
    press(Key::Escape);
    assert!(!w.global::<Edit>().get_find_open());

    // Closing with changes asks first; Escape keeps editing.
    type_text("!");
    assert!(app.is_dirty());
    chord(&[Key::Control], "w");
    assert!(w.global::<Edit>().get_dialog_open());
    picture("edit-close-888x600.png");
    press(Key::Escape);
    assert!(!w.global::<Edit>().get_dialog_open());
    assert!(app.is_dirty());
    drop(w);
    drop(app);

    // A file that is not text is not opened, and says why.
    let binary = dir.file("blob.bin", &[0x89, 0xff, 0x00, 0x01]);
    let (w, app) = open_window(&dir, Some(&binary));
    assert!(w
        .global::<Edit>()
        .get_refusal()
        .contains("not a plain text file"));
    chord(&[Key::Control], "s");
    assert!(!app.is_dirty());
    picture("edit-binary-888x600.png");
    drop(w);
    drop(app);

    // Neither is one over the limit.
    let big = dir.file("big.log", &vec![b'a'; MAX_BYTES as usize + 1]);
    let (w, _app) = open_window(&dir, Some(&big));
    assert!(w.global::<Edit>().get_refusal().contains("up to 5.0 MB"));
    drop(w);
    drop(_app);

    // A name that does not exist yet is a new file, made when saved.
    let fresh = dir.0.join("fresh.txt");
    let (w, app) = open_window(&dir, Some(&fresh));
    assert!(app.status_text().starts_with("New file"));
    type_text("first line");
    chord(&[Key::Control], "s");
    assert_eq!(fs::read_to_string(&fresh).unwrap(), "first line");
    drop(w);
    drop(app);

    // With no file at all, saving asks for a name.
    let (w, app) = open_window(&dir, None);
    assert_eq!(
        w.global::<Edit>().get_title(),
        "Untitled \u{2014} Text Editor"
    );
    type_text("draft");
    chord(&[Key::Control], "s");
    assert!(w.global::<Edit>().get_dialog_open());
    picture("edit-save-as-888x600.png");
    type_text("fresh.txt");
    press(Key::Return);
    // That name is taken: refused, the dialog stays.
    assert!(w.global::<Edit>().get_dialog_open());
    assert!(w.global::<Edit>().get_dialog().problem.contains("exists"));
    harness::advance(32);
    for _ in 0.."fresh.txt".len() {
        press(Key::Backspace);
    }
    type_text("draft.txt");
    press(Key::Return);
    assert!(!w.global::<Edit>().get_dialog_open());
    assert_eq!(
        fs::read_to_string(dir.0.join("draft.txt")).unwrap(),
        "draft"
    );
    assert_eq!(app.path(), Some(dir.0.join("draft.txt")));
    assert!(!app.is_dirty());
}
