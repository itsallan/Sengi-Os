//! Settings' side: asking CUPS what there is, as the user, and the three
//! things that need root, which go through the helper.
//!
//! Everything here is a process started and waited for. There is no
//! monitor: CUPS is asked when the page is shown and when the person asks
//! again. With CUPS socket activated, the first question is what starts
//! its daemon, and the daemon leaves when it has nothing to do.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use sengi_update::client::Control;
use sengi_update::elevate::{self, Elevation};
use sengi_update::guard::Refusal;
use sengi_update::Event;

use crate::exec::{capture, Ran};
use crate::parse::{self, Found, Job, Printer};
use crate::valid;

/// How long a question to CUPS may take.
const ASK_LIMIT: Duration = Duration::from_secs(10);
/// How long looking for printers may take: the helper stops its own search
/// at 20 seconds, and this is for the sign-in and the start of it.
pub const DISCOVER_LIMIT: Duration = Duration::from_secs(40);
/// Adding a printer asks the printer what it can do, which can be slow.
const ADD_LIMIT: Duration = Duration::from_secs(120);
const REMOVE_LIMIT: Duration = Duration::from_secs(30);

/// Where the helper is installed, and what the polkit action names.
pub const HELPER: &str = "/usr/bin/sengi-print";
/// The test page: a PDF of ours, because CUPS's own is PostScript and
/// turning that into something a printer takes needs Ghostscript, which
/// the image does not carry.
pub const TEST_PAGE: &str = "/usr/share/sengi/printing/testpage.pdf";

#[derive(Debug, Clone)]
pub struct Tools {
    pub lpstat: OsString,
    pub lpoptions: OsString,
    pub lp: OsString,
    pub cancel: OsString,
    pub pkexec: OsString,
    /// What pkexec is asked to run. The polkit action is for this path.
    pub helper: PathBuf,
    /// The update helper, which installs the print system when it is not
    /// there (`install-package cups`).
    pub update_helper: PathBuf,
    pub test_page: PathBuf,
    /// A `PATH` for the tools above, instead of the one this program has.
    pub path: Option<OsString>,
}

impl Tools {
    /// The programs of an installed system, found the ordinary way.
    pub fn system() -> Tools {
        Tools {
            lpstat: "lpstat".into(),
            lpoptions: "lpoptions".into(),
            lp: "lp".into(),
            cancel: "cancel".into(),
            pkexec: "pkexec".into(),
            helper: PathBuf::from(HELPER),
            update_helper: PathBuf::from(sengi_update::tools::HELPER),
            test_page: PathBuf::from(TEST_PAGE),
            path: None,
        }
    }

    /// For the program that runs as the user: the system's tools, and a
    /// helper somewhere else when `SENGI_PRINT_HELPER` says so (the tests
    /// and a build tree; pkexec will not authorize another path for the
    /// action, so it only ever asks for more).
    pub fn for_user() -> Tools {
        let mut tools = Tools::system();
        if let Some(path) = std::env::var_os("SENGI_PRINT_HELPER") {
            tools.helper = PathBuf::from(path);
        }
        if let Some(path) = std::env::var_os("SENGI_UPDATE_HELPER") {
            tools.update_helper = PathBuf::from(path);
        }
        tools
    }

    fn command(&self, program: &OsString) -> Command {
        let mut command = Command::new(program);
        command.env("LC_ALL", "C").env("LANG", "C");
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        command
    }
}

/// Why CUPS could not be asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The tools are not on this system.
    NotInstalled,
    /// The tools ran and CUPS did not answer.
    Unreachable(String),
    Failed(String),
}

impl Problem {
    pub fn message(&self) -> String {
        match self {
            Problem::NotInstalled => "Printing is not installed on this system.".to_string(),
            Problem::Unreachable(said) => {
                format!("The print service did not answer. {said}")
            }
            Problem::Failed(said) => said.clone(),
        }
    }
}

/// Everything the page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub printers: Vec<Printer>,
    pub default: Option<String>,
    pub jobs: Vec<Job>,
}

fn said(ran: &Ran) -> String {
    let from_stderr = sengi_update::last_line(&ran.stderr);
    if !from_stderr.is_empty() {
        from_stderr
    } else {
        sengi_update::last_line(&ran.stdout)
    }
}

fn unreachable_words(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "scheduler is not running",
        "connection refused",
        "bad file descriptor",
        "cannot connect",
        "unable to connect to server",
        "server-error-service-unavailable",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

fn run_tool(
    tools: &Tools,
    program: &OsString,
    args: &[&str],
    stop: &AtomicBool,
) -> Result<Ran, Problem> {
    let mut command = tools.command(program);
    command.args(args);
    match capture(command, stop, ASK_LIMIT) {
        Ok(ran) if ran.stopped => Err(Problem::Unreachable("It took too long.".to_string())),
        Ok(ran) => Ok(ran),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Problem::NotInstalled),
        Err(e) => Err(Problem::Failed(format!(
            "Could not run {}: {e}",
            program.to_string_lossy()
        ))),
    }
}

/// Asks CUPS for the printers, the default one and the waiting jobs.
pub fn snapshot(tools: &Tools, stop: &AtomicBool) -> Result<Snapshot, Problem> {
    let status = run_tool(tools, &tools.lpstat, &["-d", "-p", "-v"], stop)?;
    // With no printers lpstat says so on standard error and exits 1,
    // which is an answer and not a failure.
    let nothing_added = status.stderr.contains("No destinations added");
    if status.success() || nothing_added {
        return finish(tools, stop, &status);
    }
    let words = said(&status);
    if unreachable_words(&words) {
        return Err(Problem::Unreachable(words));
    }
    if status.stdout.trim().is_empty() {
        return Err(Problem::Failed(if words.is_empty() {
            "The print service could not be asked.".to_string()
        } else {
            words
        }));
    }
    // It printed what it knew: take that.
    finish(tools, stop, &status)
}

fn finish(tools: &Tools, stop: &AtomicBool, status: &Ran) -> Result<Snapshot, Problem> {
    let parsed = parse::parse_status(&status.stdout);
    let jobs = run_tool(tools, &tools.lpstat, &["-o"], stop)?;
    let jobs = if jobs.stdout.trim().is_empty() {
        Vec::new()
    } else {
        parse::parse_jobs(&jobs.stdout)
    };
    Ok(Snapshot {
        printers: parsed.printers,
        default: parsed.default,
        jobs,
    })
}

fn refuse(what: &str) -> Problem {
    Problem::Failed(format!("That {what} cannot be used."))
}

/// Makes `name` this person's default printer (`lpoptions -d`, which
/// writes to their own `~/.cups/lpoptions`).
pub fn set_default(tools: &Tools, name: &str) -> Result<(), Problem> {
    if !valid::printer_name(name) {
        return Err(refuse("printer name"));
    }
    let ran = run_tool(
        tools,
        &tools.lpoptions,
        &["-d", name],
        &AtomicBool::new(false),
    )?;
    if ran.success() {
        Ok(())
    } else {
        Err(Problem::Failed(said(&ran)))
    }
}

/// Prints the test page on `name`. Returns the job's number.
pub fn test_page(tools: &Tools, name: &str) -> Result<String, Problem> {
    if !valid::printer_name(name) {
        return Err(refuse("printer name"));
    }
    if !tools.test_page.exists() {
        return Err(Problem::Failed(
            "The test page is not installed with the print service.".to_string(),
        ));
    }
    let page = tools.test_page.to_string_lossy().into_owned();
    let ran = run_tool(
        tools,
        &tools.lp,
        &["-d", name, "--", page.as_str()],
        &AtomicBool::new(false),
    )?;
    if !ran.success() {
        return Err(Problem::Failed(said(&ran)));
    }
    // "request id is Office-12 (1 file(s))"
    Ok(ran
        .stdout
        .split_whitespace()
        .skip_while(|w| *w != "is")
        .nth(1)
        .filter(|id| valid::job_id(id))
        .unwrap_or("")
        .to_string())
}

/// Cancels a job, by the number `lpstat` gave it.
pub fn cancel_job(tools: &Tools, id: &str) -> Result<(), Problem> {
    if !valid::job_id(id) {
        return Err(refuse("job number"));
    }
    let ran = run_tool(tools, &tools.cancel, &[id], &AtomicBool::new(false))?;
    if ran.success() {
        Ok(())
    } else {
        Err(Problem::Failed(said(&ran)))
    }
}

/// What came of something that needs root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    Done(T),
    /// Not done, and why, in words.
    Failed(String),
    /// polkit did not take the password.
    WrongPassword,
    /// The person backed out of the password question.
    NoPassword,
    /// Stopped before it ended.
    Stopped,
}

impl<T> Outcome<T> {
    /// The same outcome with what was found turned into something else.
    pub fn map<U>(self, change: impl FnOnce(T) -> U) -> Outcome<U> {
        match self {
            Outcome::Done(found) => Outcome::Done(change(found)),
            Outcome::Failed(words) => Outcome::Failed(words),
            Outcome::WrongPassword => Outcome::WrongPassword,
            Outcome::NoPassword => Outcome::NoPassword,
            Outcome::Stopped => Outcome::Stopped,
        }
    }
}

fn elevated(
    tools: &Tools,
    args: &[&str],
    control: &Control,
    limit: Duration,
    need_password: &mut dyn FnMut(),
) -> Outcome<Vec<String>> {
    match elevate::run(
        &tools.pkexec,
        &tools.helper,
        args,
        control,
        limit,
        need_password,
    ) {
        Elevation::Ran(ran) => match ran.code {
            Some(0) => Outcome::Done(ran.lines),
            // pkexec: dismissed or not authorized, or the helper is not
            // where the action says.
            Some(126) | Some(127) => {
                Outcome::Failed("Permission to change the printers was not given.".to_string())
            }
            _ => {
                let words = sengi_update::last_line(&ran.errors);
                Outcome::Failed(if words.is_empty() {
                    "The change was not made, and no reason was given.".to_string()
                } else {
                    words
                })
            }
        },
        Elevation::WrongPassword => Outcome::WrongPassword,
        Elevation::NoPassword => Outcome::NoPassword,
        Elevation::Stopped => Outcome::Stopped,
        Elevation::Failed(words) => Outcome::Failed(words),
    }
}

/// How long installing the print system may take: the lists, 25 MB of
/// packages and dpkg, on a slow line.
const INSTALL_LIMIT: Duration = Duration::from_secs(20 * 60);

/// Installs the print system, which the image does not carry, through the
/// update helper (`sengi-update install-package cups`, under the update's
/// polkit action). The helper says how it ended in JSON lines; its words are
/// the ones returned.
pub fn install_support(
    tools: &Tools,
    control: &Control,
    need_password: &mut dyn FnMut(),
) -> Outcome<()> {
    let got = elevate::run(
        &tools.pkexec,
        &tools.update_helper,
        &["install-package", "cups"],
        control,
        INSTALL_LIMIT,
        need_password,
    );
    match got {
        Elevation::Ran(ran) => {
            if ran.code == Some(0) {
                return Outcome::Done(());
            }
            let ended = ran
                .lines
                .iter()
                .rev()
                .find_map(|line| match Event::from_line(line) {
                    Some(Event::Refused { code, message }) => Some(
                        Refusal::from_code(&code, &message)
                            .map(|(_, words)| words)
                            .unwrap_or(message),
                    ),
                    Some(Event::Failed { message }) => Some(message),
                    Some(Event::Cancelled) => Some("The install was cancelled.".to_string()),
                    _ => None,
                });
            Outcome::Failed(match (ended, ran.code) {
                (Some(words), _) => words,
                (None, Some(126) | Some(127)) => {
                    "Permission to install software was not given.".to_string()
                }
                (None, _) => {
                    let words = sengi_update::last_line(&ran.errors);
                    if words.is_empty() {
                        "The install stopped without saying why.".to_string()
                    } else {
                        words
                    }
                }
            })
        }
        Elevation::WrongPassword => Outcome::WrongPassword,
        Elevation::NoPassword => Outcome::NoPassword,
        Elevation::Stopped => Outcome::Stopped,
        Elevation::Failed(words) => Outcome::Failed(words),
    }
}

/// Looks for printers on the network. Runs the helper, which asks CUPS,
/// and gives up after its own 20 seconds.
pub fn discover(
    tools: &Tools,
    control: &Control,
    need_password: &mut dyn FnMut(),
) -> Outcome<Vec<Found>> {
    elevated(tools, &["discover"], control, DISCOVER_LIMIT, need_password)
        .map(|lines| parse::parse_devices(&lines.join("\n")))
}

/// Adds a driverless printer.
pub fn add(
    tools: &Tools,
    name: &str,
    uri: &str,
    control: &Control,
    need_password: &mut dyn FnMut(),
) -> Outcome<()> {
    if !valid::printer_name(name) {
        return Outcome::Failed(refuse("printer name").message());
    }
    if !valid::device_uri(uri) {
        return Outcome::Failed(refuse("printer address").message());
    }
    elevated(
        tools,
        &["add", name, uri],
        control,
        ADD_LIMIT,
        need_password,
    )
    .map(|_| ())
}

/// Removes a printer.
pub fn remove(
    tools: &Tools,
    name: &str,
    control: &Control,
    need_password: &mut dyn FnMut(),
) -> Outcome<()> {
    if !valid::printer_name(name) {
        return Outcome::Failed(refuse("printer name").message());
    }
    elevated(
        tools,
        &["remove", name],
        control,
        REMOVE_LIMIT,
        need_password,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::State;
    use crate::testutil::{scratch, script};
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::sync::Arc;

    fn tools_in(dir: &Path) -> Tools {
        // The fakes are found on a PATH of their own, as the real ones
        // are on the system's.
        let mut path = dir.as_os_str().to_owned();
        path.push(":/usr/bin:/bin");
        Tools {
            path: Some(path),
            pkexec: dir.join("pkexec").into(),
            helper: dir.join("sengi-print"),
            test_page: dir.join("testprint"),
            ..Tools::system()
        }
    }

    fn control() -> (Control, mpsc::Sender<sengi_update::client::Reply>) {
        let (tx, rx) = mpsc::channel();
        (
            Control {
                stop: Arc::new(AtomicBool::new(false)),
                replies: rx,
            },
            tx,
        )
    }

    const LPSTAT: &str = r#"
case " $* " in
*" -d -p -v "*)
  echo "system default destination: Office"
  echo "printer Office is idle.  enabled since Thu 26 Nov 2026 10:00:00 AM CET"
  echo "printer Garage disabled since Thu 26 Nov 2026 09:00:00 AM CET -"
  printf '\tUnable to connect to printer\n'
  echo "device for Office: ipp://192.168.1.50/ipp/print"
  echo "device for Garage: socket://10.0.0.9:9100" ;;
*" -o "*)
  echo "Office-12  allan  1024  Thu 26 Nov 2026 10:00:00 AM CET" ;;
esac
"#;

    #[test]
    fn the_snapshot_joins_printers_default_and_jobs() {
        let dir = scratch("snap");
        script(&dir, "lpstat", LPSTAT);
        let got = snapshot(&tools_in(&dir), &AtomicBool::new(false)).unwrap();
        assert_eq!(got.default.as_deref(), Some("Office"));
        assert_eq!(got.printers.len(), 2);
        assert_eq!(got.printers[0].state, State::Idle);
        assert_eq!(got.printers[1].state.word(), "Offline");
        assert_eq!(got.jobs.len(), 1);
        assert_eq!(got.jobs[0].id, "Office-12");
    }

    #[test]
    fn no_printers_is_an_empty_list_not_an_error() {
        let dir = scratch("none");
        script(
            &dir,
            "lpstat",
            "case \" $* \" in *\" -d \"*) echo 'no system default destination'; \
             echo 'lpstat: No destinations added.' >&2; exit 1;; *) exit 0;; esac",
        );
        let got = snapshot(&tools_in(&dir), &AtomicBool::new(false)).unwrap();
        assert_eq!(got, Snapshot::default());
    }

    #[test]
    fn a_missing_print_system_is_said_so() {
        let dir = scratch("missing");
        // A PATH with nothing on it, whatever the machine has installed.
        let tools = Tools {
            path: Some(dir.as_os_str().to_owned()),
            ..tools_in(&dir)
        };
        let got = snapshot(&tools, &AtomicBool::new(false));
        assert_eq!(got, Err(Problem::NotInstalled));
        assert!(got.unwrap_err().message().contains("not installed"));
    }

    #[test]
    fn a_service_that_does_not_answer_is_said_so() {
        let dir = scratch("down");
        script(
            &dir,
            "lpstat",
            "echo 'lpstat: Scheduler is not running.' >&2; exit 1",
        );
        let got = snapshot(&tools_in(&dir), &AtomicBool::new(false)).unwrap_err();
        assert!(matches!(got, Problem::Unreachable(_)), "{got:?}");
        assert!(got.message().contains("Scheduler is not running"));
    }

    #[test]
    fn another_failure_keeps_the_tools_words() {
        let dir = scratch("odd");
        script(&dir, "lpstat", "echo 'lpstat: Something odd.' >&2; exit 1");
        let got = snapshot(&tools_in(&dir), &AtomicBool::new(false)).unwrap_err();
        assert_eq!(got, Problem::Failed("lpstat: Something odd.".to_string()));
    }

    #[test]
    fn a_tool_that_hangs_is_given_up_on() {
        let dir = scratch("hang");
        script(&dir, "lpstat", "sleep 60");
        let stop = AtomicBool::new(true);
        let began = std::time::Instant::now();
        let got = snapshot(&tools_in(&dir), &stop).unwrap_err();
        assert!(matches!(got, Problem::Unreachable(_)));
        assert!(began.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn the_default_is_set_with_lpoptions() {
        let dir = scratch("default");
        script(&dir, "lpoptions", "echo \"$*\" >\"$(dirname \"$0\")/args\"");
        set_default(&tools_in(&dir), "Office").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("args")).unwrap().trim(),
            "-d Office"
        );
    }

    #[test]
    fn unsafe_names_are_refused_before_anything_runs() {
        let dir = scratch("refuse");
        for tool in ["lpoptions", "lp", "cancel", "pkexec"] {
            script(&dir, tool, "echo ran >\"$(dirname \"$0\")/ran\"");
        }
        std::fs::write(dir.join("testprint"), "page").unwrap();
        let tools = tools_in(&dir);
        let (control, _keep) = control();
        for bad in ["-x", "a b", "a;rm", "$(id)", "", "a/b", "--"] {
            assert!(set_default(&tools, bad).is_err(), "{bad:?}");
            assert!(test_page(&tools, bad).is_err(), "{bad:?}");
            assert!(cancel_job(&tools, bad).is_err(), "{bad:?}");
            assert!(matches!(
                remove(&tools, bad, &control, &mut || {}),
                Outcome::Failed(_)
            ));
            assert!(matches!(
                add(&tools, bad, "ipp://h/ipp/print", &control, &mut || {}),
                Outcome::Failed(_)
            ));
        }
        for bad in [
            "file:///etc/passwd",
            "ipp://h/a b",
            "-v",
            "ipp://h/$(id)",
            "",
        ] {
            assert!(matches!(
                add(&tools, "Good", bad, &control, &mut || {}),
                Outcome::Failed(_)
            ));
        }
        assert!(!dir.join("ran").exists(), "a tool was started");
    }

    #[test]
    fn the_test_page_is_printed_with_lp_and_its_job_is_returned() {
        let dir = scratch("testpage");
        std::fs::write(dir.join("testprint"), "page").unwrap();
        script(
            &dir,
            "lp",
            "echo \"$*\" >\"$(dirname \"$0\")/args\"; echo 'request id is Office-13 (1 file(s))'",
        );
        let job = test_page(&tools_in(&dir), "Office").unwrap();
        assert_eq!(job, "Office-13");
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        assert!(args.starts_with("-d Office -- "), "{args}");
        assert!(args.trim_end().ends_with("testprint"), "{args}");
    }

    #[test]
    fn a_missing_test_page_is_said_so() {
        let dir = scratch("nopage");
        script(&dir, "lp", "exit 0");
        let got = test_page(&tools_in(&dir), "Office").unwrap_err();
        assert!(got.message().contains("test page"));
    }

    #[test]
    fn a_job_is_cancelled_by_its_number() {
        let dir = scratch("cancel");
        script(&dir, "cancel", "echo \"$*\" >\"$(dirname \"$0\")/args\"");
        cancel_job(&tools_in(&dir), "Office-12").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("args")).unwrap().trim(),
            "Office-12"
        );
        script(
            &dir,
            "cancel",
            "echo 'cancel: cancel-job failed: Forbidden' >&2; exit 1",
        );
        let got = cancel_job(&tools_in(&dir), "Office-12").unwrap_err();
        assert!(got.message().contains("Forbidden"));
    }

    #[test]
    fn discovery_runs_the_helper_and_reads_what_it_found() {
        let dir = scratch("discover");
        // pkexec runs what it is given, here the fake helper.
        script(&dir, "pkexec", "exec \"$@\"");
        script(
            &dir,
            "sengi-print",
            "echo \"$*\" >>\"$(dirname \"$0\")/calls\"; \
             echo 'network dnssd://Office%20Laser._ipp._tcp.local/?uuid=1'; \
             echo 'network ipp://x/$(id)'",
        );
        let (control, _keep) = control();
        let got = discover(&tools_in(&dir), &control, &mut || panic!("no password"));
        let Outcome::Done(found) = got else {
            panic!("{got:?}")
        };
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].title, "Office Laser");
        assert_eq!(
            std::fs::read_to_string(dir.join("calls")).unwrap().trim(),
            "discover"
        );
    }

    #[test]
    fn adding_runs_the_helper_with_the_name_and_address_only() {
        let dir = scratch("add");
        script(&dir, "pkexec", "exec \"$@\"");
        script(
            &dir,
            "sengi-print",
            "echo \"$*\" >>\"$(dirname \"$0\")/calls\"",
        );
        let (control, _keep) = control();
        let got = add(
            &tools_in(&dir),
            "Office",
            "ipp://192.168.1.50/ipp/print",
            &control,
            &mut || {},
        );
        assert_eq!(got, Outcome::Done(()));
        assert_eq!(
            std::fs::read_to_string(dir.join("calls")).unwrap().trim(),
            "add Office ipp://192.168.1.50/ipp/print"
        );
        let got = remove(&tools_in(&dir), "Office", &control, &mut || {});
        assert_eq!(got, Outcome::Done(()));
        let calls = std::fs::read_to_string(dir.join("calls")).unwrap();
        assert!(calls.lines().any(|l| l == "remove Office"), "{calls}");
    }

    #[test]
    fn a_refusal_by_polkit_and_a_failure_of_the_helper_have_their_own_words() {
        let dir = scratch("refused");
        let (control, _keep) = control();
        script(&dir, "pkexec", "exit 126");
        let got = remove(&tools_in(&dir), "Office", &control, &mut || {});
        let Outcome::Failed(words) = got else {
            panic!("{got:?}")
        };
        assert!(words.contains("Permission"), "{words}");
        script(
            &dir,
            "pkexec",
            "echo 'lpadmin: Unable to connect to server' >&2; exit 1",
        );
        let got = remove(&tools_in(&dir), "Office", &control, &mut || {});
        assert_eq!(
            got,
            Outcome::Failed("lpadmin: Unable to connect to server".to_string())
        );
    }

    fn tools_with_update_helper(dir: &Path, body: &str) -> Tools {
        script(dir, "pkexec", "exec \"$@\"");
        let helper = script(dir, "sengi-update", body);
        Tools {
            update_helper: helper,
            ..tools_in(dir)
        }
    }

    #[test]
    fn the_print_system_is_installed_by_the_update_helper_with_the_one_allowed_name() {
        let dir = scratch("install");
        let tools = tools_with_update_helper(
            &dir,
            "echo \"$*\" >>\"$(dirname \"$0\")/calls\"; \
             echo '{\"event\":\"phase\",\"phase\":\"install\"}'; \
             echo '{\"event\":\"done\",\"upgraded\":2,\"reboot_required\":false}'",
        );
        let (control, _keep) = control();
        let got = install_support(&tools, &control, &mut || panic!("no password"));
        assert_eq!(got, Outcome::Done(()));
        assert_eq!(
            std::fs::read_to_string(dir.join("calls")).unwrap().trim(),
            "install-package cups"
        );
    }

    #[test]
    fn an_install_that_fails_or_is_refused_says_why_in_the_helpers_words() {
        let dir = scratch("install-fail");
        let (control, _keep) = control();
        let tools = tools_with_update_helper(
            &dir,
            "echo '{\"event\":\"failed\",\"message\":\"E: Failed to fetch\"}'; exit 1",
        );
        assert_eq!(
            install_support(&tools, &control, &mut || {}),
            Outcome::Failed("E: Failed to fetch".to_string())
        );
        let tools = tools_with_update_helper(
            &dir,
            "echo '{\"event\":\"refused\",\"code\":\"live\",\"message\":\"Updates are available after Sengi OS is installed.\"}'; exit 4",
        );
        let Outcome::Failed(words) = install_support(&tools, &control, &mut || {}) else {
            panic!("a refusal")
        };
        assert!(words.contains("after Sengi OS is installed"), "{words}");
        let tools = tools_with_update_helper(&dir, "exit 127");
        let Outcome::Failed(words) = install_support(&tools, &control, &mut || {}) else {
            panic!("no permission")
        };
        assert!(words.contains("Permission"), "{words}");
    }

    #[test]
    fn a_stop_while_looking_gives_stopped() {
        let dir = scratch("stopped");
        script(&dir, "pkexec", "sleep 30");
        let (control, _keep) = control();
        control.stop.store(true, Ordering::SeqCst);
        let got = discover(&tools_in(&dir), &control, &mut || {});
        assert_eq!(got, Outcome::Stopped);
    }
}
