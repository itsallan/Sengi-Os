//! The real popup, drawn without a display. It checks what is on it and
//! writes pictures when SENGI_RENDER_DIR is set.

use sengi_calendar::cal::WeekStart;
use sengi_calendar::ui::{Cal, CalendarWindow, ThemeSwitch};
use sengi_calendar::view::{self, View};
use sengi_shots::harness::{self, BACKDROP};
use slint::{ComponentHandle, Model};

fn picture(name: &str) {
    harness::advance(400);
    harness::draw(&harness::window(), BACKDROP).save(name);
}

#[test]
fn calendar_popup() {
    harness::install();
    let window = CalendarWindow::new().unwrap();
    window.global::<ThemeSwitch>().set_light(harness::light());
    window.show().unwrap();
    harness::resize(&harness::window(), 888, 600);
    view::attach(&window, View::new((2026, 11, 26), WeekStart::Monday), || {});
    harness::advance(400);
    let cal = window.global::<Cal>();

    assert_eq!(cal.get_title(), "November 2026");
    assert_eq!(cal.get_days().row_count(), 42);
    assert!(cal.get_on_today());
    picture("calendar-today-888x600.png");

    cal.invoke_next();
    assert_eq!(cal.get_title(), "December 2026");
    assert!(!cal.get_on_today());
    cal.invoke_previous();
    cal.invoke_previous();
    assert_eq!(cal.get_title(), "October 2026");
    picture("calendar-october-888x600.png");
    cal.invoke_go_today();
    assert_eq!(cal.get_title(), "November 2026");

    // The arrow keys move the month, Home comes back.
    let w = harness::window();
    harness::key(
        &w,
        &char::from(slint::platform::Key::RightArrow).to_string(),
    );
    harness::advance(32);
    assert_eq!(cal.get_title(), "December 2026");
    harness::key(&w, &char::from(slint::platform::Key::Home).to_string());
    harness::advance(32);
    assert_eq!(cal.get_title(), "November 2026");
}

#[test]
fn a_week_that_starts_on_sunday() {
    harness::install();
    let window = CalendarWindow::new().unwrap();
    window.global::<ThemeSwitch>().set_light(harness::light());
    window.show().unwrap();
    harness::resize(&harness::window(), 888, 600);
    view::attach(&window, View::new((2024, 2, 29), WeekStart::Sunday), || {});
    harness::advance(400);
    let cal = window.global::<Cal>();
    assert_eq!(cal.get_title(), "February 2024");
    assert_eq!(cal.get_weekdays().row_data(0).unwrap(), "Su");
    picture("calendar-february-2024-sunday-888x600.png");
}
