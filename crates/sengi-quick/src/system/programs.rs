//! The services reached by running a program: PipeWire through `wpctl`
//! and do-not-disturb through the notification service's own command.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::commands;
use crate::services::{AudioService, DndService};
use crate::volume::{self, Volume};

/// Run to the end and return what it printed, if it succeeded.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
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

pub struct Wpctl;

impl AudioService for Wpctl {
    fn read(&self) -> Option<Volume> {
        volume::parse(&output(commands::WPCTL, &["get-volume", commands::SINK])?)
    }

    fn set_volume(&self, percent: u32) {
        let args = volume::set_volume_args(percent);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        output(commands::WPCTL, &args);
    }

    fn set_muted(&self, muted: bool) {
        let args = volume::set_mute_args(muted);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        output(commands::WPCTL, &args);
    }
}

/// Where `program` is on a `PATH`-style list of directories.
pub fn find_program(program: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| {
            candidate
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// What `sengi-notify --dnd status` printed, if it was an answer.
pub fn parse_dnd(output: &str) -> Option<bool> {
    match output.trim() {
        commands::DND_ON => Some(true),
        commands::DND_OFF => Some(false),
        _ => None,
    }
}

pub struct Notify {
    installed: bool,
}

impl Notify {
    pub fn new() -> Notify {
        let path = std::env::var("PATH").unwrap_or_default();
        Notify {
            installed: find_program(commands::NOTIFY, &path).is_some(),
        }
    }
}

impl Default for Notify {
    fn default() -> Notify {
        Notify::new()
    }
}

impl DndService for Notify {
    fn available(&self) -> bool {
        self.installed
    }

    fn read(&self) -> Option<bool> {
        let args = [commands::DND_FLAG, commands::DND_STATUS];
        parse_dnd(&output(commands::NOTIFY, &args)?)
    }

    fn set(&self, on: bool) {
        let state = if on {
            commands::DND_ON
        } else {
            commands::DND_OFF
        };
        output(commands::NOTIFY, &[commands::DND_FLAG, state]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn programs_are_found_on_the_path() {
        let tmp = TempDir::new();
        tmp.write("bin/sengi-notify", "#!/bin/sh\n");
        tmp.write("bin/readme", "not a program\n");
        tmp.write("other/placeholder", "");
        let bin = tmp.path().join("bin");
        let program = bin.join("sengi-notify");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(bin.join("readme"), std::fs::Permissions::from_mode(0o644))
            .unwrap();

        let path = format!(
            "{}::{}:/nonexistent",
            tmp.path().join("other").display(),
            bin.display()
        );
        assert_eq!(find_program("sengi-notify", &path), Some(program));
        // Present but not executable, a directory, and not there at all.
        assert_eq!(find_program("readme", &path), None);
        assert_eq!(find_program("bin", &tmp.path().display().to_string()), None);
        assert_eq!(find_program("sengi-notify", ""), None);
    }

    #[test]
    fn dnd_answers() {
        assert_eq!(parse_dnd("on\n"), Some(true));
        assert_eq!(parse_dnd("off"), Some(false));
        // An older service that does not know the question.
        assert_eq!(parse_dnd(""), None);
        assert_eq!(
            parse_dnd("--dnd takes on, off or toggle, not 'status'"),
            None
        );
    }
}
