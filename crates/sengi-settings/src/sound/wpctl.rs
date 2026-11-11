//! PipeWire through WirePlumber's `wpctl`, as sengi-quick does for the
//! volume keys, and `pw-dump --monitor` to hear about changes.
//!
//! `wpctl` has no way to wait for a change, and looking every second
//! would wake the machine for nothing. `pw-dump -m` prints whenever
//! anything in PipeWire changes; nothing it prints is read for meaning,
//! only for the fact that it printed. It is a child of this process for
//! as long as the Sound page has been opened, and is killed when the
//! window closes, or by the kernel if this process dies first.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use super::{parse_status, Audio, AudioService, MAX_PERCENT};
use crate::worker::Notify;

const WPCTL: &str = "wpctl";
const MONITOR: &str = "pw-dump";

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

/// The arguments that set a volume. `-l 1` holds it at 100% whatever is
/// asked for.
pub fn volume_args(id: u32, percent: u32) -> [String; 5] {
    [
        "set-volume".to_string(),
        "-l".to_string(),
        "1".to_string(),
        id.to_string(),
        format!("{}%", percent.min(MAX_PERCENT)),
    ]
}

#[derive(Default)]
pub struct Wpctl {
    monitor: Mutex<Option<Child>>,
}

impl AudioService for Wpctl {
    fn read(&self) -> Option<Audio> {
        output(WPCTL, &["status"]).map(|status| parse_status(&status))
    }

    fn set_default(&self, id: u32) {
        output(WPCTL, &["set-default", &id.to_string()]);
    }

    fn set_volume(&self, id: u32, percent: u32) {
        let args = volume_args(id, percent);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        output(WPCTL, &args);
    }

    fn set_muted(&self, id: u32, muted: bool) {
        output(
            WPCTL,
            &["set-mute", &id.to_string(), if muted { "1" } else { "0" }],
        );
    }

    fn watch(&self, changed: Notify) {
        let mut command = Command::new(MONITOR);
        command
            .arg("--monitor")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // SAFETY: the closure runs in the child between fork and exec and
        // makes one async-signal-safe system call. It has the kernel end
        // the monitor if this process dies without cleaning up, as it
        // would on an abort.
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let Ok(mut child) = command.spawn() else {
            // No monitor: the page still works, it just does not hear
            // about changes made elsewhere until it is opened again.
            return;
        };
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return;
        };
        *self.monitor.lock().unwrap_or_else(|p| p.into_inner()) = Some(child);
        let spawned = std::thread::Builder::new()
            .name("settings-sound-watch".to_string())
            .spawn(move || {
                let mut buffer = [0u8; 8192];
                // The first thing it prints is everything there is, which
                // is no news. Its end is told from what follows by the
                // quiet after it; the worker folds a burst into one read.
                while let Ok(read) = stdout.read(&mut buffer) {
                    if read == 0 {
                        break;
                    }
                    changed();
                }
            });
        if spawned.is_err() {
            self.close();
        }
    }

    fn close(&self) {
        let child = self
            .monitor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_volume_never_goes_above_the_top_of_the_slider() {
        assert_eq!(volume_args(46, 40), ["set-volume", "-l", "1", "46", "40%"]);
        assert_eq!(volume_args(46, 400)[4], "100%");
    }

    #[test]
    fn closing_without_a_monitor_is_harmless() {
        let service = Wpctl::default();
        service.close();
        service.close();
    }
}
