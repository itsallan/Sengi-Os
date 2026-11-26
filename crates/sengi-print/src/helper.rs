//! The helper's work: `add`, `remove` and `discover`.
//!
//! It runs as root through pkexec and does exactly one of these:
//!
//! * `add NAME URI`: `lpadmin -p NAME -E -v URI -m everywhere
//!   -o printer-is-shared=false`, a driverless (IPP Everywhere) printer
//!   that is not shared with the network. It refuses a name that is in
//!   use (`lpstat -p NAME`), and takes the queue away again with
//!   `lpadmin -x NAME` if `lpadmin` failed;
//! * `remove NAME`: `lpadmin -x NAME`;
//! * `discover`: `lpinfo --include-schemes dnssd,ipp -v`, which asks
//!   CUPS for the printers on the network, for at most 20 seconds, and
//!   prints the lines that name one. It stops early when its standard
//!   input is closed or says `cancel`.
//!
//! Every argument is checked against [`crate::valid`] before anything is
//! run, a count other than the exact one is refused, and nothing else is
//! accepted: no option, no other command, no other program. It runs the
//! CUPS tools by absolute path with an environment of its own.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::exec::{capture, Ran};
use crate::valid;

/// Exit statuses.
pub const OK: i32 = 0;
pub const FAILED: i32 = 1;
pub const USAGE: i32 = 2;
pub const STOPPED: i32 = 3;

pub const HELP: &str = "\
Usage: sengi-print add NAME URI | remove NAME | discover

  add NAME URI   add a driverless printer (needs root)
  remove NAME    remove a printer (needs root)
  discover       list printers on the network
  -h, --help
  -V, --version
";

/// How long looking for printers is allowed to take.
pub const DISCOVER_SECONDS: u64 = 20;
const LPADMIN_LIMIT: Duration = Duration::from_secs(100);
/// The most printers one search reports.
const MAX_FOUND: usize = 100;

/// The CUPS programs the helper runs.
#[derive(Debug, Clone)]
pub struct Programs {
    pub lpadmin: PathBuf,
    pub lpinfo: PathBuf,
    /// Asked whether a queue is there before one is made, and not run
    /// for anything else.
    pub lpstat: PathBuf,
}

impl Programs {
    pub fn system() -> Programs {
        Programs {
            lpadmin: PathBuf::from("/usr/sbin/lpadmin"),
            lpinfo: PathBuf::from("/usr/sbin/lpinfo"),
            lpstat: PathBuf::from("/usr/bin/lpstat"),
        }
    }

    /// The system's programs. When this process is not root (a build
    /// tree, the tests), `SENGI_PRINT_LPADMIN` and `SENGI_PRINT_LPINFO`
    /// may name others; as root they are not looked at.
    pub fn for_process() -> Programs {
        let mut programs = Programs::system();
        // SAFETY: geteuid has no arguments and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        if !root {
            if let Some(path) = std::env::var_os("SENGI_PRINT_LPADMIN") {
                programs.lpadmin = PathBuf::from(path);
            }
            if let Some(path) = std::env::var_os("SENGI_PRINT_LPINFO") {
                programs.lpinfo = PathBuf::from(path);
            }
            if let Some(path) = std::env::var_os("SENGI_PRINT_LPSTAT") {
                programs.lpstat = PathBuf::from(path);
            }
        }
        programs
    }
}

fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .env("LANG", "C");
    command
}

/// The tool's last word, for the person.
fn words(ran: &Ran) -> String {
    let from_stderr = sengi_update::last_line(&ran.stderr);
    if from_stderr.is_empty() {
        sengi_update::last_line(&ran.stdout)
    } else {
        from_stderr
    }
}

fn lpadmin(programs: &Programs, args: &[&str], stop: &AtomicBool, err: &mut dyn Write) -> i32 {
    let mut cmd = command(&programs.lpadmin);
    cmd.args(args);
    match capture(cmd, stop, LPADMIN_LIMIT) {
        Ok(ran) if ran.success() => OK,
        Ok(ran) if ran.stopped => {
            let _ = writeln!(err, "lpadmin did not finish in time");
            FAILED
        }
        Ok(ran) => {
            let said = words(&ran);
            let said = if said.is_empty() {
                "lpadmin failed"
            } else {
                said.as_str()
            };
            let _ = writeln!(err, "{said}");
            FAILED
        }
        Err(e) => {
            let _ = writeln!(err, "could not run lpadmin: {e}");
            FAILED
        }
    }
}

/// Whether CUPS already has a queue called `name`.
fn exists(programs: &Programs, name: &str, stop: &AtomicBool) -> bool {
    let mut cmd = command(&programs.lpstat);
    cmd.args(["-p", name]);
    capture(cmd, stop, Duration::from_secs(20)).is_ok_and(|ran| ran.success())
}

/// Adds the queue, and takes it away again if CUPS could not finish: when
/// it cannot ask the printer what it can do, `lpadmin` has already made a
/// queue, as a raw one, and leaves it.
fn add(programs: &Programs, name: &str, uri: &str, stop: &AtomicBool, err: &mut dyn Write) -> i32 {
    if exists(programs, name, stop) {
        let _ = writeln!(err, "a printer called {name} is already there");
        return FAILED;
    }
    let code = lpadmin(
        programs,
        &[
            "-p",
            name,
            "-E",
            "-v",
            uri,
            "-m",
            "everywhere",
            "-o",
            "printer-is-shared=false",
        ],
        stop,
        err,
    );
    if code != OK {
        lpadmin(
            programs,
            &["-x", name],
            &AtomicBool::new(false),
            &mut std::io::sink(),
        );
    }
    code
}

fn discover(
    programs: &Programs,
    stop: &AtomicBool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut cmd = command(&programs.lpinfo);
    cmd.args(["--include-schemes", "dnssd,ipp", "-v"]);
    let ran = match capture(cmd, stop, Duration::from_secs(DISCOVER_SECONDS)) {
        Ok(ran) => ran,
        Err(e) => {
            let _ = writeln!(err, "could not run lpinfo: {e}");
            return FAILED;
        }
    };
    if stop.load(Ordering::SeqCst) {
        return STOPPED;
    }
    // The time running out is not a failure: what was found is what there is.
    if !ran.success() && !ran.stopped {
        let said = words(&ran);
        let said = if said.is_empty() {
            "lpinfo failed"
        } else {
            said.as_str()
        };
        let _ = writeln!(err, "{said}");
        return FAILED;
    }
    let mut count = 0;
    for line in ran.stdout.lines() {
        let mut fields = line.split_whitespace();
        let (Some(class), Some(uri), None) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        if !matches!(class, "network" | "direct") || !valid::device_uri(uri) {
            continue;
        }
        if count == MAX_FOUND {
            break;
        }
        count += 1;
        let _ = writeln!(out, "{class} {uri}");
    }
    OK
}

/// Runs what `args` ask for. `stop` is set from outside to end a search.
pub fn run(
    args: &[String],
    programs: &Programs,
    stop: &AtomicBool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["-h" | "--help"] => {
            let _ = write!(out, "{HELP}");
            OK
        }
        ["-V" | "--version"] => {
            let _ = writeln!(out, "sengi-print {}", env!("CARGO_PKG_VERSION"));
            OK
        }
        ["add", name, uri] => {
            let (name, uri) = (*name, *uri);
            if !valid::printer_name(name) {
                let _ = writeln!(err, "refused: not a printer name");
                return USAGE;
            }
            if !valid::device_uri(uri) {
                let _ = writeln!(err, "refused: not a printer address");
                return USAGE;
            }
            add(programs, name, uri, stop, err)
        }
        ["remove", name] => {
            let name = *name;
            if !valid::printer_name(name) {
                let _ = writeln!(err, "refused: not a printer name");
                return USAGE;
            }
            lpadmin(programs, &["-x", name], stop, err)
        }
        ["discover"] => discover(programs, stop, out, err),
        _ => {
            let _ = write!(err, "{HELP}");
            USAGE
        }
    }
}

/// What `main` does: the real standard streams, the system's programs,
/// and a thread that ends a search when the input closes.
pub fn main_with(args: &[String]) -> i32 {
    static STOP: AtomicBool = AtomicBool::new(false);
    if args.first().map(String::as_str) == Some("discover") {
        std::thread::spawn(|| {
            let stdin = std::io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match stdin.lock().read_line(&mut line) {
                    // The end of the input is a request to stop.
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.trim() == "cancel" => break,
                    Ok(_) => {}
                }
            }
            STOP.store(true, Ordering::SeqCst);
        });
    }
    run(
        args,
        &Programs::for_process(),
        &STOP,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{scratch, script};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    struct Rig {
        dir: PathBuf,
        programs: Programs,
    }

    fn rig(name: &str, lpadmin: &str, lpinfo: &str) -> Rig {
        let dir = scratch(name);
        let programs = Programs {
            lpadmin: script(&dir, "lpadmin", lpadmin),
            lpinfo: script(&dir, "lpinfo", lpinfo),
            // Knows one queue, called Taken.
            lpstat: script(
                &dir,
                "lpstat",
                "echo \"$*\" >>\"$(dirname \"$0\")/lpstat.log\"; [ \"$2\" = Taken ]",
            ),
        };
        Rig { dir, programs }
    }

    fn go(rig: &Rig, list: &[&str]) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(
            &args(list),
            &rig.programs,
            &AtomicBool::new(false),
            &mut out,
            &mut err,
        );
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    const LOG: &str = "echo \"$*\" >>\"$(dirname \"$0\")/calls\"";

    fn calls(rig: &Rig) -> String {
        std::fs::read_to_string(rig.dir.join("calls")).unwrap_or_default()
    }

    #[test]
    fn add_runs_lpadmin_with_these_options_and_no_others() {
        let rig = rig("add", LOG, "exit 1");
        let (code, _, _) = go(&rig, &["add", "Office", "ipp://192.168.1.50/ipp/print"]);
        assert_eq!(code, OK);
        assert_eq!(
            calls(&rig).trim(),
            "-p Office -E -v ipp://192.168.1.50/ipp/print -m everywhere -o printer-is-shared=false"
        );
    }

    #[test]
    fn a_queue_that_is_there_is_not_added_over() {
        let rig = rig("taken", LOG, "exit 1");
        let (code, _, err) = go(&rig, &["add", "Taken", "ipp://h/ipp/print"]);
        assert_eq!(code, FAILED);
        assert!(err.contains("already there"), "{err}");
        assert_eq!(calls(&rig), "", "lpadmin was run");
    }

    #[test]
    fn a_queue_that_could_not_be_finished_is_taken_away_again() {
        // CUPS makes the queue before it asks the printer what it can do.
        let rig = rig(
            "cleanup",
            "echo \"$*\" >>\"$(dirname \"$0\")/calls\"; \
             case \"$*\" in *-x*) exit 0;; esac; \
             echo 'lpadmin: Unable to connect to printer' >&2; exit 1",
            "exit 1",
        );
        let (code, _, err) = go(&rig, &["add", "Office", "ipp://h/ipp/print"]);
        assert_eq!(code, FAILED);
        assert_eq!(err.trim(), "lpadmin: Unable to connect to printer");
        let calls = calls(&rig);
        let lines: Vec<&str> = calls.lines().collect();
        assert_eq!(lines.len(), 2, "{calls}");
        assert!(lines[0].starts_with("-p Office -E -v ipp://h/ipp/print"));
        assert_eq!(lines[1], "-x Office");
    }

    #[test]
    fn remove_runs_lpadmin_x() {
        let rig = rig("remove", LOG, "exit 1");
        let (code, _, _) = go(&rig, &["remove", "Office"]);
        assert_eq!(code, OK);
        assert_eq!(calls(&rig).trim(), "-x Office");
    }

    #[test]
    fn a_failure_of_lpadmin_is_reported_in_its_words() {
        let rig = rig(
            "fail",
            "echo 'lpadmin: Printer drivers are deprecated' >&2; exit 1",
            "exit 1",
        );
        let (code, _, err) = go(&rig, &["add", "Office", "ipp://h/ipp/print"]);
        assert_eq!(code, FAILED);
        assert_eq!(err.trim(), "lpadmin: Printer drivers are deprecated");
    }

    #[test]
    fn unsafe_arguments_are_refused_and_nothing_runs() {
        let rig = rig("unsafe", LOG, LOG);
        for list in [
            vec!["add", "-x", "ipp://h/ipp/print"],
            vec!["add", "a b", "ipp://h/ipp/print"],
            vec!["add", "Office", "file:///etc/passwd"],
            vec!["add", "Office", "ipp://h/ipp/print -o x"],
            vec!["add", "Office", "-v"],
            vec!["add", "Office", "ipp://h/$(id)"],
            vec!["add", "Office"],
            vec!["add", "Office", "ipp://h/ipp/print", "extra"],
            vec!["add"],
            vec!["remove"],
            vec!["remove", "-x"],
            vec!["remove", "a;b"],
            vec!["remove", "A", "B"],
            vec!["remove", ""],
            vec!["discover", "extra"],
            vec!["--include-schemes", "dnssd"],
            vec!["lpadmin", "-x", "Office"],
            vec!["delete", "Office"],
            vec!["-v"],
            vec![],
        ] {
            let (code, _, _) = go(&rig, &list);
            assert_eq!(code, USAGE, "{list:?}");
        }
        assert_eq!(calls(&rig), "", "a program was run");
    }

    #[test]
    fn discovery_prints_only_lines_that_name_a_printer() {
        let rig = rig(
            "discover",
            "exit 1",
            &format!(
                "{LOG}; \
                 echo 'network ipp'; \
                 echo 'network dnssd://Office%20Laser._ipp._tcp.local/?uuid=1'; \
                 echo 'network file:///etc/passwd'; \
                 echo 'network ipp://h/a b'; \
                 echo 'direct usb://HP/LaserJet'; \
                 echo 'something else'"
            ),
        );
        let (code, out, _) = go(&rig, &["discover"]);
        assert_eq!(code, OK);
        assert_eq!(
            out,
            "network dnssd://Office%20Laser._ipp._tcp.local/?uuid=1\n"
        );
        assert_eq!(calls(&rig).trim(), "--include-schemes dnssd,ipp -v");
    }

    #[test]
    fn discovery_reports_a_failure() {
        let rig = rig(
            "discover-fail",
            "exit 1",
            "echo 'lpinfo: Forbidden' >&2; exit 1",
        );
        let (code, _, err) = go(&rig, &["discover"]);
        assert_eq!(code, FAILED);
        assert_eq!(err.trim(), "lpinfo: Forbidden");
    }

    #[test]
    fn discovery_that_is_asked_to_stop_stops() {
        let rig = rig("discover-stop", "exit 1", "sleep 30");
        let stop = AtomicBool::new(true);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let began = std::time::Instant::now();
        let code = run(
            &args(&["discover"]),
            &rig.programs,
            &stop,
            &mut out,
            &mut err,
        );
        assert_eq!(code, STOPPED);
        assert!(began.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_programs_get_an_environment_of_their_own() {
        let rig = rig("env", "env >\"$(dirname \"$0\")/env\"; exit 0", "exit 1");
        // Whatever the caller had is not passed on.
        std::env::set_var("SENGI_TEST_LEAK", "1");
        let (code, _, _) = go(&rig, &["remove", "Office"]);
        assert_eq!(code, OK);
        let env = std::fs::read_to_string(rig.dir.join("env")).unwrap();
        assert!(!env.contains("SENGI_TEST_LEAK"), "{env}");
        assert!(env.contains("LC_ALL=C"));
        assert!(env.contains("PATH=/usr/sbin:/usr/bin:/sbin:/bin"));
    }

    #[test]
    fn help_and_version_need_no_cups() {
        let rig = rig("help", "exit 1", "exit 1");
        assert_eq!(go(&rig, &["--help"]).0, OK);
        assert!(go(&rig, &["-V"]).1.starts_with("sengi-print "));
    }
}
