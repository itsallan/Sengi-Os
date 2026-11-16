//! Restarting the machine and shutting it down.
//!
//! Both are asked of logind through `systemctl`, as the session menu
//! does. Whether the login screen is allowed is logind's and polkit's to
//! say; this program has no privilege of its own to do either.

use std::io;
use std::process::{Command, Stdio};
use std::thread;

use crate::screen::Power;

/// The command for `power`. It never asks for a password: there is
/// nobody logged in to give one.
pub fn command(power: Power) -> Command {
    let mut command = Command::new("systemctl");
    command.arg("--no-ask-password").arg(match power {
        Power::Restart => "reboot",
        Power::ShutDown => "poweroff",
    });
    command.stdin(Stdio::null()).stdout(Stdio::null());
    command
}

/// Runs `command` on a thread of its own and calls `refused` if it did
/// not succeed. When it does succeed nothing is called: the machine is
/// on its way down.
pub fn carry_out(mut command: Command, refused: impl FnOnce() + Send + 'static) -> io::Result<()> {
    thread::Builder::new()
        .name("power".into())
        .spawn(move || {
            if !command.status().is_ok_and(|status| status.success()) {
                refused();
            }
        })
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn words(command: &Command) -> Vec<String> {
        std::iter::once(command.get_program())
            .chain(command.get_args())
            .map(|word| word.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn logind_is_asked_and_never_for_a_password() {
        assert_eq!(
            words(&command(Power::Restart)),
            ["systemctl", "--no-ask-password", "reboot"]
        );
        assert_eq!(
            words(&command(Power::ShutDown)),
            ["systemctl", "--no-ask-password", "poweroff"]
        );
    }

    #[test]
    fn only_a_failure_is_reported() {
        let (send, receive) = mpsc::channel();
        for program in ["false", "true", "/no/such/program"] {
            let send = send.clone();
            carry_out(Command::new(program), move || {
                let _ = send.send(program);
            })
            .unwrap();
        }
        drop(send);
        let mut refused: Vec<&str> = receive.iter().collect();
        refused.sort_unstable();
        assert_eq!(refused, ["/no/such/program", "false"]);
        assert!(receive.recv_timeout(Duration::from_millis(1)).is_err());
    }
}
