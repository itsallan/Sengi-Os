//! Glue between the Slint panel and a [`Session`]. Creating the window and
//! running the event loop is left to the caller, so the same code serves the
//! layer-shell surface and the preview example.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use sengi_apps::decoder::Pixels;
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};

use crate::cli::{Invocation, Mode};
use crate::icons::Request;
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

/// How the results are laid out: the application search shows a grid of
/// every app while nothing is typed, and a list once something is.
#[derive(Clone, Copy)]
struct Layout {
    apps: bool,
    columns: usize,
    grid_rows: usize,
    list_lines: usize,
}

/// The icons decoded so far, by candidate index.
type Icons = Rc<RefCell<HashMap<usize, Image>>>;

/// Takes an icon that has been decoded, on the UI thread, to show it.
pub type IconSink = Rc<dyn Fn(usize, Option<Pixels>)>;

fn to_image(pixels: Pixels) -> Image {
    let buffer =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels.data, pixels.size, pixels.size);
    Image::from_rgba8_premultiplied(buffer)
}

fn rows(session: &Session, icons: &HashMap<usize, Image>) -> Vec<Row> {
    session
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
            let icon = icons.get(&hit.index);
            Row {
                name: ModelRc::new(VecModel::from(name)),
                detail: SharedString::from(candidate.detail.as_str()),
                title: SharedString::from(candidate.name.as_str()),
                icon: icon.cloned().unwrap_or_default(),
                has_icon: icon.is_some(),
                letter: SharedString::from(sengi_text::initial(&candidate.name)),
            }
        })
        .collect()
}

fn show(window: &LauncherWindow, session: &mut Session, icons: &HashMap<usize, Image>, layout: Layout) {
    let grid = layout.apps && session.query().is_empty() && !session.hits().is_empty();
    if grid {
        session.set_grid(layout.columns, layout.grid_rows);
    } else if session.columns() != 0 {
        session.set_lines(layout.list_lines);
    }
    window.set_grid(grid);
    window.set_rows(ModelRc::new(VecModel::from(rows(session, icons))));
    // Whatever went wrong belonged to the previous choice.
    window.set_error(SharedString::new());
    window.set_total_rows(session.hits().len() as i32);
    window.set_first_row(session.first_visible() as i32);
    window.set_selected(session.selected_row().map_or(-1, |row| row as i32));
}

/// The icons to load for `session`: the ones for what is listed first, then
/// the rest, so the first screen fills in before the ones scrolled off it.
pub fn icon_requests(session: &Session) -> Vec<Request> {
    let mut order: Vec<usize> = session.hits().iter().map(|hit| hit.index).collect();
    let listed: HashSet<usize> = order.iter().copied().collect();
    order.extend((0..session.candidate_count()).filter(|index| !listed.contains(index)));
    order
        .into_iter()
        .filter_map(|index| {
            let names = session.candidate(index).icons.clone();
            (!names.is_empty()).then_some(Request { index, names })
        })
        .collect()
}

/// The size, in pixels, icons are wanted at for `window`.
pub fn icon_pixels(window: &LauncherWindow) -> u32 {
    let scale = window.window().scale_factor();
    (window.get_app_icon_px() * scale).round().max(1.0) as u32
}

/// Connect `window` to `session`, laid out as `invocation` asks.
///
/// A picked result is first put to `check`. If that fails the panel stays
/// open and shows the message. Otherwise, or when the panel is dismissed,
/// `finish` is called, once; closing the window is up to the caller.
///
/// Returns where decoded icons are to be delivered, from the UI thread.
pub fn attach(
    window: &LauncherWindow,
    mut session: Session,
    invocation: &Invocation,
    check: impl Fn(usize) -> Result<(), String> + 'static,
    finish: impl Fn(Outcome) + 'static,
) -> IconSink {
    // The length of a full list is a design token, so it is settled here.
    let full = usize::try_from(window.get_full_lines()).unwrap_or(1);
    session.set_lines(invocation.lines_for(session.candidate_count(), full));

    let apps = invocation.mode == Mode::Apps;
    let layout = Layout {
        apps,
        columns: usize::try_from(window.get_columns()).unwrap_or(1).max(1),
        grid_rows: usize::try_from(window.get_grid_rows()).unwrap_or(1).max(1),
        list_lines: session.lines(),
    };
    window.set_prompt(invocation.prompt().into());
    window.set_compact(invocation.compact());
    window.set_icons(apps);
    window.set_grid_capable(apps);
    window.set_max_lines(session.lines() as i32);
    window.set_query(session.query().into());
    let icons: Icons = Rc::default();
    show(window, &mut session, &icons.borrow(), layout);

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
        let (weak, session, icons) = (window.as_weak(), session.clone(), icons.clone());
        move |query| {
            let Some(window) = weak.upgrade() else { return };
            session.borrow_mut().set_query(query.as_str());
            show(&window, &mut session.borrow_mut(), &icons.borrow(), layout);
        }
    });
    window.on_move_selection({
        let (weak, session, icons) = (window.as_weak(), session.clone(), icons.clone());
        move |delta| {
            let Some(window) = weak.upgrade() else { return };
            session.borrow_mut().move_selection(delta);
            show(&window, &mut session.borrow_mut(), &icons.borrow(), layout);
        }
    });
    let accept = Rc::new({
        let (weak, session, icons, finish) =
            (window.as_weak(), session.clone(), icons.clone(), finish.clone());
        move |index: usize| match check(index) {
            Ok(()) => finish(Outcome::Accepted(index)),
            Err(message) => {
                if let Some(window) = weak.upgrade() {
                    show(&window, &mut session.borrow_mut(), &icons.borrow(), layout);
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

    let weak = window.as_weak();
    Rc::new(move |index, pixels| {
        let Some(window) = weak.upgrade() else { return };
        if let Some(pixels) = pixels {
            icons.borrow_mut().insert(index, to_image(pixels));
        }
        // Only what is on screen is redrawn.
        let session = session.borrow();
        if session.visible().iter().any(|hit| hit.index == index) {
            window.set_rows(ModelRc::new(VecModel::from(rows(&session, &icons.borrow()))));
        }
    })
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
