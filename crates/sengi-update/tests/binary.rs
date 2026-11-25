//! The helper as a program: arguments in, JSON lines out, with a fake
//! apt-get standing in for the real one. The helper only takes a made-up
//! system when it is not root, so these tests are skipped by root.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use sengi_update::Event;

const APT: &str = r#"#!/bin/sh
echo "$*" >>"$(dirname "$0")/calls.log"
case " $* " in
*" update "*)
  echo 'dlstatus:1:100.0000:Retrieving file 1 of 1' >&3 ;;
*" -s "*)
  echo 'Need to get 4,000 kB/40 MB of archives.'
  echo 'Inst openssl [1] (2 Debian-Security:13/stable-security [amd64])'
  echo 'Inst tzdata [1] (2 Debian:13/stable [all])' ;;
*" -d "*)
  echo 'dlstatus:1:50.0000:Retrieving file 1 of 2' >&3
  echo 'dlstatus:2:100.0000:Retrieving file 2 of 2' >&3 ;;
*)
  echo 'pmstatus:openssl:50.0000:Unpacking openssl' >&3
  echo 'pmstatus:tzdata:100.0000:Setting up tzdata' >&3 ;;
esac
"#;

fn root() -> bool {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

static NEXT: AtomicU32 = AtomicU32::new(0);

fn system() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sengi-update-bin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let apt = dir.join("apt-get");
    fs::write(&apt, APT).unwrap();
    fs::set_permissions(&apt, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn run(dir: Option<&Path>, args: &[&str]) -> (i32, Vec<Event>, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sengi-update"));
    command
        .args(args)
        .env_remove("SENGI_UPDATE_TEST_DIR")
        .stdin(Stdio::null());
    if let Some(dir) = dir {
        command.env("SENGI_UPDATE_TEST_DIR", dir);
    }
    let output = command.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let events = text.lines().filter_map(Event::from_line).collect();
    (output.status.code().unwrap_or(-1), events, text)
}

#[test]
fn check_lists_what_would_be_installed() {
    if root() {
        return;
    }
    let dir = system();
    let (code, events, _) = run(Some(&dir), &["check"]);
    assert_eq!(code, 0);
    let names: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::Package(p) => Some(p.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["openssl", "tzdata"]);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::Checked { download: Some(40_000_000), time } if *time > 0
    )));
    // A check never installs or downloads packages.
    let calls = fs::read_to_string(dir.join("calls.log")).unwrap();
    assert!(!calls.contains(" -d ") && !calls.contains("-y"), "{calls}");
}

#[test]
fn apply_goes_through_the_phases_and_ends_with_done() {
    if root() {
        return;
    }
    let dir = system();
    let (code, events, text) = run(Some(&dir), &["apply"]);
    assert_eq!(code, 0, "{text}");
    assert!(matches!(
        events.last(),
        Some(Event::Done { upgraded: 2, .. })
    ));
    let calls = fs::read_to_string(dir.join("calls.log")).unwrap();
    assert_eq!(calls.lines().count(), 4, "{calls}");
    for line in calls.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        for banned in ["dist-upgrade", "full-upgrade", "remove", "purge", "install"] {
            assert!(!words.contains(&banned), "{line}");
        }
    }
}

#[test]
fn status_is_one_line() {
    if root() {
        return;
    }
    let dir = system();
    let (code, events, text) = run(Some(&dir), &["status"]);
    assert_eq!(code, 0);
    assert_eq!(text.lines().count(), 1);
    assert!(matches!(
        events[0],
        Event::Status {
            live: false,
            busy: false,
            reboot_required: false,
            battery: None
        }
    ));
}

#[test]
fn not_root_and_not_a_test_means_no_apply() {
    if root() {
        return;
    }
    let (code, events, _) = run(None, &["apply"]);
    assert_eq!(code, 4);
    assert!(matches!(&events[0], Event::Refused { code, .. } if code == "root"));
}

#[test]
fn anything_but_one_known_word_is_refused_without_running_apt() {
    let dir = system();
    for args in [
        vec![],
        vec!["apply", "--force"],
        vec!["dist-upgrade"],
        vec!["install", "vim"],
        vec!["check", "apply"],
        vec!["--apply"],
    ] {
        let (code, events, text) = run(Some(&dir), &args);
        assert_eq!(code, 2, "{args:?}");
        assert!(events.is_empty() && text.is_empty(), "{args:?}: {text}");
    }
    assert!(!dir.join("calls.log").exists());
}

#[test]
fn it_says_its_version() {
    let (code, _, text) = run(None, &["--version"]);
    assert_eq!(code, 0);
    assert!(text.starts_with("sengi-update "));
}
