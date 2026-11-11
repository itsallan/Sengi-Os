//! The Notifications page: do not disturb, and clearing what is on
//! screen. Both are the notification service's own, reached through its
//! command as the quick settings panel does: `sengi-notify --dnd
//! on|off|status` and `sengi-notify --dismiss-all`.

use std::path::Path;
use std::process::{Command, Stdio};
use std::rc::Rc;

use slint::ComponentHandle;

use crate::app::{self, App, Controller};
use crate::ui::NotificationsData;

const NOTIFY: &str = "sengi-notify";

/// Where `program` is on a `PATH`-style list of directories.
pub fn on_path(program: &str, path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .any(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// What `sengi-notify --dnd status` printed, if it was an answer.
pub fn parse_dnd(output: &str) -> Option<bool> {
    match output.trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

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
        let installed = on_path(NOTIFY, &std::env::var("PATH").unwrap_or_default());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_service_is_looked_for_on_the_path() {
        let tmp = TempDir::new();
        tmp.write("bin/sengi-notify", "#!/bin/sh\n");
        tmp.write("bin/readme", "not a program\n");
        let bin = tmp.path().join("bin");
        std::fs::set_permissions(
            bin.join("sengi-notify"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::set_permissions(bin.join("readme"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let path = format!("/nonexistent::{}", bin.display());
        assert!(on_path("sengi-notify", &path));
        assert!(!on_path("readme", &path));
        assert!(!on_path("bin", &tmp.path().display().to_string()));
        assert!(!on_path("sengi-notify", ""));
    }

    #[test]
    fn dnd_answers() {
        assert_eq!(parse_dnd("on\n"), Some(true));
        assert_eq!(parse_dnd("off"), Some(false));
        assert_eq!(parse_dnd(""), None);
        assert_eq!(parse_dnd("--dnd takes on, off or toggle"), None);
    }
}
