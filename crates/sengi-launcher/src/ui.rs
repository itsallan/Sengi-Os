//! Glue between the Slint panel and a [`Session`]. Creating the window and
//! running the event loop is left to the caller, so the same code serves the
//! layer-shell surface and the preview example.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::cli::Invocation;
use crate::session::Session;

slint::include_modules!();

/// How the panel was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A result was picked; this is its candidate index.
    Accepted(usize),
    Dismissed,
}

/// Cut `name` into runs of matched and unmatched text.
pub fn segments(name: &str, ranges: &[Range<usize>]) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut push = |text: &str, matched: bool| {
        if !text.is_empty() {
            out.push((keep_edge_spaces(text), matched));
        }
    };
    let mut at = 0;
    for range in ranges {
        // Ranges come from the matcher, but a bad one must not panic here.
        let (Some(before), Some(inside)) =
            (name.get(at..range.start), name.get(range.start..range.end))
        else {
            continue;
        };
        push(before, false);
        push(inside, true);
        at = range.end;
    }
    push(name.get(at..).unwrap_or(""), false);
    out
}

/// Each run is laid out as its own piece of text, and text layout drops
/// leading and trailing spaces. No-break spaces keep their width.
fn keep_edge_spaces(text: &str) -> String {
    let start = text.len() - text.trim_start_matches(' ').len();
    let end = text.trim_end_matches(' ').len().max(start);
    let nbsp = |count: usize| "\u{a0}".repeat(count);
    format!(
        "{}{}{}",
        nbsp(start),
        &text[start..end],
        nbsp(text.len() - end)
    )
}

fn show(window: &LauncherWindow, session: &Session) {
    let rows: Vec<Row> = session
        .visible()
        .iter()
        .map(|hit| {
            let candidate = session.candidate(hit.index);
            let name: Vec<Segment> = segments(&candidate.name, &hit.ranges)
                .into_iter()
                .map(|(text, matched)| Segment {
                    text: text.into(),
                    matched,
                })
                .collect();
            Row {
                name: ModelRc::new(VecModel::from(name)),
                detail: SharedString::from(candidate.detail.as_str()),
            }
        })
        .collect();
    window.set_rows(ModelRc::new(VecModel::from(rows)));
    // Whatever went wrong belonged to the previous choice.
    window.set_error(SharedString::new());
    window.set_total_rows(session.hits().len() as i32);
    window.set_first_row(session.first_visible() as i32);
    window.set_selected(session.selected_row().map_or(-1, |row| row as i32));
}

/// Connect `window` to `session`, laid out as `invocation` asks.
///
/// A picked result is first put to `check`. If that fails the panel stays
/// open and shows the message. Otherwise, or when the panel is dismissed,
/// `finish` is called, once; closing the window is up to the caller.
pub fn attach(
    window: &LauncherWindow,
    mut session: Session,
    invocation: &Invocation,
    check: impl Fn(usize) -> Result<(), String> + 'static,
    finish: impl Fn(Outcome) + 'static,
) {
    // The length of a full list is a design token, so it is settled here.
    let full = usize::try_from(window.get_full_lines()).unwrap_or(1);
    session.set_lines(invocation.lines_for(session.candidate_count(), full));

    window.set_prompt(invocation.prompt().into());
    window.set_compact(invocation.compact());
    window.set_max_lines(session.lines() as i32);
    window.set_query(session.query().into());
    show(window, &session);

    let session = Rc::new(RefCell::new(session));
    // Only the first outcome counts: closing the surface after a choice
    // takes the keyboard away, which would otherwise read as a dismissal.
    let finished = std::cell::Cell::new(false);
    let finish = Rc::new(move |outcome| {
        if !finished.replace(true) {
            finish(outcome);
        }
    });

    window.on_query_edited({
        let (weak, session) = (window.as_weak(), session.clone());
        move |query| {
            let Some(window) = weak.upgrade() else { return };
            session.borrow_mut().set_query(query.as_str());
            show(&window, &session.borrow());
        }
    });
    window.on_move_selection({
        let (weak, session) = (window.as_weak(), session.clone());
        move |delta| {
            let Some(window) = weak.upgrade() else { return };
            session.borrow_mut().move_selection(delta);
            show(&window, &session.borrow());
        }
    });
    let accept = Rc::new({
        let (weak, session, finish) = (window.as_weak(), session.clone(), finish.clone());
        move |index: usize| match check(index) {
            Ok(()) => finish(Outcome::Accepted(index)),
            Err(message) => {
                if let Some(window) = weak.upgrade() {
                    show(&window, &session.borrow());
                    window.set_error(message.into());
                }
            }
        }
    });
    window.on_accept({
        let (session, accept) = (session.clone(), accept.clone());
        move || {
            // Return with nothing to pick keeps the panel open.
            let chosen = session.borrow().chosen();
            if let Some(index) = chosen {
                accept(index);
            }
        }
    });
    window.on_row_clicked({
        let session = session.clone();
        move |row| {
            let chosen = usize::try_from(row)
                .ok()
                .and_then(|row| session.borrow_mut().choose_row(row));
            if let Some(index) = chosen {
                accept(index);
            }
        }
    });
    window.on_dismiss(move || finish(Outcome::Dismissed));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::single_range_in_vec_init)]

    use super::*;

    fn seg(text: &str, matched: bool) -> (String, bool) {
        (text.to_string(), matched)
    }

    #[test]
    fn splits_a_name_around_its_matches() {
        assert_eq!(segments("Terminal", &[]), vec![seg("Terminal", false)]);
        assert_eq!(
            segments("Terminal", &[0..4]),
            vec![seg("Term", true), seg("inal", false)]
        );
        assert_eq!(
            segments("Firefox", &[0..1, 4..5, 6..7]),
            vec![
                seg("F", true),
                seg("ire", false),
                seg("f", true),
                seg("o", false),
                seg("x", true),
            ]
        );
        assert_eq!(segments("Files", &[0..5]), vec![seg("Files", true)]);
        assert!(segments("", &[]).is_empty());
    }

    #[test]
    fn spaces_at_the_cut_keep_their_width() {
        assert_eq!(
            segments("Web Browser", &[4..7]),
            vec![
                seg("Web\u{a0}", false),
                seg("Bro", true),
                seg("wser", false)
            ]
        );
        assert_eq!(
            segments("a b", &[1..2]),
            vec![seg("a", false), seg("\u{a0}", true), seg("b", false)]
        );
        assert_eq!(
            keep_edge_spaces("  two  words  "),
            "\u{a0}\u{a0}two  words\u{a0}\u{a0}"
        );
        assert_eq!(keep_edge_spaces("   "), "\u{a0}\u{a0}\u{a0}");
    }

    #[test]
    fn multibyte_names_and_bad_ranges_do_not_panic() {
        assert_eq!(
            segments("Café Écran", &[0..8]),
            vec![seg("Café É", true), seg("cran", false)]
        );
        // Out of bounds, and cutting through the two bytes of "é".
        assert_eq!(segments("Café", &[2..40]), vec![seg("Café", false)]);
        assert_eq!(segments("Café", &[4..5]), vec![seg("Café", false)]);
    }
}
