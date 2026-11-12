//! The Notifications page: do not disturb, and clearing what is on
//! screen. Both are the notification service's own, reached through its
//! command as the quick settings panel does: `sengi-notify --dnd
//! on|off|status` and `sengi-notify --dismiss-all`.

use std::rc::Rc;

use slint::ComponentHandle;

use sengi_system::commands::{DISMISS_ALL, DND_FLAG, DND_OFF, DND_ON, DND_STATUS, NOTIFY};
use sengi_system::programs::{find_program, output, parse_dnd};

use crate::app::{self, App, Controller};
use crate::ui::NotificationsData;

/// Run the command with `args`, then ask for the state and show it.
fn act(args: &'static [&'static str]) {
    std::thread::spawn(move || {
        let path = std::env::var("PATH").unwrap_or_default();
        let installed = find_program(NOTIFY, &path).is_some();
        if installed && !args.is_empty() {
            output(NOTIFY, args);
        }
        let dnd = if installed {
            output(NOTIFY, &[DND_FLAG, DND_STATUS]).and_then(|printed| parse_dnd(&printed))
        } else {
            None
        };
        app::post(move |app| {
            let data = app.window.global::<NotificationsData>();
            data.set_available(dnd.is_some());
            data.set_dnd(dnd == Some(true));
            data.set_note(
                match (installed, dnd) {
                    (false, _) => "The notification service (sengi-notify) is not installed.",
                    (true, None) => "The notification service is not answering.",
                    _ => "",
                }
                .into(),
            );
        });
    });
}

pub struct NotificationsPage;

impl NotificationsPage {
    pub fn attach(app: &Rc<App>) -> Rc<NotificationsPage> {
        let data = app.window.global::<NotificationsData>();
        data.on_set_dnd(|on| {
            act(if on {
                &[DND_FLAG, DND_ON]
            } else {
                &[DND_FLAG, DND_OFF]
            });
        });
        data.on_clear(|| act(&[DISMISS_ALL]));
        Rc::new(NotificationsPage)
    }
}

impl Controller for NotificationsPage {
    fn show(&self, _app: &Rc<App>) {
        act(&[]);
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}
