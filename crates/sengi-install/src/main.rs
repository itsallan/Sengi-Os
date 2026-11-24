//! sengi-install: puts Sengi OS on a disk, as a plan says.
//!
//! It asks nothing. The installer's window starts it with `start`, which
//! hands the installation to a service of its own and returns: from then
//! on the installation belongs to the system, not to the window, the
//! session or the terminal that asked for it. `run` is the installation
//! itself. What happens is said a line an event, on standard output and
//! in a file anyone may follow; the full record is in the log.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use sengi_install::install::{self, Paths};
use sengi_install::names::{self, MAX_PASSWORD_BYTES};
use sengi_install::plan::Plan;
use sengi_install::progress::Event;
use sengi_install::real::{Real, INTERRUPTED};
use sengi_install::{beside, disks, LOCK, LOG, PROGRESS, REQUEST, TARGET, UNIT};
use sengi_system::secret::{self, Secret};

const USAGE: &str = "\
usage: sengi-install start
       sengi-install run [--plan FILE]
       sengi-install disks
       sengi-install --help | --version

start   Install Sengi OS as the plan says: erase a disk and take it, or
        add one partition in free space beside another system and
        change nothing else. Runs in a service of its own (sengi-install.service) that goes on whatever
        becomes of whoever started it. Returns as soon as it is started.
        Follow it in /run/sengi-install/progress.
run     The same, here and now, until it is done.
disks   List the disks of this machine and whether each can be installed
        on, and whether beside what is on it.

start and run need root. For both, the first line of standard input is
the password of the account to create, and the plan follows it; run can
read the plan from FILE instead.
";

/// The most a plan is read.
const MAX_PLAN_BYTES: u64 = 64 * 1024;

/// Where the events go besides standard output.
static PROGRESS_FILE: Mutex<Option<File>> = Mutex::new(None);

extern "C" fn on_signal(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

/// The first line of `input`, read a byte at a time so that nothing
/// after it is taken and no buffer is left holding it.
fn read_password(input: &mut File) -> Result<Secret, String> {
    let mut bytes = [0u8; MAX_PASSWORD_BYTES + 1];
    let mut length = 0;
    let result = loop {
        let mut byte = [0u8; 1];
        match input.read(&mut byte) {
            Ok(0) => break Ok(()),
            Ok(_) if byte[0] == b'\n' => break Ok(()),
            Ok(_) if length == bytes.len() => break Err("the password is too long"),
            Ok(_) => {
                bytes[length] = byte[0];
                length += 1;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break Err("the password could not be read"),
        }
    };
    let secret = result.and_then(|()| {
        let text = std::str::from_utf8(&bytes[..length]).map_err(|_| "the password is not text")?;
        let mut secret = Secret::with_capacity(bytes.len());
        if secret.push(text) {
            Ok(secret)
        } else {
            Err("the password is too long")
        }
    });
    for byte in &mut bytes {
        // SAFETY: a byte of an array this function owns.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    secret.map_err(str::to_string)
}

/// The rest of `input`, as it stands.
fn read_plan_text(input: &mut File) -> Result<String, String> {
    let mut text = String::new();
    input
        .take(MAX_PLAN_BYTES)
        .read_to_string(&mut text)
        .map_err(|error| format!("the plan could not be read: {error}"))?;
    Ok(text)
}

/// The lock, held until the program ends. An error if another
/// installation has it.
fn lock() -> Result<File, String> {
    let path = Path::new(LOCK);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let file = File::create(path).map_err(|error| format!("{LOCK}: {error}"))?;
    // SAFETY: flock is given a descriptor that is open.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("another installation is running".to_string());
    }
    Ok(file)
}

fn say(event: &Event) {
    let line = event.line();
    // Whoever is listening on standard output may be gone. The
    // installation goes on without them: half a system on the disk is
    // worse than a whole one nobody watched.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
    if let Ok(mut progress) = PROGRESS_FILE.lock() {
        if let Some(file) = progress.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    }
}

fn needs_root() -> Option<ExitCode> {
    // SAFETY: geteuid only reads.
    if unsafe { libc::geteuid() } == 0 {
        None
    } else {
        eprintln!("sengi-install: installing needs root");
        Some(ExitCode::from(2))
    }
}

fn refuse(problem: String) -> ExitCode {
    eprintln!("sengi-install: {problem}");
    say(&Event::Failed("check".to_string(), problem));
    ExitCode::FAILURE
}

/// The installation, with the password and the plan in hand.
fn install_now(password: Secret, plan_text: &str) -> ExitCode {
    // The file the window follows, started afresh. Readable by all:
    // nothing secret is said in it.
    if let Some(parent) = Path::new(PROGRESS).parent() {
        let _ = fs::create_dir_all(parent);
    }
    let progress = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(PROGRESS);
    if let (Ok(file), Ok(mut slot)) = (progress, PROGRESS_FILE.lock()) {
        *slot = Some(file);
    }
    let plan = match Plan::parse(plan_text) {
        Ok(plan) => plan,
        Err(problem) => return refuse(problem),
    };
    let _lock = match lock() {
        Ok(lock) => lock,
        Err(problem) => return refuse(problem),
    };
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            libc::signal(
                signal,
                on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
            )
        };
    }

    let paths = Paths {
        target: PathBuf::from(TARGET),
        log: PathBuf::from(LOG),
    };
    let mut system = Real::new(&paths.log);
    match install::install(&plan, &password, &mut system, &paths, &mut say) {
        Ok(_) => ExitCode::SUCCESS,
        Err((step, why)) => {
            eprintln!("sengi-install: {step}: {why}");
            ExitCode::FAILURE
        }
    }
}

/// `run`: the password from standard input, and the plan from there or
/// from a file.
fn run(plan_file: Option<PathBuf>) -> ExitCode {
    if let Some(refused) = needs_root() {
        return refused;
    }
    // Standard input, unbuffered, and not closed when this goes.
    // SAFETY: descriptor 0 is open for the life of the process.
    let mut input = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let password = match read_password(&mut input) {
        Ok(password) => password,
        Err(problem) => {
            eprintln!("sengi-install: {problem}");
            return ExitCode::from(2);
        }
    };
    let text = match plan_file {
        Some(path) => File::open(&path)
            .map_err(|error| format!("the plan could not be read: {error}"))
            .and_then(|mut file| read_plan_text(&mut file)),
        None => read_plan_text(&mut input),
    };
    match text {
        Ok(text) => install_now(password, &text),
        Err(problem) => refuse(problem),
    }
}

/// `run --request FILE`: what `start` left. The file is taken out of the
/// directory before it is read, so it is there for as long as it takes
/// the service to start and no longer.
fn run_request(path: &Path) -> ExitCode {
    if let Some(refused) = needs_root() {
        return refused;
    }
    let opened = File::open(path);
    let _ = fs::remove_file(path);
    let mut file = match opened {
        Ok(file) => file,
        Err(error) => return refuse(format!("the request could not be read: {error}")),
    };
    let password = match read_password(&mut file) {
        Ok(password) => password,
        Err(problem) => return refuse(problem),
    };
    match read_plan_text(&mut file) {
        Ok(text) => install_now(password, &text),
        Err(problem) => refuse(problem),
    }
}

/// `start`: hand the installation to a service of its own.
///
/// The service is a transient unit under the system's slice. It is no
/// child of this process, of sudo or of the window, and no part of the
/// session: closing the window, logging out and the session's scope
/// being ended do not reach it. It is wrapped in systemd-inhibit, which
/// holds off sleep, the idle action and shutdown until it is over.
///
/// The password and the plan cannot go to the service the way they came
/// here, down a pipe, because nothing that holds a pipe outlives this
/// process. They go through one file in /run, which is memory and not a
/// disk: made here for root alone, and unlinked by the service as it
/// opens it. They are never in the unit's command line, environment or
/// properties, all of which any user can read with systemctl.
fn start() -> ExitCode {
    if let Some(refused) = needs_root() {
        return refused;
    }
    // SAFETY: descriptor 0 is open for the life of the process.
    let mut input = ManuallyDrop::new(unsafe { File::from_raw_fd(0) });
    let password = match read_password(&mut input) {
        Ok(password) => password,
        Err(problem) => {
            eprintln!("sengi-install: {problem}");
            return ExitCode::from(2);
        }
    };
    let fail = |problem: String| {
        eprintln!("sengi-install: {problem}");
        ExitCode::FAILURE
    };
    // Looked at here too, so that a plan that cannot be read is said to
    // whoever asked and not only to a journal.
    let text = match read_plan_text(&mut input) {
        Ok(text) => text,
        Err(problem) => return fail(problem),
    };
    if let Err(problem) = Plan::parse(&text) {
        return fail(problem);
    }
    if let Err(problem) = names::password(password.expose()) {
        return fail(problem);
    }
    let active = Command::new("systemctl")
        .args(["is-active", "--quiet", UNIT])
        .status()
        .is_ok_and(|status| status.success());
    if active {
        return fail("an installation is already running".to_string());
    }

    let request = Path::new(REQUEST);
    if let Some(parent) = request.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return fail(format!("{}: {error}", parent.display()));
        }
    }
    let _ = fs::remove_file(request);
    let _ = fs::remove_file(PROGRESS);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(request)
        .and_then(|mut file| {
            file.write_all(password.expose().as_bytes())?;
            file.write_all(b"\n")?;
            file.write_all(text.as_bytes())
        });
    drop(password);
    if let Err(error) = written {
        let _ = fs::remove_file(request);
        return fail(format!("the request could not be handed over: {error}"));
    }

    let program =
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/bin/sengi-install"));
    let started = Command::new("systemd-run")
        .arg(format!("--unit={UNIT}"))
        .args([
            "--quiet",
            "--collect",
            "--no-block",
            "--description=Install Sengi OS",
            "--slice=system.slice",
            "--property=ManagedOOMPreference=omit",
            "--property=TimeoutStopSec=120",
            "--",
            "systemd-inhibit",
            "--what=sleep:idle:shutdown",
            "--who=Sengi OS installer",
            "--why=Sengi OS is being installed",
            "--mode=block",
        ])
        .arg(&program)
        .args(["run", "--request", REQUEST])
        .stdin(Stdio::null())
        .status();
    match started {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            let _ = fs::remove_file(request);
            fail(format!(
                "the installation could not be started: systemd-run ended with {status}"
            ))
        }
        Err(error) => {
            let _ = fs::remove_file(request);
            fail(format!("the installation could not be started: {error}"))
        }
    }
}

fn list_disks() -> ExitCode {
    let disks = disks::scan_system(false);
    if disks.is_empty() {
        println!("No disks found.");
    }
    for disk in disks {
        let model = if disk.model.is_empty() {
            "Disk"
        } else {
            &disk.model
        };
        println!(
            "{}  {}  {}{}",
            disk.path,
            disks::size_words(disk.size),
            plain(model),
            if disk.tran.is_empty() {
                String::new()
            } else {
                format!("  ({})", disk.tran)
            }
        );
        println!("    Now on it: {}", plain(&disks::contents_words(&disk)));
        match disks::refusal(&disk) {
            Some(refusal) => println!("    Not offered: {}", refusal.words()),
            None => {
                println!("    Can be installed on");
                let uefi = Path::new("/sys/firmware/efi").is_dir();
                match beside::offer(&disk, uefi) {
                    Ok(offer) => println!(
                        "    Can be installed beside what is on it, in {} of free space",
                        disks::size_words(offer.region().bytes())
                    ),
                    Err(why) => println!("    Not beside what is on it: {}", why.words()),
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// A stranger's text made fit for a terminal: no control characters.
fn plain(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn main() -> ExitCode {
    sengi_system::set_program_name("sengi-install");
    // First, before the password can be anywhere in memory.
    secret::forbid_core_dumps();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["start"] => start(),
        ["run"] => run(None),
        ["run", "--plan", file] => run(Some(PathBuf::from(file))),
        ["run", "--request", file] => run_request(Path::new(file)),
        ["disks"] => list_disks(),
        ["--help" | "-h"] => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        ["--version"] => {
            println!("sengi-install {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
