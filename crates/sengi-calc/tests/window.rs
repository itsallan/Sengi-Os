//! The real calculator window, drawn without a display and driven with the
//! keyboard. It writes pictures when SENGI_RENDER_DIR is set.

use sengi_calc::app;
use sengi_calc::ui::{CalcWindow, Pad, ThemeSwitch};
use sengi_shots::harness::{self, BACKDROP};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, Model};

fn type_text(text: &str) {
    let window = harness::window();
    for c in text.chars() {
        harness::key(&window, &c.to_string());
    }
    harness::advance(32);
}

fn press(key: Key) {
    type_text(&String::from(char::from(key)));
}

fn picture(name: &str) {
    harness::advance(400);
    harness::draw(&harness::window(), BACKDROP).save(name);
}

#[test]
fn calculator_window() {
    harness::install();
    let window = CalcWindow::new().unwrap();
    sengi_shots::apply_look!(window.global::<ThemeSwitch>());
    window.show().unwrap();
    harness::resize(&harness::window(), 320, 480);
    let app = app::attach(&window);
    harness::advance(400);
    let pad = window.global::<Pad>();
    picture("calc-empty.png");

    // Keys typed on the keyboard.
    type_text("0.1+0.2");
    assert_eq!(app.expression(), "0.1+0.2");
    assert_eq!(pad.get_expression(), "0.1+0.2");
    assert_eq!(pad.get_preview(), "0.3");
    picture("calc-typing.png");
    press(Key::Return);
    assert_eq!(pad.get_expression(), "0.3");
    assert_eq!(pad.get_history().row_count(), 1);

    type_text("*(2+3)%");
    assert_eq!(pad.get_preview(), "0.015");
    press(Key::Backspace);
    type_text("=");
    assert_eq!(pad.get_expression(), "1.5");

    // A mistake is said, and kept for mending.
    press(Key::Escape);
    type_text("8/0=");
    assert_eq!(pad.get_error(), "Cannot divide by zero");
    assert_eq!(pad.get_expression(), "8\u{f7}0");
    picture("calc-error.png");
    press(Key::Backspace);
    type_text("4=");
    assert_eq!(pad.get_expression(), "2");
    assert_eq!(pad.get_error(), "");

    // Ctrl+C takes the answer.
    assert_eq!(pad.get_copy_text(), "2");
    let w = harness::window();
    w.dispatch_event(WindowEvent::KeyPressed {
        text: char::from(Key::Control).to_string().into(),
    });
    harness::key(&w, "c");
    w.dispatch_event(WindowEvent::KeyReleased {
        text: char::from(Key::Control).to_string().into(),
    });
    harness::advance(32);
    assert_eq!(pad.get_notice(), "Copied");

    // The history, with a few sums in it.
    for sum in ["12*12=", "1/3=", "100-0.01="] {
        press(Key::Escape);
        type_text(sum);
    }
    assert_eq!(pad.get_history().row_count(), 5);
    picture("calc-history.png");
    for (width, height) in [(720, 480), (888, 600)] {
        harness::resize(&harness::window(), width, height);
        picture(&format!("calc-history-{width}x{height}.png"));
    }
    harness::resize(&harness::window(), 320, 480);
    pad.invoke_recall(0);
    assert_eq!(pad.get_expression(), "1.5");
}
