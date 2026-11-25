//! Glue between the window and the calculator's state.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::calc::Calc;
use crate::ui::{CalcWindow, HistoryItem, Pad};

/// How long "Copied" stays under the display.
const NOTICE: Duration = Duration::from_millis(1500);

pub struct App {
    window: slint::Weak<CalcWindow>,
    calc: RefCell<Calc>,
    notice_timer: slint::Timer,
}

pub fn run(window: CalcWindow) -> Result<(), slint::PlatformError> {
    let _app = attach(&window);
    window.run()
}

/// Connects the state to the window. The app lives as long as the value
/// returned.
pub fn attach(window: &CalcWindow) -> Rc<App> {
    let app = Rc::new(App {
        window: window.as_weak(),
        calc: RefCell::new(Calc::new()),
        notice_timer: slint::Timer::default(),
    });
    let pad = window.global::<Pad>();
    pad.on_key({
        let app = app.clone();
        move |text| {
            let taken = app.calc.borrow_mut().key(&text);
            app.refresh();
            taken
        }
    });
    pad.on_recall({
        let app = app.clone();
        move |index| {
            app.calc.borrow_mut().recall(index as usize);
            app.refresh();
        }
    });
    pad.on_copied({
        let app = app.clone();
        move || app.say("Copied")
    });
    pad.on_close_requested({
        let window = window.as_weak();
        move || {
            if let Some(window) = window.upgrade() {
                let _ = window.hide();
            }
        }
    });
    app.refresh();
    app
}

impl App {
    fn with_pad<T>(&self, f: impl FnOnce(Pad<'_>) -> T) -> Option<T> {
        self.window
            .upgrade()
            .map(|window| f(window.global::<Pad>()))
    }

    fn refresh(&self) {
        let calc = self.calc.borrow();
        let history: Vec<HistoryItem> = calc
            .history()
            .iter()
            .map(|entry| HistoryItem {
                expression: entry.expression.as_str().into(),
                result: entry.result.as_str().into(),
            })
            .collect();
        self.with_pad(|pad| {
            pad.set_expression(calc.shown().into());
            pad.set_preview(calc.preview().unwrap_or_default().into());
            pad.set_error(calc.error().map_or("", |e| e.message()).into());
            pad.set_copy_text(calc.copy_text().unwrap_or_default().into());
            pad.set_notice("".into());
            pad.set_history(ModelRc::new(VecModel::from(history)));
        });
    }

    fn say(&self, notice: &str) {
        self.with_pad(|pad| pad.set_notice(notice.into()));
        let window = self.window.clone();
        self.notice_timer
            .start(slint::TimerMode::SingleShot, NOTICE, move || {
                if let Some(window) = window.upgrade() {
                    window.global::<Pad>().set_notice("".into());
                }
            });
    }

    /// What is typed, as it is worked out.
    pub fn expression(&self) -> String {
        self.calc.borrow().expression().to_string()
    }
}
