//! Opening and taking away drives, by asking `udisksctl`. It is the one
//! way in this program to mount, unmount or power off, and it does only
//! those three things to a drive that `devices` listed: never format,
//! partition or write to a device. `udisksctl` talks to udisksd, which the
//! bus starts when it is first asked and which asks polkit; the person at
//! the screen is allowed, as for any drive plugged into their seat, with no
//! password. Nothing here runs between those requests.

use std::process::{Command, Stdio};

use sengi_apps::Env;

use crate::devices::Device;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    Mount,
    Unmount,
    PowerOff,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Mount => "mount",
            Verb::Unmount => "unmount",
            Verb::PowerOff => "power-off",
        }
    }
}

/// A kernel name is letters, digits, dashes and underscores. Anything else
/// is not passed to a program.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// What `udisksctl` said, as what to tell the person. None when what it
/// reports is not a problem (it is already as asked).
pub fn explain(verb: &str, title: &str, said: &str) -> Option<String> {
    let said = said.trim();
    let lower = said.to_lowercase();
    if lower.contains("alreadymounted") || lower.contains("already mounted") {
        return None;
    }
    if verb == "unmount" && (lower.contains("notmounted") || lower.contains("not mounted")) {
        return None;
    }
    let action = match verb {
        "mount" => "open",
        "unmount" => "eject",
        _ => "power off",
    };
    if lower.contains("devicebusy") || lower.contains("target is busy") || lower.contains("busy") {
        return Some(format!(
            "\"{title}\" is in use. Close the programs and windows that are using it, then try again."
        ));
    }
    if lower.contains("notauthorized") || lower.contains("not authorized") {
        return Some(format!(
            "You are not allowed to {action} \"{title}\" from this session."
        ));
    }
    if lower.contains("unknown filesystem")
        || lower.contains("unknownfilesystem")
        || lower.contains("wrong fs type")
        || lower.contains("not a mountable")
    {
        return Some(format!(
            "Sengi OS cannot read the file system on \"{title}\"."
        ));
    }
    if lower.contains("no such file or directory") && lower.contains("/dev/") {
        return Some(format!("\"{title}\" is not there any more."));
    }
    // The last line is the one with the reason; the rest is the program's
    // own prefix: "Error mounting /dev/sdb1: GDBus.Error:...Failed: ".
    let reason = said
        .lines()
        .last()
        .unwrap_or("")
        .rsplit("Error.Failed: ")
        .next()
        .unwrap_or("")
        .trim();
    if reason.is_empty() {
        Some(format!("Cannot {action} \"{title}\"."))
    } else {
        Some(format!("Cannot {action} \"{title}\": {reason}"))
    }
}

/// Runs `udisksctl VERB --block-device /dev/NAME`. Ok when it did what was
/// asked or it was so already.
fn run(env: &Env, verb: Verb, name: &str, title: &str) -> Result<(), String> {
    if !plain_name(name) {
        return Err(format!("\"{title}\" has a name that cannot be used."));
    }
    let Some(program) = env.find_program("udisksctl") else {
        return Err("Opening drives needs udisks2, which is not installed.".to_string());
    };
    let node = format!("/dev/{name}");
    let output = Command::new(program)
        .arg(verb.word())
        .arg("--block-device")
        .arg(&node)
        // A password box would have no one to answer it.
        .arg("--no-user-interaction")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot run udisksctl: {error}."))?;
    if output.status.success() {
        return Ok(());
    }
    let mut said = String::from_utf8_lossy(&output.stderr).into_owned();
    said.push_str(&String::from_utf8_lossy(&output.stdout));
    match explain(verb.word(), title, &said) {
        None => Ok(()),
        Some(text) => Err(text),
    }
}

/// Opens the drive: it is then mounted under /run/media/USER.
pub fn mount(env: &Env, device: &Device) -> Result<(), String> {
    run(env, Verb::Mount, &device.name, &device.title())
}

/// Takes the drive away: every opened volume on its disk is unmounted, and
/// then the disk is powered off, so that it is safe to pull out. A disk that
/// cannot be powered off is still unmounted, which is as much as can be
/// done. `all` is every drive listed, to find the others on the same disk.
pub fn eject(env: &Env, all: &[Device], device: &Device) -> Result<String, String> {
    for other in all.iter().filter(|other| other.disk == device.disk) {
        if other.is_mounted() {
            run(env, Verb::Unmount, &other.name, &other.title())?;
        }
    }
    let title = device.title();
    // Not every reader can be switched off; the data is safe all the same.
    let _ = run(env, Verb::PowerOff, &device.disk, &title);
    Ok(format!("\"{title}\" can be taken out now."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::tests::Temp;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn device(name: &str, disk: &str, label: &str, mounted: bool) -> Device {
        Device {
            name: name.to_string(),
            disk: disk.to_string(),
            label: label.to_string(),
            fs: "vfat".to_string(),
            size: 1_000_000_000,
            mount: mounted.then(|| PathBuf::from(format!("/run/media/live/{label}"))),
            read_only: false,
        }
    }

    /// A PATH with a `udisksctl` that writes its arguments to a log and
    /// then runs `body`.
    fn fake(t: &Temp, body: &str) -> (Env, PathBuf) {
        let bin = t.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log = t.path().join("calls.log");
        let script = format!("#!/bin/sh\necho \"$@\" >>'{}'\n{body}\n", log.display());
        let path = bin.join("udisksctl");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let path_var = bin.into_os_string();
        let env = Env::from_vars(move |name| (name == "PATH").then(|| path_var.clone()));
        (env, log)
    }

    #[test]
    fn mounting_asks_for_the_block_device_and_never_a_password() {
        let t = Temp::new();
        let (env, log) = fake(&t, "echo 'Mounted /dev/sdc1 at /run/media/live/S.'");
        mount(&env, &device("sdc1", "sdc", "S", false)).unwrap();
        let calls = fs::read_to_string(log).unwrap();
        assert_eq!(
            calls.trim(),
            "mount --block-device /dev/sdc1 --no-user-interaction"
        );
    }

    #[test]
    fn ejecting_unmounts_each_volume_of_the_disk_then_powers_it_off() {
        let t = Temp::new();
        let (env, log) = fake(&t, "exit 0");
        let all = [
            device("sdc1", "sdc", "ONE", true),
            device("sdc2", "sdc", "TWO", true),
            device("sdd1", "sdd", "OTHER", true),
            device("sdc3", "sdc", "THREE", false),
        ];
        let said = eject(&env, &all, &all[0]).unwrap();
        assert_eq!(said, "\"ONE\" can be taken out now.");
        let calls: Vec<String> = fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect();
        assert_eq!(
            calls,
            [
                "unmount --block-device /dev/sdc1 --no-user-interaction",
                "unmount --block-device /dev/sdc2 --no-user-interaction",
                "power-off --block-device /dev/sdc --no-user-interaction",
            ]
        );
    }

    #[test]
    fn a_busy_drive_is_not_powered_off_and_the_person_is_told() {
        let t = Temp::new();
        let (env, log) = fake(
            &t,
            "echo 'Error unmounting /dev/sdc1: GDBus.Error:org.freedesktop.UDisks2.Error.DeviceBusy: target is busy' >&2; exit 1",
        );
        let stick = device("sdc1", "sdc", "S", true);
        let error = eject(&env, std::slice::from_ref(&stick), &stick).unwrap_err();
        assert!(error.contains("\"S\" is in use"), "{error}");
        let calls = fs::read_to_string(log).unwrap();
        assert!(!calls.contains("power-off"), "{calls}");
    }

    #[test]
    fn a_reader_that_cannot_be_powered_off_is_still_ejected() {
        let t = Temp::new();
        let (env, _log) = fake(
            &t,
            "case \"$1\" in power-off) echo 'NotSupported' >&2; exit 1;; esac",
        );
        let stick = device("sdc1", "sdc", "S", true);
        assert!(eject(&env, std::slice::from_ref(&stick), &stick).is_ok());
    }

    #[test]
    fn what_udisksctl_says_becomes_a_sentence() {
        let busy = explain("unmount", "S", "target is busy").unwrap();
        assert!(busy.contains("in use"));
        assert!(explain(
            "mount",
            "S",
            "GDBus.Error:org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain: x"
        )
        .unwrap()
        .contains("not allowed to open"));
        assert!(
            explain("mount", "S", "Error mounting: unknown filesystem type 'x'")
                .unwrap()
                .contains("cannot read the file system")
        );
        assert_eq!(
            explain("mount", "S", "Error mounting /dev/sdc1: GDBus.Error:org.freedesktop.UDisks2.Error.AlreadyMounted: x"),
            None
        );
        assert_eq!(
            explain(
                "unmount",
                "S",
                "GDBus.Error:org.freedesktop.UDisks2.Error.NotMounted: x"
            ),
            None
        );
        assert_eq!(
            explain(
                "mount",
                "S",
                "Error mounting /dev/sdc1: GDBus.Error:org.freedesktop.UDisks2.Error.Failed: Something odd"
            )
            .unwrap(),
            "Cannot open \"S\": Something odd"
        );
    }

    #[test]
    fn without_udisksctl_the_message_says_what_is_missing() {
        let env = Env::from_vars(|name| (name == "PATH").then(|| OsString::from("/nonexistent")));
        let error = mount(&env, &device("sdc1", "sdc", "S", false)).unwrap_err();
        assert!(error.contains("udisks2"), "{error}");
    }

    #[test]
    fn a_name_that_is_not_a_kernel_name_is_never_passed_on() {
        let env = Env::from_vars(|_| None);
        let mut bad = device("sdc1", "sdc", "S", false);
        bad.name = "../etc".to_string();
        assert!(mount(&env, &bad).unwrap_err().contains("cannot be used"));
    }
}
