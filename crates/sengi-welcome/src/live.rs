//! The question of the live image: install Sengi OS, or try it first. Either
//! answer closes the window, and closing it any other way counts as trying
//! first. It is asked once per start of the computer.

use std::path::Path;
use std::rc::Rc;

use slint::{ComponentHandle, PlatformError};

use crate::marker;
use crate::system::{self, Runner};
use crate::ui::{Live, LiveWindow};

/// Connects the two answers to the window.
pub fn attach(window: &LiveWindow, runner: Rc<dyn Runner>) {
    let live = window.global::<Live>();
    let hide = {
        let weak = window.as_weak();
        move || {
            if let Some(window) = weak.upgrade() {
                let _ = window.hide();
            }
        }
    };
    live.on_install({
        let hide = hide.clone();
        move || {
            runner.open(system::installer_command());
            hide();
        }
    });
    live.on_try_first(hide);
}

/// Shows the question and returns when it is answered. It counts as asked
/// from the moment it is on screen, so that no way of ending it brings it
/// back before the next start.
pub fn run(
    window: LiveWindow,
    state_home: &Path,
    boot: &str,
    runner: Rc<dyn Runner>,
) -> Result<(), PlatformError> {
    attach(&window, runner);
    window.show()?;
    if let Err(error) = marker::mark_asked(state_home, boot) {
        eprintln!("sengi-welcome: cannot note that the question was asked: {error}");
    }
    window.run()
}
