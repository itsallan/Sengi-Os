//! sengi-install: puts Sengi OS on a disk, as a plan says.
//!
//! It asks nothing and is started by the installer's window, as root.
//! The password comes as the first line of standard input and the plan
//! after it, unless the plan is in a file. What happens is said on
//! standard output, a line an event; the full record is in the log.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;

use sengi_install::install::{self, Paths};
use sengi_install::names::MAX_PASSWORD_BYTES;
use sengi_install::plan::Plan;
use sengi_install::progress::Event;
use sengi_install::real::{Real, INTERRUPTED};
use sengi_install::{disks, LOCK, LOG, TARGET};
use sengi_system::secret::{self, Secret};

const USAGE: &str = "\
usage: sengi-install run [--plan FILE]
       sengi-install disks
       sengi-install --help | --version

run     Erase a disk and install Sengi OS on it, as the plan says. Needs
        root. The first line of standard input is the password of the
        account to create; the plan follows it, or is read from FILE.
disks   List the disks of this machine and whether each can be installed
        on.
";

/// The most a plan is read.
const MAX_PLAN_BYTES: u64 = 64 * 1024;

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

fn read_plan(input: &mut File, file: Option<&Path>) -> Result<Plan, String> {
    let mut text = String::new();
    let read = match file {
        Some(path) => {
            File::open(path).and_then(|file| file.take(MAX_PLAN_BYTES).read_to_string(&mut text))
        }
        None => input.take(MAX_PLAN_BYTES).read_to_string(&mut text),
    };
    read.map_err(|error| format!("the plan could not be read: {error}"))?;
    Plan::parse(&text)
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
    let mut out = std::io::stdout().lock();
    // The window may be gone. The installation goes on without it: half
    // a system on the disk is worse than a whole one nobody watched.
    let _ = writeln!(out, "{}", event.line());
    let _ = out.flush();
}

fn run(plan_file: Option<PathBuf>) -> ExitCode {
    // SAFETY: geteuid only reads.
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("sengi-install: installing needs root");
        return ExitCode::from(2);
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
    let plan = match read_plan(&mut input, plan_file.as_deref()) {
        Ok(plan) => plan,
        Err(problem) => {
            eprintln!("sengi-install: {problem}");
            say(&Event::Failed("check".to_string(), problem));
            return ExitCode::FAILURE;
        }
    };
    let _lock = match lock() {
        Ok(lock) => lock,
        Err(problem) => {
            eprintln!("sengi-install: {problem}");
            say(&Event::Failed("check".to_string(), problem));
            return ExitCode::FAILURE;
        }
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
            None => println!("    Can be installed on"),
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
        ["run"] => run(None),
        ["run", "--plan", file] => run(Some(PathBuf::from(file))),
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
