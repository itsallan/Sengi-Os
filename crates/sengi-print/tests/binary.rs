//! The helper as a program: arguments in, CUPS tools run, words and an
//! exit status out, with fake lpadmin and lpinfo standing in for the real
//! ones. The helper only takes made-up programs when it is not root, so
//! these tests are skipped by root.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn root() -> bool {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sengi-print-bin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn helper(dir: &Path) -> Command {
    let lpadmin = script(dir, "lpadmin", "echo \"$*\" >>\"$(dirname \"$0\")/calls\"");
    let lpinfo = script(
        dir,
        "lpinfo",
        "echo \"$*\" >>\"$(dirname \"$0\")/calls\"; \
         echo 'network dnssd://Office%20Laser._ipp._tcp.local/?uuid=1'; \
         echo 'network ipp'; sleep $(cat \"$(dirname \"$0\")/sleep\" 2>/dev/null || echo 0)",
    );
    // No queue is there yet.
    let lpstat = script(dir, "lpstat", "exit 1");
    let mut command = Command::new(env!("CARGO_BIN_EXE_sengi-print"));
    command
        .env("SENGI_PRINT_LPADMIN", lpadmin)
        .env("SENGI_PRINT_LPSTAT", lpstat)
        .env("SENGI_PRINT_LPINFO", lpinfo)
        .stdin(Stdio::null());
    command
}

#[test]
fn add_and_remove_reach_lpadmin() {
    if root() {
        return;
    }
    let dir = scratch();
    let done = helper(&dir)
        .args(["add", "Office", "ipp://192.168.1.50/ipp/print"])
        .output()
        .unwrap();
    assert_eq!(done.status.code(), Some(0));
    let done = helper(&dir).args(["remove", "Office"]).output().unwrap();
    assert_eq!(done.status.code(), Some(0));
    let calls = fs::read_to_string(dir.join("calls")).unwrap();
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            "-p Office -E -v ipp://192.168.1.50/ipp/print -m everywhere -o printer-is-shared=false",
            "-x Office"
        ]
    );
}

#[test]
fn unsafe_arguments_exit_with_usage_and_run_nothing() {
    if root() {
        return;
    }
    let dir = scratch();
    for args in [
        vec!["add", "-E", "ipp://h/ipp/print"],
        vec!["add", "Office", "file:///etc/shadow"],
        vec!["add", "Office", "ipp://h/ipp/print;reboot"],
        vec!["remove", "Office;reboot"],
        vec!["remove", "--help-me"],
        vec!["discover", "--extra"],
        vec!["restart"],
        vec![],
    ] {
        let done = helper(&dir).args(&args).output().unwrap();
        assert_eq!(done.status.code(), Some(2), "{args:?}");
    }
    assert!(!dir.join("calls").exists(), "a program was run");
}

#[test]
fn discovery_prints_what_lpinfo_found() {
    if root() {
        return;
    }
    let dir = scratch();
    // The input stays open until the search is over: closing it is a stop.
    let mut child = helper(&dir)
        .arg("discover")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = child.stdin.take();
    let done = child.wait_with_output().unwrap();
    drop(input);
    assert_eq!(done.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&done.stdout),
        "network dnssd://Office%20Laser._ipp._tcp.local/?uuid=1\n"
    );
    let calls = fs::read_to_string(dir.join("calls")).unwrap();
    assert_eq!(calls.trim(), "--include-schemes dnssd,ipp -v");
}

#[test]
fn closing_the_input_ends_a_search_that_is_still_running() {
    if root() {
        return;
    }
    let dir = scratch();
    // The fake lpinfo reads how long to take from a file: the helper gives
    // its programs an environment of their own.
    fs::write(dir.join("sleep"), "30").unwrap();
    let mut child = helper(&dir)
        .arg("discover")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let began = Instant::now();
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(3));
    assert!(began.elapsed() < Duration::from_secs(10));
}
