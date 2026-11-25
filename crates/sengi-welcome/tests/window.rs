//! The real welcome window, drawn without a display and driven with the
//! keyboard. It checks what each step does and writes a picture of every
//! step at two window sizes when SENGI_RENDER_DIR is set; with
//! SENGI_THEME=light they are the light ones.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use sengi_config::settings::Layout;
use sengi_shots::harness::{self, BACKDROP};
use sengi_welcome::app::{self, App, Start};
use sengi_welcome::marker;
use sengi_welcome::model::Step;
use sengi_welcome::system::Runner;
use sengi_welcome::ui::{ThemeSwitch, Welcome, WelcomeWindow};
use slint::platform::Key;
use slint::{ComponentHandle, Model};

const XKB: &str = "! layout\n  us              English (US)\n  de              German\n  fr              French\n  gb              English (UK)\n  es              Spanish\n  it              Italian\n! variant\n  nodeadkeys      de: German (no dead keys)\n";

const WALLPAPERS: [&str; 6] = [
    "01-ridge.png",
    "02-dusk.png",
    "03-tide.png",
    "04-grid.png",
    "05-dunes.png",
    "06-night.png",
];

/// What was asked to be run, in order.
#[derive(Default)]
struct Recorder {
    changes: RefCell<Vec<Vec<String>>>,
    opened: RefCell<Vec<Vec<String>>>,
}

impl Runner for Recorder {
    fn change(&self, command: Vec<String>) {
        self.changes.borrow_mut().push(command);
    }

    fn open(&self, command: Vec<String>) {
        self.opened.borrow_mut().push(command);
    }
}

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Dir {
        let path = std::env::temp_dir().join(format!("sengi-welcome-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    /// A made-up /sys/class/net with one interface.
    fn interface(&self, name: &str, wireless: bool, up: bool) {
        let dir = self.0.join("net").join(name);
        fs::create_dir_all(dir.join("device")).unwrap();
        if wireless {
            fs::create_dir_all(dir.join("wireless")).unwrap();
        }
        fs::write(dir.join("operstate"), if up { "up\n" } else { "down\n" }).unwrap();
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn setup(tag: &str, net: Option<(&str, bool)>) -> (Dir, Rc<Recorder>, Start) {
    let dir = Dir::new(tag);
    fs::create_dir_all(dir.0.join("net")).unwrap();
    if let Some((name, wireless)) = net {
        dir.interface(name, wireless, true);
    }
    fs::write(dir.0.join("evdev.lst"), XKB).unwrap();
    let pictures = dir.0.join("pictures");
    fs::create_dir_all(&pictures).unwrap();
    for name in WALLPAPERS {
        fs::write(pictures.join(name), b"x").unwrap();
    }
    let recorder = Rc::new(Recorder::default());
    let start = Start {
        state_home: dir.0.join("state"),
        net_dir: dir.0.join("net"),
        xkb_list: dir.0.join("evdev.lst"),
        wallpaper_dir: pictures,
        wallpaper: "solid".to_string(),
        layouts: vec![Layout::parse("us").unwrap()],
        runner: recorder.clone(),
    };
    (dir, recorder, start)
}

fn open_window(start: Start, size: (u32, u32)) -> (WelcomeWindow, Rc<App>) {
    let window = WelcomeWindow::new().unwrap();
    window.global::<ThemeSwitch>().set_light(harness::light());
    window.show().unwrap();
    harness::resize(&harness::window(), size.0, size.1);
    let app = app::attach(&window, start);
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

fn picture(name: &str) {
    harness::advance(400);
    harness::draw(&harness::window(), BACKDROP).save(name);
}

fn step_name(step: Step) -> &'static str {
    match step {
        Step::Welcome => "welcome",
        Step::Look => "look",
        Step::Network => "network",
        Step::Keyboard => "keyboard",
        Step::Apps => "apps",
        Step::Done => "done",
    }
}

#[test]
fn every_step_is_drawn_at_both_sizes() {
    harness::install();
    for size in [(888, 600), (720, 480)] {
        let (_dir, _recorder, start) = setup("shots", Some(("wlan0", true)));
        let (w, app) = open_window(start, size);
        loop {
            let step = app.step();
            assert_eq!(w.global::<Welcome>().get_step(), step.index());
            picture(&format!(
                "welcome-{}-{}x{}.png",
                step_name(step),
                size.0,
                size.1
            ));
            if step == Step::Done {
                break;
            }
            w.global::<Welcome>().invoke_next();
            harness::advance(32);
        }
        assert!(w.global::<Welcome>().get_last());
        assert_eq!(w.global::<Welcome>().get_counter(), "Step 6 of 6");
    }
}

#[test]
fn the_choices_are_run_as_settings_changes() {
    harness::install();
    let (dir, recorder, start) = setup("choices", Some(("wlan0", true)));
    let state = start.state_home.clone();
    let (w, app) = open_window(start, (888, 600));
    let welcome = w.global::<Welcome>();

    // Welcome, then Look.
    assert_eq!(app.step(), Step::Welcome);
    assert!(!welcome.get_can_back());
    welcome.invoke_next();
    assert_eq!(app.step(), Step::Look);
    assert!(welcome.get_can_back());

    // The theme applies in this window at once and is asked of Settings.
    welcome.invoke_choose_theme(true);
    assert!(w.global::<ThemeSwitch>().get_light());
    welcome.invoke_choose_theme(false);
    assert!(!w.global::<ThemeSwitch>().get_light());

    // The plain colour and the pictures, in order.
    assert_eq!(welcome.get_wallpapers().row_count(), 7);
    welcome.invoke_choose_wallpaper(2);
    assert_eq!(welcome.get_wallpaper(), 2);
    welcome.invoke_choose_wallpaper(0);
    {
        let changes = recorder.changes.borrow();
        assert_eq!(
            changes[0],
            ["sengi-settings", "--set", "appearance.theme", "light"]
        );
        assert_eq!(changes[1][3], "dark");
        assert_eq!(changes[2][2], "appearance.wallpaper");
        assert!(changes[2][3].ends_with("/pictures/02-dusk.png"));
        assert_eq!(changes[3][3], "solid");
    }

    // Network: Wi-Fi is up, and the button opens Settings on its page.
    welcome.invoke_next();
    assert_eq!(app.step(), Step::Network);
    assert_eq!(welcome.get_net(), 1);
    welcome.invoke_open_network();
    assert_eq!(recorder.opened.borrow()[0], ["sengi-settings", "network"]);

    // Keyboard: a few common layouts at first, a search narrows them, and
    // Return on the field takes the first.
    welcome.invoke_next();
    assert_eq!(app.step(), Step::Keyboard);
    assert_eq!(welcome.get_layouts().row_count(), 5);
    type_text("germ");
    assert_eq!(welcome.get_layouts().row_count(), 1);
    press(Key::Return);
    assert_eq!(
        recorder.changes.borrow().last().unwrap(),
        &["sengi-settings", "--set", "keyboard.layouts", "de,us"]
    );
    assert_eq!(welcome.get_layout_now(), "German");

    // Apps: a suggestion asks the Store to search; the other button just
    // opens it.
    welcome.invoke_next();
    assert_eq!(app.step(), Step::Apps);
    welcome.invoke_open_store(0);
    welcome.invoke_open_store(-1);
    {
        let opened = recorder.opened.borrow();
        assert_eq!(opened[1], ["sengi-store", "--search", "firefox"]);
        assert_eq!(opened[2], ["sengi-store"]);
    }

    // Done: Files and Settings, and Finish closes and marks it seen.
    welcome.invoke_next();
    assert_eq!(app.step(), Step::Done);
    welcome.invoke_open_files();
    welcome.invoke_open_settings();
    {
        let opened = recorder.opened.borrow();
        assert_eq!(opened[3], ["sengi-files"]);
        assert_eq!(opened[4], ["sengi-settings"]);
    }
    assert!(!marker::is_done(&state));
    welcome.invoke_next();
    assert!(marker::is_done(&state));
    drop(dir);
}

#[test]
fn a_cable_skips_the_network_step() {
    harness::install();
    let (_dir, _recorder, start) = setup("wired", Some(("eth0", false)));
    let (w, app) = open_window(start, (888, 600));
    let welcome = w.global::<Welcome>();
    assert_eq!(welcome.get_counter(), "Step 1 of 5");
    welcome.invoke_next();
    welcome.invoke_next();
    assert_eq!(app.step(), Step::Keyboard);
    welcome.invoke_back();
    assert_eq!(app.step(), Step::Look);
    assert!(app.wired());
}

#[test]
fn skipping_marks_it_seen_and_changes_nothing() {
    harness::install();
    let (_dir, recorder, start) = setup("skip", None);
    let state = start.state_home.clone();
    let (w, _app) = open_window(start, (888, 600));
    let welcome = w.global::<Welcome>();
    welcome.invoke_next();
    assert_eq!(welcome.get_net(), 0);
    assert!(!marker::is_done(&state));
    welcome.invoke_skip();
    assert!(marker::is_done(&state));
    assert!(recorder.changes.borrow().is_empty());
    assert!(recorder.opened.borrow().is_empty());
}

#[test]
fn return_goes_on_and_escape_goes_back() {
    harness::install();
    let (_dir, _recorder, start) = setup("keys", Some(("wlan0", true)));
    let (_w, app) = open_window(start, (888, 600));
    press(Key::Return);
    assert_eq!(app.step(), Step::Look);
    press(Key::Escape);
    assert_eq!(app.step(), Step::Welcome);
}
