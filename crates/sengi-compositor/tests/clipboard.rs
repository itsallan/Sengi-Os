//! Data control and the clipboard history, against the running compositor:
//! a test client with no window sets and reads the clipboard the way
//! wl-copy and clipboard managers do, the history records it, Super+V shows
//! it, and a choice puts the text back.

mod common;
mod harness;

use std::time::{Duration, Instant};

use common::{
    apps::Picture,
    clipboard::{Control, EXT, TEXT, WLR},
    Client,
};
use harness::{Compositor, SOON};

const HISTORY_ON: &str = "[clipboard]\nhistory = true\n";

/// Not a test of its own: the compositor starts this test binary as the
/// screen locker when a test asks for one, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

fn with_history() -> Compositor {
    Compositor::start_with(None, Some(HISTORY_ON), None)
}

/// How many texts the history holds, as the compositor reports it.
fn entries(compositor: &Compositor) -> usize {
    compositor
        .status_line("clipboard:")
        .rsplit(' ')
        .next()
        .and_then(|count| count.parse().ok())
        .expect("a count")
}

/// Waits for the history to hold `want` texts. A copy is read in the
/// background, a moment after it is made.
fn wait_for(compositor: &Compositor, want: usize) {
    let end = Instant::now() + SOON;
    let mut have = entries(compositor);
    while have != want && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
        have = entries(compositor);
    }
    assert_eq!(have, want, "texts in the history");
}

/// Gives the compositor time to have read a copy that it should skip, then
/// checks nothing was kept. The same copy followed by one that is kept
/// proves it in order: see `skips`.
fn settle() {
    std::thread::sleep(Duration::from_millis(300));
}

fn overlay(compositor: &Compositor) -> String {
    compositor.status_line("overlay:")
}

fn token(name: &str) -> u32 {
    let c = sengi_tokens::Tokens::builtin().color(name).unwrap();
    u32::from(c.r) << 16 | u32::from(c.g) << 8 | u32::from(c.b)
}

fn shot(picture: &Picture, name: &str) {
    if let Some(dir) = std::env::var_os("SENGI_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        picture.write_ppm(&dir.join(name)).unwrap();
    }
}

/// The rows from the top of the picture to the bottom that are drawn in
/// the accent colour at the middle of the screen: where the selection is.
fn accent_rows(picture: &Picture) -> Option<(u32, u32)> {
    let accent = token("color.accent");
    let x = picture.width / 2;
    let mut seen: Option<(u32, u32)> = None;
    for y in 0..picture.height {
        // A few columns, so that the text on the row does not hide it.
        let lit = (x - 150..x - 100).any(|x| picture.at(x, y) == accent);
        if lit {
            seen = Some(match seen {
                Some((first, _)) => (first, y),
                None => (y, y),
            });
        }
    }
    seen
}

/// A client with a listener on the clipboard, which sees it change.
fn reader(compositor: &Compositor) -> (Client, Control) {
    let mut app = compositor.connect();
    let control = app.data_control(true).unwrap();
    (app, control)
}

/// Copies `text` from a client of its own, which is returned with its
/// source so that the text stays the clipboard's.
fn copy(compositor: &Compositor, text: &str) -> (Client, impl Sized) {
    let mut app = compositor.connect();
    let control = app.data_control(true).unwrap();
    let source = app
        .copy(&control, &[TEXT, "UTF8_STRING"], text.as_bytes())
        .unwrap();
    (app, (control, source))
}

#[test]
fn both_data_control_protocols_are_offered_but_not_to_a_sandbox() {
    let compositor = Compositor::start(None);
    let mut portal = compositor.connect();
    assert!(portal.has_global(EXT), "ext data control is missing");
    assert!(portal.has_global(WLR), "wlr data control is missing");
    portal.sandbox(&compositor.dir.join("sandbox-0")).unwrap();
    let confined = Client::connect(Some(&compositor.dir.join("sandbox-0"))).unwrap();
    assert!(!confined.has_global(EXT), "ext data control in a sandbox");
    assert!(!confined.has_global(WLR), "wlr data control in a sandbox");
}

#[test]
fn a_program_with_no_window_sets_the_clipboard_and_another_reads_it() {
    let compositor = Compositor::start(None);
    for (sets_ext, reads_ext) in [(true, true), (false, false), (true, false), (false, true)] {
        let mut writer = compositor.connect();
        let writing = writer.data_control(sets_ext).unwrap();
        let mut watcher = compositor.connect();
        let watching = watcher.data_control(reads_ext).unwrap();
        let before = watcher.seen.clip.selections;
        let _source = writer
            .copy(&writing, &["image/png"], b"\x89PNG not really")
            .unwrap();
        assert!(
            watcher.wait(SOON, |seen| seen.clip.selections > before),
            "the watcher was not told ({sets_ext} {reads_ext})"
        );
        assert_eq!(watcher.seen.clip.types(), ["image/png"]);
        // The writer answers requests for its picture while the watcher asks.
        let done = std::sync::atomic::AtomicBool::new(false);
        let pasted = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    writer.roundtrip().unwrap();
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            let pasted = watcher.paste(&watching, "image/png").unwrap();
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            pasted
        });
        assert_eq!(
            pasted.as_deref(),
            Some(&b"\x89PNG not really"[..]),
            "{sets_ext} {reads_ext}"
        );
    }
}

#[test]
fn the_history_is_off_unless_it_was_turned_on() {
    let compositor = Compositor::start(None);
    let status = compositor.status_line("clipboard:");
    assert!(status.ends_with("off 0"), "{status}");
    let _held = copy(&compositor, "private");
    settle();
    assert_eq!(entries(&compositor), 0);
    // Super+V says so rather than showing nothing.
    compositor.input("key super+v");
    assert!(overlay(&compositor).contains("clipboard"));
    compositor.input("key esc");
    assert!(overlay(&compositor).contains("none"));
}

#[test]
fn a_text_copied_is_recorded_newest_first_without_doubles() {
    let compositor = with_history();
    assert!(compositor.status_line("clipboard:").ends_with("on 0"));
    let _a = copy(&compositor, "one");
    wait_for(&compositor, 1);
    let _b = copy(&compositor, "two");
    wait_for(&compositor, 2);
    // Copying the first again moves it up, it is not listed twice.
    let _c = copy(&compositor, "one");
    settle();
    assert_eq!(entries(&compositor), 2);
}

#[test]
fn what_is_not_wanted_is_not_kept() {
    let compositor = with_history();
    let mut app = compositor.connect();
    let control = app.data_control(true).unwrap();

    // A picture is not text.
    let _picture = app.copy(&control, &["image/png"], b"png").unwrap();
    // Text from a password manager, which says so.
    let _secret = app
        .copy(&control, &[TEXT, "x-kde-passwordManagerHint"], b"hunter2")
        .unwrap();
    // Text of the wrong kind, too long, and blank.
    let _plain = app.copy(&control, &["text/plain"], b"old style").unwrap();
    let _long = app
        .copy(&control, &[TEXT], &vec![b'x'; 100 * 1024 + 1])
        .unwrap();
    let _blank = app.copy(&control, &[TEXT], b"  \n ").unwrap();
    // A program that never answers is given up on.
    let _hung = app.copy_hanging(&control, &[TEXT]).unwrap();
    // Then one that is kept: it comes after the others, so once it is
    // there, they have all been looked at.
    let _kept = app.copy(&control, &[TEXT], b"the one").unwrap();
    wait_for(&compositor, 1);
    settle();
    assert_eq!(entries(&compositor), 1);
}

#[test]
fn a_text_of_exactly_the_limit_is_kept_whole_and_comes_back_whole() {
    let compositor = with_history();
    let text = "é".repeat(50 * 1024);
    assert_eq!(text.len(), 100 * 1024);
    let _held = copy(&compositor, &text);
    wait_for(&compositor, 1);

    let (mut watcher, watching) = reader(&compositor);
    compositor.input("key super+v");
    assert!(overlay(&compositor).contains("clipboard"));
    let before = watcher.seen.clip.selections;
    compositor.input("key enter");
    assert!(watcher.wait(SOON, |seen| seen.clip.selections > before));
    assert_eq!(
        watcher.paste_text(&watching).unwrap().as_deref(),
        Some(text.as_str())
    );
}

#[test]
fn only_twenty_texts_are_kept() {
    let compositor = with_history();
    let mut app = compositor.connect();
    let control = app.data_control(true).unwrap();
    for n in 0..25 {
        let _source = app
            .copy(&control, &[TEXT], format!("text {n}").as_bytes())
            .unwrap();
        // Each is read before the next replaces it.
        let end = Instant::now() + SOON;
        while entries(&compositor) < (n + 1).min(20) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    wait_for(&compositor, 20);
}

#[test]
fn super_v_lists_the_texts_and_return_puts_the_chosen_one_back() {
    let compositor = with_history();
    let (mut watcher, watching) = reader(&compositor);
    let _one = copy(&compositor, "first text");
    wait_for(&compositor, 1);
    let _two = copy(&compositor, "second text");
    wait_for(&compositor, 2);
    let mut app = compositor.connect();
    let plain = app.capture().unwrap();
    assert!(accent_rows(&plain).is_none(), "a selection before the list");

    compositor.input("key super+v");
    assert!(overlay(&compositor).contains("clipboard"));
    let open = app.capture().unwrap();
    shot(&open, "clipboard-two.ppm");
    let rows = accent_rows(&open).expect("the newest is selected");
    // Down moves to the older text.
    compositor.input("key down");
    let moved = app.capture().unwrap();
    let rows_after = accent_rows(&moved).expect("a selection");
    assert!(rows_after.0 > rows.0, "{rows:?} then {rows_after:?}");

    let before = watcher.seen.clip.selections;
    compositor.input("key enter");
    assert!(overlay(&compositor).contains("none"), "Return closes it");
    assert!(watcher.wait(SOON, |seen| seen.clip.selections > before));
    assert_eq!(
        watcher.paste_text(&watching).unwrap().as_deref(),
        Some("first text")
    );
    // The text chosen is the newest now, and nothing was added.
    settle();
    assert_eq!(entries(&compositor), 2);
    compositor.input("key super+v");
    let reopened = app.capture().unwrap();
    assert_eq!(accent_rows(&reopened), Some(rows), "the first row again");
    compositor.input("key esc");
}

#[test]
fn escape_closes_it_and_changes_nothing() {
    let compositor = with_history();
    let (mut watcher, _watching) = reader(&compositor);
    let _one = copy(&compositor, "keep me");
    wait_for(&compositor, 1);
    watcher.roundtrip().unwrap();
    let before = watcher.seen.clip.selections;
    compositor.input("key super+v");
    assert!(overlay(&compositor).contains("clipboard"));
    compositor.input("key esc");
    assert!(overlay(&compositor).contains("none"));
    watcher.roundtrip().unwrap();
    assert_eq!(watcher.seen.clip.selections, before, "the clipboard moved");
    assert_eq!(entries(&compositor), 1);
    // Super+V again while it is open closes it too.
    compositor.input("key super+v");
    compositor.input("key super+v");
    assert!(overlay(&compositor).contains("none"));
}

#[test]
fn delete_forgets_the_selected_entry() {
    let compositor = with_history();
    let _one = copy(&compositor, "alpha");
    wait_for(&compositor, 1);
    let _two = copy(&compositor, "beta");
    wait_for(&compositor, 2);
    compositor.input("key super+v");
    compositor.input("key delete");
    assert_eq!(entries(&compositor), 1);
    assert!(overlay(&compositor).contains("clipboard"), "still open");
    compositor.input("key delete");
    assert_eq!(entries(&compositor), 0);
    // An empty list says so, and Delete there does nothing.
    compositor.input("key delete");
    assert_eq!(entries(&compositor), 0);
    compositor.input("key esc");
}

#[test]
fn clear_history_is_the_last_row() {
    let compositor = with_history();
    let _one = copy(&compositor, "alpha");
    wait_for(&compositor, 1);
    let _two = copy(&compositor, "beta");
    wait_for(&compositor, 2);
    compositor.input("key super+v");
    // Two texts, a separator, then Clear history: up from the first wraps
    // round to it.
    compositor.input("key up");
    compositor.input("key enter");
    assert!(overlay(&compositor).contains("none"));
    assert_eq!(entries(&compositor), 0);
}

#[test]
fn the_pointer_selects_and_a_click_puts_the_text_back() {
    let compositor = with_history();
    let (mut watcher, watching) = reader(&compositor);
    let _one = copy(&compositor, "older");
    wait_for(&compositor, 1);
    let _two = copy(&compositor, "newer");
    wait_for(&compositor, 2);
    let mut app = compositor.connect();
    compositor.input("key super+v");
    let open = app.capture().unwrap();
    let (top, bottom) = accent_rows(&open).expect("a selection");
    let row = bottom - top + 1;
    let x = open.width / 2;
    // The second row is the older text.
    compositor.input(&format!("move {} {}", x, top + row + row / 2));
    let hovered = app.capture().unwrap();
    assert_eq!(accent_rows(&hovered).map(|rows| rows.0 > top), Some(true));
    let before = watcher.seen.clip.selections;
    compositor.input("press left");
    compositor.input("release left");
    assert!(overlay(&compositor).contains("none"));
    assert!(watcher.wait(SOON, |seen| seen.clip.selections > before));
    assert_eq!(
        watcher.paste_text(&watching).unwrap().as_deref(),
        Some("older")
    );
}

#[test]
fn a_click_outside_closes_it_and_nothing_is_pasted() {
    let compositor = with_history();
    let (mut watcher, _watching) = reader(&compositor);
    let _one = copy(&compositor, "text");
    wait_for(&compositor, 1);
    watcher.roundtrip().unwrap();
    let before = watcher.seen.clip.selections;
    compositor.input("key super+v");
    compositor.input("move 5 5");
    compositor.input("press left");
    compositor.input("release left");
    assert!(overlay(&compositor).contains("none"));
    watcher.roundtrip().unwrap();
    assert_eq!(watcher.seen.clip.selections, before);
}

#[test]
fn the_list_is_not_there_on_the_lock_screen_and_locking_forgets_it() {
    let compositor = with_history();
    let _one = copy(&compositor, "do not leave me");
    wait_for(&compositor, 1);
    let mut locker = compositor.locker();
    locker.keyboard().unwrap();
    let lock = locker.lock().unwrap();
    let output = locker.outputs().remove(0);
    let _surface = locker.lock_surface(&lock, &output, 0xffb0_2020).unwrap();
    assert!(locker.wait(SOON, |seen| seen.locked == 1));
    assert_eq!(entries(&compositor), 0, "the lock left the history alone");
    compositor.input("key super+v");
    assert!(overlay(&compositor).contains("none"), "opened when locked");
}

#[test]
fn turning_the_setting_off_forgets_everything() {
    let compositor = with_history();
    let _one = copy(&compositor, "gone soon");
    wait_for(&compositor, 1);
    compositor.input("key super+v");
    assert!(overlay(&compositor).contains("clipboard"));
    compositor.write_settings("[clipboard]\nhistory = false\n");
    compositor.signal("HUP");
    let end = Instant::now() + SOON;
    while !compositor.status_line("clipboard:").ends_with("off 0") && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(compositor.status_line("clipboard:").ends_with("off 0"));
    assert!(
        overlay(&compositor).contains("none"),
        "the list stayed open"
    );
}

#[test]
fn the_list_is_drawn_for_the_pictures() {
    for (theme, name) in [("dark", "dark"), ("light", "light")] {
        let settings = format!("[clipboard]\nhistory = true\n[appearance]\ntheme = \"{theme}\"\n");
        let compositor = Compositor::start_with(None, Some(&settings), None);
        let mut app = compositor.connect();
        // Empty.
        compositor.input("key super+v");
        shot(
            &app.capture().unwrap(),
            &format!("clipboard-empty-{name}.ppm"),
        );
        compositor.input("key esc");
        // Five texts, one of them long and with several lines.
        let mut held = Vec::new();
        for text in [
            "git commit -m \"fix the thing\"",
            "https://example.org/some/long/address?with=parameters&and=more",
            "first line\nsecond line\n\tthird, indented",
            "short",
            "The quick brown fox jumps over the lazy dog, again and again, until the line is much too long to fit in the list",
        ] {
            held.push(copy(&compositor, text));
            wait_for(&compositor, held.len());
        }
        compositor.input("key super+v");
        shot(
            &app.capture().unwrap(),
            &format!("clipboard-five-{name}.ppm"),
        );
        compositor.input("key down");
        compositor.input("key down");
        shot(
            &app.capture().unwrap(),
            &format!("clipboard-selected-{name}.ppm"),
        );
        compositor.input("key esc");
    }
}

/// Whether a process called sengi-shot is running.
fn shot_running() -> bool {
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim() == "sengi-shot")
        })
}

/// The real sengi-shot, built in the main workspace, against this
/// compositor: it needs `SENGI_SHOT` to name the program, and the test is
/// skipped without it.
#[test]
fn sengi_shot_puts_a_picture_on_the_clipboard_and_stays_until_it_is_replaced() {
    let Some(program) = std::env::var_os("SENGI_SHOT") else {
        eprintln!("SENGI_SHOT is not set: skipped");
        return;
    };
    let compositor = Compositor::start(None);
    let (mut watcher, watching) = reader(&compositor);
    let started = Instant::now();
    let mut shot = std::process::Command::new(&program)
        .arg("--clipboard")
        .env("XDG_RUNTIME_DIR", &compositor.dir)
        .env("WAYLAND_DISPLAY", compositor.socket.file_name().unwrap())
        .env("HOME", &compositor.dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("sengi-shot starts");
    let status = shot.wait().unwrap();
    assert!(status.success(), "sengi-shot said {status}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the shortcut was held up for {:?}",
        started.elapsed()
    );
    // It has left its parent behind, with the clipboard.
    assert!(shot_running(), "nothing keeps the clipboard");
    assert!(watcher.wait(SOON, |seen| seen.clip.selections >= 1));
    assert_eq!(watcher.seen.clip.types(), ["image/png"]);
    let picture = watcher.paste(&watching, "image/png").unwrap().unwrap();
    assert_eq!(&picture[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    // And again: it keeps serving.
    let again = watcher.paste(&watching, "image/png").unwrap().unwrap();
    assert_eq!(again, picture);
    // Something else copied takes the clipboard, and it goes.
    let _text = copy(&compositor, "something else");
    let end = Instant::now() + SOON;
    while shot_running() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!shot_running(), "it did not leave when replaced");
}

#[test]
fn a_clipboard_set_by_the_compositor_goes_to_programs_with_a_window_too() {
    let compositor = with_history();
    let _one = copy(&compositor, "for a window");
    wait_for(&compositor, 1);
    let mut app = compositor.connect();
    app.keyboard().unwrap();
    let _window = app.window(0xff20_b020).unwrap();
    assert!(app.wait(SOON, |seen| seen.keyboard_enters == 1));
    let (mut watcher, watching) = reader(&compositor);
    let before = watcher.seen.clip.selections;
    compositor.input("key super+v");
    // Super itself reaches the window, being no shortcut on its own.
    app.roundtrip().unwrap();
    let keys_before = app.seen.keys;
    compositor.input("key enter");
    assert!(watcher.wait(SOON, |seen| seen.clip.selections > before));
    // All the kinds of text programs ask for.
    assert_eq!(
        watcher.seen.clip.types(),
        [TEXT, "text/plain", "UTF8_STRING"]
    );
    assert_eq!(
        watcher.paste(&watching, "UTF8_STRING").unwrap().as_deref(),
        Some(&b"for a window"[..])
    );
    // No keys were sent to the window: the paste is the user's to make.
    app.roundtrip().unwrap();
    assert_eq!(app.seen.keys, keys_before, "Return reached the window");
}
