//! The menu a program has open, which holds the keyboard and the pointer
//! until it is dismissed. Smithay ends it for a click elsewhere or a key
//! after it is gone; here is what has to end it besides.

use smithay::{
    desktop::{PopupPointerGrab, PopupUngrabStrategy, Window},
    utils::SERIAL_COUNTER,
};

use crate::state::State;

impl State {
    /// The window the open menu hangs off, if it hangs off a window.
    pub fn menu_window(&self) -> Option<Window> {
        let (root, _) = self.menu.as_ref()?;
        self.window_for_surface(root)
    }

    /// Tells the program its menu is closed, and takes the keyboard and
    /// the pointer back from it.
    pub fn dismiss_menu(&mut self) {
        let Some((_, mut grab)) = self.menu.take() else {
            return;
        };
        if !grab.has_ended() {
            grab.ungrab(PopupUngrabStrategy::All);
        }
        self.let_go_of_menu(grab.serial());
    }

    /// A menu that its program took away, or that Smithay dismissed, lets
    /// go at once and not only at the next key or movement; and the keyboard
    /// goes to whoever should have it by now.
    pub fn release_ended_menu(&mut self) {
        let ended = self.menu.as_ref().is_some_and(|(root, grab)| {
            grab.has_ended() || grab.current_grab().as_ref() == Some(root)
        });
        if !ended {
            return;
        }
        if let Some((_, grab)) = self.menu.take() {
            self.let_go_of_menu(grab.serial());
        }
    }

    fn let_go_of_menu(&mut self, serial: smithay::utils::Serial) {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if keyboard.has_grab(serial) {
            keyboard.unset_grab(self);
        }
        let held = pointer.has_grab(serial)
            && pointer
                .with_grab(|_, grab| grab.is::<PopupPointerGrab<State>>())
                .unwrap_or(false);
        if held {
            let time = self.start_time.elapsed().as_millis() as u32;
            pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), time);
            self.pointer_stale = true;
        }
        self.update_keyboard_focus();
        self.request_redraw();
    }
}
