//! Which month is shown, and the window kept in line with it.

use std::cell::RefCell;
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::cal::{self, WeekStart};
use crate::ui::{Cal, CalendarWindow, DayCell};

/// The month on show and what today is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub year: i32,
    pub month: u32,
    pub today: (i32, u32, u32),
    pub start: WeekStart,
}

impl View {
    /// Opens on the month of `today`.
    pub fn new(today: (i32, u32, u32), start: WeekStart) -> View {
        View {
            year: today.0,
            month: today.1,
            today,
            start,
        }
    }

    pub fn step(&mut self, months: i32) {
        (self.year, self.month) = cal::step_month(self.year, self.month, months);
    }

    pub fn go_today(&mut self) {
        (self.year, self.month, _) = self.today;
    }

    pub fn on_today(&self) -> bool {
        (self.year, self.month) == (self.today.0, self.today.1)
    }

    pub fn title(&self) -> String {
        format!("{} {}", cal::MONTHS[self.month as usize - 1], self.year)
    }

    pub fn cells(&self) -> Vec<DayCell> {
        cal::month_grid(self.year, self.month, self.start)
            .iter()
            .map(|day| DayCell {
                label: day.day.to_string().into(),
                inside: day.inside,
                today: (day.year, day.month, day.day) == self.today,
            })
            .collect()
    }
}

fn show(window: &CalendarWindow, view: &View) {
    let cal = window.global::<Cal>();
    cal.set_title(view.title().into());
    cal.set_weekdays(ModelRc::new(VecModel::from(
        cal::weekday_names(view.start)
            .iter()
            .map(|name| (*name).into())
            .collect::<Vec<slint::SharedString>>(),
    )));
    cal.set_days(ModelRc::new(VecModel::from(view.cells())));
    cal.set_on_today(view.on_today());
}

/// Draws the view in the window and follows the buttons and keys.
/// `dismiss` is called when the popup asks to be closed.
pub fn attach(window: &CalendarWindow, view: View, dismiss: impl Fn() + 'static) {
    let view = Rc::new(RefCell::new(view));
    show(window, &view.borrow());
    let cal = window.global::<Cal>();
    let change = |change: fn(&mut View)| {
        let (weak, view) = (window.as_weak(), view.clone());
        move || {
            change(&mut view.borrow_mut());
            if let Some(window) = weak.upgrade() {
                show(&window, &view.borrow());
            }
        }
    };
    cal.on_previous(change(|view| view.step(-1)));
    cal.on_next(change(|view| view.step(1)));
    cal.on_go_today(change(View::go_today));
    cal.on_dismiss(dismiss);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_and_the_month_stepping() {
        let mut view = View::new((2026, 11, 26), WeekStart::Monday);
        assert_eq!(view.title(), "November 2026");
        assert!(view.on_today());
        view.step(2);
        assert_eq!(view.title(), "January 2027");
        assert!(!view.on_today());
        view.go_today();
        assert_eq!(view.title(), "November 2026");
        view.step(-11);
        assert_eq!(view.title(), "December 2025");
    }

    #[test]
    fn only_today_is_marked() {
        let view = View::new((2026, 11, 26), WeekStart::Monday);
        let cells = view.cells();
        assert_eq!(cells.len(), cal::CELLS);
        let marked: Vec<_> = cells.iter().filter(|c| c.today).collect();
        assert_eq!(marked.len(), 1);
        assert_eq!(marked[0].label, "26");
        assert!(marked[0].inside);
        // The 26th that ends October is in the grid and is not today.
        assert_eq!(cells[0].label, "26");
        assert!(!cells[0].today && !cells[0].inside);
        let mut next = view;
        next.step(1);
        assert!(next.cells().iter().all(|c| !c.today));
    }
}
