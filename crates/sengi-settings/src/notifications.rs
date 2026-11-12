//! The Notifications page: do not disturb, and clearing what is on
//! screen. Both are the notification service's own, reached through its
//! command as the quick settings panel does: `sengi-notify --dnd
//! on|off|status` and `sengi-notify --dismiss-all`.

use std::process::{Command, Stdio};
use std::rc::Rc;

use slint::ComponentHandle;

use sengi_system::programs::{find_program, parse_dnd};

use crate::app::{self, App, Controller};
use crate::ui::NotificationsData;

const NOTIFY: &str = "sengi-notify";

fn run(args: &[&str]) -> Option<String> {
    let output = Command::new(NOTIFY)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run the command with `args`, then ask for the state and show it.
fn act(args: &'static [&'static str]) {
    std::thread::spawn(move || {
        let path = std::env::var("PATH").unwrap_or_default();
        let installed = find_program(NOTIFY, &path).is_some();
        if installed && !args.is_empty() {
            run(args);
        }
        let dnd = if installed {
            run(&["--dnd", "status"]).and_then(|output| parse_dnd(&output))
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
                &["--dnd", "on"]
            } else {
                &["--dnd", "off"]
            });
        });
        data.on_clear(|| act(&["--dismiss-all"]));
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
