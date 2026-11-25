//! The login screen and the lock screen, at full-screen sizes.

use sengi_shots::harness::{self, BACKDROP};
use sengi_shots::{greeter, lock};
use slint::ComponentHandle;

const SIZES: [(u32, u32); 3] = [(800, 600), (1366, 768), (1920, 1080)];

fn greet(variant: &str, w: &greeter::GreeterWindow) {
    w.set_time("9:41".into());
    w.set_period("".into());
    w.set_date("Tuesday, 25 November".into());
    w.set_user("Allan Smith".into());
    w.set_choice(false);
    w.set_placeholder("Password".into());
    w.set_layout("".into());
    match variant {
        "idle" => {}
        "typed" => w.set_length(9),
        "wrong" => {
            w.set_length(6);
            w.set_rejected(true);
            w.set_note("Wrong password".into());
            w.set_note_is_problem(true);
        }
        "waiting" => {
            w.set_enabled(false);
            w.set_length(6);
            w.set_rejected(true);
            w.set_note("Wrong password. Wait 4 seconds.".into());
            w.set_note_is_problem(true);
        }
        "busy" => {
            w.set_busy(true);
            w.set_enabled(false);
            w.set_length(8);
            w.set_placeholder("Checking…".into());
        }
        "choice" => {
            w.set_choice(true);
            w.set_user("A person with a really quite long full name indeed".into());
            w.set_layout("FR".into());
            w.set_period("PM".into());
            w.set_time("12:07".into());
        }
        "naming" => {
            w.set_user("".into());
            w.set_naming(true);
            w.set_caption("Who is logging in?".into());
            w.set_placeholder("User name".into());
            w.set_plain("allan".into());
        }
        "armed" => {
            w.set_restart_armed(true);
            w.set_note("Caps Lock is on".into());
            w.set_note_is_warning(true);
        }
        "dim" => w.set_prompt(false),
        "nopower" => w.set_power(false),
        _ => {}
    }
}

fn locked(variant: &str, w: &lock::LockWindow) {
    w.set_time("9:41".into());
    w.set_date("Tuesday, 25 November".into());
    w.set_user("Allan Smith".into());
    w.set_placeholder("Password".into());
    match variant {
        "idle" => {}
        "typed" => w.set_length(9),
        "wrong" => {
            w.set_length(6);
            w.set_rejected(true);
            w.set_note("Wrong password".into());
            w.set_note_is_problem(true);
        }
        "busy" => {
            w.set_busy(true);
            w.set_enabled(false);
            w.set_length(8);
            w.set_placeholder("Checking…".into());
        }
        "layout" => {
            w.set_layout("FR".into());
            w.set_period("AM".into());
            w.set_note("Caps Lock is on".into());
            w.set_note_is_warning(true);
        }
        _ => {}
    }
}

#[test]
fn login_and_lock() {
    harness::install();
    for variant in [
        "idle", "typed", "wrong", "waiting", "busy", "choice", "naming", "armed", "dim", "nopower",
    ] {
        for (width, height) in SIZES {
            let w = greeter::themed(greeter::GreeterWindow::new().unwrap());
            greet(variant, &w);
            w.show().unwrap();
            let win = harness::window();
            harness::resize(&win, width, height);
            harness::advance(500);
            harness::draw(&win, BACKDROP).save(&format!("greeter-{variant}-{width}x{height}.png"));
            w.hide().unwrap();
        }
    }
    for variant in ["idle", "typed", "wrong", "busy", "layout"] {
        for (width, height) in SIZES {
            let w = lock::themed(lock::LockWindow::new().unwrap());
            locked(variant, &w);
            w.show().unwrap();
            let win = harness::window();
            harness::resize(&win, width, height);
            harness::advance(500);
            harness::draw(&win, BACKDROP).save(&format!("lock-{variant}-{width}x{height}.png"));
            w.hide().unwrap();
        }
    }
}
