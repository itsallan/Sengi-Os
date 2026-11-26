//! Clipboard history: the last texts copied, kept in memory so that Super+V
//! can offer them again.
//!
//! It is off unless `clipboard.history` is set, because what people copy
//! is often private. When it is on, a copy is recorded only if it offers
//! `text/plain;charset=utf-8`, does not carry the password manager hint,
//! and is not over `MAX_BYTES`. Nothing here is ever written to disk or
//! to the log, the list is emptied when the session locks and when the
//! setting is turned off, and a program that is slow to hand over its text
//! is given up on rather than waited for.
//!
//! The text is read from the program that owns the clipboard as soon as it
//! is offered, through the same data-device request an ordinary paste
//! makes. Nothing wakes up while the clipboard is not changing.

use std::{
    collections::VecDeque,
    fs::File,
    io::{ErrorKind, Read},
    os::fd::{FromRawFd, OwnedFd},
    sync::Arc,
    time::Duration,
};

use calloop::{
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, Mode, PostAction, RegistrationToken,
};
use smithay::wayland::selection::data_device::{
    request_data_device_client_selection, set_data_device_selection,
};

use crate::state::State;

/// How many texts are kept.
pub const MAX_ENTRIES: usize = 20;

/// The most a text may come to: 100 KiB. A longer one is not kept at all,
/// not cut: half of a password or a command is worse than none. In the
/// worst case the list holds `MAX_ENTRIES` times this, 2 MB.
pub const MAX_BYTES: usize = 100 * 1024;

/// How long the program that owns the clipboard has to hand its text over.
const READ_TIME: Duration = Duration::from_secs(2);

/// The one kind of text kept.
pub const TEXT: &str = "text/plain;charset=utf-8";

/// What a password manager adds to what it copies, so that nothing keeps
/// it (KeePassXC, Bitwarden and others do). Its value is "secret"; the
/// mere offer is enough to skip the copy.
const SECRET_HINT: &str = "x-kde-passwordManagerHint";

/// What is put on the clipboard when an entry is chosen: the text, as the
/// kinds of text programs ask for.
const OFFERED: [&str; 3] = [TEXT, "text/plain", "UTF8_STRING"];

/// Whether the compositor's own clipboard offers `mime`.
pub fn is_offered(mime: &str) -> bool {
    OFFERED.contains(&mime)
}

/// How many characters of a text a row of the list shows.
pub const PREVIEW_CHARS: usize = 60;

/// The texts, the newest first.
#[derive(Default)]
pub struct History {
    entries: VecDeque<Arc<str>>,
}

impl History {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Arc<str>> {
        self.entries.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<str>> {
        self.entries.iter()
    }

    /// Adds a text as the newest. A text that is already there moves to
    /// the front instead of being listed twice. Empty text, text with
    /// nothing but white space, and text over `MAX_BYTES` are not kept.
    /// Returns whether the list changed.
    pub fn push(&mut self, text: &str) -> bool {
        if text.trim().is_empty() || text.len() > MAX_BYTES {
            return false;
        }
        if self.entries.front().is_some_and(|front| &**front == text) {
            return false;
        }
        self.entries.retain(|entry| &**entry != text);
        self.entries.push_front(Arc::from(text));
        self.entries.truncate(MAX_ENTRIES);
        true
    }

    pub fn remove(&mut self, index: usize) -> bool {
        self.entries.remove(index).is_some()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        // Give the memory back: it held what was copied.
        self.entries.shrink_to_fit();
    }
}

/// Whether a clipboard that offers `mime_types` is to be recorded.
pub fn worth_keeping<S: AsRef<str>>(mime_types: &[S]) -> bool {
    let mut text = false;
    for mime in mime_types {
        let mime = mime.as_ref();
        if mime
            .to_ascii_lowercase()
            .starts_with(&SECRET_HINT.to_ascii_lowercase())
        {
            return false;
        }
        text |= mime.eq_ignore_ascii_case(TEXT);
    }
    text
}

/// What a row of the list says: the text on one line, white space of any
/// kind (new lines and tabs too) squeezed to a space, cut to
/// `PREVIEW_CHARS`, and what is cut off replaced by an ellipsis.
pub fn preview(text: &str) -> String {
    let mut line = String::new();
    let mut chars = 0;
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = !line.is_empty();
            continue;
        }
        if chars >= PREVIEW_CHARS {
            line.push('…');
            return line;
        }
        if pending_space {
            line.push(' ');
            chars += 1;
            pending_space = false;
            if chars >= PREVIEW_CHARS {
                line.push('…');
                return line;
            }
        }
        line.push(c);
        chars += 1;
    }
    line
}

/// A read of the clipboard that has begun.
pub struct Reading {
    source: Option<RegistrationToken>,
    timer: Option<RegistrationToken>,
}

impl State {
    /// Whether the history is on, and the session is one it may run in.
    pub fn clipboard_history_on(&self) -> bool {
        self.config.clipboard.history && !self.is_locked() && !self.login_screen
    }

    /// A program set the clipboard to something offering `mime_types`.
    pub fn clipboard_changed(&mut self, mime_types: &[String]) {
        self.stop_reading();
        if !self.clipboard_history_on() || !worth_keeping(mime_types) {
            return;
        }
        // Smithay announces the new selection before it has stored it, so
        // asking for it now would fetch the one it replaces.
        if !std::mem::replace(&mut self.clipboard_scheduled, true) {
            self.loop_handle.insert_idle(|state| {
                state.clipboard_scheduled = false;
                if state.clipboard_history_on() {
                    state.start_reading();
                }
            });
        }
    }

    /// The clipboard was emptied, or taken by the compositor itself.
    pub fn clipboard_cleared(&mut self) {
        self.stop_reading();
    }

    fn start_reading(&mut self) {
        let Some((read, write)) = pipe() else {
            return;
        };
        let seat = self.seat.clone();
        if request_data_device_client_selection::<State>(&seat, TEXT.to_string(), write).is_err() {
            return;
        }
        let timer =
            self.loop_handle
                .insert_source(Timer::from_duration(READ_TIME), |_, _, state| {
                    // This timer is ending itself: it is not removed again.
                    if let Some(reading) = state.clipboard_reading.as_mut() {
                        reading.timer = None;
                    }
                    state.stop_reading();
                    TimeoutAction::Drop
                });
        let Ok(timer) = timer else {
            return;
        };
        let mut text: Vec<u8> = Vec::new();
        let source = self.loop_handle.insert_source(
            Generic::new(File::from(read), Interest::READ, Mode::Level),
            move |_, file, state| {
                let mut chunk = [0u8; 8192];
                loop {
                    // Reading through a shared reference: the file is not
                    // ours to move out of the source.
                    match (&**file).read(&mut chunk) {
                        Ok(0) => {
                            state.finish_reading(std::mem::take(&mut text), true);
                            return Ok(PostAction::Remove);
                        }
                        Ok(n) => {
                            text.extend_from_slice(&chunk[..n]);
                            if text.len() > MAX_BYTES {
                                state.finish_reading(Vec::new(), true);
                                return Ok(PostAction::Remove);
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::WouldBlock => {
                            return Ok(PostAction::Continue)
                        }
                        Err(e) if e.kind() == ErrorKind::Interrupted => {}
                        Err(_) => {
                            state.finish_reading(Vec::new(), true);
                            return Ok(PostAction::Remove);
                        }
                    }
                }
            },
        );
        match source {
            Ok(source) => {
                self.clipboard_reading = Some(Reading {
                    source: Some(source),
                    timer: Some(timer),
                });
            }
            Err(_) => self.loop_handle.remove(timer),
        }
    }

    /// The text arrived, or did not. `from_source` is whether the read
    /// source itself is the caller, which then removes itself.
    fn finish_reading(&mut self, bytes: Vec<u8>, from_source: bool) {
        let Some(mut reading) = self.clipboard_reading.take() else {
            return;
        };
        if from_source {
            reading.source = None;
        }
        self.release(reading);
        // A paste that was cut short, or that is not text, is not kept.
        if let Ok(text) = String::from_utf8(bytes) {
            if self.clipboard_history_on() && self.clipboard.push(&text) {
                self.after_clipboard_change();
            }
        }
    }

    /// Gives up on the read that is under way, if one is.
    fn stop_reading(&mut self) {
        if let Some(reading) = self.clipboard_reading.take() {
            self.release(reading);
        }
    }

    fn release(&mut self, reading: Reading) {
        if let Some(timer) = reading.timer {
            self.loop_handle.remove(timer);
        }
        if let Some(source) = reading.source {
            self.loop_handle.remove(source);
        }
    }

    /// The list changed under an open list: it is drawn again.
    fn after_clipboard_change(&mut self) {
        if matches!(
            self.overlay,
            Some(crate::overlay::Overlay::Clipboard { .. })
        ) {
            self.clamp_clipboard_selection();
            self.request_redraw();
        }
    }

    /// Makes the entry at `index` what is on the clipboard. No keys are
    /// sent anywhere: the program in front pastes it when the user does.
    pub fn clipboard_paste(&mut self, index: usize) {
        let Some(text) = self.clipboard.get(index).cloned() else {
            return;
        };
        // What was just put there is not read back as a new copy.
        self.stop_reading();
        let seat = self.seat.clone();
        let dh = self.display_handle.clone();
        set_data_device_selection(
            &dh,
            &seat,
            OFFERED.iter().map(|mime| mime.to_string()).collect(),
            text.clone(),
        );
        // The chosen text is the newest again.
        if self.clipboard.push(&text) {
            self.after_clipboard_change();
        }
    }

    /// Forgets everything: the Clear history action, the lock screen, and
    /// the setting being turned off.
    pub fn clipboard_clear(&mut self) {
        self.stop_reading();
        let had = !self.clipboard.is_empty();
        self.clipboard.clear();
        if had {
            self.after_clipboard_change();
        }
    }

    /// One line for the SIGUSR1 report. How many, never what.
    pub fn clipboard_status(&self) -> String {
        format!(
            "clipboard: {} {}",
            if self.config.clipboard.history {
                "on"
            } else {
                "off"
            },
            self.clipboard.len()
        )
    }
}

/// A pipe, non-blocking at the reading end.
fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut ends = [0 as libc::c_int; 2];
    // SAFETY: pipe2 fills two descriptors, which are then owned.
    unsafe {
        if libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) != 0 {
            return None;
        }
        let (read, write) = (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1]));
        // The writing end goes to another program, which should block on
        // it as programs expect to.
        let flags = libc::fcntl(ends[1], libc::F_GETFL);
        libc::fcntl(ends[1], libc::F_SETFL, flags & !libc::O_NONBLOCK);
        Some((read, write))
    }
}

/// What a program that asks for the clipboard we set is sent.
pub fn send_text(text: &Arc<str>, fd: OwnedFd) {
    let text = text.clone();
    // A thread, so that a reader that stops reading holds up nothing but
    // itself, and gives up after a while.
    std::thread::spawn(move || {
        let _ = write_within(fd, text.as_bytes(), Duration::from_secs(10));
    });
}

/// Writes all of `data` to `fd`, giving up once `timeout` has passed.
fn write_within(fd: OwnedFd, data: &[u8], timeout: Duration) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::time::Instant;
    let raw = fd.as_raw_fd();
    // SAFETY: fcntl on a descriptor owned here.
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut file = File::from(fd);
    let deadline = Instant::now() + timeout;
    let mut done = 0;
    while done < data.len() {
        match file.write(&data[done..]) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => done += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(ErrorKind::TimedOut.into());
                }
                let mut poll = libc::pollfd {
                    fd: raw,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: one valid pollfd.
                unsafe { libc::poll(&mut poll, 1, left.as_millis().clamp(1, 500) as _) };
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_text_comes_first_and_twenty_are_kept() {
        let mut history = History::default();
        for n in 0..25 {
            assert!(history.push(&format!("text {n}")));
        }
        assert_eq!(history.len(), MAX_ENTRIES);
        assert_eq!(&**history.get(0).unwrap(), "text 24");
        assert_eq!(&**history.get(19).unwrap(), "text 5");
    }

    #[test]
    fn a_text_copied_again_moves_up_instead_of_being_listed_twice() {
        let mut history = History::default();
        history.push("one");
        history.push("two");
        history.push("three");
        assert!(history.push("one"));
        let order: Vec<&str> = history.iter().map(|text| &**text).collect();
        assert_eq!(order, ["one", "three", "two"]);
        // Said twice in a row changes nothing.
        assert!(!history.push("one"));
    }

    #[test]
    fn empty_blank_and_oversized_texts_are_not_kept() {
        let mut history = History::default();
        assert!(!history.push(""));
        assert!(!history.push("  \n\t "));
        assert!(!history.push(&"x".repeat(MAX_BYTES + 1)));
        assert!(history.is_empty());
        // Right at the limit is kept, whole.
        assert!(history.push(&"y".repeat(MAX_BYTES)));
        assert_eq!(history.get(0).unwrap().len(), MAX_BYTES);
    }

    #[test]
    fn removing_and_clearing() {
        let mut history = History::default();
        history.push("a");
        history.push("b");
        assert!(history.remove(0));
        assert_eq!(&**history.get(0).unwrap(), "a");
        assert!(!history.remove(5));
        history.clear();
        assert!(history.is_empty());
    }

    #[test]
    fn only_utf8_text_without_the_password_hint_is_worth_keeping() {
        assert!(worth_keeping(&["text/plain;charset=utf-8", "UTF8_STRING"]));
        assert!(worth_keeping(&["TEXT/PLAIN;CHARSET=UTF-8"]));
        assert!(!worth_keeping(&["text/plain"]));
        assert!(!worth_keeping(&["image/png"]));
        assert!(!worth_keeping::<&str>(&[]));
        assert!(!worth_keeping(&[
            "text/plain;charset=utf-8",
            "x-kde-passwordManagerHint"
        ]));
        assert!(!worth_keeping(&[
            "x-kde-passwordManagerHint",
            "text/plain;charset=utf-8"
        ]));
        assert!(!worth_keeping(&[
            "text/plain;charset=utf-8",
            "x-kde-passwordManagerHint=secret"
        ]));
    }

    #[test]
    fn a_preview_is_one_line_of_sixty_characters() {
        assert_eq!(preview("hello"), "hello");
        assert_eq!(preview("  two\n\nlines\there  "), "two lines here");
        assert_eq!(preview(""), "");
        assert_eq!(preview(" \n "), "");
        let long = "abcdefghij".repeat(10);
        let shown = preview(&long);
        assert_eq!(shown.chars().count(), PREVIEW_CHARS + 1);
        assert!(shown.ends_with('…'));
        assert!(shown.starts_with("abcdefghijabcdefghij"));
        // Exactly sixty is not cut.
        let exact = "z".repeat(PREVIEW_CHARS);
        assert_eq!(preview(&exact), exact);
        // Multi-byte characters count once each.
        let wide = "é".repeat(70);
        assert_eq!(preview(&wide).chars().count(), PREVIEW_CHARS + 1);
    }

    #[test]
    fn a_slow_reader_does_not_hold_the_writer_for_ever() {
        let (read, write) = pipe().unwrap();
        let started = std::time::Instant::now();
        let outcome = write_within(write, &vec![1u8; 1 << 20], Duration::from_millis(300));
        assert_eq!(outcome.unwrap_err().kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(read);
    }

    #[test]
    fn a_text_goes_through_a_pipe_whole() {
        let (read, write) = pipe().unwrap();
        let text = "ünï".repeat(40_000);
        let expected = text.clone();
        let reader = std::thread::spawn(move || {
            let mut out = String::new();
            let mut file = File::from(read);
            // Back to blocking for the test's own reader.
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0u8; 4096];
                match file.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            out.push_str(&String::from_utf8(bytes).unwrap());
            out
        });
        write_within(write, text.as_bytes(), Duration::from_secs(10)).unwrap();
        assert_eq!(reader.join().unwrap(), expected);
    }
}
