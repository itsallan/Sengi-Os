//! The selection screen and the countdown, drawn without a display and
//! driven with the pointer and keys. They write pictures when
//! SENGI_RENDER_DIR is set.

use std::cell::Cell;
use std::rc::Rc;

use sengi_shots::harness::{self, BACKDROP};
use sengi_snip::select::Rect;
use sengi_snip::ui::{CountdownWindow, SnipWindow, ThemeSwitch};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition};

fn picture(name: &str) {
    harness::advance(400);
    harness::draw(&harness::window(), BACKDROP).save(name);
}

fn moved(x: f32, y: f32) {
    harness::window().dispatch_event(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x, y),
    });
    harness::advance(16);
}

fn button(x: f32, y: f32, down: bool) {
    let position = LogicalPosition::new(x, y);
    let button = PointerEventButton::Left;
    harness::window().dispatch_event(if down {
        WindowEvent::PointerPressed { position, button }
    } else {
        WindowEvent::PointerReleased { position, button }
    });
    harness::advance(16);
}

fn open() -> (SnipWindow, Rc<Cell<Option<Option<Rect>>>>) {
    let window = SnipWindow::new().unwrap();
    sengi_shots::apply_look!(window.global::<ThemeSwitch>());
    window.show().unwrap();
    harness::resize(&harness::window(), 888, 600);
    let result = Rc::new(Cell::new(None));
    sengi_snip::attach_select(&window, {
        let result = result.clone();
        move |rect| result.set(Some(rect))
    });
    harness::advance(400);
    (window, result)
}

#[test]
fn selecting_an_area() {
    harness::install();
    let (window, result) = open();
    picture("snip-start-888x600.png");

    // Dragging shows the rectangle; letting go chooses it.
    moved(200.0, 150.0);
    button(200.0, 150.0, true);
    moved(520.0, 380.0);
    assert!(window.get_dragging());
    picture("snip-drag-888x600.png");
    button(520.0, 380.0, false);
    assert_eq!(
        result.take(),
        Some(Some(Rect {
            x: 200,
            y: 150,
            width: 320,
            height: 230
        }))
    );

    // Up and to the left, and past the edge, is the same maths.
    let (_window, result) = open();
    moved(300.0, 300.0);
    button(300.0, 300.0, true);
    moved(-40.0, 100.0);
    button(-40.0, 100.0, false);
    assert_eq!(
        result.take(),
        Some(Some(Rect {
            x: 0,
            y: 100,
            width: 300,
            height: 200
        }))
    );
}

#[test]
fn enter_takes_the_rectangle_so_far_and_escape_cancels() {
    harness::install();
    let (_window, result) = open();
    moved(100.0, 100.0);
    button(100.0, 100.0, true);
    moved(160.0, 140.0);
    harness::key(&harness::window(), &char::from(Key::Return).to_string());
    harness::advance(16);
    assert_eq!(
        result.take(),
        Some(Some(Rect {
            x: 100,
            y: 100,
            width: 60,
            height: 40
        }))
    );

    // Enter with nothing dragged does nothing.
    harness::key(&harness::window(), &char::from(Key::Return).to_string());
    harness::advance(16);
    assert_eq!(result.take(), None);

    harness::key(&harness::window(), &char::from(Key::Escape).to_string());
    harness::advance(16);
    assert_eq!(result.take(), Some(None));
}

#[test]
fn a_click_is_not_a_selection() {
    harness::install();
    let (window, result) = open();
    moved(50.0, 50.0);
    button(50.0, 50.0, true);
    button(51.0, 50.0, false);
    assert_eq!(result.take(), None);
    assert!(!window.get_dragging());
}

#[test]
fn the_countdown() {
    harness::install();
    let window = CountdownWindow::new().unwrap();
    sengi_shots::apply_look!(window.global::<ThemeSwitch>());
    window.show().unwrap();
    harness::resize(&harness::window(), 168, 72);
    let outcome = Rc::new(Cell::new(None));
    let _timer = sengi_snip::attach_countdown(&window, 5, {
        let outcome = outcome.clone();
        move |ran_out| outcome.set(Some(ran_out))
    });
    harness::advance(50);
    assert_eq!(window.get_seconds(), 5);
    picture("snip-countdown-168x72.png");
    harness::advance(2200);
    assert_eq!(window.get_seconds(), 3);
    assert_eq!(outcome.get(), None);
    harness::advance(3000);
    assert_eq!(outcome.get(), Some(true));
}
