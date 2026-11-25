//! The helper's work: `check`, `apply` and `status`.
//!
//! `apply` runs as root through pkexec and does exactly this, in this
//! order, and nothing else:
//!
//! 1. refuse on the live system, on a low battery, or while another apt
//!    or dpkg runs;
//! 2. `apt-get update`;
//! 3. `apt-get -s upgrade`, and refuse if it would remove anything;
//! 4. `apt-get -d upgrade`: download, which can be cancelled;
//! 5. `apt-get upgrade`: install, which cannot be, and ignores the
//!    signals that would cut it short.
//!
//! It takes no options and no package names. Its environment is its own.
//! `upgrade` with `--with-new-pkgs` and `--no-remove` never removes a
//! package and never moves to another Debian release: that is
//! `dist-upgrade`, which is never run.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::apt::{self, Plan, Status, Steps};
use crate::cache;
use crate::event::{Event, Phase, Progress};
use crate::guard::{self, Paths, Refusal};
use crate::run;
use crate::tools::Tools;

/// Exit statuses.
pub const OK: i32 = 0;
pub const FAILED: i32 = 1;
pub const USAGE: i32 = 2;
pub const CANCELLED: i32 = 3;
pub const REFUSED: i32 = 4;

pub const HELP: &str = "\
Usage: sengi-update check | apply | status

  check    list what could be updated, as JSON lines
  apply    update the system's packages (needs root)
  status   say what would stop an update, as one JSON line
  -h, --help
  -V, --version
";

/// Set by a cancel request: a line saying so on standard input, or SIGTERM.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Where apt keeps what a check that is not root fetches.
pub fn private_state(cache_dir: &Path) -> PathBuf {
    cache_dir.join("apt")
}

/// The apt options that point a check that is not root at lists of its
/// own, and turn off the package cache files it could not write.
fn private_options(state: &Path) -> Vec<String> {
    vec![
        "-o".to_string(),
        format!("Dir::State::Lists={}/lists", state.display()),
        "-o".to_string(),
        "Dir::Cache::pkgcache=".to_string(),
        "-o".to_string(),
        "Dir::Cache::srcpkgcache=".to_string(),
    ]
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

fn refused(refusal: &Refusal) -> Event {
    Event::Refused {
        code: refusal.code().to_string(),
        message: refusal.message(),
    }
}

/// Turns apt's status lines into the progress of one phase. The percent
/// never goes backwards, and nothing is said twice.
struct Mapper {
    phase: Phase,
    total: Option<u64>,
    steps: Steps,
    best: f32,
    last: Option<Progress>,
}

impl Mapper {
    fn new(phase: Phase, total: Option<u64>, packages: u32) -> Mapper {
        Mapper {
            phase,
            total,
            steps: Steps::new(packages),
            best: 0.0,
            last: None,
        }
    }

    fn map(&mut self, status: Status) -> Option<Progress> {
        let (percent, step, detail, size) = match status {
            Status::Download {
                percent,
                file,
                message,
            } => {
                let size = self.total.filter(|t| *t > 0).map(|total| {
                    let got = (total as f64 * f64::from(percent) / 100.0) as u64;
                    format!("{} of {}", apt::format_size(got), apt::format_size(total))
                });
                (percent, file, message, size)
            }
            Status::Install {
                package,
                percent,
                message,
            } => (percent, self.steps.reach(&package), message, None),
            Status::Error { .. } => return None,
        };
        self.best = self.best.max(percent);
        let progress = Progress {
            phase: self.phase,
            percent: Some((self.best * 10.0).round() / 10.0),
            step,
            size,
            detail,
        };
        if self.last.as_ref() == Some(&progress) {
            return None;
        }
        self.last = Some(progress.clone());
        Some(progress)
    }
}

/// What a check found.
pub struct Found {
    pub plan: Plan,
}

/// Looks for system updates: `apt-get update`, then a simulated upgrade.
/// Run as root it reads and refreshes the system's lists. Run as anyone
/// else it uses lists of its own under `state`, which anyone may do, so
/// a check never asks for a password. `Err` carries the event that says
/// why not.
pub fn check_system(
    tools: &Tools,
    paths: &Paths,
    state: Option<&Path>,
    stop: &AtomicBool,
    emit: &mut dyn FnMut(Event),
) -> Result<Found, Event> {
    guard::allow_check(paths).map_err(|refusal| refused(&refusal))?;
    let mut options = Vec::new();
    if let Some(state) = state {
        std::fs::create_dir_all(state.join("lists/partial")).map_err(|e| Event::Failed {
            message: format!("Could not make a place for the package lists: {e}"),
        })?;
        options = private_options(state);
    }

    emit(Event::Phase(Phase::Refresh));
    let mut mapper = Mapper::new(Phase::Refresh, None, 0);
    let mut args = strings(&["update"]);
    args.extend(options.clone());
    let ran = run::apt_watch(tools, &args, stop, |status| {
        if let Some(progress) = mapper.map(status) {
            emit(Event::Progress(progress));
        }
    })
    .map_err(|e| Event::Failed {
        message: format!("Could not run apt-get: {e}"),
    })?;
    if ran.stopped {
        return Err(Event::Cancelled);
    }
    if !ran.success() {
        return Err(Event::Failed {
            message: ran.problem(),
        });
    }
    let plan = simulate(tools, &options)?;
    Ok(Found { plan })
}

/// `apt-get -s upgrade`, read.
fn simulate(tools: &Tools, options: &[String]) -> Result<Plan, Event> {
    let mut args = strings(&["-s", "--with-new-pkgs", "upgrade"]);
    args.extend(options.iter().cloned());
    let ran = run::apt_capture(tools, &args).map_err(|e| Event::Failed {
        message: format!("Could not run apt-get: {e}"),
    })?;
    if !ran.success() {
        return Err(Event::Failed {
            message: ran.problem(),
        });
    }
    Ok(apt::parse_plan(&ran.stdout))
}

/// Everything `apply` does, in order. Returns the exit status, after
/// saying how it ended.
pub fn apply_system(
    tools: &Tools,
    paths: &Paths,
    stop: &AtomicBool,
    emit: &mut dyn FnMut(Event),
) -> i32 {
    if let Err(refusal) = guard::allow_apply(paths) {
        emit(refused(&refusal));
        return REFUSED;
    }
    let found = match check_system(tools, paths, None, stop, emit) {
        Ok(found) => found,
        Err(event) => return finish(event, emit),
    };
    let plan = found.plan;
    if !plan.removals.is_empty() {
        let names: Vec<&str> = plan.removals.iter().take(5).map(String::as_str).collect();
        return finish(
            Event::Failed {
                message: format!(
                    "The update would remove {} ({}), so it was not started.",
                    if plan.removals.len() == 1 {
                        "a package".to_string()
                    } else {
                        format!("{} packages", plan.removals.len())
                    },
                    names.join(", ")
                ),
            },
            emit,
        );
    }
    let reboot = || guard::reboot_required(&paths.reboot_required);
    if plan.upgrades.is_empty() {
        emit(Event::Done {
            upgraded: 0,
            reboot_required: reboot(),
        });
        return OK;
    }
    let count = plan.upgrades.len() as u32;

    // The download: the last place to change one's mind.
    emit(Event::Phase(Phase::Download));
    let mut mapper = Mapper::new(Phase::Download, plan.download, count);
    let ran = run::apt_watch(
        tools,
        &strings(&["-y", "-d", "--with-new-pkgs", "--no-remove", "upgrade"]),
        stop,
        |status| {
            if let Some(progress) = mapper.map(status) {
                emit(Event::Progress(progress));
            }
        },
    );
    match ran {
        Err(e) => {
            return finish(
                Event::Failed {
                    message: format!("Could not run apt-get: {e}"),
                },
                emit,
            )
        }
        Ok(ran) if ran.stopped => return finish(Event::Cancelled, emit),
        Ok(ran) if !ran.success() => {
            return finish(
                Event::Failed {
                    message: ran.problem(),
                },
                emit,
            )
        }
        Ok(_) => {}
    }
    if stop.load(Ordering::SeqCst) {
        return finish(Event::Cancelled, emit);
    }

    // The install. From here nothing the caller does ends it.
    emit(Event::Phase(Phase::Install));
    let never = AtomicBool::new(false);
    let mut mapper = Mapper::new(Phase::Install, None, count);
    let ran = run::apt_watch(
        tools,
        &strings(&[
            "-y",
            "--with-new-pkgs",
            "--no-remove",
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
            "upgrade",
        ]),
        &never,
        |status| {
            if let Some(progress) = mapper.map(status) {
                emit(Event::Progress(progress));
            }
        },
    );
    let end = match ran {
        Err(e) => Event::Failed {
            message: format!("Could not run apt-get: {e}"),
        },
        Ok(ran) if !ran.success() => Event::Failed {
            message: ran.problem(),
        },
        Ok(_) => Event::Done {
            upgraded: count,
            reboot_required: reboot(),
        },
    };
    finish(end, emit)
}

/// Says how it ended and gives the exit status that goes with it.
fn finish(event: Event, emit: &mut dyn FnMut(Event)) -> i32 {
    let code = match &event {
        Event::Done { .. } => OK,
        Event::Cancelled => CANCELLED,
        Event::Refused { .. } => REFUSED,
        _ => FAILED,
    };
    emit(event);
    code
}

/// The helper as the command line has it. `args` are the arguments after
/// the program's name.
pub fn main_with(args: &[String]) -> i32 {
    let mut out = std::io::stdout();
    let mut emit = |event: Event| {
        // The reader may be gone; the work goes on regardless.
        let _ = writeln!(out, "{}", event.to_line());
        let _ = out.flush();
    };
    let command = match args {
        [one] if one == "-h" || one == "--help" => {
            print!("{HELP}");
            return OK;
        }
        [one] if one == "-V" || one == "--version" => {
            println!("sengi-update {}", env!("CARGO_PKG_VERSION"));
            return OK;
        }
        [one] if matches!(one.as_str(), "check" | "apply" | "status") => one.as_str(),
        _ => {
            eprint!("{HELP}");
            return USAGE;
        }
    };

    // SAFETY: geteuid has no arguments and cannot fail.
    let root = unsafe { libc::geteuid() } == 0;
    // The tests give the helper a made-up system. Root never takes it.
    let test_dir = std::env::var_os("SENGI_UPDATE_TEST_DIR")
        .filter(|_| !root)
        .map(PathBuf::from);
    let (tools, paths, state) = match &test_dir {
        Some(dir) => (
            Tools {
                apt_get: dir.join("apt-get"),
                ..Tools::system()
            },
            Paths::under(dir),
            Some(private_state(&dir.join("cache"))),
        ),
        None => (
            Tools::system(),
            Paths::system(),
            if root {
                None
            } else {
                cache::dir().map(|dir| private_state(&dir))
            },
        ),
    };

    match command {
        "status" => {
            emit(Event::Status {
                live: guard::allow_check(&paths).is_err(),
                busy: guard::locked(&paths.locks),
                reboot_required: guard::reboot_required(&paths.reboot_required),
                battery: guard::battery_discharging(&paths.power_supply),
            });
            OK
        }
        "check" => {
            let mut report = |event: Event| emit(event);
            match check_system(&tools, &paths, state.as_deref(), &STOP, &mut report) {
                Ok(found) => {
                    for upgrade in found.plan.upgrades {
                        emit(Event::Package(upgrade));
                    }
                    emit(Event::Checked {
                        download: found.plan.download,
                        time: crate::now(),
                    });
                    OK
                }
                Err(event) => finish(event, &mut emit),
            }
        }
        _ => {
            if !root && test_dir.is_none() {
                emit(refused(&Refusal::NotRoot));
                return REFUSED;
            }
            watch_for_cancel();
            apply_system(&tools, &paths, &STOP, &mut emit)
        }
    }
}

/// From here on a hang-up or an interrupt does not end the update, and
/// a request to stop (a line "cancel" on standard input, or SIGTERM) is
/// only a request: the install phase never looks at it.
fn watch_for_cancel() {
    // SAFETY: signal installs a handler that only stores to an atomic, or
    // ignores the signal.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        libc::signal(
            libc::SIGTERM,
            on_terminate as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    let spawned = std::thread::Builder::new()
        .name("cancel".to_string())
        .spawn(|| {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                match line {
                    Ok(line) if line.trim() == "cancel" => STOP.store(true, Ordering::SeqCst),
                    Ok(_) => {}
                    // End of input is not a request: whoever started this
                    // may be gone, and a half-done update is the worst
                    // outcome.
                    Err(_) => break,
                }
            }
        });
    let _ = spawned;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{scratch, script};

    /// A fake apt-get whose behaviour is chosen by its first argument
    /// (after our own `-q -o APT::Color=0` and the status option).
    const FAKE: &str = r#"
log="$(dirname "$0")/calls.log"
echo "$*" >>"$log"
case " $* " in
*" update "*)
  echo 'dlstatus:1:50.0000:Retrieving file 1 of 2' >&3
  echo 'dlstatus:2:100.0000:Retrieving file 2 of 2' >&3
  ;;
*" -s "*)
  cat "$(dirname "$0")/plan.txt"
  ;;
*" -d "*)
  echo 'dlstatus:1:25.0000:Retrieving file 1 of 4' >&3
  echo 'dlstatus:4:100.0000:Retrieving file 4 of 4' >&3
  ;;
*" upgrade "*)
  echo 'pmstatus:dpkg-exec:0.0000:Running dpkg' >&3
  echo 'pmstatus:openssl:50.0000:Unpacking openssl' >&3
  echo 'pmstatus:tzdata:100.0000:Setting up tzdata' >&3
  ;;
esac
"#;

    fn setup(plan: &str) -> (PathBuf, Tools, Paths) {
        let dir = scratch("helper");
        script(&dir, "apt-get", FAKE);
        std::fs::write(dir.join("plan.txt"), plan).unwrap();
        let tools = Tools {
            apt_get: dir.join("apt-get"),
            ..Tools::system()
        };
        (dir.clone(), tools, Paths::under(&dir))
    }

    const PLAN: &str = "\
Need to get 4,000 kB/40 MB of archives.
Inst openssl [1] (2 Debian-Security:13/stable-security [amd64])
Inst tzdata [1] (2 Debian:13/stable [all])
";

    fn run_apply(tools: &Tools, paths: &Paths, stop: &AtomicBool) -> (i32, Vec<Event>) {
        let mut events = Vec::new();
        let code = apply_system(tools, paths, stop, &mut |e| events.push(e));
        (code, events)
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn an_update_goes_through_its_phases_in_order() {
        let (dir, tools, paths) = setup(PLAN);
        let (code, events) = run_apply(&tools, &paths, &AtomicBool::new(false));
        assert_eq!(code, OK);
        let phases: Vec<Phase> = events
            .iter()
            .filter_map(|e| match e {
                Event::Phase(p) => Some(*p),
                _ => None,
            })
            .collect();
        assert_eq!(phases, [Phase::Refresh, Phase::Download, Phase::Install]);
        assert_eq!(
            events.last(),
            Some(&Event::Done {
                upgraded: 2,
                reboot_required: false
            })
        );
        // The download reports a size from the plan: 25% of 40 MB.
        let sized = events.iter().find_map(|e| match e {
            Event::Progress(p) if p.phase == Phase::Download => p.size.clone(),
            _ => None,
        });
        assert_eq!(sized.as_deref(), Some("10 MB of 40 MB"));
        // The install counts its packages.
        let step = events.iter().rev().find_map(|e| match e {
            Event::Progress(p) if p.phase == Phase::Install => p.step,
            _ => None,
        });
        assert_eq!(step, Some((2, 2)));
        // Exactly these commands were run, in this order, and no other.
        let log = calls(&dir);
        assert_eq!(log.len(), 4, "{log:?}");
        assert!(log[0].contains(" update"));
        assert!(log[1].contains(" -s --with-new-pkgs upgrade"));
        assert!(log[2].contains(" -y -d --with-new-pkgs --no-remove upgrade"));
        assert!(log[3].contains("--no-remove"));
        assert!(log[3].contains("Dpkg::Options::=--force-confold"));
        assert!(log[3].contains("Dpkg::Options::=--force-confdef"));
        for line in &log {
            let words: Vec<&str> = line.split_whitespace().collect();
            for banned in ["dist-upgrade", "full-upgrade", "remove", "purge", "install"] {
                assert!(!words.contains(&banned), "{line}");
            }
        }
    }

    #[test]
    fn progress_never_goes_backwards_or_repeats() {
        let mut mapper = Mapper::new(Phase::Install, None, 3);
        let line = |pct: &str| apt::parse_status(&format!("pmstatus:a:{pct}:Unpacking a")).unwrap();
        assert_eq!(mapper.map(line("40")).unwrap().percent, Some(40.0));
        assert_eq!(mapper.map(line("40")), None);
        assert_eq!(mapper.map(line("30")), None);
        assert_eq!(mapper.map(line("41.04")).unwrap().percent, Some(41.0));
    }

    #[test]
    fn nothing_to_update_ends_without_downloading() {
        let (dir, tools, paths) = setup("0 upgraded, 0 newly installed, 0 to remove.\n");
        let (code, events) = run_apply(&tools, &paths, &AtomicBool::new(false));
        assert_eq!(code, OK);
        assert_eq!(
            events.last(),
            Some(&Event::Done {
                upgraded: 0,
                reboot_required: false
            })
        );
        assert_eq!(calls(&dir).len(), 2);
    }

    #[test]
    fn a_reboot_is_asked_for_when_the_system_says_so() {
        let (dir, tools, paths) = setup(PLAN);
        std::fs::create_dir_all(dir.join("run")).unwrap();
        std::fs::write(dir.join("run/reboot-required"), "").unwrap();
        let (_, events) = run_apply(&tools, &paths, &AtomicBool::new(false));
        assert_eq!(
            events.last(),
            Some(&Event::Done {
                upgraded: 2,
                reboot_required: true
            })
        );
    }

    #[test]
    fn an_update_that_would_remove_something_is_not_started() {
        let (dir, tools, paths) =
            setup("Remv oldlib [1]\nInst openssl [1] (2 Debian:13/stable [amd64])\n");
        let (code, events) = run_apply(&tools, &paths, &AtomicBool::new(false));
        assert_eq!(code, FAILED);
        match events.last() {
            Some(Event::Failed { message }) => assert!(message.contains("oldlib"), "{message}"),
            other => panic!("{other:?}"),
        }
        // It stopped after the simulation: nothing was downloaded.
        assert_eq!(calls(&dir).len(), 2);
    }

    #[test]
    fn the_live_system_is_refused_before_apt_is_touched() {
        let (dir, tools, paths) = setup(PLAN);
        std::fs::create_dir_all(dir.join("proc")).unwrap();
        std::fs::write(dir.join("proc/mounts"), "overlay / overlay rw 0 0\n").unwrap();
        let (code, events) = run_apply(&tools, &paths, &AtomicBool::new(false));
        assert_eq!(code, REFUSED);
        assert!(matches!(&events[0], Event::Refused { code, .. } if code == "live"));
        assert!(calls(&dir).is_empty());
    }

    #[test]
    fn a_low_battery_is_refused_before_apt_is_touched() {
        let (dir, tools, paths) = setup(PLAN);
        let bat = dir.join("sys/class/power_supply/BAT0");
        std::fs::create_dir_all(&bat).unwrap();
        std::fs::write(bat.join("type"), "Battery\n").unwrap();
        std::fs::write(bat.join("status"), "Discharging\n").unwrap();
        std::fs::write(bat.join("capacity"), "12\n").unwrap();
        let (code, events) = run_apply(&tools, &paths, &AtomicBool::new(false));
        assert_eq!(code, REFUSED);
        match &events[0] {
            Event::Refused { code, message } => {
                assert_eq!(code, "battery");
                assert!(message.contains("12%"));
            }
            other => panic!("{other:?}"),
        }
        assert!(calls(&dir).is_empty());
    }

    #[test]
    fn another_apt_at_work_is_refused() {
        let (dir, tools, paths) = setup(PLAN);
        let lock = dir.join("var/lib/dpkg/lock-frontend");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "").unwrap();
        let path = std::ffi::CString::new(lock.to_str().unwrap()).unwrap();
        // SAFETY: the child only calls fcntl and pause.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // SAFETY: only async-signal-safe calls between fork and _exit.
            unsafe {
                let fd = libc::open(path.as_ptr(), libc::O_RDWR);
                let mut want: libc::flock = std::mem::zeroed();
                want.l_type = libc::F_WRLCK as libc::c_short;
                want.l_whence = libc::SEEK_SET as libc::c_short;
                libc::fcntl(fd, libc::F_SETLK, &want);
                libc::pause();
                libc::_exit(0);
            }
        }
        for _ in 0..100 {
            if guard::locked(&paths.locks) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (code, events) = run_apply(&tools, &paths, &AtomicBool::new(false));
        // SAFETY: ends and reaps the child made above.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        assert_eq!(code, REFUSED);
        assert!(matches!(&events[0], Event::Refused { code, .. } if code == "busy"));
        assert!(calls(&dir).is_empty());
    }

    #[test]
    fn a_cancel_before_the_install_installs_nothing() {
        let (dir, tools, paths) = setup(PLAN);
        // The download takes a while, and a cancel arrives in it.
        script(
            &dir,
            "apt-get",
            r#"
echo "$*" >>"$(dirname "$0")/calls.log"
case " $* " in
*" update "*) ;;
*" -s "*) cat "$(dirname "$0")/plan.txt" ;;
*" -d "*) echo 'dlstatus:1:5.0000:Retrieving file 1 of 4' >&3; sleep 30 ;;
*) echo installed >"$(dirname "$0")/installed" ;;
esac
"#,
        );
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let setter = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(500));
                stop.store(true, Ordering::SeqCst);
            })
        };
        let (code, events) = run_apply(&tools, &paths, &stop);
        setter.join().unwrap();
        assert_eq!(code, CANCELLED);
        assert_eq!(events.last(), Some(&Event::Cancelled));
        assert!(!dir.join("installed").exists());
        assert!(!events.contains(&Event::Phase(Phase::Install)));
    }

    #[test]
    fn a_cancel_during_the_install_is_not_heard() {
        let (dir, tools, paths) = setup(PLAN);
        script(
            &dir,
            "apt-get",
            r#"
echo "$*" >>"$(dirname "$0")/calls.log"
case " $* " in
*" update "*) ;;
*" -s "*) cat "$(dirname "$0")/plan.txt" ;;
*" -d "*) ;;
*) echo 'pmstatus:openssl:10.0000:Unpacking openssl' >&3; sleep 1; echo installed >"$(dirname "$0")/installed" ;;
esac
"#,
        );
        // The cancel is set only once the install has begun.
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let mut events = Vec::new();
        let code = std::thread::scope(|scope| {
            let stop_in = stop.clone();
            let watcher = scope.spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(700));
                stop_in.store(true, Ordering::SeqCst);
            });
            let code = apply_system(&tools, &paths, &stop, &mut |e| events.push(e));
            watcher.join().unwrap();
            code
        });
        // Whether the flag came before or after the download ended, an
        // install that began is finished.
        if events.contains(&Event::Phase(Phase::Install)) {
            assert_eq!(code, OK);
            assert!(dir.join("installed").exists());
        } else {
            assert_eq!(code, CANCELLED);
        }
    }

    #[test]
    fn a_failed_download_is_a_failure_with_apts_words() {
        let (dir, tools, paths) = setup(PLAN);
        script(
            &dir,
            "apt-get",
            r#"
case " $* " in
*" update "*) ;;
*" -s "*) cat "$(dirname "$0")/plan.txt" ;;
*" -d "*) echo 'E: Failed to fetch http://x/openssl.deb  404  Not Found' >&2; exit 100 ;;
esac
"#,
        );
        let (code, events) = run_apply(&tools, &paths, &AtomicBool::new(false));
        assert_eq!(code, FAILED);
        match events.last() {
            Some(Event::Failed { message }) => assert!(message.starts_with("E: Failed to fetch")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_check_that_is_not_root_uses_lists_of_its_own() {
        let (dir, tools, paths) = setup(PLAN);
        let state = dir.join("cache/apt");
        let mut events = Vec::new();
        let Ok(found) = check_system(
            &tools,
            &paths,
            Some(&state),
            &AtomicBool::new(false),
            &mut |e| events.push(e),
        ) else {
            panic!("a check");
        };
        assert_eq!(found.plan.upgrades.len(), 2);
        assert_eq!(found.plan.download, Some(40_000_000));
        assert!(state.join("lists/partial").is_dir());
        let log = calls(&dir);
        assert!(log.iter().all(|l| l.contains("Dir::State::Lists=")));
        assert!(log.iter().all(|l| l.contains("Dir::Cache::pkgcache=")));
        assert!(!log.iter().any(|l| l.contains(" -d ") || l.contains("-y")));
    }

    #[test]
    fn arguments_are_checked() {
        for bad in [
            vec![],
            vec!["apply", "--force"],
            vec!["dist-upgrade"],
            vec!["install", "vim"],
            vec!["check", "apply"],
            vec!["APPLY"],
        ] {
            let args: Vec<String> = bad.iter().map(|a| a.to_string()).collect();
            assert_eq!(main_with(&args), USAGE, "{bad:?}");
        }
    }
}
