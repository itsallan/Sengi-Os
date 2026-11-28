//! The system report: one text file about this computer, for the person to
//! attach to a problem report. It is written where they can see it and
//! goes nowhere: nothing here opens a network connection.
//!
//! Everything is read as the ordinary user who asked, from `/proc`, `/sys`,
//! the two radio services and a few commands. What that user may not read,
//! the file says was not read. What identifies the person or the machine is
//! taken out of the whole text before it is written (see [`redact`]).

mod command;
mod logs;
pub mod redact;
mod sections;

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::bus::{self, Bus, Listing};
use crate::iwd::{
    DEVICE as IWD_DEVICE, KNOWN as IWD_KNOWN, NETWORK as IWD_NETWORK, STATION as IWD_STATION,
};
use redact::Redactor;

/// How long one command may take. Most answer at once; the journal of a
/// machine with a fault that repeats can take seconds.
const COMMAND_LIMIT: Duration = Duration::from_secs(10);
/// How long one section may take, its commands included. A file under
/// `/sys` that a sick device never answers must not hold up the rest.
const SECTION_LIMIT: Duration = Duration::from_secs(30);
/// How long iwd and BlueZ have to say what they know.
const RADIO_LIMIT: Duration = Duration::from_secs(8);
/// The most taken from one file.
const MAX_FILE: u64 = 256 * 1024;

const PREAMBLE: &str = "\
This file describes this computer's hardware and what went wrong since it
was last started, to attach to a problem report. It was saved on this
computer and nothing was sent anywhere.

Names of people, of this computer, of Wi-Fi networks and of Bluetooth
devices, personal file paths, network addresses, serial numbers and unique
identifiers are replaced with markers such as <user> or <mac>. This is done
by a program and it can miss things: read the file before you share it.
";

/// Where the report reads from: the real system, or a test's corner of one.
#[derive(Debug, Clone)]
pub struct Sources {
    pub proc: PathBuf,
    pub sys: PathBuf,
    pub etc: PathBuf,
    pub run: PathBuf,
    /// Where commands are looked for.
    pub path: String,
    /// The name of the account asking, if the environment says.
    pub user: Option<String>,
    pub home: PathBuf,
}

impl Sources {
    /// This system, as the person running the program sees it.
    pub fn system() -> Sources {
        let variable = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Sources {
            proc: "/proc".into(),
            sys: "/sys".into(),
            etc: "/etc".into(),
            run: "/run".into(),
            // The administrator's directories are not on every user's path.
            path: format!(
                "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                variable("PATH").unwrap_or_default()
            ),
            user: variable("USER").or_else(|| variable("LOGNAME")),
            home: variable("HOME").map_or_else(|| "/".into(), PathBuf::from),
        }
    }
}

/// What iwd and BlueZ say: the state of the radios, and the names that
/// have to be kept out of the file.
#[derive(Debug, Clone)]
pub struct Radios {
    pub wifi: Listing,
    pub bluetooth: Listing,
}

impl Radios {
    /// Ask both services, neither of which this starts.
    pub fn ask(bus: &Bus) -> Radios {
        use crate::bluez::{ADAPTER, DEVICE, SERVICE};
        Radios {
            wifi: bus.objects(
                crate::iwd::SERVICE,
                &[IWD_DEVICE, IWD_STATION, IWD_NETWORK, IWD_KNOWN],
            ),
            bluetooth: bus.objects(SERVICE, &[ADAPTER, DEVICE]),
        }
    }

    fn silent() -> Radios {
        Radios {
            wifi: Listing::Absent,
            bluetooth: Listing::Absent,
        }
    }
}

/// One section as it was collected, before anything is taken out of it.
#[derive(Debug, Default)]
pub(crate) struct Out {
    pub text: String,
    /// What could not be read, and why.
    pub unread: Vec<String>,
    /// Bluetooth names met along the way, to be taken out everywhere.
    pub bluetooth_names: Vec<String>,
}

impl Out {
    pub fn line(&mut self, line: impl AsRef<str>) {
        self.text.push_str(line.as_ref());
        self.text.push('\n');
    }

    pub fn fact(&mut self, label: &str, value: Option<String>) {
        let value = value.unwrap_or_else(|| "not available".to_string());
        self.line(format!("{label}: {value}"));
    }

    /// Say here and at the top of the file that `what` was not read.
    pub fn unread(&mut self, what: &str, why: &str) {
        self.line(format!("{what}: {why}"));
        self.unread.push(format!("{what}: {why}"));
    }
}

/// A file's contents without surrounding space; `None` if it cannot be
/// read or holds nothing.
pub(crate) fn read(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    // Read to a bound rather than trusting the length, which /proc gives
    // as nothing.
    fs::File::open(path)
        .ok()?
        .take(MAX_FILE)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!text.is_empty()).then(|| text.to_string())
}

/// Everything that identifies this person and machine that can be looked
/// up. What cannot is left to the shape it has.
fn identity(sources: &Sources, radios: &Radios) -> Redactor {
    let mut redactor = Redactor::default();
    let passwd = read(&sources.etc.join("passwd")).unwrap_or_default();
    for line in passwd.lines() {
        let mut fields = line.split(':');
        let (Some(name), Some(uid)) = (fields.next(), fields.nth(1)) else {
            continue;
        };
        // The accounts of people, not of services.
        if uid
            .parse::<u32>()
            .is_ok_and(|uid| (1000..65534).contains(&uid))
        {
            redactor.user(name);
            let full = fields.nth(1).and_then(|gecos| gecos.split(',').next());
            redactor.full_name(full.unwrap_or_default());
        }
    }
    if let Some(user) = &sources.user {
        redactor.user(user);
    }
    redactor.home(&sources.home.to_string_lossy());
    for path in [
        sources.proc.join("sys/kernel/hostname"),
        sources.etc.join("hostname"),
    ] {
        redactor.computer(&read(&path).unwrap_or_default());
    }
    let machine = read(&sources.etc.join("machine-info")).unwrap_or_default();
    if let Some(pretty) = machine
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_HOSTNAME="))
    {
        redactor.computer(pretty.trim_matches(['"', '\'']));
    }
    let names = |listing: &Listing, interfaces: &[&str], keys: &[&str]| {
        let mut names = Vec::new();
        if let Listing::Objects(objects) = listing {
            for object in objects {
                for props in interfaces.iter().filter_map(|i| object.interface(i)) {
                    let found = keys.iter().filter_map(|key| bus::text(props, key));
                    names.extend(found.map(str::to_string));
                }
            }
        }
        names
    };
    for name in names(&radios.wifi, &[IWD_NETWORK, IWD_KNOWN], &["Name"]) {
        redactor.network(&name);
    }
    for name in names(
        &radios.bluetooth,
        &[crate::bluez::ADAPTER, crate::bluez::DEVICE],
        &["Name", "Alias"],
    ) {
        redactor.bluetooth(&name);
    }
    redactor
}

/// Run `job` on a thread of its own and wait for it only so long.
fn within(limit: Duration, job: impl FnOnce() -> Out + Send + 'static) -> Out {
    let (send, receive) = mpsc::channel();
    let spawned = thread::Builder::new()
        .name("report-section".to_string())
        .spawn(move || {
            let _ = send.send(job());
        });
    let why = match spawned {
        Err(_) => "not collected: no thread could be started".to_string(),
        Ok(_) => match receive.recv_timeout(limit) {
            Ok(out) => return out,
            // The thread is left to finish, or not, by itself.
            Err(mpsc::RecvTimeoutError::Timeout) => format!(
                "not collected: it did not finish within {} s",
                limit.as_secs()
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                "not collected: collecting it failed".to_string()
            }
        },
    };
    let mut out = Out::default();
    out.unread("This section", &why);
    out
}

type Job = fn(&Sources, &Radios) -> Out;

const SECTIONS: &[(&str, Job)] = &[
    ("System", sections::system),
    ("Desktop", sections::desktop),
    ("Processor and memory", sections::memory),
    ("Disks", sections::disks),
    ("PCI devices", sections::pci),
    ("USB devices", sections::usb),
    ("Sound", sections::sound),
    ("Wi-Fi, network and Bluetooth", sections::network),
    ("Graphics and displays", sections::graphics),
    ("Battery and power", sections::power),
    ("Services that failed", sections::failed_units),
    ("Errors and warnings since start-up", logs::errors),
];

/// The whole report as text, made at `made`. `radios` asks iwd and BlueZ,
/// and is given a limited time to.
pub fn collect(
    sources: &Sources,
    radios: impl FnOnce() -> Radios + Send + 'static,
    made: &str,
) -> String {
    let (send, receive) = mpsc::channel();
    let asked = thread::Builder::new()
        .name("report-radios".to_string())
        .spawn(move || {
            let _ = send.send(radios());
        });
    let radios = asked
        .ok()
        .and_then(|_| receive.recv_timeout(RADIO_LIMIT).ok())
        .unwrap_or_else(Radios::silent);
    let mut redactor = identity(sources, &radios);

    let sources = Arc::new(sources.clone());
    let radios = Arc::new(radios);
    let collected: Vec<(&str, Out)> = SECTIONS
        .iter()
        .map(|&(title, job)| {
            let (sources, radios) = (sources.clone(), radios.clone());
            let out = within(SECTION_LIMIT, move || job(&sources, &radios));
            (title, out)
        })
        .collect();
    // All names first: one met in the sound section may stand in a log.
    for name in collected.iter().flat_map(|(_, out)| &out.bluetooth_names) {
        redactor.bluetooth(name);
    }
    render(&redactor, &collected, made)
}

fn render(redactor: &Redactor, collected: &[(&str, Out)], made: &str) -> String {
    let mut report = format!("Sengi OS system report\nMade {made}\n\n{PREAMBLE}");
    let unread: Vec<&String> = collected.iter().flat_map(|(_, out)| &out.unread).collect();
    report.push_str("\n== Could not be read ==\n");
    if unread.is_empty() {
        report.push_str("Everything asked for was read.\n");
    }
    for what in unread {
        report.push_str(&redactor.line(what));
        report.push('\n');
    }
    for (title, out) in collected {
        report.push_str(&format!("\n== {title} ==\n"));
        report.push_str(&redactor.text(&out.text));
    }
    report
}

/// Where reports go: the Documents folder that Files shows in its sidebar.
pub fn folder(home: &Path) -> PathBuf {
    home.join("Documents")
}

/// Write `text` to a new file in `folder`, named after `date`. A file that
/// is there already is never written over: the next one is numbered.
pub fn save(text: &str, folder: &Path, date: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(folder)?;
    for number in 1..1000 {
        let name = if number == 1 {
            format!("sengi-os-report-{date}.txt")
        } else {
            format!("sengi-os-report-{date}-{number}.txt")
        };
        let path = folder.join(name);
        let opened = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            // The owner's alone until they choose to give it to someone.
            .mode(0o600)
            .open(&path);
        let mut file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| file.sync_all());
        if let Err(error) = written {
            // Half a report is worse than none.
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        return Ok(path);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "there are too many reports from today already",
    ))
}

/// Collect the report on this system and save it in the person's Documents
/// folder. It takes some seconds: not for the thread that draws.
pub fn make() -> Result<PathBuf, String> {
    let sources = Sources::system();
    if sources.home == Path::new("/") {
        return Err("There is no home folder to save it in".to_string());
    }
    // `2026-11-28T14:03:27`.
    let now = crate::trash::now();
    let (date, time) = now.split_once('T').unwrap_or((now.as_str(), ""));
    let made = format!("{date} {} (local time)", time.get(..5).unwrap_or(""));
    let text = collect(&sources, || Radios::ask(&Bus::default()), &made);
    save(&text, &folder(&sources.home), date).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
