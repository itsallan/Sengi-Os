//! The theme, dark or light: `appearance.theme`.
//!
//! Every program with a window reads the setting once as it starts, so a
//! change reaches the ones that are always running by starting them again.
//! The session already does that for a program that dies: the bar, the
//! dock and the desktop are each run in a loop by session-start, so ending
//! them is all it takes, and they are back within a second or two in the
//! new colours. The notification service is started by systemd on demand
//! and is restarted through it. The compositor reads the setting itself
//! when it is told the settings changed, and the picture or plain colour
//! behind the desktop is made again. Anything else, the launcher or the
//! quick settings panel, is not running between uses and takes the new
//! theme the next time it is opened. Restarting costs no memory the
//! programs did not use already, which a poller or a signal handler in each
//! of them would.

use std::process::{Command, Stdio};

/// The programs the session supervises, ended to have them come back.
pub const SUPERVISED: [&str; 3] = ["sengi-bar", "sengi-dock", "sengi-desktop"];

/// The user services that are restarted if they are running.
pub const SERVICES: [&str; 1] = ["sengi-notify.service"];

/// What is run, besides telling the compositor, to have the session show
/// the theme: each a program and its arguments.
pub fn restarts(uid: u32) -> Vec<Vec<String>> {
    let mut commands: Vec<Vec<String>> = SUPERVISED
        .iter()
        .map(|name| {
            ["pkill", "-U", &uid.to_string(), "-x", name]
                .map(String::from)
                .to_vec()
        })
        .collect();
    for service in SERVICES {
        commands.push(
            ["systemctl", "--user", "try-restart", service]
                .map(String::from)
                .to_vec(),
        );
    }
    commands
}

/// The type scale for a percent: 120 is 1.2.
pub fn scale_of(percent: u32) -> f32 {
    percent as f32 / 100.0
}

/// Show this window as `settings` say: the palette, the type size and the
/// motion, at once. The rest of the session follows with
/// [`apply_to_session`].
pub fn follow(window: &crate::ui::SettingsWindow, settings: &crate::settings::Settings) {
    use slint::ComponentHandle;
    let switch = window.global::<crate::ui::ThemeSwitch>();
    switch.set_light(settings.light());
    switch.set_high_contrast(settings.high_contrast());
    switch.set_text_scale(scale_of(settings.text_percent()));
    switch.set_reduce_motion(settings.reduce_motion());
}

/// Have the running session take up the theme as it is now in the file.
/// Blocks until done; call it off the UI thread.
pub fn apply_to_session() {
    crate::derive::reconfigure();
    crate::wallpaper::refresh();
    // SAFETY: getuid has no failure and no arguments.
    let uid = unsafe { libc::getuid() };
    for command in restarts(uid) {
        let _ = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_is_the_percent_over_a_hundred() {
        assert_eq!(scale_of(100), 1.0);
        assert_eq!(scale_of(90), 0.9);
        assert_eq!(scale_of(140), 1.4);
    }

    #[test]
    fn only_the_supervised_programs_of_this_user_are_ended_by_exact_name() {
        let commands = restarts(1000);
        let ended: Vec<&str> = commands
            .iter()
            .filter(|c| c[0] == "pkill")
            .map(|c| {
                assert_eq!(c[1..4], ["-U", "1000", "-x"]);
                c[4].as_str()
            })
            .collect();
        assert_eq!(ended, SUPERVISED);
    }

    #[test]
    fn the_notification_service_is_restarted_only_if_it_runs() {
        let commands = restarts(1000);
        assert!(commands
            .iter()
            .any(|c| c[..3] == ["systemctl", "--user", "try-restart"]
                && c[3] == "sengi-notify.service"));
    }
}
