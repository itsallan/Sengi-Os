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
Usage: sengi-update check | apply | status | install-package cups

  check    list what could be updated, as JSON lines
  apply    update the system's packages (needs root)
  status   say what would stop an update, as one JSON line
  install-package cups
           install the print system (needs root). Only the names in a
           short list built into the program are accepted
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

/// What `install-package` accepts: a name, and the packages it installs.
/// The name that was typed is only looked up here; what apt-get is given
/// is the right-hand side, never anything of the caller's.
///
/// `avahi-daemon` is there because CUPS finds network printers through it
/// (`lpinfo -v` lists nothing under `dnssd` without it).
pub const INSTALLABLE: [(&str, &[&str]); 1] = [("cups", &["cups", "cups-client", "avahi-daemon"])];

/// The packages for an allowed name.
pub fn installable(name: &str) -> Option<&'static [&'static str]> {
    INSTALLABLE
        .iter()
        .find(|(allowed, _)| *allowed == name)
        .map(|(_, packages)| *packages)
}

/// What the CUPS configuration is changed to: no listening on the
/// network, no announcing or looking for printers, no web page. Anything
/// else in the file is left as it was.
pub fn harden_cupsd_conf(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let words = line.split_whitespace().collect::<Vec<_>>().join(" ");
        match words.as_str() {
            "Listen localhost:631" => continue,
            "Browsing Yes" => out.push_str("Browsing No"),
            "BrowseLocalProtocols dnssd" => out.push_str("BrowseLocalProtocols none"),
            "WebInterface Yes" => out.push_str("WebInterface No"),
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// A systemctl command with an environment of our own.
fn systemctl(paths: &Paths, args: &[&str]) -> Result<(), String> {
    let mut command = std::process::Command::new(&paths.systemctl);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child =
        run::spawn_group(&mut command).map_err(|e| format!("Could not run systemctl: {e}"))?;
    let errors = child.stderr.take().map(run::collect);
    let status = child
        .wait()
        .map_err(|e| format!("Could not run systemctl: {e}"))?;
    let errors = errors.and_then(|t| t.join().ok()).unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let said = crate::last_line(&errors);
        Err(if said.is_empty() {
            format!("systemctl {} failed", args.join(" "))
        } else {
            said
        })
    }
}

/// What is done after CUPS is installed, so that it is as it would be if
/// the image had been made with it: Debian enables `cups.service` for
/// boot, which would keep cupsd in memory from the first second; what
/// should be enabled is its socket, which starts it when something asks.
/// Disabling the service takes the socket and the path unit with it, so
/// the socket is enabled again. The path unit stays off: it starts cupsd
/// whenever `/var/cache/cups/org.cups.cupsd` exists, and cupsd leaves that
/// file behind, so with it every boot after the first print would start
/// cupsd. (Jobs left over wait until something asks.)
fn set_up_cups(paths: &Paths) -> Result<(), String> {
    match std::fs::read_to_string(&paths.cupsd_conf) {
        Ok(text) => {
            let hardened = harden_cupsd_conf(&text);
            if hardened != text {
                let mut tmp = paths.cupsd_conf.clone().into_os_string();
                tmp.push(".sengi-new");
                let tmp = PathBuf::from(tmp);
                let written = std::fs::write(&tmp, hardened).and_then(|()| {
                    let mode = std::fs::metadata(&paths.cupsd_conf)?.permissions();
                    std::fs::set_permissions(&tmp, mode)?;
                    std::fs::rename(&tmp, &paths.cupsd_conf)
                });
                if let Err(e) = written {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(format!("Could not set up the print service: {e}"));
                }
            }
        }
        Err(e) => return Err(format!("Could not read the print service's settings: {e}")),
    }
    systemctl(paths, &["disable", "cups.service"])?;
    systemctl(paths, &["enable", "cups.socket"])?;
    set_up_avahi(paths)?;
    // So that the settings are in effect now. Fine if it is not running.
    systemctl(paths, &["try-restart", "cups.service"])
}

/// Avahi, which CUPS asks over the bus when it looks for printers on the
/// network, is not to run at boot either: it would announce this computer
/// on every network it joins, for something that is used now and then.
/// The bus starts a service by an alias that `enable` links
/// (`dbus-org.freedesktop.Avahi.service`), so the service is enabled and then
/// only the link that makes it start at boot is taken away. The socket unit
/// is off, and what the install started is stopped, so that the system is as
/// it is after a boot.
fn set_up_avahi(paths: &Paths) -> Result<(), String> {
    systemctl(paths, &["enable", "avahi-daemon.service"])?;
    match std::fs::remove_file(paths.boot_wants.join("avahi-daemon.service")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "Could not set up the network discovery service: {e}"
            ))
        }
    }
    systemctl(paths, &["disable", "avahi-daemon.socket"])?;
    systemctl(paths, &["daemon-reload"])?;
    // Not running is the state wanted, and a failure to get there is not
    // worth failing the install for.
    let _ = systemctl(
        paths,
        &["stop", "avahi-daemon.service", "avahi-daemon.socket"],
    );
    Ok(())
}

/// `install-package NAME`: installs what [`INSTALLABLE`] says for the name,
/// and nothing else. The checks are the update's (not on the live
/// system, not on a low battery, not while apt runs), and so is the way
/// of working: refresh the lists, fetch, which can be cancelled, then
/// install, which cannot. Nothing is removed.
pub fn install_package(
    tools: &Tools,
    paths: &Paths,
    name: &str,
    stop: &AtomicBool,
    emit: &mut dyn FnMut(Event),
) -> i32 {
    let Some(packages) = installable(name) else {
        emit(Event::Failed {
            message: format!("{name} is not a package that can be installed from here."),
        });
        return USAGE;
    };
    if let Err(refusal) = guard::allow_apply(paths) {
        emit(refused(&refusal));
        return REFUSED;
    }
    emit(Event::Phase(Phase::Refresh));
    let mut mapper = Mapper::new(Phase::Refresh, None, 0);
    let ran = run::apt_watch(tools, &strings(&["update"]), stop, |status| {
        if let Some(progress) = mapper.map(status) {
            emit(Event::Progress(progress));
        }
    });
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
    let count = packages.len() as u32;
    let install = |download_only: bool| {
        let mut args = strings(&["-y"]);
        if download_only {
            args.push("-d".to_string());
        }
        args.extend(strings(&[
            "--no-install-recommends",
            "--no-remove",
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
            "install",
        ]));
        args.extend(packages.iter().map(|p| p.to_string()));
        args
    };

    emit(Event::Phase(Phase::Download));
    let mut mapper = Mapper::new(Phase::Download, None, count);
    let ran = run::apt_watch(tools, &install(true), stop, |status| {
        if let Some(progress) = mapper.map(status) {
            emit(Event::Progress(progress));
        }
    });
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

    // From here nothing the caller does ends it.
    emit(Event::Phase(Phase::Install));
    let never = AtomicBool::new(false);
    let mut mapper = Mapper::new(Phase::Install, None, count);
    let ran = run::apt_watch(tools, &install(false), &never, |status| {
        if let Some(progress) = mapper.map(status) {
            emit(Event::Progress(progress));
        }
    });
    let end = match ran {
        Err(e) => Event::Failed {
            message: format!("Could not run apt-get: {e}"),
        },
        Ok(ran) if !ran.success() => Event::Failed {
            message: ran.problem(),
        },
        Ok(_) => match name {
            "cups" => match set_up_cups(paths) {
                Ok(()) => Event::Done {
                    upgraded: count,
                    reboot_required: false,
                },
                Err(message) => Event::Failed { message },
            },
            _ => Event::Done {
                upgraded: count,
                reboot_required: false,
            },
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
    let mut package: Option<&str> = None;
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
        // The one name that is looked up before anything runs. Any other
        // is refused here, with the usage.
        [one, name] if one == "install-package" && installable(name).is_some() => {
            package = Some(name.as_str());
            "install-package"
        }
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
        "install-package" => {
            if !root && test_dir.is_none() {
                emit(refused(&Refusal::NotRoot));
                return REFUSED;
            }
            watch_for_cancel();
            install_package(&tools, &paths, package.unwrap_or(""), &STOP, &mut emit)
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
            vec!["install-package"],
            vec!["install-package", "vim"],
            vec!["install-package", "cups", "vim"],
            vec!["install-package", "cups;vim"],
            vec!["install-package", "cups,vim"],
            vec!["install-package", "CUPS"],
            vec!["install-package", "-y"],
            vec!["install-package", ""],
            vec!["install-package", "cups-client"],
        ] {
            let args: Vec<String> = bad.iter().map(|a| a.to_string()).collect();
            assert_eq!(main_with(&args), USAGE, "{bad:?}");
        }
    }

    const CUPSD: &str = "\
LogLevel warn
Listen localhost:631
Listen /run/cups/cups.sock
Browsing Yes
BrowseLocalProtocols dnssd
WebInterface Yes
IdleExitTimeout 60
<Location />
  Order allow,deny
</Location>
";

    /// A system with the print system's settings and a fake systemctl.
    fn cups_setup() -> (PathBuf, Tools, Paths) {
        let (dir, tools, paths) = setup("");
        std::fs::create_dir_all(dir.join("etc/cups")).unwrap();
        std::fs::write(dir.join("etc/cups/cupsd.conf"), CUPSD).unwrap();
        // What `systemctl enable avahi-daemon` leaves behind.
        let units = dir.join("etc/systemd/system");
        std::fs::create_dir_all(units.join("multi-user.target.wants")).unwrap();
        std::fs::write(
            units.join("multi-user.target.wants/avahi-daemon.service"),
            "",
        )
        .unwrap();
        std::fs::write(units.join("dbus-org.freedesktop.Avahi.service"), "").unwrap();
        script(
            &dir,
            "systemctl",
            "echo \"$*\" >>\"$(dirname \"$0\")/systemctl.log\"",
        );
        (dir, tools, paths)
    }

    fn run_install(
        tools: &Tools,
        paths: &Paths,
        name: &str,
        stop: &AtomicBool,
    ) -> (i32, Vec<Event>) {
        let mut events = Vec::new();
        let code = install_package(tools, paths, name, stop, &mut |e| events.push(e));
        (code, events)
    }

    #[test]
    fn the_print_system_is_installed_with_the_packages_on_the_list_only() {
        let (dir, tools, paths) = cups_setup();
        let (code, events) = run_install(&tools, &paths, "cups", &AtomicBool::new(false));
        assert_eq!(code, OK, "{events:?}");
        assert_eq!(
            events.last(),
            Some(&Event::Done {
                upgraded: 3,
                reboot_required: false
            })
        );
        let phases: Vec<Phase> = events
            .iter()
            .filter_map(|e| match e {
                Event::Phase(p) => Some(*p),
                _ => None,
            })
            .collect();
        assert_eq!(phases, [Phase::Refresh, Phase::Download, Phase::Install]);
        let log = calls(&dir);
        assert_eq!(log.len(), 3, "{log:?}");
        assert!(log[0].contains(" update"), "{}", log[0]);
        assert!(log[1].contains(" -y -d --no-install-recommends --no-remove "));
        assert!(
            log[1].contains(" install cups cups-client avahi-daemon"),
            "{}",
            log[1]
        );
        assert!(log[2].contains(" -y --no-install-recommends --no-remove "));
        assert!(log[2].contains("Dpkg::Options::=--force-confold"));
        assert!(
            log[2].contains(" install cups cups-client avahi-daemon"),
            "{}",
            log[2]
        );
        for line in &log {
            let words: Vec<&str> = line.split_whitespace().collect();
            for banned in ["dist-upgrade", "full-upgrade", "upgrade", "remove", "purge"] {
                assert!(!words.contains(&banned), "{line}");
            }
        }
    }

    #[test]
    fn after_the_install_cups_starts_on_demand_and_not_at_boot() {
        let (dir, tools, paths) = cups_setup();
        let (code, _) = run_install(&tools, &paths, "cups", &AtomicBool::new(false));
        assert_eq!(code, OK);
        let systemctl = std::fs::read_to_string(dir.join("systemctl.log")).unwrap();
        assert_eq!(
            systemctl.lines().collect::<Vec<_>>(),
            [
                "disable cups.service",
                "enable cups.socket",
                "enable avahi-daemon.service",
                "disable avahi-daemon.socket",
                "daemon-reload",
                "stop avahi-daemon.service avahi-daemon.socket",
                "try-restart cups.service"
            ]
        );
        // The link that would start Avahi at boot is gone, and the one that
        // lets the bus start it was not touched.
        assert!(!dir
            .join("etc/systemd/system/multi-user.target.wants/avahi-daemon.service")
            .exists());
        assert!(dir
            .join("etc/systemd/system/dbus-org.freedesktop.Avahi.service")
            .exists());
        let conf = std::fs::read_to_string(dir.join("etc/cups/cupsd.conf")).unwrap();
        assert!(conf.contains("Browsing No\n"));
        assert!(conf.contains("BrowseLocalProtocols none\n"));
        assert!(conf.contains("WebInterface No\n"));
        assert!(!conf.contains("Listen localhost"));
        assert!(conf.contains("Listen /run/cups/cups.sock\n"));
        assert!(!dir.join("etc/cups/cupsd.conf.sengi-new").exists());
    }

    #[test]
    fn the_configuration_changes_only_what_it_means_to() {
        assert_eq!(
            harden_cupsd_conf(CUPSD),
            "LogLevel warn\nListen /run/cups/cups.sock\nBrowsing No\n\
             BrowseLocalProtocols none\nWebInterface No\nIdleExitTimeout 60\n\
             <Location />\n  Order allow,deny\n</Location>\n"
        );
        // Already as wanted: nothing changes.
        let once = harden_cupsd_conf(CUPSD);
        assert_eq!(harden_cupsd_conf(&once), once);
        // A comment that mentions a setting is not the setting.
        assert_eq!(harden_cupsd_conf("# Browsing Yes\n"), "# Browsing Yes\n");
    }

    #[test]
    fn a_failure_to_set_up_is_a_failure_of_the_install() {
        let (dir, tools, paths) = cups_setup();
        script(
            &dir,
            "systemctl",
            "echo 'Failed to enable unit' >&2; exit 1",
        );
        let (code, events) = run_install(&tools, &paths, "cups", &AtomicBool::new(false));
        assert_eq!(code, FAILED);
        match events.last() {
            Some(Event::Failed { message }) => {
                assert!(message.contains("Failed to enable unit"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_name_that_is_not_on_the_list_installs_nothing() {
        let (dir, tools, paths) = cups_setup();
        for name in ["vim", "cups-client", "cups cups-client", "-y", "", "CUPS"] {
            let (code, events) = run_install(&tools, &paths, name, &AtomicBool::new(false));
            assert_eq!(code, USAGE, "{name:?}");
            assert!(matches!(events.last(), Some(Event::Failed { .. })));
        }
        assert!(calls(&dir).is_empty(), "apt-get was run");
        assert!(!dir.join("systemctl.log").exists());
    }

    #[test]
    fn the_live_system_and_a_busy_apt_stop_the_install_before_it_starts() {
        let (dir, tools, paths) = cups_setup();
        std::fs::create_dir_all(dir.join("proc")).unwrap();
        std::fs::write(dir.join("proc/mounts"), "overlay / overlay rw 0 0\n").unwrap();
        let (code, events) = run_install(&tools, &paths, "cups", &AtomicBool::new(false));
        assert_eq!(code, REFUSED);
        assert!(matches!(events.last(), Some(Event::Refused { code, .. }) if code == "live"));
        assert!(calls(&dir).is_empty());
    }

    #[test]
    fn a_failed_download_installs_nothing_and_sets_nothing_up() {
        let (dir, _, paths) = cups_setup();
        let tools = Tools {
            apt_get: script(
                &dir,
                "apt-get",
                "echo \"$*\" >>\"$(dirname \"$0\")/calls.log\"; \
                 case \" $* \" in *\" -d \"*) echo 'E: Failed to fetch' >&2; exit 100;; esac",
            ),
            ..Tools::system()
        };
        let (code, events) = run_install(&tools, &paths, "cups", &AtomicBool::new(false));
        assert_eq!(code, FAILED);
        assert!(
            matches!(events.last(), Some(Event::Failed { message }) if message.contains("Failed to fetch"))
        );
        assert_eq!(calls(&dir).len(), 2);
        assert!(!dir.join("systemctl.log").exists());
        let conf = std::fs::read_to_string(dir.join("etc/cups/cupsd.conf")).unwrap();
        assert_eq!(conf, CUPSD);
    }
}
