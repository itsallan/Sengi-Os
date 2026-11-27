//! The real editor window, drawn without a display and driven with the
//! keyboard and the pointer. It checks what they do to the text and the
//! file, and writes pictures of the window when SENGI_RENDER_DIR is set.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use sengi_edit::app::{self, App, Start};
use sengi_edit::doc::{EDIT_BYTES, MAX_BYTES};
use sengi_edit::ui::{Edit, EditWindow, ThemeSwitch, Tip};
use sengi_shots::harness::{self, BACKDROP};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model};

const WIDE: (u32, u32) = (888, 600);
const SMALL: (u32, u32) = (720, 480);

/// The middle of the toolbar and of each of its buttons from the left.
const BAR_Y: f32 = 23.0;
const NEW_X: f32 = 23.0;
const OPEN_X: f32 = 55.0;
const SAVE_X: f32 = 87.0;
const UNDO_X: f32 = 134.0;
const REDO_X: f32 = 166.0;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let path = std::env::temp_dir().join(format!("sengi-edit-{tag}-{}", std::process::id()));
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

struct Editor {
    window: EditWindow,
    app: Rc<App>,
    size: (u32, u32),
}

impl Editor {
    fn open(dir: &Dir, path: Option<&Path>, size: (u32, u32)) -> Editor {
        harness::install();
        let window = EditWindow::new().unwrap();
        sengi_shots::apply_look!(window.global::<ThemeSwitch>());
        window.show().unwrap();
        harness::resize(&harness::window(), size.0, size.1);
        let app = app::attach(
            &window,
            Start {
                home: dir.0.clone(),
                path: path.map(Path::to_path_buf),
            },
        );
        settle();
        Editor { window, app, size }
    }

    fn edit(&self) -> Edit<'_> {
        self.window.global::<Edit>()
    }

    fn text(&self) -> String {
        self.app.text().unwrap()
    }

    fn picture(&self, name: &str) {
        settle();
        harness::draw(&harness::window(), BACKDROP)
            .save(&format!("edit-{name}-{}x{}.png", self.size.0, self.size.1));
    }

    fn line_numbers_x(&self) -> f32 {
        self.size.0 as f32 - 55.0
    }

    fn find_x(&self) -> f32 {
        self.size.0 as f32 - 23.0
    }
}

fn settle() {
    for _ in 0..4 {
        harness::advance(100);
        // A draw lays things out, which is when changes are heard.
        harness::draw(&harness::window(), BACKDROP);
    }
}

fn send(event: WindowEvent) {
    harness::window().dispatch_event(event);
}

fn move_to(x: f32, y: f32) {
    harness::pointer(&harness::window(), x, y);
    harness::advance(32);
}

fn button(x: f32, y: f32, down: bool) {
    let position = LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    send(if down {
        WindowEvent::PointerPressed { position, button }
    } else {
        WindowEvent::PointerReleased { position, button }
    });
    harness::advance(32);
}

fn click(x: f32, y: f32) {
    // Past the time in which a second press counts as a double click.
    harness::advance(800);
    move_to(x, y);
    button(x, y, true);
    button(x, y, false);
    settle();
}

fn drag(from: (f32, f32), to: (f32, f32)) {
    harness::advance(800);
    move_to(from.0, from.1);
    button(from.0, from.1, true);
    harness::advance(200);
    for step in 1..=4 {
        let part = step as f32 / 4.0;
        move_to(
            from.0 + (to.0 - from.0) * part,
            from.1 + (to.1 - from.1) * part,
        );
    }
    button(to.0, to.1, false);
    settle();
}

fn press(key: Key) {
    type_text(&String::from(char::from(key)));
}

fn type_text(text: &str) {
    let window = harness::window();
    for c in text.chars() {
        harness::key(&window, &c.to_string());
    }
    settle();
}

/// A key with modifiers held: they are pressed first and let go after.
fn chord(mods: &[Key], text: &str) {
    for key in mods {
        send(WindowEvent::KeyPressed {
            text: char::from(*key).to_string().into(),
        });
    }
    harness::key(&harness::window(), text);
    for key in mods.iter().rev() {
        send(WindowEvent::KeyReleased {
            text: char::from(*key).to_string().into(),
        });
    }
    settle();
}

fn ctrl(text: &str) {
    chord(&[Key::Control], text);
}

fn ctrl_key(key: Key) {
    ctrl(&char::from(key).to_string());
}

fn clear_field(length: usize) {
    press(Key::End);
    for _ in 0..length {
        press(Key::Backspace);
    }
}

const NOTES: &str = "Sengi OS notes\n\nA short file to open, change and save again.\nThe second line is here to be found: needle.\n\nA long line follows so that the wrapping can be seen at the size of the window: it goes on and on and on, past the right edge, with more words than fit in one line of this window, and then some.\n\nThings to do\n- water the plants\n- send the needle back\n\nEnd of file.\n";

fn long_file() -> String {
    let mut text = String::new();
    for i in 1..=200 {
        text.push_str(&format!("This is line {i} of a long file.\n"));
        if i % 40 == 3 {
            text.push_str(&"and this one is far too wide to fit ".repeat(6));
            text.push('\n');
        }
    }
    text
}

#[test]
fn typing_undo_redo_and_save() {
    let dir = Dir::new("typing");
    let notes = dir.file("notes.txt", NOTES.as_bytes());
    let e = Editor::open(&dir, Some(&notes), WIDE);
    assert_eq!(e.text(), NOTES);
    assert!(!e.app.is_dirty());
    assert_eq!(e.app.status_text(), "Line 1, Column 1");
    assert_eq!(e.edit().get_title(), "notes.txt \u{2014} Text Editor");
    assert_eq!(e.edit().get_line_ending(), "LF");
    assert!(e.edit().get_size().ends_with(" characters"));
    e.picture("text");

    // Typing changes it and marks it.
    type_text("Hi ");
    assert!(e.text().starts_with("Hi Sengi OS notes"));
    assert!(e.app.is_dirty());
    assert!(e.edit().get_title().starts_with("* notes.txt"));
    assert_eq!(e.app.status_text(), "Line 1, Column 4");
    e.picture("unsaved");

    // Undo, and redo by either key.
    ctrl("z");
    assert_eq!(e.text(), NOTES, "undo");
    assert!(!e.app.is_dirty());
    ctrl("y");
    assert!(e.text().starts_with("Hi Sengi"), "Ctrl+Y redoes");
    ctrl("z");
    chord(&[Key::Control, Key::Shift], "Z");
    assert!(e.text().starts_with("Hi Sengi"), "Ctrl+Shift+Z redoes");

    // And by the buttons, after which typing goes on in the text.
    click(UNDO_X, BAR_Y);
    assert_eq!(e.text(), NOTES, "the Undo button");
    assert!(!e.app.is_dirty());
    click(REDO_X, BAR_Y);
    assert!(e.text().starts_with("Hi Sengi"), "the Redo button");
    type_text("x");
    assert!(e.text().starts_with("Hi xSengi"));

    // The Save button writes it.
    click(SAVE_X, BAR_Y);
    assert_eq!(fs::read_to_string(&notes).unwrap(), format!("Hi x{NOTES}"));
    assert!(!e.app.is_dirty());
    assert_eq!(e.app.status_text(), "Saved \"notes.txt\".");
    e.picture("saved");
    // The notice goes when typing starts again.
    type_text("y");
    assert!(e.app.status_text().starts_with("Line 1"));
    ctrl("s");
    assert!(fs::read_to_string(&notes).unwrap().starts_with("Hi xy"));

    // Resting on a button names it, and the name goes with the pointer.
    harness::advance(800);
    move_to(OPEN_X, BAR_Y);
    harness::advance(800);
    assert!(e.window.global::<Tip>().get_shown());
    assert_eq!(e.window.global::<Tip>().get_text(), "Open");
    e.picture("tooltip");
    move_to(400.0, 300.0);
    assert!(!e.window.global::<Tip>().get_shown());
}

#[test]
fn selecting_tabs_and_finding() {
    let dir = Dir::new("find");
    let notes = dir.file("notes.txt", NOTES.as_bytes());
    for size in [WIDE, SMALL] {
        fs::write(&notes, NOTES).unwrap();
        let e = Editor::open(&dir, Some(&notes), size);

        // A drag with the pointer selects, and does not scroll instead.
        drag((30.0, 72.0), (200.0, 110.0));
        let (a, b) = (e.edit().get_anchor(), e.edit().get_cursor());
        assert!(b > a + 10, "dragged from {a} to {b}");
        assert!(e.app.status_text().contains("selected)"));
        e.picture("selection");

        // Tab leaves selected lines alone.
        press(Key::Tab);
        assert_eq!(e.text(), NOTES);

        // At a caret it fills with spaces up to the next stop, and one
        // undo takes them away.
        ctrl_key(Key::Home);
        type_text("a");
        press(Key::Tab);
        assert!(e.text().starts_with("a   Sengi"), "{:?}", &e.text()[..12]);
        assert_eq!(e.app.status_text(), "Line 1, Column 5");
        press(Key::Tab);
        assert!(e.text().starts_with("a       Sengi"));
        ctrl("z");
        assert_eq!(e.text(), NOTES);

        // Ctrl+F finds without regard to case and selects what it found.
        ctrl("f");
        assert!(e.edit().get_find_open());
        type_text("NEEDLE");
        assert_eq!(e.edit().get_find_result(), "1 of 2");
        let first = NOTES.find("needle").unwrap() as i32;
        let second = NOTES.rfind("needle").unwrap() as i32;
        assert_eq!(e.edit().get_anchor(), first);
        assert_eq!(e.edit().get_cursor(), first + 6);
        e.picture("find");

        // Return, F3, the arrows and the buttons go both ways and round.
        press(Key::Return);
        assert_eq!(e.edit().get_anchor(), second);
        assert_eq!(e.edit().get_find_result(), "2 of 2");
        press(Key::F3);
        assert_eq!(e.edit().get_anchor(), first);
        chord(&[Key::Shift], &char::from(Key::F3).to_string());
        assert_eq!(e.edit().get_anchor(), second);
        press(Key::UpArrow);
        assert_eq!(e.edit().get_anchor(), first);
        let (previous, next, close) = (size.0 as f32 - 91.0, size.0 as f32 - 57.0, e.find_x());
        let find_y = 68.0;
        click(next, find_y);
        assert_eq!(e.edit().get_anchor(), second);
        click(previous, find_y);
        assert_eq!(e.edit().get_anchor(), first);

        // Words that are not there are said to be not there.
        press(Key::End);
        type_text("x");
        assert_eq!(e.edit().get_find_result(), "No match");
        e.picture("find-none");
        click(close, find_y);
        assert!(!e.edit().get_find_open());
        // The keyboard is back in the text, where the match was selected.
        type_text("pin");
        assert!(e.text().contains("pin"));
        ctrl("z");

        // With a word selected, Ctrl+F looks for it. Escape closes.
        e.edit().set_select_from(0);
        e.edit().set_select_to(5);
        e.edit().set_select_tick(e.edit().get_select_tick() + 1);
        settle();
        ctrl("f");
        assert_eq!(e.edit().get_find_text(), "Sengi");
        assert_eq!(e.edit().get_find_result(), "1 of 1");
        press(Key::Escape);
        assert!(!e.edit().get_find_open());

        // The Find button opens and closes it too.
        click(e.find_x(), BAR_Y);
        assert!(e.edit().get_find_open());
        click(e.find_x(), BAR_Y);
        assert!(!e.edit().get_find_open());
    }
}

#[test]
fn long_files_scroll_and_number_their_lines() {
    let dir = Dir::new("long");
    let long = long_file();
    let path = dir.file("long.txt", long.as_bytes());
    for size in [WIDE, SMALL] {
        let e = Editor::open(&dir, Some(&path), size);
        let lines = long.matches('\n').count() as i32 + 1;
        assert_eq!(e.edit().get_lines(), lines);
        // The first line of the view, clicked.
        let top = (200.0, 70.0);
        click(top.0, top.1);
        assert_eq!(e.edit().get_line(), 1);

        // The view follows the caret to the end and back.
        ctrl_key(Key::End);
        assert_eq!(e.edit().get_line(), lines);
        e.picture("end");
        click(top.0, top.1);
        assert!(e.edit().get_line() > lines - 40, "{}", e.edit().get_line());
        ctrl_key(Key::Home);
        click(top.0, top.1);
        assert_eq!(e.edit().get_line(), 1);

        // Page Down moves by about a view.
        press(Key::PageDown);
        assert!(e.edit().get_line() > 10, "{}", e.edit().get_line());
        ctrl_key(Key::Home);

        // The wheel scrolls without moving the caret.
        send(WindowEvent::PointerScrolled {
            position: LogicalPosition::new(300.0, 200.0),
            delta_x: 0.0,
            delta_y: -400.0,
        });
        settle();
        assert_eq!(e.edit().get_line(), 1);
        click(top.0, top.1);
        let after_wheel = e.edit().get_line();
        assert!(after_wheel > 5, "{after_wheel}");

        // So does the bar at the side, dragged to the bottom.
        let edge = size.0 as f32 - 6.0;
        ctrl_key(Key::Home);
        drag((edge, 60.0), (edge, size.1 as f32));
        assert_eq!(e.edit().get_line(), 1);
        click(top.0, top.1);
        assert!(e.edit().get_line() > lines - 40, "{}", e.edit().get_line());

        // Line numbers, by the button. Lines stop wrapping.
        ctrl_key(Key::Home);
        click(e.line_numbers_x(), BAR_Y);
        assert!(e.edit().get_line_numbers());
        e.picture("numbers");
        // Row n of the view is line n: the pitch is even.
        for _ in 0..8 {
            press(Key::DownArrow);
        }
        assert_eq!(e.edit().get_line(), 9);
        e.picture("numbers-caret");
        // The end of a wide line is brought into view.
        press(Key::UpArrow);
        press(Key::UpArrow);
        press(Key::UpArrow);
        press(Key::UpArrow);
        press(Key::UpArrow);
        assert_eq!(e.edit().get_line(), 4);
        press(Key::End);
        e.picture("numbers-wide");
        ctrl_key(Key::End);
        assert_eq!(e.edit().get_line(), lines);
        e.picture("numbers-end");
        ctrl("l");
        assert!(!e.edit().get_line_numbers());
    }
}

#[test]
fn unsaved_changes_are_asked_about() {
    let dir = Dir::new("ask");
    let notes = dir.file("notes.txt", b"one\n");
    dir.file("taken.txt", b"keep me\n");
    for size in [SMALL, WIDE] {
        fs::write(&notes, "one\n").unwrap();
        fs::write(dir.0.join("taken.txt"), "keep me\n").unwrap();
        let _ = fs::remove_file(dir.0.join("draft.txt"));
        let e = Editor::open(&dir, Some(&notes), size);

        // Nothing to lose: New goes straight there.
        click(NEW_X, BAR_Y);
        assert!(!e.edit().get_asking());
        assert_eq!(e.text(), "");
        assert!(e.edit().get_untitled());
        assert_eq!(e.edit().get_title(), "Untitled \u{2014} Text Editor");
        e.picture("empty");
        // The new page cannot undo its way back to the old one.
        ctrl("z");
        assert_eq!(e.text(), "");

        // Escape keeps editing.
        type_text("draft");
        ctrl("w");
        assert!(e.edit().get_asking());
        assert_eq!(
            e.edit().get_question().title,
            "Save changes to \"Untitled\"?"
        );
        assert_eq!(e.edit().get_question().confirm, "Save");
        assert_eq!(e.edit().get_question().other, "Discard");
        e.picture("unsaved-question");
        // Nothing typed goes behind the dialog.
        type_text("zz");
        assert_eq!(e.text(), "draft");
        press(Key::Escape);
        assert!(!e.edit().get_asking());
        assert!(e.window.window().is_visible());
        type_text("!");
        assert_eq!(e.text(), "draft!", "the keyboard is back in the text");

        // Tab goes from Save to the next button and round; Return on
        // Cancel cancels.
        ctrl("n");
        press(Key::Tab);
        press(Key::Return);
        assert!(!e.edit().get_asking());
        assert_eq!(e.text(), "draft!");

        // Return saves. A page with no name asks for one, and a taken name
        // is replaced only on a second yes.
        ctrl("w");
        press(Key::Return);
        assert!(e.edit().get_browsing());
        assert_eq!(e.edit().get_browse().title, "Save as");
        assert_eq!(e.edit().get_file_name(), "Untitled.txt");
        e.picture("save-as");
        // The offered name is selected, so typing replaces it.
        type_text("taken.txt");
        press(Key::Return);
        assert!(e.edit().get_asking());
        assert_eq!(e.edit().get_question().title, "Replace \"taken.txt\"?");
        e.picture("replace");
        // The keyboard starts on Cancel.
        press(Key::Return);
        assert!(!e.edit().get_asking());
        assert!(e.edit().get_browsing());
        assert_eq!(
            fs::read_to_string(dir.0.join("taken.txt")).unwrap(),
            "keep me\n"
        );
        type_text("no/such/place.txt");
        press(Key::Return);
        assert_eq!(e.edit().get_browse().problem, "That folder does not exist.");
        e.picture("save-as-problem");
        clear_field(30);
        press(Key::Return);
        assert_eq!(e.edit().get_browse().problem, "Type a name.");
        type_text("draft.txt");
        press(Key::Return);
        assert_eq!(
            fs::read_to_string(dir.0.join("draft.txt")).unwrap(),
            "draft!"
        );
        // Saving was the way to closing the window, so it closes.
        assert!(!e.window.window().is_visible());
    }

    // The window's close button asks too, and Discard closes.
    let e = Editor::open(&dir, Some(&notes), WIDE);
    type_text("x");
    send(WindowEvent::CloseRequested);
    settle();
    assert!(e.edit().get_asking());
    assert!(e.window.window().is_visible());
    send(WindowEvent::CloseRequested);
    settle();
    assert!(e.window.window().is_visible());
    // Discard is two steps back from Save: Cancel, then Discard.
    chord(&[Key::Shift], &char::from(Key::Backtab).to_string());
    chord(&[Key::Shift], &char::from(Key::Backtab).to_string());
    press(Key::Return);
    assert!(!e.window.window().is_visible());
    assert_eq!(fs::read_to_string(&notes).unwrap(), "one\n");
    drop(e);

    // Save on a named file saves and goes on to the new page.
    let e = Editor::open(&dir, Some(&notes), WIDE);
    type_text("x");
    click(NEW_X, BAR_Y);
    press(Key::Return);
    assert_eq!(fs::read_to_string(&notes).unwrap(), "xone\n");
    assert_eq!(e.text(), "");

    // Cancel in the name dialog leaves the page as it was.
    type_text("keep");
    ctrl("n");
    press(Key::Return);
    assert!(e.edit().get_browsing());
    press(Key::Escape);
    assert!(!e.edit().get_browsing());
    assert_eq!(e.text(), "keep");
    assert!(e.app.is_dirty());
}

fn names(edit: &Edit) -> Vec<String> {
    edit.get_entries()
        .iter()
        .map(|e| e.name.to_string())
        .collect()
}

#[test]
fn tab_indented_files_keep_their_tabs() {
    let dir = Dir::new("tabs");
    let code = "fn main() {\n\tif true {\n\t\tprintln!(\"hi\");\n\t}\n}\n";
    let path = dir.file("main.rs", code.as_bytes());
    let e = Editor::open(&dir, Some(&path), SMALL);
    assert!(!e.app.is_dirty());
    assert_eq!(e.edit().get_line_ending(), "Tabs, LF");
    assert!(e.text().contains("\n        println!"));
    ctrl("l");
    // A new line indented with Tab is saved with a tab.
    press(Key::DownArrow);
    press(Key::End);
    press(Key::Return);
    press(Key::Tab);
    press(Key::Tab);
    type_text("// here");
    e.picture("tabs");
    ctrl("s");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        code.replace("true {\n", "true {\n\t\t// here\n")
    );
}

#[test]
fn opening_from_the_dialog() {
    let dir = Dir::new("open");
    fs::create_dir_all(dir.0.join("Documents/Old")).unwrap();
    fs::create_dir_all(dir.0.join("Pictures")).unwrap();
    dir.file("Documents/letter.txt", b"Dear reader,\r\n\r\nHello.\r\n");
    dir.file("Documents/plan.md", b"# Plan\n");
    let start = dir.file("start.txt", b"start\n");
    for size in [SMALL, WIDE] {
        let e = Editor::open(&dir, Some(&start), size);
        click(OPEN_X, BAR_Y);
        assert!(e.edit().get_browsing());
        assert_eq!(e.edit().get_browse().folder, "~");
        assert_eq!(names(&e.edit()), ["Documents", "Pictures", "start.txt"]);
        e.picture("open");

        // A name that leads nowhere.
        press(Key::Return);
        assert_eq!(
            e.edit().get_browse().problem,
            "Pick a file or type its name."
        );
        type_text("nothing.txt");
        press(Key::Return);
        assert_eq!(
            e.edit().get_browse().problem,
            "There is no file of that name."
        );
        clear_field(20);

        // The arrows walk the list and Return goes into a folder.
        press(Key::DownArrow);
        assert_eq!(e.edit().get_file_name(), "Documents/");
        press(Key::Return);
        assert_eq!(e.edit().get_browse().folder, "~/Documents");
        assert_eq!(names(&e.edit()), ["Old", "letter.txt", "plan.md"]);
        assert_eq!(e.edit().get_file_name(), "");
        press(Key::DownArrow);
        press(Key::DownArrow);
        assert_eq!(e.edit().get_file_name(), "letter.txt");
        e.picture("open-folder");
        press(Key::Return);
        assert!(!e.edit().get_browsing());
        assert_eq!(e.text(), "Dear reader,\n\nHello.\n");
        assert_eq!(e.edit().get_line_ending(), "CRLF");
        assert!(!e.app.is_dirty());
        assert_eq!(e.app.path(), Some(dir.0.join("Documents/letter.txt")));

        // Its DOS line endings are written back.
        type_text("> ");
        ctrl("s");
        assert_eq!(
            fs::read_to_string(dir.0.join("Documents/letter.txt")).unwrap(),
            "> Dear reader,\r\n\r\nHello.\r\n"
        );
        fs::write(
            dir.0.join("Documents/letter.txt"),
            "Dear reader,\r\n\r\nHello.\r\n",
        )
        .unwrap();

        // The next dialog starts beside the file, and goes up.
        ctrl("o");
        assert_eq!(e.edit().get_browse().folder, "~/Documents");
        e.edit().invoke_folder_up();
        settle();
        assert_eq!(e.edit().get_browse().folder, "~");
        type_text("start.txt");
        press(Key::Return);
        assert_eq!(e.text(), "start\n");

        // Save As under the file's own name needs no second yes.
        chord(&[Key::Control, Key::Shift], "S");
        assert_eq!(e.edit().get_file_name(), "start.txt");
        press(Key::Return);
        assert!(!e.edit().get_asking() && !e.edit().get_browsing());
    }

    // A file with the very words of the one shown starts with nothing to undo.
    let copy = dir.file("copy.txt", b"start\n");
    let e = Editor::open(&dir, Some(&start), WIDE);
    type_text("x");
    press(Key::Backspace);
    ctrl("o");
    type_text("copy.txt");
    press(Key::Return);
    assert_eq!(e.app.path(), Some(copy));
    assert_eq!(e.text(), "start\n");
    ctrl("z");
    assert_eq!(e.text(), "start\n");
    assert!(!e.app.is_dirty());
}

#[test]
fn what_is_not_opened_and_what_is_new() {
    let dir = Dir::new("refuse");
    for size in [WIDE, SMALL] {
        // A file that is not text is not opened, and says why.
        let binary = dir.file("blob.bin", &[0x89, 0xff, 0x00, 0x01]);
        let e = Editor::open(&dir, Some(&binary), size);
        assert!(e.edit().get_refusal().contains("not a plain text file"));
        ctrl("s");
        ctrl("f");
        assert!(!e.app.is_dirty() && !e.edit().get_find_open() && !e.edit().get_browsing());
        e.picture("binary");
        // From there another can be opened.
        ctrl("o");
        assert!(e.edit().get_browsing());
    }

    // Neither is one over the limit.
    let big = dir.file("big.log", &vec![b'a'; MAX_BYTES as usize + 1]);
    let e = Editor::open(&dir, Some(&big), WIDE);
    assert!(e.edit().get_refusal().contains("up to 5.0 MB"));
    drop(e);

    // A large one is shown, searched and left as it is.
    let log = "a line of a large log\n".repeat(EDIT_BYTES as usize / 20);
    let large = dir.file("large.log", log.as_bytes());
    let e = Editor::open(&dir, Some(&large), WIDE);
    assert!(e.edit().get_read_only());
    assert!(e.app.status_text().starts_with("A large file"));
    e.picture("large");
    type_text("x");
    press(Key::Tab);
    ctrl("s");
    chord(&[Key::Control, Key::Shift], "S");
    assert_eq!(e.text(), log);
    assert!(!e.app.is_dirty() && !e.edit().get_browsing());
    ctrl("f");
    type_text("large");
    assert!(e.edit().get_find_result().starts_with("1 of "));
    drop(e);

    // A name that does not exist yet is a new file, made when saved.
    let fresh = dir.0.join("fresh.txt");
    let e = Editor::open(&dir, Some(&fresh), WIDE);
    assert!(e.app.status_text().starts_with("New file"));
    e.picture("new-file");
    type_text("first line");
    ctrl("s");
    assert_eq!(fs::read_to_string(&fresh).unwrap(), "first line");

    // A save that fails says so and keeps the changes.
    type_text(" more");
    fs::remove_file(&fresh).unwrap();
    fs::create_dir(&fresh).unwrap();
    ctrl("s");
    assert!(e.app.status_text().starts_with("Cannot save"));
    assert!(e.app.is_dirty());
    e.picture("save-failed");
}
