//! The live image's question, drawn without a display and answered with the
//! keyboard. With SENGI_RENDER_DIR set it writes a picture of it as it opens
//! and as large as the two screens the tests use.

use std::cell::RefCell;
use std::fs;
use std::rc::Rc;

use sengi_shots::harness::{self, BACKDROP};
use sengi_welcome::system::Runner;
use sengi_welcome::ui::{LiveWindow, ThemeSwitch};
use sengi_welcome::{live, marker};
use slint::platform::Key;
use slint::ComponentHandle;

/// The size it opens at, from the tokens.
const OPENS: (u32, u32) = (440, 192);

/// What was asked to be opened.
#[derive(Default)]
struct Recorder {
    opened: RefCell<Vec<Vec<String>>>,
}

impl Runner for Recorder {
    fn change(&self, command: Vec<String>) {
        panic!("the question changes no setting: {command:?}");
    }

    fn open(&self, command: Vec<String>) {
        self.opened.borrow_mut().push(command);
    }
}

fn open_window(size: (u32, u32)) -> (LiveWindow, Rc<Recorder>) {
    harness::install();
    let window = LiveWindow::new().unwrap();
    sengi_shots::apply_look!(window.global::<ThemeSwitch>());
    window.show().unwrap();
    harness::resize(&harness::window(), size.0, size.1);
    let recorder = Rc::new(Recorder::default());
    live::attach(&window, recorder.clone());
    harness::advance(400);
    // Drawn once, as on a screen, so that everything has its place.
    harness::draw(&harness::window(), BACKDROP);
    (window, recorder)
}

fn press(key: Key) {
    harness::key(&harness::window(), &String::from(char::from(key)));
    harness::advance(32);
}

fn installed(recorder: &Recorder) -> bool {
    let opened = recorder.opened.borrow();
    assert!(opened.len() <= 1, "opened more than once: {opened:?}");
    opened
        .first()
        .is_some_and(|command| command == &["sengi-installer"])
}

#[test]
fn it_is_drawn_as_it_opens_and_on_both_screens() {
    for size in [OPENS, (888, 600), (800, 600)] {
        let (_window, _recorder) = open_window(size);
        harness::draw(&harness::window(), BACKDROP)
            .save(&format!("live-{}x{}.png", size.0, size.1));
    }
}

#[test]
fn return_installs_and_closes() {
    let (window, recorder) = open_window(OPENS);
    assert!(window.window().is_visible());
    press(Key::Return);
    assert!(installed(&recorder));
    assert!(!window.window().is_visible());
}

#[test]
fn escape_tries_it_first() {
    let (window, recorder) = open_window(OPENS);
    press(Key::Escape);
    assert!(recorder.opened.borrow().is_empty());
    assert!(!window.window().is_visible());
}

#[test]
fn tab_reaches_try_it_first() {
    let (window, recorder) = open_window(OPENS);
    press(Key::Tab);
    press(Key::Return);
    assert!(recorder.opened.borrow().is_empty());
    assert!(!window.window().is_visible());
}

#[test]
fn tab_goes_round_the_two_buttons_alone() {
    let (_window, recorder) = open_window(OPENS);
    press(Key::Tab);
    press(Key::Tab);
    press(Key::Return);
    assert!(installed(&recorder));

    let (window, recorder) = open_window(OPENS);
    press(Key::Backtab);
    press(Key::Return);
    assert!(recorder.opened.borrow().is_empty());
    assert!(!window.window().is_visible());
}

#[test]
fn escape_still_works_after_tab() {
    let (window, recorder) = open_window(OPENS);
    press(Key::Tab);
    press(Key::Escape);
    assert!(recorder.opened.borrow().is_empty());
    assert!(!window.window().is_visible());
}

#[test]
fn it_counts_as_asked_as_soon_as_it_is_shown() {
    harness::install();
    let state = std::env::temp_dir().join(format!("sengi-welcome-live-{}", std::process::id()));
    let _ = fs::remove_dir_all(&state);
    let window = LiveWindow::new().unwrap();
    // There is no event loop here, so it comes straight back, as it would
    // if the window were taken away without an answer.
    let _ = live::run(window, &state, "boot", Rc::new(Recorder::default()));
    assert!(marker::was_asked(&state, "boot"));
    let _ = fs::remove_dir_all(&state);
}
