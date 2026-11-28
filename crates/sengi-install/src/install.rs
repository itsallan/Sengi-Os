//! The installation itself: a fixed list of steps, each of which either
//! succeeds or stops everything.
//!
//! Nothing here runs a program or looks at the machine directly. That
//! goes through [`System`], which the real program implements with
//! processes and the tests with a recorder, so the tests see every
//! command an installation would run, in order, without one being run.
//!
//! The order matters for what a failure costs. `check` reads and
//! compares and changes nothing; an installation that cannot plausibly
//! finish stops there, with the disk as it was. From `partition` on the
//! disk is being written. Whatever happens after that, everything that
//! was mounted is unmounted again before the program ends: the live
//! session is left as it was found, less the contents of the disk the
//! person agreed to erase.
//!
//! A later version that installs beside another system, or encrypts,
//! changes `partition`, `format` and `mount`. The steps after them work
//! on a mounted tree and two identifiers and do not care how those came
//! to be.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sengi_config::settings::zone_file;
use sengi_system::secret::Secret;

use crate::beside::{self, Region, ShrinkRules, Table, Writing};
use crate::disks::{self, partition_path, Disk};
use crate::edit;
use crate::names;
use crate::plan::{Mode, Plan};
use crate::progress::{Event, Resize};
use crate::shrink::{self, Analysis, Examined, Refusal, Shrink, Stop};

pub struct Step {
    pub id: &'static str,
    /// What the window shows while it runs.
    pub title: &'static str,
}

/// The steps, in the order they run.
pub const STEPS: [Step; 9] = [
    Step {
        id: "check",
        title: "Checking the disk and the plan",
    },
    Step {
        id: "partition",
        title: "Preparing the disk",
    },
    Step {
        id: "format",
        title: "Making the file systems",
    },
    Step {
        id: "copy",
        title: "Copying Sengi OS",
    },
    Step {
        id: "configure",
        title: "Setting up the system",
    },
    Step {
        id: "account",
        title: "Creating your account",
    },
    Step {
        id: "boot-image",
        title: "Building the start-up image",
    },
    Step {
        id: "boot-loader",
        title: "Installing the boot loader",
    },
    Step {
        id: "finish",
        title: "Finishing",
    },
];

/// The step that comes after the check where a partition is made smaller
/// first.
pub const SHRINK_STEP: Step = Step {
    id: "shrink",
    title: "Making room",
};

/// The steps of an installation, with or without making room first.
pub fn steps(shrinks: bool) -> Vec<&'static Step> {
    let mut steps: Vec<&'static Step> = STEPS.iter().collect();
    if shrinks {
        steps.insert(1, &SHRINK_STEP);
    }
    steps
}

/// Programs of the running system that the steps call. All of them are
/// looked for before the disk is touched.
const HOST_PROGRAMS: [&str; 16] = [
    "chown",
    "chmod",
    "sfdisk",
    "blockdev",
    "udevadm",
    "mkfs.vfat",
    "mkfs.ext4",
    "blkid",
    "mount",
    "umount",
    "cp",
    "du",
    "chroot",
    "sync",
    "systemctl",
    "efibootmgr",
];

/// What installing beside another system calls in addition: it adds one
/// partition without a reboot, and reads the table as root sees it.
const BESIDE_PROGRAMS: [&str; 2] = ["partx", "sfdisk"];

/// What the copied system must have for the steps that run inside it.
const TARGET_FILES: [&str; 9] = [
    "usr/sbin/grub-install",
    "usr/sbin/update-grub",
    "usr/sbin/update-initramfs",
    "usr/sbin/useradd",
    "usr/sbin/userdel",
    "usr/sbin/chpasswd",
    "usr/bin/passwd",
    "usr/lib/grub/i386-pc/modinfo.sh",
    "usr/lib/grub/x86_64-efi/modinfo.sh",
];

/// What belongs to the live image alone and is not left on an installed
/// system: the installer, the live user's right to be root without a
/// password, the start-up scripts that find the live medium, and the
/// boot test's reporter, which runs commands handed to it by the host.
pub const LIVE_ONLY: [&str; 10] = [
    "usr/bin/sengi-install",
    "usr/bin/sengi-installer",
    "usr/share/applications/sengi-installer.desktop",
    "usr/share/icons/hicolor/scalable/apps/sengi-installer.svg",
    "etc/sudoers.d/live",
    "etc/initramfs-tools/hooks/sengi",
    "etc/initramfs-tools/scripts/sengi",
    "usr/libexec/sengi-boot-report",
    "etc/systemd/system/sengi-boot-report.service",
    "etc/systemd/system/graphical.target.wants/sengi-boot-report.service",
];

/// Packages of the image that only the live system needs.
const LIVE_ONLY_PACKAGES: [&str; 2] = ["ntfs-3g", "libntfs-3g89t64"];

/// The install test's reporter. It is left on the installed system only
/// when that test is what is installing.
pub const INSTALL_TEST: [&str; 4] = [
    "usr/libexec/sengi-install-report",
    "usr/lib/sengi-os/report.sh",
    "etc/systemd/system/sengi-install-report.service",
    "etc/systemd/system/graphical.target.wants/sengi-install-report.service",
];

/// The account of the live image.
const LIVE_USER: &str = "live";

/// Room on the disk beyond the system itself, without which it is not
/// installed: the other two partitions, and space to work in.
const HEADROOM_BYTES: u64 = 2_500_000_000;

pub enum Input<'a> {
    None,
    Text(&'a str),
    /// Written to the program and never to the log.
    Secret(&'a Secret),
}

pub struct Command<'a> {
    pub argv: Vec<String>,
    pub input: Input<'a>,
    /// Part of putting things back after a failure: run it even when the
    /// installation has been told to stop, and do not cut it short.
    pub cleanup: bool,
    /// Cutting it short would damage what it works on: once started it is
    /// let run to its end whatever happens, and signals to stop do not
    /// reach it. For the resize of a file system.
    pub whole: bool,
}

impl Command<'_> {
    pub fn new(argv: &[&str]) -> Command<'static> {
        Command {
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            input: Input::None,
            cleanup: false,
            whole: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Output {
    /// `None` if a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// The start of what a program printed.
#[derive(Debug, Clone, Default)]
pub struct Head {
    /// `None` if a signal ended it, as when it was ended for having given
    /// enough.
    pub code: Option<i32>,
    pub bytes: Vec<u8>,
    /// What it said besides.
    pub said: String,
}

/// The machine, as far as an installation needs it.
pub trait System {
    /// Run a program to its end. `tick` is called now and then while it
    /// runs, with what it has printed so far. An error is a program that
    /// could not be run at all, or an installation that was told to stop.
    fn run(&mut self, command: &Command, tick: &mut dyn FnMut(&str)) -> Result<Output, String>;
    fn disks(&mut self, allow_loop: bool) -> Vec<Disk>;
    fn has_program(&mut self, name: &str) -> bool;
    /// Whether a device node is there.
    fn exists(&mut self, path: &Path) -> bool;
    /// Whether this machine was started by UEFI firmware.
    fn uefi(&mut self) -> bool;
    /// Whether the install test started this machine, which it says
    /// with a credential only whoever runs the machine can set.
    fn under_test(&mut self) -> bool;
    /// Every mount point at or under `path`, deepest first.
    fn mounts_under(&mut self, path: &Path) -> Vec<PathBuf>;
    /// Thirty-two random hexadecimal digits.
    fn random_id(&mut self) -> Result<String, String>;
    fn pause(&mut self, time: Duration);
    /// Open the whole disk so that nothing else can open it to use it:
    /// no mount, no volume built on it. An error if something already
    /// has. Kept until [`System::release`].
    fn hold(&mut self, device: &str) -> Result<(), String>;
    fn release(&mut self);
    /// The partitions of a disk as the kernel has them now: number,
    /// first sector and length, in sectors of 512 bytes.
    fn partitions(&mut self, device: &str) -> Vec<(u32, u64, u64)>;
    /// Whether UEFI Secure Boot is on.
    fn secure_boot(&mut self) -> bool;
    /// Sector 0 of a disk or a partition, as root reads it.
    fn read_sector0(&mut self, device: &str) -> Result<Vec<u8>, String>;
    /// 512 bytes of a disk or a partition, from byte `at`.
    fn read_sector(&mut self, device: &str, at: u64) -> Result<Vec<u8>, String>;
    /// Run a program and take only the first `bytes` of what it prints, as
    /// they are; it is ended once it has given that much. For reading the
    /// start of a file that can be many gigabytes long.
    fn head(&mut self, command: &Command, bytes: usize) -> Result<Head, String>;
    /// That the disk that is held is still the one the node `device`
    /// names, and still answers.
    fn still_held(&mut self, device: &str) -> Result<(), String>;
    /// Whether the power can be relied on: no battery, or a charger.
    fn power(&mut self) -> Result<(), shrink::Refusal>;
    /// A point at which a test may have the installation wait, so that it
    /// can cut the power there. Nothing happens outside such a test.
    fn test_hold(&mut self, point: &str);
    fn log(&mut self, line: &str);
}

pub struct Paths {
    /// Where the new system is mounted while it is put together.
    pub target: PathBuf,
    /// The record of the installation, which is also copied to the disk.
    pub log: PathBuf,
}

/// The step that failed and why.
pub type Failure = (&'static str, String);

struct Installer<'a> {
    plan: &'a Plan,
    password: &'a Secret,
    sys: &'a mut dyn System,
    paths: &'a Paths,
    report: &'a mut dyn FnMut(&Event),
    /// What this run mounted, in order.
    mounts: Vec<PathBuf>,
    kernel: String,
    source_bytes: u64,
    root_uuid: String,
    efi_uuid: String,
    /// The numbers of the EFI system partition and of the one that holds
    /// the system: those of the table a whole-disk install makes, or what
    /// was found and what was made beside another system.
    esp_number: u32,
    root_number: u32,
    /// Beside another system: the table as it was before anything was
    /// written, and the new partition once there is one.
    before: Option<Table>,
    made: bool,
    /// The identifier chosen for the new partition before it is written,
    /// by which it is known again before anything is formatted on it.
    new_uuid: String,
    /// The table as it was after the new partition was written.
    after: Option<Table>,
    /// Sector 0 of the disk as it is held to: as it was, and after a
    /// write of the table as that left it.
    sector0: Option<Vec<u8>>,
    /// The first listing of the table `sfdisk` gave, which is the table
    /// as it was before this installation.
    listing: String,
    /// What was put on the EFI system partition and in the firmware, so
    /// that a failure can take it out again: whether the folder was made
    /// by this installation, and the firmware entry it made.
    made_dir: bool,
    new_bootnum: Option<String>,
    /// `efibootmgr --create` ran, whether or not the entry was found
    /// again; and how many earlier entries of ours it replaced.
    create_ran: bool,
    replaced: usize,
    /// The firmware's boot order as last read: a new order is held to be
    /// a permutation of it.
    boot_order: Vec<String>,
    /// Firmware entries of ours that were there before and may be
    /// replaced: those that start the loader on this EFI system partition.
    deletable: Vec<String>,
    /// Where the new partition goes, as the plan says, and once Windows
    /// has been made smaller, the same in the table as it is then.
    beside: Option<beside::Beside>,
    /// The partition that is made smaller: what it is called by its
    /// identifier, its number, the size its file system gets, and what
    /// may be written to it at this moment.
    windows: Option<Windows>,
}

#[derive(Clone)]
struct Windows {
    node: String,
    number: u32,
    bytes: u64,
    writing: Writing,
}

/// What the guard is told, owned so that it can be used while a program
/// is run.
struct Guard {
    device: String,
    old: Vec<String>,
    esp: String,
    esp_number: u32,
    new: Option<String>,
    new_number: Option<u32>,
    deletable: Vec<String>,
    boot_order: Vec<String>,
    windows: Option<Windows>,
}

impl Guard {
    fn judge(&self, argv: &[String]) -> Result<(), String> {
        beside::guard(
            argv,
            &beside::Rules {
                device: &self.device,
                old: &self.old,
                esp: &self.esp,
                esp_number: self.esp_number,
                new: self.new.as_deref(),
                new_number: self.new_number,
                deletable: &self.deletable,
                boot_order: &self.boot_order,
                shrink: self.windows.as_ref().map(|windows| ShrinkRules {
                    node: &windows.node,
                    number: windows.number,
                    bytes: windows.bytes,
                    writing: windows.writing,
                }),
            },
        )
    }
}

/// How far `ntfsresize` says it has got in moving data, which is the long
/// part: the last figure it printed for it.
fn resize_percent(said: &str) -> Option<u8> {
    let part = said.rsplit("Relocating needed data").next()?;
    if part.len() == said.len() {
        return None;
    }
    let before = &part[..part.rfind(" percent completed")?];
    let number = before
        .rsplit(|c: char| !(c.is_ascii_digit() || c == '.'))
        .next()?;
    let whole = number.split('.').next()?.parse::<u8>().ok()?;
    Some(whole.min(100))
}

/// How much of the file system at `path` is in use, in bytes.
fn used_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs is given a string that ends and room for its answer.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some((stat.f_blocks.saturating_sub(stat.f_bfree) as u64).saturating_mul(stat.f_frsize as u64))
}

/// How much room is left on the file system at `path`, in bytes.
fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs is given a string that ends and room for its answer.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

/// The last thing a program said, for the one line a failure gets.
fn last_line(output: &Output) -> &str {
    fn pick(text: &str) -> Option<&str> {
        text.lines().rev().find(|line| !line.trim().is_empty())
    }
    pick(&output.stderr)
        .or_else(|| pick(&output.stdout))
        .unwrap_or("it said nothing")
        .trim()
}

impl Installer<'_> {
    fn tell(&mut self, event: Event) {
        self.sys.log(&event.line());
        (self.report)(&event);
    }

    fn warn(&mut self, text: String) {
        self.tell(Event::Warning(text));
    }

    /// Run a program that has to succeed, and give back what it printed.
    fn run_command(&mut self, command: &Command) -> Result<String, String> {
        self.guard(&command.argv)?;
        let output = self.sys.run(command, &mut |_| {})?;
        Self::judge(command, output)
    }

    fn judge(command: &Command, output: Output) -> Result<String, String> {
        let program = command.argv.first().map(String::as_str).unwrap_or("?");
        // What ran inside the new system is named by what ran there.
        let program = match (program, command.argv.get(2)) {
            ("chroot", Some(inner)) => inner,
            _ => program,
        };
        match output.code {
            Some(0) => Ok(output.stdout),
            Some(code) => Err(format!(
                "{program} ended with status {code}: {}",
                last_line(&output)
            )),
            None => Err(format!("{program} was ended by a signal")),
        }
    }

    fn run(&mut self, argv: &[&str]) -> Result<String, String> {
        self.run_command(&Command::new(argv))
    }

    /// Run a program inside the new system.
    fn inside(&mut self, argv: &[&str]) -> Result<String, String> {
        let target = self.paths.target.to_string_lossy().into_owned();
        let mut full = vec!["chroot", target.as_str()];
        full.extend_from_slice(argv);
        self.run(&full)
    }

    fn at(&self, path: &str) -> PathBuf {
        self.paths.target.join(path)
    }

    fn read(&self, path: &str) -> Result<String, String> {
        fs::read_to_string(self.at(path))
            .map_err(|error| format!("could not read /{path} of the new system: {error}"))
    }

    fn write(&mut self, path: &str, contents: &str) -> Result<(), String> {
        self.sys.log(&format!("write /{path}"));
        let full = self.at(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not make the directory of /{path}: {error}"))?;
        }
        fs::write(&full, contents)
            .map_err(|error| format!("could not write /{path} of the new system: {error}"))
    }

    /// Take a file out of the new system. One that is not there is
    /// already out.
    fn remove(&mut self, path: &str) -> Result<(), String> {
        match fs::remove_file(self.at(path)) {
            Ok(()) => {
                self.sys.log(&format!("remove /{path}"));
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "could not remove /{path} from the new system: {error}"
            )),
        }
    }

    fn mount(&mut self, argv: &[&str], at: PathBuf) -> Result<(), String> {
        self.run(argv)?;
        self.mounts.push(at);
        Ok(())
    }

    fn part(&self, number: u32) -> String {
        partition_path(&self.plan.device, number)
    }

    /// Beside another system, every command is held to the one rule that
    /// matters: nothing writes to anything but the new partition, but for
    /// the exact commands that make the Windows partition smaller.
    fn guard(&self, argv: &[String]) -> Result<(), String> {
        match self.guarding() {
            Some(guard) => guard.judge(argv),
            None => Ok(()),
        }
    }

    fn guarding(&self) -> Option<Guard> {
        let (Mode::Beside, Some(before)) = (self.plan.mode, &self.before) else {
            return None;
        };
        Some(Guard {
            device: self.plan.device.clone(),
            old: beside::nodes(&self.plan.device, before),
            esp: self.part(self.esp_number),
            esp_number: self.esp_number,
            new: self.made.then(|| self.root_node()),
            new_number: self.made.then_some(self.root_number),
            deletable: self.deletable.clone(),
            boot_order: self.boot_order.clone(),
            windows: self.windows.clone(),
        })
    }

    /// What the system partition is called to format and mount it. Beside
    /// another system that is its partition identifier, chosen before it
    /// was written and checked just before the format: the name a disk
    /// has can move when another is plugged in, the identifier cannot.
    fn root_node(&self) -> String {
        if self.plan.mode == Mode::Beside && self.made {
            format!(
                "/dev/disk/by-partuuid/{}",
                self.new_uuid.to_ascii_lowercase()
            )
        } else {
            self.part(self.root_number)
        }
    }

    /// A device node follows the kernel in a moment.
    fn wait_for(&mut self, node: &Path) -> Result<(), String> {
        let mut tries = 0;
        while !self.sys.exists(node) {
            tries += 1;
            if tries > 50 {
                return Err(format!("{} did not appear", node.display()));
            }
            self.sys.pause(Duration::from_millis(200));
        }
        Ok(())
    }

    /// The table of the disk as root reads it now.
    fn read_table(&mut self) -> Result<Table, String> {
        let command = reading_table(&self.plan.device);
        self.guard(&command.argv)?;
        let output = self.sys.run(&command, &mut |_| {})?;
        if self.listing.is_empty() {
            self.listing = output.stdout.clone();
        }
        table_from(&command, output, &self.plan.device)
    }

    /// Everything that can be known before the disk is touched.
    fn check(&mut self) -> Result<(), String> {
        let plan = self.plan;
        names::password(self.password.expose())?;

        // What an earlier run that was killed left mounted.
        self.unmount_all();

        let disks = self.sys.disks(plan.allow_loop);
        let disk = disks
            .iter()
            .find(|disk| disk.path == plan.device)
            .ok_or(format!("there is no disk called {}", plan.device))?;
        if let Some(refusal) = disks::refusal(disk) {
            return Err(format!(
                "{} cannot be installed on: {}",
                plan.device,
                refusal.words()
            ));
        }
        let same = disk.size == plan.size
            && disk.model == plan.model
            && disk.serial == plan.serial
            && disk.wwn == plan.wwn;
        if !same {
            return Err(format!(
                "{} is not the disk that was chosen: it is \u{201c}{}\u{201d} of {} bytes, serial \u{201c}{}\u{201d}, and the plan is for \u{201c}{}\u{201d} of {} bytes, serial \u{201c}{}\u{201d}",
                plan.device, disk.model, disk.size, disk.serial, plan.model, plan.size, plan.serial
            ));
        }

        let beside = plan.beside.clone();
        if beside.is_some() {
            if !self.sys.uefi() {
                return Err(
                    "installing beside another system needs UEFI, and this computer did not start with it"
                        .to_string(),
                );
            }
            if self.sys.secure_boot() {
                return Err(
                    "Secure Boot is on: turn it off in the firmware settings first, since the boot loader put on the EFI system partition is not signed"
                        .to_string(),
                );
            }
            for program in BESIDE_PROGRAMS {
                if !self.sys.has_program(program) {
                    return Err(format!("{program} is missing from this system"));
                }
            }
        }
        if beside
            .as_ref()
            .is_some_and(|beside| beside.shrink.is_some())
        {
            for program in shrink::PROGRAMS {
                if !self.sys.has_program(program) {
                    return Err(format!("{program} is missing from this system"));
                }
            }
        }

        let source = &plan.source_root;
        let release = fs::read_to_string(source.join("etc/os-release")).unwrap_or_default();
        if !release.lines().any(|line| line == "ID=sengios") {
            return Err(format!(
                "there is no Sengi OS system to copy at {}",
                source.display()
            ));
        }
        if !plan.kernel.is_file() {
            return Err(format!("there is no kernel at {}", plan.kernel.display()));
        }
        if zone_file(&source.join("usr/share/zoneinfo"), &plan.time_zone).is_none() {
            return Err(format!("there is no time zone called {}", plan.time_zone));
        }
        let read = |path: &str| fs::read_to_string(source.join(path)).unwrap_or_default();
        if names::taken(&plan.user, &read("etc/passwd"), &read("etc/group")) {
            return Err(format!(
                "the user name {} is already used by the system",
                plan.user
            ));
        }
        for program in HOST_PROGRAMS {
            if !self.sys.has_program(program) {
                return Err(format!("{program} is missing from this system"));
            }
        }
        for file in TARGET_FILES {
            if !source.join(file).exists() {
                return Err(format!("/{file} is missing from the system to copy"));
            }
        }
        if beside.is_some() && !source.join("usr/bin/os-prober").exists() {
            return Err("/usr/bin/os-prober is missing from the system to copy".to_string());
        }
        let mut kernels: Vec<String> = fs::read_dir(source.join("lib/modules"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        self.kernel = match (kernels.pop(), kernels.is_empty()) {
            (Some(kernel), true) => kernel,
            _ => return Err("the system to copy does not have exactly one kernel".to_string()),
        };

        let source_text = source.to_string_lossy().into_owned();
        let counted = self.run(&["du", "-sxk", source_text.as_str()])?;
        self.source_bytes = counted
            .split_whitespace()
            .next()
            .and_then(|kilobytes| kilobytes.parse::<u64>().ok())
            .filter(|kilobytes| *kilobytes > 0)
            .ok_or("du did not say how large the system is")?
            .saturating_mul(1024);
        let room = match &beside {
            Some(beside) => beside.region().bytes(),
            None => plan.size,
        };
        if self.source_bytes.saturating_add(HEADROOM_BYTES) > room {
            return Err(format!(
                "the disk is too small: the system alone is {}",
                disks::size_words(self.source_bytes)
            ));
        }
        let shrink = beside.as_ref().and_then(|beside| beside.shrink);
        if let Some(beside) = beside {
            self.check_beside(&beside)?;
        }
        // Kept until the new table is written: the disk, opened so that
        // nothing else can open it to mount it or build on it. If
        // something already has, this is where it shows, whatever the
        // tables read above said.
        self.sys.hold(&plan.device)?;
        if let Some(shrink) = shrink {
            // Looked at with the disk held, so that the nodes of its
            // partitions cannot come to be another disk's meanwhile.
            self.verify_unchanged()?;
            self.look_at_windows(&shrink)?;
        }
        Ok(())
    }

    /// Beside another system, all of this before the disk is held and
    /// before a byte is written: root reads the table, and it has to be
    /// the one the person was shown; the EFI system partition has to be a
    /// FAT volume with room for GRUB.
    fn check_beside(&mut self, beside: &beside::Beside) -> Result<(), String> {
        // The reads here (the table, the type of the EFI system partition)
        // change nothing and are not held to the rules of the commands
        // that write.
        let table = self.read_table()?;
        beside::check_before(&table, beside)?;
        let device = self.plan.device.clone();
        let sector0 = self.sys.read_sector0(&device)?;
        beside::mbr_is_plain(&sector0)?;
        beside::mbr_stays(&sector0)?;
        self.sector0 = Some(sector0);
        self.esp_number = beside.esp;
        // The guard knows the old partitions from here on.
        self.before = Some(table);
        let esp = self.part(beside.esp);
        let fs = self.run(&["blkid", "-s", "TYPE", "-o", "value", &esp])?;
        if fs.trim() != "vfat" {
            return Err(format!("{esp} is not a FAT volume"));
        }
        // Looked at read-only: how much room is on it.
        let target = self.paths.target.clone();
        let target_text = target.to_string_lossy().into_owned();
        fs::create_dir_all(&target)
            .map_err(|error| format!("could not make {target_text}: {error}"))?;
        self.mount(
            &[
                "mount",
                "-t",
                "vfat",
                "-o",
                "ro,noexec,nosuid,nodev",
                &esp,
                &target_text,
            ],
            target.clone(),
        )?;
        let free = free_bytes(&target);
        self.run(&["umount", &target_text])?;
        self.mounts.clear();
        match free {
            Some(free) if free >= beside::ESP_MIN_FREE_BYTES => Ok(()),
            Some(free) => Err(format!(
                "the EFI system partition has {} free, and GRUB needs {}",
                disks::size_words(free),
                disks::size_words(beside::ESP_MIN_FREE_BYTES)
            )),
            None => Err("could not tell how much room the EFI system partition has".to_string()),
        }
    }

    /// Everything about the partition that is to be made smaller, all of
    /// it by looking: whether it may be, and whether `ntfsresize` can go
    /// through the whole of the resize to the size asked for without
    /// writing. Gives what was found, and whether the file system has to
    /// change at all. It does not where it already is as large as asked,
    /// as an attempt that stopped after it leaves it; it then has to pass
    /// the same check as after a resize.
    fn look_at_windows(&mut self, shrink: &Shrink) -> Result<(Examined, bool), String> {
        let before = self
            .before
            .clone()
            .ok_or("the partition table was not checked")?;
        let entry = before
            .entry(shrink.partition)
            .cloned()
            .ok_or("there is no partition to make smaller")?;
        let bytes = shrink.file_system_bytes();
        self.windows = Some(Windows {
            node: self.part(entry.number),
            number: entry.number,
            bytes,
            writing: Writing::Nothing,
        });
        let guard = self.guarding().ok_or("there is nothing to guard with")?;
        let allow = |argv: &[String]| guard.judge(argv);
        let device = self.plan.device.clone();
        let number = entry.number;
        let stopped = move |stop: Stop| match stop {
            Stop::Refused(refusal) => format!(
                "partition {number} cannot be made smaller: {}. {}",
                refusal.short(),
                refusal.words()
            ),
            Stop::Failed(why) => why,
        };
        let examined =
            shrink::examine(self.sys, &allow, &device, &before, &entry, true).map_err(stopped)?;
        let kept = shrink.sectors.saturating_mul(512);
        if kept < examined.room.keep_bytes {
            return Err(format!(
                "partition {number} would be left {}, and it needs {} for what it uses and room to work in",
                disks::size_words(kept),
                disks::size_words(examined.room.keep_bytes)
            ));
        }
        let end = examined.ntfs.end_bytes().unwrap_or(u64::MAX);
        let resize = end > bytes;
        if resize {
            shrink::dry_run(self.sys, &allow, &examined.node, bytes).map_err(stopped)?;
        } else {
            let (first, last) = self.volume_ends(&examined, bytes)?;
            shrink::check_volume(&examined.ntfs, &first, &last, shrink).map_err(|why| {
                format!(
                    "the file system on partition {number} already ends before the size asked for, and cannot be taken as it is: {why}. Nothing was changed. {}",
                    shrink::EXTEND
                )
            })?;
        }
        Ok((examined, resize))
    }

    /// The first sector of the volume, and the last sector of the `bytes`
    /// it is to fit in, where the copy of the first belongs.
    fn volume_ends(
        &mut self,
        examined: &Examined,
        bytes: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        let first = self.sys.read_sector0(&examined.node)?;
        let last = self.sys.read_sector(
            &examined.node,
            bytes.saturating_sub(examined.ntfs.sector_bytes),
        )?;
        Ok((first, last))
    }

    /// Immediately before the partition that is made smaller is written to
    /// or opened for writing: the disk that is held is still the one of
    /// this name and the one that was chosen, no other partition has the
    /// identifier of this one, as one on a copy of the disk would, and the
    /// node still holds it.
    fn confirm_target(&mut self, number: u32) -> Result<(), String> {
        let device = self.plan.device.clone();
        self.sys.still_held(&device)?;
        self.same_disk()?;
        let uuid = self
            .before
            .as_ref()
            .and_then(|table| table.entry(number))
            .map(|entry| entry.uuid.clone())
            .ok_or("there is no partition to make smaller")?;
        if shrink::twins(&self.sys.disks(self.plan.allow_loop), &uuid) > 1 {
            return Err(shrink::Refusal::Twin.words());
        }
        let node = self.part(number);
        let found = self.run(&["blkid", "-s", "PARTUUID", "-o", "value", &node])?;
        if !found.trim().eq_ignore_ascii_case(&uuid) {
            return Err(format!("{node} is not the partition that was chosen"));
        }
        Ok(())
    }

    /// The disk, its table and its first sector are still what they were,
    /// and the power can still be relied on.
    fn verify_unchanged(&mut self) -> Result<(), String> {
        let plan = self.plan;
        let device = plan.device.clone();
        self.same_disk()?;
        let disks = self.sys.disks(plan.allow_loop);
        if let Some(refusal) = disks
            .iter()
            .find(|disk| disk.path == device)
            .and_then(disks::refusal)
        {
            return Err(format!("{device}: {}", refusal.words()));
        }
        let now = self.read_table()?;
        if self.before.as_ref() != Some(&now) {
            return Err("the partition table of the disk changed".to_string());
        }
        if self.sys.read_sector0(&device)?.as_slice() != self.sector0.as_deref().unwrap_or_default()
        {
            return Err("sector 0 of the disk changed".to_string());
        }
        self.sys.power().map_err(|refusal| refusal.words())
    }

    fn same_disk(&mut self) -> Result<(), String> {
        let plan = self.plan;
        let device = &plan.device;
        let disks = self.sys.disks(plan.allow_loop);
        let same = disks
            .iter()
            .find(|disk| disk.path == *device)
            .is_some_and(|disk| {
                disk.size == plan.size
                    && disk.model == plan.model
                    && disk.serial == plan.serial
                    && disk.wwn == plan.wwn
            });
        if same {
            Ok(())
        } else {
            Err(format!("{device} is no longer the disk that was chosen"))
        }
    }

    /// After sfdisk has written the table of a disk with sectors of
    /// `sector_size` bytes: sector 0 is as it may be
    /// ([`beside::check_sector0`]), and is what it is held to from now on.
    fn sector0_after_writing(&mut self, sector_size: u64) -> Result<(), String> {
        let sector0 = self.sys.read_sector0(&self.plan.device)?;
        beside::check_sector0(
            self.sector0.as_deref().unwrap_or_default(),
            &sector0,
            self.plan.size / sector_size,
        )?;
        self.sector0 = Some(sector0);
        Ok(())
    }

    fn may_write(&mut self, writing: Writing) {
        if let Some(windows) = self.windows.as_mut() {
            windows.writing = writing;
        }
    }

    /// Make a partition smaller: first its file system, to less
    /// than the partition is going to be, then the partition. What each
    /// stage leaves is said as it goes, so that a failure can be answered
    /// with what state the disk is in.
    fn shrink(&mut self) -> Result<(), String> {
        let beside = self.beside.clone().ok_or("there is no place to install")?;
        let shrink = beside.shrink.ok_or("there is nothing to make smaller")?;
        // All of it once more, right before the first write.
        self.verify_unchanged()?;
        let (examined, resize) = self.look_at_windows(&shrink)?;
        // The looking takes a while on a large volume.
        self.verify_unchanged()?;
        let node = examined.node.clone();
        let bytes = shrink.file_system_bytes();

        if resize {
            self.confirm_target(shrink.partition)?;
            self.may_write(Writing::FileSystem);
            let command = Command {
                argv: shrink::resize_argv(&node, bytes),
                // Its one question, answered. It is never told to force
                // anything.
                input: Input::Text("y\n"),
                cleanup: false,
                whole: true,
            };
            self.guard(&command.argv)?;
            self.tell(Event::Resize(Resize::Writing));
            let (sys, report) = (&mut *self.sys, &mut *self.report);
            let mut shown = 0;
            let output = sys.run(&command, &mut |said| {
                if let Some(percent) = resize_percent(said).map(|percent| percent.min(99)) {
                    if percent > shown {
                        shown = percent;
                        report(&Event::Percent("shrink".to_string(), percent));
                    }
                }
            });
            self.may_write(Writing::Nothing);
            Self::judge(&command, output?)?;
        }

        // What is on the disk now, read from it. Until it has passed, a
        // resize leaves a file system that may be damaged.
        let failed = |why: String| {
            if resize {
                format!("the file system did not pass the check after the resize: {why}")
            } else {
                why
            }
        };
        let (first, last) = self.volume_ends(&examined, bytes)?;
        shrink::check_volume(&examined.ntfs, &first, &last, &shrink).map_err(failed)?;
        self.confirm_target(shrink.partition).map_err(failed)?;
        let probe = Command {
            argv: shrink::probe_argv(&node),
            input: Input::None,
            cleanup: false,
            whole: false,
        };
        self.guard(&probe.argv)?;
        let output = self.sys.run(&probe, &mut |_| {})?;
        if let Some(refusal) = shrink::probe_refusal(output.code) {
            return Err(failed(refusal.short().to_string()));
        }
        if resize {
            self.tell(Event::Resize(Resize::FileSystem));
            self.sys.test_hold("after-filesystem");
        }

        // The partition, and only its size.
        self.verify_unchanged()?;
        self.confirm_target(shrink.partition)?;
        // The table as it was, together, in the record of the installation,
        // for whoever has to put it back.
        let listing = self.listing.clone();
        self.sys.log(&format!(
            "the partition table of {} as it is before the one change, as sfdisk lists it:",
            self.plan.device
        ));
        for line in listing.lines() {
            self.sys.log(&format!("    {line}"));
        }
        let before = self
            .before
            .clone()
            .ok_or("the partition table was not checked")?;
        let device = self.plan.device.clone();
        self.may_write(Writing::Table);
        let input = shrink::sfdisk_input(&before, &shrink);
        let command = Command {
            argv: shrink::sfdisk_argv(&device, shrink.partition),
            input: Input::Text(&input),
            cleanup: false,
            whole: true,
        };
        self.guard(&command.argv)?;
        self.tell(Event::Resize(Resize::Table));
        let output = self.sys.run(&command, &mut |_| {});
        self.may_write(Writing::Nothing);
        Self::judge(&command, output?)?;
        let after = self.read_table()?;
        shrink::check_shrunk(&before, &after, &shrink)?;
        self.sector0_after_writing(before.sector_size)?;
        // The kernel is told of the one new size.
        self.sys.release();
        self.may_write(Writing::Kernel);
        let command = Command {
            argv: shrink::partx_argv(&device, shrink.partition),
            input: Input::None,
            cleanup: false,
            whole: false,
        };
        let told = self.run_command(&command);
        self.may_write(Writing::Nothing);
        told?;
        self.run(&["udevadm", "settle", "--timeout=30"])?;
        self.wait_for(Path::new(&node))?;
        let mut want: Vec<(u32, u64, u64)> = after
            .entries
            .iter()
            .map(|entry| (entry.number, entry.start, entry.sectors))
            .collect();
        want.sort();
        let mut seen = self.sys.partitions(&device);
        seen.sort();
        if seen != want {
            return Err(format!(
                "the kernel does not show the table that was written to {device}"
            ));
        }
        let last = self.sys.read_sector0(&node)?;
        if last.get(..512) != first.get(..512) {
            return Err(
                "the first sector of the partition that was made smaller changed".to_string(),
            );
        }
        self.tell(Event::Resize(Resize::Done));

        // From here this is an installation in free space like any other.
        self.sys.hold(&device)?;
        self.beside = Some(beside::Beside {
            table: after.snapshot(),
            shrink: None,
            ..beside
        });
        self.before = Some(after);
        Ok(())
    }

    /// From here on the disk is no longer what it was.
    fn partition(&mut self) -> Result<(), String> {
        if self.plan.beside.is_some() {
            return self.partition_beside();
        }
        let device = self.plan.device.clone();
        // The table is written while the disk is held, and the kernel is
        // not asked to read it yet: it would refuse, for the same reason
        // nobody else can open the disk.
        let written = self.run_command(&Command {
            argv: [
                "sfdisk",
                "--lock",
                "--no-reread",
                "--no-tell-kernel",
                "--wipe",
                "always",
                "--wipe-partitions",
                "always",
                &device,
            ]
            .map(String::from)
            .to_vec(),
            input: Input::Text(edit::PARTITIONS),
            cleanup: false,
            whole: false,
        });
        self.sys.release();
        written?;
        // Now the kernel reads it. If anything took the disk or one of
        // its old partitions in the moment between, this fails.
        self.run(&["blockdev", "--rereadpt", &device])?;
        self.run(&["udevadm", "settle", "--timeout=30"])?;
        // The kernel has the new table; the nodes follow in a moment.
        for number in [edit::BIOS_PART, edit::EFI_PART, edit::ROOT_PART] {
            self.wait_for(Path::new(&self.part(number)))?;
        }
        // And it is the table that was asked for, as the kernel sees it,
        // before anything is formatted by partition number: a kernel
        // still on the old table would have mkfs write somewhere else.
        let seen = self.sys.partitions(&device);
        edit::table_is_as_asked(&seen, self.plan.size / 512).map_err(|why| {
            format!("the kernel does not show the new partition table on {device}: {why}")
        })
    }

    /// One partition is added in the free space and nothing else changes.
    /// The table is read again while the disk is held, written with
    /// `sfdisk --append`, read again, and compared with what it was; the
    /// kernel is then told of the one new partition and its view compared
    /// too. Any difference stops the installation here, before mkfs.
    fn partition_beside(&mut self) -> Result<(), String> {
        let device = self.plan.device.clone();
        let beside = self.beside.clone().ok_or("there is no place to install")?;
        let region: Region = beside.region();

        // Held, then looked at once more: nothing can have mounted
        // anything on the disk between the look and the write.
        let before = self.read_table()?;
        beside::check_before(&before, &beside)?;
        if self.before.as_ref() != Some(&before) {
            return Err(
                "the partition table of the disk changed between the check and the write; nothing was written"
                    .to_string(),
            );
        }
        if self.sys.read_sector0(&device)?.as_slice() != self.sector0.as_deref().unwrap_or_default()
        {
            return Err("sector 0 of the disk changed; nothing was written".to_string());
        }
        let id = self.sys.random_id()?;
        self.new_uuid = beside::make_uuid(&id).ok_or("no identifier could be made")?;
        let input = beside::append_input(&before, &region, &self.new_uuid);
        let written = self.run_command(&Command {
            argv: [
                "sfdisk",
                "--lock",
                "--no-reread",
                "--no-tell-kernel",
                "--wipe",
                "never",
                "--wipe-partitions",
                "always",
                "--append",
                &device,
            ]
            .map(String::from)
            .to_vec(),
            input: Input::Text(&input),
            cleanup: false,
            whole: false,
        });
        self.sys.release();
        written?;

        let after = self.read_table()?;
        let uuid = self.new_uuid.clone();
        let new = beside::check_after(&before, &after, &beside, &uuid)?;
        self.sector0_after_writing(before.sector_size)?;
        self.root_number = new;
        self.made = true;
        self.tell(Event::Made(new));
        // Only the new partition is added to the kernel; the old ones are
        // not read again, so nothing that is using them is disturbed.
        let number = new.to_string();
        self.run(&["partx", "--add", "--nr", &number, &device])?;
        self.run(&["udevadm", "settle", "--timeout=30"])?;
        for number in [self.esp_number, new] {
            self.wait_for(Path::new(&self.part(number)))?;
        }
        self.wait_for(Path::new(&self.root_node()))?;
        let seen = self.sys.partitions(&device);
        beside::check_kernel(&before, &seen, &region, new).map_err(|why| {
            format!("the kernel does not show the table that was written to {device}: {why}")
        })?;
        self.after = Some(after);
        Ok(())
    }

    /// Beside another system, just before anything is formatted: the
    /// disk is still the one that was chosen, the table is still the one
    /// that was written, and the node about to be formatted holds the
    /// partition that was made, by the identifier chosen for it, and the
    /// EFI system partition the one that was there. The disk cannot be
    /// held now, since the new partition is about to be opened for
    /// formatting; names can move when a disk is plugged in or out, which
    /// is what this is for.
    fn verify_before_format(&mut self) -> Result<(), String> {
        self.same_disk()
            .map_err(|why| format!("{why}; nothing was formatted"))?;
        let now = self.read_table()?;
        let (Some(after), Some(before)) = (self.after.clone(), self.before.clone()) else {
            return Err("the partition table was not checked".to_string());
        };
        if now != after {
            return Err(
                "the partition table of the disk is not the one that was written; nothing was formatted"
                    .to_string(),
            );
        }
        let root = self.part(self.root_number);
        let found = self.run(&["blkid", "-s", "PARTUUID", "-o", "value", &root])?;
        if !found.trim().eq_ignore_ascii_case(&self.new_uuid) {
            return Err(format!(
                "{root} is not the partition that was made (its identifier is \u{201c}{}\u{201d}); nothing was formatted",
                found.trim()
            ));
        }
        let esp = self.part(self.esp_number);
        let found = self.run(&["blkid", "-s", "PARTUUID", "-o", "value", &esp])?;
        let was = before
            .entry(self.esp_number)
            .map(|entry| entry.uuid.clone())
            .unwrap_or_default();
        if found.trim().to_ascii_lowercase() != was {
            return Err(format!(
                "{esp} is not the EFI system partition that was there; nothing was formatted"
            ));
        }
        Ok(())
    }

    fn uuid_of(&mut self, node: &str) -> Result<String, String> {
        let uuid = self.run(&["blkid", "-s", "UUID", "-o", "value", node])?;
        let uuid = uuid.trim();
        let fine = !uuid.is_empty()
            && uuid.len() <= 64
            && uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        if fine {
            Ok(uuid.to_string())
        } else {
            Err(format!("blkid gave no identifier for {node}"))
        }
    }

    fn format(&mut self) -> Result<(), String> {
        let (efi, root) = (self.part(self.esp_number), self.root_node());
        // Beside another system the EFI system partition is not ours to
        // format, and the root must be the partition that was just made.
        if self.plan.mode == Mode::Erase {
            self.run(&["mkfs.vfat", "-F", "32", "-n", "SENGI-EFI", &efi])?;
        } else if !self.made
            || self
                .before
                .as_ref()
                .is_some_and(|before| before.entry(self.root_number).is_some())
        {
            return Err("the partition to format is not one that was just made".to_string());
        } else {
            self.verify_before_format()?;
        }
        self.run(&["mkfs.ext4", "-q", "-F", "-L", "sengi-os", &root])?;
        self.run(&["udevadm", "settle", "--timeout=30"])?;
        self.efi_uuid = self.uuid_of(&efi)?;
        self.root_uuid = self.uuid_of(&root)?;
        Ok(())
    }

    fn copy(&mut self) -> Result<(), String> {
        let target = self.paths.target.clone();
        let target_text = target.to_string_lossy().into_owned();
        fs::create_dir_all(&target)
            .map_err(|error| format!("could not make {target_text}: {error}"))?;
        let root = self.root_node();
        self.mount(
            &["mount", "-t", "ext4", &root, &target_text],
            target.clone(),
        )?;

        // Everything, as it is: owners, modes, links, extended
        // attributes. From the image as it was built, not from the
        // running session with whatever was changed in it.
        let source = format!("{}/.", self.plan.source_root.display());
        let command = Command::new(&["cp", "-ax", &source, &target_text]);
        let before = used_bytes(&target).unwrap_or(0);
        let total = self.source_bytes.max(1);
        let (sys, report) = (&mut *self.sys, &mut *self.report);
        let mut shown = 0;
        let output = sys.run(&command, &mut |_| {
            let copied = used_bytes(&target).unwrap_or(0).saturating_sub(before);
            // Never a hundred until cp has said it is done.
            let percent = (copied.saturating_mul(100) / total).min(99) as u8;
            if percent > shown {
                shown = percent;
                report(&Event::Percent("copy".to_string(), percent));
            }
        })?;
        Self::judge(&command, output)?;
        // The copy gave the top directory the owner and mode of the one
        // it was copied from. In the image those are root and 0755, and
        // a system whose top directory is anyone else's starts no
        // session, so they are set and not assumed.
        self.run(&["chown", "0:0", &target_text])?;
        self.run(&["chmod", "0755", &target_text])?;

        // The live image keeps its kernel beside the system, not in it.
        let kernel = format!("boot/vmlinuz-{}", self.kernel);
        self.sys
            .log(&format!("copy {} to /{kernel}", self.plan.kernel.display()));
        fs::create_dir_all(self.at("boot"))
            .and_then(|()| fs::copy(&self.plan.kernel, self.at(&kernel)))
            .and_then(|_| fs::set_permissions(self.at(&kernel), fs::Permissions::from_mode(0o644)))
            .map_err(|error| format!("could not copy the kernel: {error}"))?;

        let efi_dir = self.at("boot/efi");
        fs::create_dir_all(&efi_dir)
            .map_err(|error| format!("could not make /boot/efi: {error}"))?;
        let efi = self.part(self.esp_number);
        let efi_text = efi_dir.to_string_lossy().into_owned();
        self.mount(
            &["mount", "-t", "vfat", "-o", "umask=0077", &efi, &efi_text],
            efi_dir,
        )?;
        self.tell(Event::Percent("copy".to_string(), 100));
        Ok(())
    }

    fn configure(&mut self) -> Result<(), String> {
        let plan = self.plan;
        self.write("etc/fstab", &edit::fstab(&self.root_uuid, &self.efi_uuid))?;
        self.write("etc/hostname", &format!("{}\n", plan.host_name))?;
        self.write("etc/hosts", &edit::hosts(&plan.host_name))?;
        // An identity of its own: the image has none, so that no two
        // machines started from it share one.
        let id = self.sys.random_id()?;
        self.write("etc/machine-id", &format!("{id}\n"))?;

        let localtime = self.at("etc/localtime");
        match fs::remove_file(&localtime) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("could not replace /etc/localtime: {error}"));
            }
            _ => {}
        }
        std::os::unix::fs::symlink(
            format!("/usr/share/zoneinfo/{}", plan.time_zone),
            &localtime,
        )
        .map_err(|error| format!("could not set the time zone: {error}"))?;
        self.write("etc/timezone", &format!("{}\n", plan.time_zone))?;

        let settings = "etc/xdg/sengi-os/settings.toml";
        let existing = fs::read_to_string(self.at(settings)).unwrap_or_default();
        self.write(settings, &edit::system_settings(&existing, &plan.keyboard))?;
        let greeter = "usr/share/sengi-os/greeter/environment";
        let existing = fs::read_to_string(self.at(greeter)).unwrap_or_default();
        self.write(
            greeter,
            &edit::greeter_environment(&existing, &plan.keyboard),
        )?;

        // The login screen at every start, with nobody logged in for them.
        let greetd = "etc/greetd/config.toml";
        let without = edit::greetd_without_autologin(&self.read(greetd)?).ok_or(
            "/etc/greetd/config.toml has no automatic login to remove; it is not the live image's",
        )?;
        self.write(greetd, &without)?;

        let dock = "etc/xdg/sengi-os/dock.toml";
        if self.at(dock).is_file() {
            let without = edit::dock_without(&self.read(dock)?, "sengi-installer");
            self.write(dock, &without)?;
        }
        for file in LIVE_ONLY {
            self.remove(file)?;
        }
        // The install test's reporter stays only where the test asked
        // for one: the machine this runs on was started with the test's
        // credential. Anywhere else it is no part of an installed system.
        if self.sys.under_test() {
            self.sys
                .log("the install test is running: its reporter stays");
        } else {
            for file in INSTALL_TEST {
                self.remove(file)?;
            }
        }
        // Host keys would be the same on every machine. The image has
        // no SSH server, and if one ever gets in, its keys stay behind.
        if let Ok(entries) = fs::read_dir(self.at("etc/ssh")) {
            for name in entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()) {
                if name.starts_with("ssh_host_") {
                    self.remove(&format!("etc/ssh/{name}"))?;
                }
            }
        }

        let grub = match plan.mode {
            Mode::Erase => edit::GRUB_DEFAULT.to_string(),
            Mode::Beside => edit::grub_default_beside(),
        };
        self.write("etc/default/grub", &grub)?;
        // The way in when the desktop does not start. The menu gets an
        // entry that starts the same kernel without it, and a login on
        // the first console that only that entry switches on: the image
        // masks that login, because the desktop has no use for it.
        self.write("etc/grub.d/11_sengi-text", edit::GRUB_TEXT_ENTRY)?;
        fs::set_permissions(
            self.at("etc/grub.d/11_sengi-text"),
            fs::Permissions::from_mode(0o755),
        )
        .map_err(|error| format!("could not make the menu entry runnable: {error}"))?;
        self.remove("etc/systemd/system/getty@tty1.service")?;
        self.write(
            "etc/systemd/system/getty@tty1.service.d/10-sengi.conf",
            edit::TEXT_LOGIN,
        )?;
        // There is no swap partition to wake from, and the start-up
        // image must not wait for one.
        self.write("etc/initramfs-tools/conf.d/resume", "RESUME=none\n")?;
        Ok(())
    }

    fn account(&mut self) -> Result<(), String> {
        let plan = self.plan;
        let target = self.paths.target.clone();
        // The programs that run inside the new system from here on find
        // the devices and the kernel's tables where they expect them.
        let mut places = vec![
            (vec!["mount", "--bind", "/dev"], "dev"),
            (vec!["mount", "-t", "proc", "proc"], "proc"),
            (vec!["mount", "-t", "sysfs", "sysfs"], "sys"),
        ];
        // os-prober, run by update-grub, looks in /run for what udev knows.
        if plan.mode == Mode::Beside {
            places.push((vec!["mount", "--bind", "/run"], "run"));
        }
        for (argv, at) in places {
            let place = target.join(at);
            let place_text = place.to_string_lossy().into_owned();
            let mut argv = argv;
            argv.push(&place_text);
            self.mount(&argv, place.clone())?;
        }

        // The programs that make Windows smaller are the live system's
        // alone. Taken off before the start-up image is made, so that it
        // does not carry them either. Not worth failing an installation:
        // they do nothing unless somebody runs them.
        if self.at("usr/sbin/ntfsresize").is_file() {
            let mut purge = vec!["dpkg", "--purge"];
            purge.extend_from_slice(&LIVE_ONLY_PACKAGES);
            if let Err(why) = self.inside(&purge) {
                self.warn(format!(
                    "The packages that only the live system needs were left on the disk ({why})."
                ));
            }
        }

        // The new account takes the live user's place: the same number,
        // the same groups, which are the ones the desktop needs.
        let groups = edit::groups_of(&self.read("etc/group")?, LIVE_USER);
        if names::taken(LIVE_USER, &self.read("etc/passwd")?, "") {
            self.inside(&["userdel", LIVE_USER])?;
        }
        let home = self.at("home").join(LIVE_USER);
        if home.symlink_metadata().is_ok() {
            self.sys.log("remove /home/live");
            fs::remove_dir_all(&home)
                .map_err(|error| format!("could not remove the live user's files: {error}"))?;
        }
        self.remove("etc/sudoers.d/live")?;

        let groups = groups.join(",");
        let mut useradd = vec![
            "useradd",
            "--create-home",
            "--uid",
            "1000",
            "--user-group",
            "--shell",
            "/bin/bash",
            "--comment",
            &plan.full_name,
        ];
        if !groups.is_empty() {
            useradd.extend_from_slice(&["--groups", &groups]);
        }
        useradd.push(&plan.user);
        self.inside(&useradd)?;

        // On chpasswd's standard input, as `name:password`. Not in its
        // arguments or its environment, where other programs could read
        // it, and not in the log.
        let mut line = Secret::with_capacity(plan.user.len() + self.password.len() + 2);
        let fits = line.push(&plan.user)
            && line.push(":")
            && line.push(self.password.expose())
            && line.push("\n");
        if !fits {
            return Err("the password could not be handed over".to_string());
        }
        let chpasswd = Command {
            argv: vec![
                "chroot".to_string(),
                target.to_string_lossy().into_owned(),
                "chpasswd".to_string(),
            ],
            input: Input::Secret(&line),
            cleanup: false,
            whole: false,
        };
        self.run_command(&chpasswd)?;
        self.inside(&["passwd", "--lock", "root"])?;

        // Looked at, not assumed: who can get in and how.
        let passwd = self.read("etc/passwd")?;
        let shadow = self.read("etc/shadow")?;
        if names::taken(LIVE_USER, &passwd, "") {
            return Err("the live user is still there".to_string());
        }
        if self.at("etc/sudoers.d/live").symlink_metadata().is_ok() {
            return Err("the live user's sudo rule is still there".to_string());
        }
        match edit::shadow_field(&shadow, &plan.user) {
            Some(field) if !edit::is_locked(field) => {}
            _ => return Err(format!("the account {} has no password", plan.user)),
        }
        match edit::shadow_field(&shadow, "root") {
            Some(field) if edit::is_locked(field) => {}
            _ => return Err("root is not locked".to_string()),
        }
        Ok(())
    }

    /// An initramfs that mounts a disk, in place of the live image's,
    /// which looks for a medium.
    fn boot_image(&mut self) -> Result<(), String> {
        let kernel = self.kernel.clone();
        self.inside(&["update-initramfs", "-c", "-k", &kernel])?;
        if !self.at(&format!("boot/initrd.img-{kernel}")).is_file() {
            return Err("update-initramfs left no start-up image".to_string());
        }
        Ok(())
    }

    /// What the menu GRUB wrote must hold for the system to start from it.
    fn check_menu(&self, menu: &str) -> Result<(), String> {
        if !menu.contains(&self.root_uuid) || !menu.contains(&format!("vmlinuz-{}", self.kernel)) {
            return Err("GRUB's menu does not start the system that was installed".to_string());
        }
        if !menu.contains(edit::TEXT_MODE_FLAG) {
            return Err("GRUB's menu has no entry for starting without the desktop".to_string());
        }
        if !menu.contains(edit::LABWC_FLAG) {
            return Err("GRUB's menu has no entry for starting the desktop on labwc".to_string());
        }
        Ok(())
    }

    /// GRUB for UEFI, in a directory of its own on the EFI system
    /// partition that is already there, and an entry for it that comes
    /// first. No BIOS boot sector and no file at the fallback path: those
    /// are somebody else's. The menu then lists what os-prober found.
    fn boot_loader_beside(&mut self) -> Result<(), String> {
        self.made_dir = !self.at("boot/efi/EFI/sengios").exists();
        let result = self.boot_loader_beside_steps();
        if let Err(why) = result {
            // What this installation put on the EFI system partition and
            // in the firmware is taken out again, so that the other
            // system is left as it was. The new partition stays.
            let notes = self.rollback_boot();
            return Err(format!("{why}{notes}"));
        }
        Ok(())
    }

    fn boot_loader_beside_steps(&mut self) -> Result<(), String> {
        self.inside(&[
            "grub-install",
            "--target=x86_64-efi",
            "--efi-directory=/boot/efi",
            "--boot-directory=/boot",
            "--bootloader-id=sengios",
            "--no-nvram",
            "--no-uefi-secure-boot",
        ])?;
        if !self.at("boot/efi/EFI/sengios/grubx64.efi").is_file() {
            return Err("grub-install left no /boot/efi/EFI/sengios/grubx64.efi".to_string());
        }
        // Without the fallback path the firmware's entry is the only way
        // in, so here it is not allowed to fail. Made first, while the EFI
        // system partition is mounted, so that a failure can still take
        // the folder out again.
        self.firmware_entry()
            .map_err(|why| format!("the firmware did not take a boot entry for Sengi OS: {why}"))?;
        // os-prober looks for a mounted partition at the place it is
        // mounted, which is not a place inside this chroot. Unmounted, it
        // mounts the EFI system partition itself, read-only, and finds
        // the other system's boot manager on it. (It mounts with GRUB's own
        // read-only driver, which replays no journal and writes nothing.)
        let efi = self.at("boot/efi").to_string_lossy().into_owned();
        self.run(&["umount", &efi])?;
        // The stick Sengi OS is running from is not another system: its
        // partitions are left out of the search.
        let skipped = self.medium_skips();
        let mut grub = edit::grub_default_beside();
        if !skipped.is_empty() {
            grub = edit::with_skip_list(&grub, &skipped);
        }
        self.write("etc/default/grub", &grub)?;
        self.inside(&["update-grub"])?;
        let mut menu = self.read("boot/grub/grub.cfg")?;
        if edit::menu_has_other_systems(&menu) {
            // Somebody has to be able to choose: the menu is shown.
            self.write("etc/default/grub", &edit::grub_with_menu(&grub))?;
            self.inside(&["update-grub"])?;
            menu = self.read("boot/grub/grub.cfg")?;
        } else {
            self.warn(
                "No other system was found to add to the boot menu. If there is one, choose it from the computer's own boot menu."
                    .to_string(),
            );
        }
        self.check_menu(&menu)
    }

    /// Take out again what the boot loader step put on the EFI system
    /// partition and in the firmware: the firmware entry it made and the
    /// folder, if the folder was not there before. Says what was done.
    fn rollback_boot(&mut self) -> String {
        let mut said: Vec<String> = Vec::new();
        match self.new_bootnum.clone() {
            Some(number) => {
                let gone = self.run(&[
                    "efibootmgr",
                    "--quiet",
                    "--bootnum",
                    &number,
                    "--delete-bootnum",
                ]);
                match gone {
                    Ok(_) => said.push(
                        "the boot entry it made was removed from the firmware".to_string(),
                    ),
                    Err(why) => {
                        self.sys
                            .log(&format!("could not remove boot entry {number}: {why}"));
                        said.push(format!(
                            "the boot entry it made (Boot{number}) could not be removed and is still in the firmware's list"
                        ));
                    }
                }
            }
            None if self.create_ran => said.push(
                "a Sengi OS boot entry may have been made and is still in the firmware's list: it could not be found again"
                    .to_string(),
            ),
            None => {}
        }
        if self.replaced > 0 && self.new_bootnum.is_none() {
            said.push(format!(
                "{} earlier Sengi OS boot entr{} on this disk had been removed to be replaced, and was not put back",
                self.replaced,
                if self.replaced == 1 { "y" } else { "ies" }
            ));
        }
        let efi = self.at("boot/efi");
        let folder = efi.join("EFI/sengios");
        if self.made_dir {
            if !efi.join("EFI").is_dir() {
                let esp = self.part(self.esp_number);
                let place = efi.to_string_lossy().into_owned();
                let _ = fs::create_dir_all(&efi);
                let _ = self.run(&["mount", "-t", "vfat", "-o", "umask=0077", &esp, &place]);
            }
            if folder.is_dir() {
                match fs::remove_dir_all(&folder) {
                    Ok(()) => said.push(
                        "its folder EFI/sengios was removed from the EFI system partition"
                            .to_string(),
                    ),
                    Err(error) => {
                        self.sys
                            .log(&format!("could not remove {}: {error}", folder.display()));
                        said.push(
                            "its folder EFI/sengios could not be removed and is still on the EFI system partition"
                                .to_string(),
                        );
                    }
                }
            }
        } else if folder.is_dir() {
            said.push(
                "the folder EFI/sengios was there before and was left, with what the boot loader put in it"
                    .to_string(),
            );
        }
        if said.is_empty() {
            String::new()
        } else {
            format!(
                ". What the installer had put on the EFI system partition and in the firmware: {}",
                said.join("; ")
            )
        }
    }

    /// `identifier@device` for each file system, as GRUB reads them, of what is on the
    /// disk the live system runs from. What cannot be read is left out.
    fn medium_skips(&mut self) -> Vec<String> {
        let disks = self.sys.disks(self.plan.allow_loop);
        let mut nodes = Vec::new();
        for disk in disks.iter().filter(|disk| disk.medium) {
            if !disk.fstype.is_empty() {
                nodes.push(disk.path.clone());
            }
            nodes.extend(disk.parts.iter().map(|part| format!("/dev/{}", part.name)));
        }
        let mut found: Vec<String> = Vec::new();
        for node in nodes {
            let probed = self.inside(&["grub-probe", "--target=fs_uuid", "--device", &node]);
            if let Ok(uuid) = probed {
                let uuid = uuid.trim().to_string();
                let plain =
                    !uuid.is_empty() && uuid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
                // GRUB matches the identifier and the device together.
                let entry = format!("{uuid}@{node}");
                if plain && !found.contains(&entry) {
                    found.push(entry);
                }
            }
        }
        found
    }

    fn boot_loader(&mut self) -> Result<(), String> {
        if self.plan.mode == Mode::Beside {
            return self.boot_loader_beside();
        }
        let device = self.plan.device.clone();
        // For firmware that starts the old way: into the first sectors
        // and the BIOS boot partition.
        self.inside(&[
            "grub-install",
            "--target=i386-pc",
            "--boot-directory=/boot",
            &device,
        ])?;
        // For UEFI, twice: under our own name, and at the path firmware
        // falls back to when it has no entry for the disk or forgets it.
        let efi = [
            "grub-install",
            "--target=x86_64-efi",
            "--efi-directory=/boot/efi",
            "--boot-directory=/boot",
            "--bootloader-id=sengios",
            "--no-nvram",
            "--no-uefi-secure-boot",
        ];
        self.inside(&efi)?;
        let mut removable = efi.to_vec();
        removable.push("--removable");
        self.inside(&removable)?;
        self.inside(&["update-grub"])?;

        let menu = self.read("boot/grub/grub.cfg")?;
        self.check_menu(&menu)?;
        for file in [
            "boot/grub/i386-pc/core.img",
            "boot/efi/EFI/BOOT/BOOTX64.EFI",
            "boot/efi/EFI/sengios/grubx64.efi",
        ] {
            if !self.at(file).is_file() {
                return Err(format!("grub-install left no /{file}"));
            }
        }
        if self.sys.uefi() && self.sys.secure_boot() {
            self.warn(
                "Secure Boot is on, and the boot loader is not signed: the computer may not start Sengi OS until Secure Boot is turned off in the firmware settings."
                    .to_string(),
            );
        }
        if self.sys.uefi() {
            if let Err(why) = self.firmware_entry() {
                self.warn(format!(
                    "The firmware did not take a boot entry ({why}). The disk still starts from its fallback path."
                ));
            }
        }
        Ok(())
    }

    /// Tell the firmware about the new system, replacing what an earlier
    /// installation told it. The disk starts without this, from the
    /// fallback path, so a firmware that refuses is not a failure.
    fn firmware_entry(&mut self) -> Result<(), String> {
        let beside = self.plan.mode == Mode::Beside;
        let listing = if beside {
            self.run(&["efibootmgr", "-v"])?
        } else {
            self.run(&["efibootmgr"])?
        };
        let had = edit::boot_entries(&listing, edit::EFI_LABEL);
        if beside {
            // Only what starts our own loader from this EFI system
            // partition is replaced; an entry of the same name that
            // belongs to a Sengi OS on another disk is left alone.
            let esp_id = self
                .before
                .as_ref()
                .and_then(|before| before.entry(self.esp_number))
                .map(|entry| entry.uuid.clone())
                .unwrap_or_default();
            let loader = format!("\\EFI\\{}\\grubx64.efi", edit::EFI_ID);
            self.deletable = edit::boot_entries_at(&listing, edit::EFI_LABEL, &esp_id, &loader);
            self.replaced = self.deletable.len();
            self.boot_order = edit::boot_order(&listing);
            for entry in self.deletable.clone() {
                self.run(&[
                    "efibootmgr",
                    "--quiet",
                    "--bootnum",
                    &entry,
                    "--delete-bootnum",
                ])?;
            }
        } else {
            for entry in &had {
                self.run(&[
                    "efibootmgr",
                    "--quiet",
                    "--bootnum",
                    entry,
                    "--delete-bootnum",
                ])?;
            }
        }
        let device = self.plan.device.clone();
        let part = self.esp_number.to_string();
        let loader = format!("\\EFI\\{}\\grubx64.efi", edit::EFI_ID);
        self.run(&[
            "efibootmgr",
            "--quiet",
            "--create",
            "--disk",
            &device,
            "--part",
            &part,
            "--label",
            edit::EFI_LABEL,
            "--loader",
            &loader,
        ])?;
        self.create_ran = true;
        if beside {
            self.entry_first(&had)?;
        }
        Ok(())
    }

    /// The new entry is tried first, and the ones that were there stay,
    /// after it, in the order they were in: Windows still starts.
    fn entry_first(&mut self, had: &[String]) -> Result<(), String> {
        let listing = self.run(&["efibootmgr"])?;
        // The entry that was just made is the one that was not there.
        let made: Vec<String> = edit::boot_entries(&listing, edit::EFI_LABEL)
            .into_iter()
            .filter(|entry| !had.contains(entry) || self.deletable.contains(entry))
            .collect();
        let Some(first) = made.last().cloned() else {
            return Err("the firmware does not list the new entry".to_string());
        };
        self.new_bootnum = Some(first.clone());
        self.deletable.push(first.clone());
        let order = edit::boot_order(&listing);
        self.boot_order = order.clone();
        if order.first() == Some(&first) {
            return Ok(());
        }
        let mut new_order = vec![first.clone()];
        new_order.extend(order.into_iter().filter(|entry| *entry != first));
        let joined = new_order.join(",");
        self.run(&["efibootmgr", "--quiet", "--bootorder", &joined])?;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        let target = self.paths.target.to_string_lossy().into_owned();
        // The disk is most likely an SSD; tell it weekly what is free.
        let root = format!("--root={target}");
        self.run(&["systemctl", &root, "--quiet", "enable", "fstrim.timer"])?;
        self.sys.log("copy this log to /var/log/sengi-install.log");
        if let Err(error) = fs::create_dir_all(self.at("var/log"))
            .and_then(|()| fs::copy(&self.paths.log, self.at("var/log/sengi-install.log")))
        {
            self.warn(format!("The log could not be copied to the disk: {error}"));
        }
        self.run(&["sync"])?;
        let left = self.unmount_all();
        if left.is_empty() {
            Ok(())
        } else {
            Err(format!("{} could not be unmounted", left[0].display()))
        }
    }

    /// Unmount everything at or under the target, deepest first, whether
    /// this run mounted it or an earlier one did. Gives back what is
    /// still mounted afterwards. Never an error: this is also what runs
    /// when something else has already failed.
    fn unmount_all(&mut self) -> Vec<PathBuf> {
        for attempt in 0..2 {
            let mounted = self.sys.mounts_under(&self.paths.target);
            if mounted.is_empty() {
                break;
            }
            for place in mounted {
                let place = place.to_string_lossy().into_owned();
                // The second time round, let go of it even if something
                // still has a file open there.
                let argv: &[&str] = if attempt == 0 {
                    &["umount", &place]
                } else {
                    &["umount", "--lazy", &place]
                };
                // Without `run`'s check: a failure is looked for below.
                let command = Command {
                    cleanup: true,
                    ..Command::new(argv)
                };
                if let Err(why) = self.sys.run(&command, &mut |_| {}) {
                    self.sys.log(&format!("umount {place}: {why}"));
                }
            }
        }
        self.mounts.clear();
        self.sys.mounts_under(&self.paths.target)
    }
}

fn reading_table(device: &str) -> Command<'static> {
    Command::new(&["sfdisk", "-d", device])
}

/// What `sfdisk -d` said, as a table. Anything it says besides the table
/// is a warning about the table (a backup copy not at the end of a disk
/// that was made larger, a mismatched protective MBR): not a table to
/// write to.
fn table_from(command: &Command, output: Output, device: &str) -> Result<Table, String> {
    let warning = output
        .stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string());
    let dump = Installer::judge(command, output)?;
    if let Some(warning) = warning {
        return Err(format!(
            "sfdisk reports a problem with the partition table of {device}: {warning}"
        ));
    }
    Table::parse_dump(&dump)
}

/// Look at a disk and say whether, and how far, its Windows partition
/// could be made smaller. Nothing is written: this is what the window
/// asks before it offers the choice.
pub fn analyze(
    sys: &mut dyn System,
    device: &str,
    allow_loop: bool,
) -> Result<Result<Analysis, Refusal>, String> {
    let disks = sys.disks(allow_loop);
    let disk = disks
        .iter()
        .find(|disk| disk.path == device)
        .ok_or(format!("there is no disk called {device}"))?;
    match disks::refusal(disk) {
        None => {}
        Some(disks::Refusal::Mounted(_) | disks::Refusal::Swap | disks::Refusal::Held) => {
            return Ok(Err(Refusal::Busy))
        }
        Some(refusal) => return Err(format!("{device}: {}", refusal.words())),
    }
    let firmware = beside::Firmware {
        uefi: sys.uefi(),
        secure_boot: sys.secure_boot(),
    };
    let number = beside::squeeze(disk, firmware)
        .map(|part| part.number)
        .ok_or("there is nothing on this disk that would be made smaller")?;
    // Said at once, before anything that takes long.
    if let Err(refusal) = sys.power() {
        return Ok(Err(refusal));
    }
    let command = reading_table(device);
    let output = sys.run(&command, &mut |_| {})?;
    let table = table_from(&command, output, device)?;
    let sector0 = sys.read_sector0(device)?;
    beside::mbr_is_plain(&sector0)?;
    beside::mbr_stays(&sector0)?;
    let esp = table
        .entries
        .iter()
        .find(|entry| entry.kind == beside::ESP_TYPE)
        .map(|entry| entry.number)
        .ok_or("there is no EFI system partition on this disk")?;
    let entry = table
        .entry(number)
        .cloned()
        .ok_or("the partition table is not the one the system shows")?;
    let node = partition_path(device, number);
    let guard = Guard {
        device: device.to_string(),
        old: beside::nodes(device, &table),
        esp: partition_path(device, esp),
        esp_number: esp,
        new: None,
        new_number: None,
        deletable: Vec::new(),
        boot_order: Vec::new(),
        windows: Some(Windows {
            node,
            number,
            bytes: 0,
            writing: Writing::Nothing,
        }),
    };
    let allow = |argv: &[String]| guard.judge(argv);
    match shrink::examine(sys, &allow, device, &table, &entry, false) {
        Ok(examined) => Ok(Ok(Analysis {
            table: table.snapshot(),
            esp,
            number,
            start: entry.start,
            sectors: entry.sectors,
            used_bytes: examined.info.used_bytes,
            room: examined.room,
            windows: examined.windows,
        })),
        Err(Stop::Refused(refusal)) => Ok(Err(refusal)),
        Err(Stop::Failed(why)) => Err(why),
    }
}

/// Install as `plan` says. Every step is reported as it begins and ends;
/// the first that fails ends the installation and is the error. Gives
/// back how many seconds it took.
pub fn install(
    plan: &Plan,
    password: &Secret,
    sys: &mut dyn System,
    paths: &Paths,
    report: &mut dyn FnMut(&Event),
) -> Result<u64, Failure> {
    let started = Instant::now();
    let mut installer = Installer {
        plan,
        password,
        sys,
        paths,
        report,
        mounts: Vec::new(),
        kernel: String::new(),
        source_bytes: 0,
        root_uuid: String::new(),
        efi_uuid: String::new(),
        esp_number: edit::EFI_PART,
        root_number: edit::ROOT_PART,
        before: None,
        made: false,
        new_uuid: String::new(),
        after: None,
        sector0: None,
        listing: String::new(),
        made_dir: false,
        new_bootnum: None,
        create_ran: false,
        replaced: 0,
        boot_order: Vec::new(),
        deletable: Vec::new(),
        beside: plan.beside.clone(),
        windows: None,
    };
    installer.tell(Event::Log(paths.log.to_string_lossy().into_owned()));
    installer.tell(Event::Disk(plan.device.clone()));
    if plan.mode == Mode::Beside {
        installer.tell(Event::Beside);
    }
    let shrink = plan.beside.as_ref().and_then(|beside| beside.shrink);
    if let Some(shrink) = &shrink {
        installer.tell(Event::Shrinks(shrink.partition));
    }
    for step in steps(shrink.is_some()) {
        installer.tell(Event::Begin(step.id.to_string()));
        let result = match step.id {
            "check" => installer.check(),
            "shrink" => installer.shrink(),
            "partition" => installer.partition(),
            "format" => installer.format(),
            "copy" => installer.copy(),
            "configure" => installer.configure(),
            "account" => installer.account(),
            "boot-image" => installer.boot_image(),
            "boot-loader" => installer.boot_loader(),
            "finish" => installer.finish(),
            _ => Err("there is no such step".to_string()),
        };
        if let Err(why) = result {
            installer.sys.release();
            let left = installer.unmount_all();
            for place in left {
                installer
                    .sys
                    .log(&format!("still mounted: {}", place.display()));
            }
            installer.tell(Event::Failed(step.id.to_string(), why.clone()));
            return Err((step.id, why));
        }
        installer.tell(Event::End(step.id.to_string()));
    }
    let seconds = started.elapsed().as_secs();
    installer.tell(Event::Done(seconds));
    Ok(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disks::tests::add_disk;
    use crate::disks::{scan, Sources};
    use crate::testutil::TempDir;

    const KERNEL: &str = "6.12.48+deb13-amd64";
    const GB: u64 = 1_000_000_000;

    /// A Windows partition as the fake machine has it, for making it
    /// smaller. Statuses apply from the command named onwards.
    #[derive(Default)]
    struct FakeWindows {
        /// Its first sector as it is now.
        first: Vec<u8>,
        info: String,
        /// `ntfsresize --info` says this from its second run on.
        info_later: Option<String>,
        probe: i32,
        /// What the probe says once the volume has been resized.
        probe_after: i32,
        dry_run: Option<&'static str>,
        /// The dry run ends well and does not say that it did.
        dry_run_silent: bool,
        resize: i32,
        /// The resize says it worked and left the volume as it was, or
        /// left no copy of the first sector at the new end.
        resize_does_nothing: bool,
        no_copy: bool,
        /// The start of hiberfil.sys, where there is one.
        hiberfile: Option<&'static [u8]>,
        resized: bool,
        /// The charger is pulled once a command with this in it has run.
        unplugged_after: Option<&'static str>,
        twin: bool,
        /// Once so many commands with this in them have run: a copy of the
        /// disk is plugged in, the held disk is gone from its name, or the
        /// node holds another partition.
        twin_after: Option<(&'static str, usize)>,
        gone_after: Option<(&'static str, usize)>,
        moved_after: Option<(&'static str, usize)>,
        /// The disk has another serial number once this has run.
        swapped_after: Option<&'static str>,
        /// The volume holds data only: no Windows is installed on it.
        data: bool,
        /// Other partitions: their first sectors, and what reading the
        /// hibernation file gives where it has something to say.
        others: Vec<(&'static str, Vec<u8>)>,
        other_hiberfile: Vec<(&'static str, &'static [u8], &'static str)>,
        /// sfdisk moves another partition as it writes the new size.
        tamper: bool,
        /// The kernel does not take the new size.
        stale: bool,
        /// Once a command with this in it has run, the table reads as this.
        table_later: Option<(&'static str, String)>,
    }

    /// A machine that runs nothing and remembers what it was asked.
    struct Fake {
        windows: FakeWindows,
        /// The commands that were to run to their end.
        whole: Vec<String>,
        holds: Vec<String>,
        target: PathBuf,
        disks: Vec<Disk>,
        uefi: bool,
        /// A command with this in it ends with status 1.
        fails: Option<&'static str>,
        missing: Option<&'static str>,
        commands: Vec<String>,
        /// What was written to programs: the command, and the text, or
        /// `None` where it was a secret.
        inputs: Vec<(String, Option<String>)>,
        secrets: Vec<String>,
        mounted: Vec<PathBuf>,
        log: Vec<String>,
        firmware: &'static str,
        /// update-grub writes a menu for a disk that is not this one.
        wrong_menu: bool,
        /// A signal arrives while a command with this in it runs.
        /// Something else has the disk open.
        busy: bool,
        under_test: bool,
        /// Each command, and whether the disk was held while it ran.
        held_during: Vec<(String, bool)>,
        held: bool,
        /// The kernel did not take the new partition table.
        stale_table: bool,
        /// Beside another system: what `sfdisk -d` prints, and what the
        /// kernel knows of it. `partx --add` makes the second the first.
        dump: Option<String>,
        kernel_dump: Option<String>,
        /// sfdisk changes an old partition as it writes the new one.
        tamper: bool,
        /// os-prober finds another system.
        other_system: bool,
        /// What efibootmgr lists once an entry has been created.
        created: &'static str,
        /// The EFI system partition is not a FAT volume.
        esp_type: &'static str,
        secure_boot: bool,
        /// The firmware's list cannot be read once an entry was made.
        relist_fails: bool,
        /// The disk has another serial number once the kernel knows the
        /// new partition.
        disk_changes: bool,
        sector0: Vec<u8>,
        /// Sector 0 is not what it was after the new partition is written.
        /// A byte of sector 0 that is not as it should be once the table
        /// has been written.
        tamper_mbr: Option<(usize, u8)>,
        appended: bool,
        /// The new partition's node holds another partition's identifier.
        wrong_partuuid: bool,
        /// What `sfdisk -d` says besides the table.
        dump_warning: &'static str,
        /// The partitions the disk is said to have changed under the
        /// installation: the table read just before the format.
        late_dump: Option<String>,
        stops_at: Option<&'static str>,
        stopped: bool,
        /// What was asked for after the signal and not run.
        refused: Vec<String>,
    }

    impl Fake {
        fn append(&self, path: &str, line: &str) {
            let path = self.target.join(path);
            let mut text = fs::read_to_string(&path).unwrap_or_default();
            text.push_str(line);
            fs::write(path, text).unwrap();
        }

        fn touch(&self, path: &str, contents: &str) {
            let path = self.target.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn ran(&self, start: &str) -> bool {
            self.commands
                .iter()
                .any(|command| command.starts_with(start))
        }

        /// Whether `after.1` commands with `after.0` in them have run.
        fn happened(&self, after: Option<(&str, usize)>) -> bool {
            after.is_some_and(|(what, times)| {
                self.commands.iter().filter(|c| c.contains(what)).count() >= times
            })
        }

        fn position(&self, start: &str) -> usize {
            self.commands
                .iter()
                .position(|command| command.starts_with(start))
                .unwrap_or_else(|| panic!("{start} was not run; ran: {:#?}", self.commands))
        }
    }

    impl System for Fake {
        fn run(&mut self, command: &Command, tick: &mut dyn FnMut(&str)) -> Result<Output, String> {
            let line = command.argv.join(" ");
            self.commands.push(line.clone());
            if command.whole {
                self.whole.push(line.clone());
            }
            match &command.input {
                Input::None => {}
                Input::Text(text) => self.inputs.push((line.clone(), Some(text.to_string()))),
                Input::Secret(secret) => {
                    self.inputs.push((line.clone(), None));
                    self.secrets.push(secret.expose().to_string());
                }
            }
            tick(
                "Relocating needed data ...\n  4.00 percent completed\r 42.50 percent completed\r",
            );
            self.held_during.push((line.clone(), self.held));
            let was_stopped = self.stopped;
            if self.stops_at.is_some_and(|what| line.contains(what)) {
                self.stopped = true;
            }
            if self.stopped && !command.cleanup && !(command.whole && !was_stopped) {
                // As the real machine does: nothing more is run.
                if !self.stops_at.is_some_and(|what| line.contains(what)) {
                    self.refused.push(line.clone());
                    self.commands.pop();
                }
                return Err("the installation was told to stop".to_string());
            }
            if self.fails.is_some_and(|what| line.contains(what)) {
                return Ok(Output {
                    code: Some(1),
                    stdout: String::new(),
                    stderr: "warming up\nit broke\n\n".to_string(),
                });
            }
            let inner: Vec<&str> = match command.argv.first().map(String::as_str) {
                Some("chroot") => command.argv[2..].iter().map(String::as_str).collect(),
                _ => command.argv.iter().map(String::as_str).collect(),
            };
            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut code = Some(0);
            match inner[0] {
                "du" => stdout = "1500000\t/source\n".to_string(),
                "sfdisk" if inner.contains(&"-d") => {
                    stdout = self.dump.clone().unwrap_or_default();
                    if self.commands.iter().any(|c| c.starts_with("partx")) {
                        if let Some(late) = &self.late_dump {
                            stdout = late.clone();
                        }
                    }
                    stderr = self.dump_warning.to_string();
                    if let Some((after, later)) = &self.windows.table_later {
                        if self.commands.iter().any(|c| c.contains(after)) {
                            stdout = later.clone();
                        }
                    }
                }
                "sfdisk" if inner.contains(&"--append") => {
                    let Input::Text(text) = &command.input else {
                        panic!("sfdisk was given no script");
                    };
                    let dump = self.dump.clone().unwrap();
                    let table = crate::beside::Table::parse_dump(&dump).unwrap();
                    let number = (1..).find(|n| table.entry(*n).is_none()).unwrap();
                    let line = format!("/dev/sda{number} : {}\n", text.trim_end());
                    self.appended = true;
                    let mut dump = format!("{dump}{line}");
                    if self.tamper {
                        dump = dump.replacen("size=      204800", "size=      204799", 1);
                    }
                    self.dump = Some(dump);
                }
                "grub-probe" => stdout = "0000000000000000\n".to_string(),
                "ntfs-3g.probe" => {
                    code = Some(if self.windows.resized {
                        self.windows.probe_after
                    } else {
                        self.windows.probe
                    });
                }
                "ntfsresize" if inner.contains(&"--info") => {
                    let looks = self
                        .commands
                        .iter()
                        .filter(|c| c.contains("--info"))
                        .count();
                    stdout = match &self.windows.info_later {
                        Some(later) if looks > 1 => later.clone(),
                        _ => self.windows.info.clone(),
                    };
                    if stdout.starts_with("ERROR") {
                        code = Some(1);
                    }
                }
                "ntfsresize" if inner.contains(&"--no-action") => match self.windows.dry_run {
                    Some(why) => {
                        code = Some(1);
                        stderr = why.to_string();
                    }
                    None if self.windows.dry_run_silent => {
                        stdout = "Resizing nothing.\n".to_string()
                    }
                    None => stdout = "The read-only test run ended successfully.\n".to_string(),
                },
                "ntfsresize" => {
                    code = Some(self.windows.resize);
                    if self.windows.resize != 0 {
                        stderr = "ERROR: Write failed\n".to_string();
                    } else if !self.windows.resize_does_nothing {
                        // As ntfsresize does it: whole clusters, short of
                        // the size by room for the copy.
                        let bytes: u64 = inner[2].parse().unwrap();
                        let sectors = (bytes - 512) / 4096 * 8;
                        self.windows.first = crate::shrink::tests::ntfs_sector(sectors);
                        self.windows.resized = true;
                    }
                }
                "sfdisk" if inner.contains(&"-N") => {
                    let Input::Text(text) = &command.input else {
                        panic!("sfdisk was given no size");
                    };
                    let size = text.trim().strip_prefix("size=").unwrap();
                    let number = inner[inner.len() - 2];
                    let dump: String = self
                        .dump
                        .as_ref()
                        .unwrap()
                        .lines()
                        .map(|line| {
                            if line.starts_with(&format!("/dev/sda{number} :")) {
                                let (head, rest) = line.split_once("size=").unwrap();
                                let (_, tail) = rest.split_once(',').unwrap();
                                format!("{head}size={size},{tail}\n")
                            } else if self.windows.tamper && line.starts_with("/dev/sda1 :") {
                                format!("{}\n", line.replace("start=        2048", "start=4096"))
                            } else {
                                format!("{line}\n")
                            }
                        })
                        .collect();
                    self.dump = Some(dump);
                    self.appended = true;
                }
                "partx" if inner.contains(&"--update") => {
                    if !self.windows.stale {
                        self.kernel_dump = self.dump.clone();
                    }
                }
                "partx" if !self.stale_table => self.kernel_dump = self.dump.clone(),
                "blkid" if inner.contains(&"PARTUUID") => {
                    let node = inner.last().unwrap();
                    let number: u32 = node
                        .chars()
                        .rev()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                        .parse()
                        .unwrap();
                    let table =
                        crate::beside::Table::parse_dump(self.dump.as_ref().unwrap()).unwrap();
                    let uuid = table.entry(number).map(|entry| entry.uuid.clone());
                    stdout = if (self.wrong_partuuid && number >= 3)
                        || (number == 2 && self.happened(self.windows.moved_after))
                    {
                        "ffffffff-ffff-4fff-8fff-ffffffffffff\n".to_string()
                    } else {
                        format!("{}\n", uuid.unwrap_or_default())
                    };
                }
                "blkid" if inner.contains(&"TYPE") => {
                    stdout = format!("{}\n", self.esp_type);
                }
                "blkid" => {
                    let node = inner.last().unwrap();
                    stdout = if node.ends_with('2') || node.ends_with('1') {
                        "ABCD-1234\n"
                    } else {
                        "0a1b2c3d-e5f6\n"
                    }
                    .to_string();
                }
                "mount" => self.mounted.push(PathBuf::from(inner.last().unwrap())),
                "umount" => {
                    let place = PathBuf::from(inner.last().unwrap());
                    self.mounted.retain(|mounted| *mounted != place);
                }
                "userdel" => {
                    let passwd = fs::read_to_string(self.target.join("etc/passwd")).unwrap();
                    let kept: String = passwd
                        .lines()
                        .filter(|line| !line.starts_with("live:"))
                        .map(|line| format!("{line}\n"))
                        .collect();
                    fs::write(self.target.join("etc/passwd"), kept).unwrap();
                }
                "useradd" => {
                    let name = inner.last().unwrap();
                    self.append(
                        "etc/passwd",
                        &format!("{name}:x:1000:1000::/home/{name}:/bin/bash\n"),
                    );
                    self.append("etc/shadow", &format!("{name}:!:20000:0:99999:7:::\n"));
                }
                "chpasswd" => {
                    let shadow = fs::read_to_string(self.target.join("etc/shadow")).unwrap();
                    fs::write(
                        self.target.join("etc/shadow"),
                        shadow.replace("allan:!:", "allan:$y$j9T$made-up:"),
                    )
                    .unwrap();
                }
                "update-initramfs" => self.touch(&format!("boot/initrd.img-{KERNEL}"), "initrd"),
                "grub-install" if inner.contains(&"--target=i386-pc") => {
                    self.touch("boot/grub/i386-pc/core.img", "core");
                }
                "grub-install" if inner.contains(&"--removable") => {
                    self.touch("boot/efi/EFI/BOOT/BOOTX64.EFI", "efi");
                }
                "grub-install" => self.touch("boot/efi/EFI/sengios/grubx64.efi", "efi"),
                "update-grub" => {
                    let root = if self.wrong_menu {
                        "other"
                    } else {
                        "0a1b2c3d-e5f6"
                    };
                    let others = if self.other_system {
                        "### BEGIN /etc/grub.d/30_os-prober ###\nmenuentry 'Windows Boot Manager (on /dev/sda1)' {\n}\n### END /etc/grub.d/30_os-prober ###\n"
                    } else {
                        ""
                    };
                    self.touch(
                        "boot/grub/grub.cfg",
                        &format!("{others}linux /boot/vmlinuz-{KERNEL} root=UUID={root} ro quiet\nlinux /boot/vmlinuz-{KERNEL} root=UUID={root} ro quiet sengi.compositor=labwc\nlinux /boot/vmlinuz-{KERNEL} root=UUID={root} ro sengi.text systemd.unit=multi-user.target\n"),
                    );
                }
                "efibootmgr" if inner.contains(&"--create") => self.firmware = self.created,
                "efibootmgr" if inner.len() == 1 || inner.contains(&"-v") => {
                    stdout = self.firmware.to_string();
                    if self.relist_fails
                        && self
                            .commands
                            .iter()
                            .any(|c| c.starts_with("efibootmgr") && c.contains("--create"))
                    {
                        code = Some(1);
                    }
                }
                _ => {}
            }
            Ok(Output {
                code,
                stdout,
                stderr,
            })
        }

        fn read_sector(&mut self, device: &str, at: u64) -> Result<Vec<u8>, String> {
            assert_eq!(device, WINDOWS_NODE);
            if at > 0 && self.windows.no_copy {
                return Ok(vec![0; 512]);
            }
            Ok(self.windows.first.clone())
        }

        fn head(&mut self, command: &Command, bytes: usize) -> Result<Head, String> {
            let line = command.argv.join(" ");
            self.commands.push(line);
            assert_eq!(command.argv[0], "ntfscat");
            let (node, file) = (command.argv[1].as_str(), command.argv[2].as_str());
            let missing = Head {
                code: Some(1),
                bytes: Vec::new(),
                said: "ERROR: Couldn't open inode: No such file or directory\n".to_string(),
            };
            let found = |start: &[u8]| {
                let mut head = vec![0; bytes];
                head[..start.len()].copy_from_slice(start);
                Head {
                    code: None,
                    bytes: head,
                    said: String::new(),
                }
            };
            if file != "hiberfil.sys" {
                let kernel = !self.windows.data && file == shrink::WINDOWS_KERNELS[0];
                return Ok(if kernel { found(b"MZ") } else { missing });
            }
            if node == WINDOWS_NODE {
                return Ok(self.windows.hiberfile.map_or(missing, found));
            }
            let other = self
                .windows
                .other_hiberfile
                .iter()
                .find(|(one, _, _)| *one == node);
            Ok(match other {
                None => missing,
                Some((_, start, "")) => found(start),
                Some((_, _, said)) => Head {
                    code: Some(1),
                    bytes: Vec::new(),
                    said: said.to_string(),
                },
            })
        }

        fn disks(&mut self, _allow_loop: bool) -> Vec<Disk> {
            let mut disks = self.disks.clone();
            if self.disk_changes && self.commands.iter().any(|c| c.starts_with("partx")) {
                disks[0].serial = "another disk".to_string();
            }
            if self.windows.twin {
                for number in [2, 7] {
                    let mut part = disks[0].parts[0].clone();
                    part.number = number;
                    part.uuid = "dd068ba6-d1df-4fe6-9721-13fc754e035f".to_string();
                    disks[0].parts.push(part);
                }
            }
            if self.happened(self.windows.twin_after) {
                // A copy of the disk, plugged in.
                let mut copy = disks[0].clone();
                copy.name = "sdb".to_string();
                copy.path = "/dev/sdb".to_string();
                let mut part =
                    disks[0]
                        .parts
                        .first()
                        .cloned()
                        .unwrap_or_else(|| crate::disks::Part {
                            name: String::new(),
                            number: 2,
                            start: 0,
                            size: 0,
                            kind: String::new(),
                            uuid: String::new(),
                            fstype: String::new(),
                            label: String::new(),
                        });
                part.uuid = "DD068BA6-D1DF-4FE6-9721-13FC754E035F".to_string();
                copy.parts = vec![part.clone()];
                disks.push(copy);
                // And the one it is a copy of, as udev lists it.
                disks[0].parts.push(part);
            }
            if let Some(what) = self.windows.swapped_after {
                if self.commands.iter().any(|c| c.contains(what)) {
                    disks[0].serial = "another disk".to_string();
                }
            }
            disks
        }

        fn still_held(&mut self, device: &str) -> Result<(), String> {
            if !self.held || self.happened(self.windows.gone_after) {
                return Err(format!("{device} is no longer the disk that is held"));
            }
            Ok(())
        }

        fn power(&mut self) -> Result<(), Refusal> {
            let unplugged = self
                .windows
                .unplugged_after
                .is_some_and(|what| self.commands.iter().any(|c| c.contains(what)));
            if unplugged {
                Err(Refusal::Battery)
            } else {
                Ok(())
            }
        }

        fn test_hold(&mut self, point: &str) {
            self.holds.push(point.to_string());
        }

        fn has_program(&mut self, name: &str) -> bool {
            self.missing != Some(name)
        }

        fn exists(&mut self, _path: &Path) -> bool {
            true
        }

        fn under_test(&mut self) -> bool {
            self.under_test
        }

        fn uefi(&mut self) -> bool {
            self.uefi
        }

        fn mounts_under(&mut self, path: &Path) -> Vec<PathBuf> {
            let mut under: Vec<PathBuf> = self
                .mounted
                .iter()
                .filter(|place| place.starts_with(path))
                .cloned()
                .collect();
            under.sort();
            under.reverse();
            under
        }

        fn random_id(&mut self) -> Result<String, String> {
            Ok("0123456789abcdef0123456789abcdef".to_string())
        }

        fn pause(&mut self, _time: Duration) {}

        fn hold(&mut self, device: &str) -> Result<(), String> {
            if self.busy {
                return Err(format!("{device} is in use by something else"));
            }
            self.held = true;
            self.log.push(format!("hold {device}"));
            Ok(())
        }

        fn release(&mut self) {
            self.held = false;
        }

        fn partitions(&mut self, _device: &str) -> Vec<(u32, u64, u64)> {
            if let Some(dump) = &self.kernel_dump {
                let table = crate::beside::Table::parse_dump(dump).unwrap();
                return table
                    .entries
                    .iter()
                    .map(|entry| (entry.number, entry.start, entry.sectors))
                    .collect();
            }
            if self.stale_table {
                // What was there before: two partitions, elsewhere.
                return vec![(1, 2048, 195_312_500), (2, 195_314_548, 304_687_500)];
            }
            let sectors = self.disks[0].size / 512;
            vec![
                (1, 2048, 2048),
                (2, 4096, 1_048_576),
                (3, 1_052_672, sectors - 1_052_672 - 2048),
            ]
        }

        fn secure_boot(&mut self) -> bool {
            self.secure_boot
        }

        fn read_sector0(&mut self, device: &str) -> Result<Vec<u8>, String> {
            if device == WINDOWS_NODE {
                return Ok(self.windows.first.clone());
            }
            if device != "/dev/sda" {
                assert!(device.starts_with("/dev/sda"), "{device}");
                let other = self.windows.others.iter().find(|(one, _)| *one == device);
                return Ok(other.map_or_else(|| vec![0; 512], |(_, sector)| sector.clone()));
            }
            let mut sector = self.sector0.clone();
            if self.appended {
                // As sfdisk does when it writes the table: the protective
                // MBR gets the disk's own size.
                let size = crate::beside::mbr_size(self.disks[0].size / 512);
                sector[458..462].copy_from_slice(&size);
                if let Some((at, byte)) = self.tamper_mbr {
                    sector[at] = byte;
                }
            }
            Ok(sector)
        }

        fn log(&mut self, line: &str) {
            self.log.push(line.to_string());
        }
    }

    /// A machine with one empty 256 GB disk, a system to copy, and a
    /// target that already holds what `cp` would have put there.
    struct World {
        _sys: TempDir,
        source: TempDir,
        target: TempDir,
        plan: Plan,
        fake: Fake,
    }

    const PASSWD: &str = "root:x:0:0:root:/root:/bin/bash\n_greetd:x:999:999::/var/lib/greetd:/usr/sbin/nologin\nlive:x:1000:1000:Sengi OS:/home/live:/bin/bash\n";
    const GROUP: &str = "root:x:0:\nsudo:x:27:live\naudio:x:29:live\nvideo:x:44:live\ninput:x:101:live\nrender:x:102:live\nnetdev:x:103:live\nbluetooth:x:104:live\nlive:x:1000:\n";

    fn populate(root: &TempDir) {
        root.write("etc/os-release", "NAME=\"Sengi OS\"\nID=sengios\n");
        root.write("etc/passwd", PASSWD);
        root.write("etc/group", GROUP);
        root.write(
            "etc/shadow",
            "root:!:20000:0:99999:7:::\nlive:$6$x:20000:0:99999:7:::\n",
        );
        root.write("etc/machine-id", "");
        root.write("etc/hostname", "sengios\n");
        root.write("etc/sudoers.d/live", "live ALL=(ALL) NOPASSWD: ALL\n");
        root.write("etc/systemd/system/getty@tty1.service", "");
        root.write(
            "etc/greetd/config.toml",
            "[terminal]\nvt = 7\n\n[default_session]\ncommand = \"/usr/libexec/sengi-greeter-session\"\nuser = \"_greetd\"\n\n# The live image logs straight in.\n[initial_session]\ncommand = \"/usr/bin/sengi-session\"\nuser = \"live\"\n",
        );
        root.write(
            "etc/xdg/sengi-os/dock.toml",
            "pinned = [\"sengi-installer\", \"foot\", \"sengi-settings\"]\n",
        );
        root.write(
            "usr/share/sengi-os/greeter/environment",
            "XCURSOR_SIZE=24\n",
        );
        root.write("usr/share/zoneinfo/Europe/Paris", "TZif");
        root.write("usr/share/zoneinfo/UTC", "TZif");
        root.mkdir(&format!("lib/modules/{KERNEL}"));
        root.write("home/live/.profile", "");
        root.write("usr/bin/os-prober", "");
        for file in TARGET_FILES
            .iter()
            .chain(LIVE_ONLY.iter())
            .chain(INSTALL_TEST.iter())
        {
            root.write(file, "");
        }
        std::os::unix::fs::symlink("/usr/share/zoneinfo/UTC", root.join("etc/localtime")).unwrap();
    }

    fn world() -> World {
        let sys = TempDir::new();
        add_disk(&sys, "sda", "8", 256 * GB, &[100 * GB, 156 * GB]);
        sys.write("block/sda/device/model", "SanDisk SD8SN8U\n");
        add_disk(&sys, "sdb", "8", 32 * GB, &[]);
        let mounts = "31 24 8:0 / /run/sengi/medium ro - iso9660 /dev/sdb ro\n";
        // sdb is 8:0 in this made-up /sys, as sda is; give it its own.
        sys.write("block/sda/dev", "8:16\n");
        let disks = scan(
            &Sources {
                sys: sys.path(),
                mountinfo: mounts,
                swaps: "",
                contents: "",
                medium: disks::MEDIUM,
            },
            false,
        );
        let source = TempDir::new();
        populate(&source);
        source.write("vmlinuz", "kernel");
        let target = TempDir::new();
        populate(&target);
        let plan = Plan::parse(&format!(
            "{}\n[source]\nroot = \"{}\"\nkernel = \"{}\"\n",
            crate::plan::tests::PLAN,
            source.path().display(),
            source.join("vmlinuz").display()
        ))
        .unwrap();
        let plan = Plan {
            size: 256 * GB,
            ..plan
        };
        let fake = Fake {
            windows: FakeWindows::default(),
            whole: Vec::new(),
            holds: Vec::new(),
            target: target.path().to_path_buf(),
            disks,
            uefi: false,
            fails: None,
            missing: None,
            commands: Vec::new(),
            inputs: Vec::new(),
            secrets: Vec::new(),
            mounted: Vec::new(),
            log: Vec::new(),
            firmware: "",
            wrong_menu: false,
            stops_at: None,
            busy: false,
            under_test: false,
            held_during: Vec::new(),
            held: false,
            stale_table: false,
            dump: None,
            kernel_dump: None,
            tamper: false,
            other_system: false,
            created: "",
            esp_type: "vfat",
            secure_boot: false,
            relist_fails: false,
            disk_changes: false,
            // The protective MBR as Windows writes it, with a size of all
            // ones.
            sector0: {
                let mut sector = vec![0u8; 512];
                sector[446..458].copy_from_slice(&[0, 0, 2, 0, 0xee, 0xff, 0xff, 0xff, 1, 0, 0, 0]);
                sector[458..462].copy_from_slice(&[0xff; 4]);
                sector[510] = 0x55;
                sector[511] = 0xaa;
                sector
            },
            tamper_mbr: None,
            appended: false,
            wrong_partuuid: false,
            dump_warning: "",
            late_dump: None,
            stopped: false,
            refused: Vec::new(),
        };
        World {
            _sys: sys,
            source,
            target,
            plan,
            fake,
        }
    }

    const PASSWORD: &str = "correct horse: battery";

    fn run(world: &mut World) -> (Result<u64, Failure>, Vec<Event>) {
        let paths = Paths {
            target: world.target.path().to_path_buf(),
            log: world.source.join("install.log"),
        };
        world.source.write("install.log", "the log\n");
        let password = Secret::new(PASSWORD.to_string());
        let mut events = Vec::new();
        let result = install(
            &world.plan,
            &password,
            &mut world.fake,
            &paths,
            &mut |event| events.push(event.clone()),
        );
        (result, events)
    }

    #[test]
    fn every_step_runs_in_order_and_says_so() {
        let mut world = world();
        let (result, events) = run(&mut world);
        assert_eq!(result.map(|_| ()), Ok(()), "{:#?}", world.fake.commands);
        let begun: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                Event::Begin(step) => Some(step.as_str()),
                _ => None,
            })
            .collect();
        let ids: Vec<&str> = STEPS.iter().map(|step| step.id).collect();
        assert_eq!(begun, ids);
        let ended = events.iter().filter(|e| matches!(e, Event::End(_))).count();
        assert_eq!(ended, STEPS.len());
        assert!(matches!(events.first(), Some(Event::Log(_))));
        assert!(matches!(events.last(), Some(Event::Done(_))));
        assert!(events.contains(&Event::Percent("copy".to_string(), 100)));
        assert!(!events
            .iter()
            .any(|e| matches!(e, Event::Warning(_) | Event::Failed(..))));
    }

    #[test]
    fn the_commands_are_the_ones_that_install_a_system() {
        let mut world = world();
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let target = world.target.path().display().to_string();
        let fake = &world.fake;
        let expected = [
            format!("du -sxk {}", world.source.path().display()),
            "sfdisk --lock --no-reread --no-tell-kernel --wipe always --wipe-partitions always /dev/sda".to_string(),
            "blockdev --rereadpt /dev/sda".to_string(),
            "udevadm settle".to_string(),
            "mkfs.vfat -F 32 -n SENGI-EFI /dev/sda2".to_string(),
            "mkfs.ext4 -q -F -L sengi-os /dev/sda3".to_string(),
            "blkid -s UUID -o value /dev/sda2".to_string(),
            "blkid -s UUID -o value /dev/sda3".to_string(),
            format!("mount -t ext4 /dev/sda3 {target}"),
            format!("cp -ax {}/. {target}", world.source.path().display()),
            format!("chown 0:0 {target}"),
            format!("chmod 0755 {target}"),
            format!("mount -t vfat -o umask=0077 /dev/sda2 {target}/boot/efi"),
            format!("mount --bind /dev {target}/dev"),
            format!("mount -t proc proc {target}/proc"),
            format!("mount -t sysfs sysfs {target}/sys"),
            format!("chroot {target} userdel live"),
            format!("chroot {target} useradd --create-home --uid 1000 --user-group --shell /bin/bash --comment Allan Smith --groups sudo,audio,video,input,render,netdev,bluetooth allan"),
            format!("chroot {target} chpasswd"),
            format!("chroot {target} passwd --lock root"),
            format!("chroot {target} update-initramfs -c -k {KERNEL}"),
            format!("chroot {target} grub-install --target=i386-pc --boot-directory=/boot /dev/sda"),
            format!("chroot {target} grub-install --target=x86_64-efi --efi-directory=/boot/efi --boot-directory=/boot --bootloader-id=sengios --no-nvram --no-uefi-secure-boot"),
            format!("chroot {target} grub-install --target=x86_64-efi --efi-directory=/boot/efi --boot-directory=/boot --bootloader-id=sengios --no-nvram --no-uefi-secure-boot --removable"),
            format!("chroot {target} update-grub"),
            format!("systemctl --root={target} --quiet enable fstrim.timer"),
            "sync".to_string(),
            format!("umount {target}/sys"),
            format!("umount {target}/proc"),
            format!("umount {target}/dev"),
            format!("umount {target}/boot/efi"),
            format!("umount {target}"),
        ];
        let mut at = 0;
        for command in &expected {
            let found = fake.commands[at..]
                .iter()
                .position(|ran| ran.starts_with(command.as_str()))
                .unwrap_or_else(|| {
                    panic!(
                        "`{command}` was not run in its place; ran: {:#?}",
                        fake.commands
                    )
                });
            at += found + 1;
        }
        // Nothing is run that is not on that list.
        assert_eq!(
            fake.commands.len(),
            expected.len() + 1,
            "{:#?}",
            fake.commands
        );
        assert!(fake.mounted.is_empty());
        // Started the old way, the firmware is left alone.
        assert!(!fake.ran("efibootmgr"));
        // Only the chosen disk and its partitions are ever named.
        for command in &fake.commands {
            assert!(!command.contains("/dev/sdb"), "{command}");
        }
    }

    #[test]
    fn the_partition_table_goes_to_sfdisk_and_the_password_to_chpasswd_alone() {
        let mut world = world();
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let fake = &world.fake;
        assert_eq!(fake.inputs.len(), 2);
        assert!(fake.inputs[0].0.starts_with("sfdisk"));
        assert_eq!(fake.inputs[0].1.as_deref(), Some(edit::PARTITIONS));
        assert!(fake.inputs[1].0.ends_with("chpasswd"));
        assert_eq!(fake.inputs[1].1, None);
        assert_eq!(fake.secrets, [format!("allan:{PASSWORD}\n")]);
        for line in fake.commands.iter().chain(fake.log.iter()) {
            assert!(!line.contains(PASSWORD), "the password is in: {line}");
            assert!(!line.contains("horse"), "the password is in: {line}");
        }
    }

    #[test]
    fn the_copy_is_made_into_an_installed_system() {
        let mut world = world();
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let target = &world.target;
        let fstab = target.read("etc/fstab");
        assert!(fstab.contains("UUID=0a1b2c3d-e5f6 / ext4"));
        assert!(fstab.contains("UUID=ABCD-1234 /boot/efi vfat"));
        assert_eq!(target.read("etc/hostname"), "zenbook\n");
        assert!(target.read("etc/hosts").contains("127.0.1.1\tzenbook"));
        assert_eq!(
            target.read("etc/machine-id"),
            "0123456789abcdef0123456789abcdef\n"
        );
        assert_eq!(
            fs::read_link(target.join("etc/localtime")).unwrap(),
            PathBuf::from("/usr/share/zoneinfo/Europe/Paris")
        );
        assert_eq!(target.read("etc/timezone"), "Europe/Paris\n");
        assert!(target
            .read("etc/xdg/sengi-os/settings.toml")
            .contains("layouts = [\"fr(azerty)\"]"));
        let greeter = target.read("usr/share/sengi-os/greeter/environment");
        assert!(greeter.contains("XKB_DEFAULT_LAYOUT=fr\nXKB_DEFAULT_VARIANT=azerty\n"));
        let greetd = target.read("etc/greetd/config.toml");
        assert!(!greetd.contains("initial_session") && !greetd.contains("live"));
        assert!(greetd.contains("[default_session]"));
        assert_eq!(
            target.read("etc/xdg/sengi-os/dock.toml"),
            "pinned = [\"foot\", \"sengi-settings\"]\n"
        );
        for file in LIVE_ONLY {
            assert!(!target.exists(file), "{file} is still there");
        }
        // The boot test's reporter runs commands the host hands it, as
        // root. So does nothing on an installed system: it is gone, and so
        // is the install test's, which was not asked for here.
        for file in INSTALL_TEST {
            assert!(!target.exists(file), "{file} is still there");
        }
        assert!(!target.exists("home/live"));
        assert_eq!(target.read("etc/default/grub"), edit::GRUB_DEFAULT);
        // A way in when the desktop does not start: an entry in the menu,
        // and a text login that only that entry switches on.
        let entry = target.read("etc/grub.d/11_sengi-text");
        assert!(entry.starts_with("#!/bin/sh\n"));
        assert!(entry.contains(edit::LABWC_FLAG));
        let mode = fs::metadata(target.join("etc/grub.d/11_sengi-text"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(!target.exists("etc/systemd/system/getty@tty1.service"));
        assert!(target
            .read("etc/systemd/system/getty@tty1.service.d/10-sengi.conf")
            .contains("ConditionKernelCommandLine=sengi.text"));
        assert_eq!(
            target.read("etc/initramfs-tools/conf.d/resume"),
            "RESUME=none\n"
        );
        assert_eq!(target.read(&format!("boot/vmlinuz-{KERNEL}")), "kernel");
        assert!(!target.read("etc/passwd").contains("live"));
        assert_eq!(target.read("var/log/sengi-install.log"), "the log\n");
        // The system that was copied from is as it was.
        assert!(world.source.exists("etc/sudoers.d/live"));
        assert!(world
            .source
            .read("etc/greetd/config.toml")
            .contains("initial_session"));
        assert_eq!(world.source.read("etc/hostname"), "sengios\n");
    }

    fn refused(world: &mut World, word: &str) {
        let (result, events) = run(world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains(word), "{why}");
        assert_eq!(
            events.last(),
            Some(&Event::Failed("check".to_string(), why))
        );
        // Nothing was written, and nothing run that could write.
        for command in &world.fake.commands {
            assert!(command.starts_with("du "), "ran {command}");
        }
        assert!(world.target.exists("etc/sudoers.d/live"));
    }

    #[test]
    fn a_disk_that_must_not_be_touched_stops_everything_before_it_starts() {
        // The medium the live system runs from.
        let mut live = world();
        live.plan.device = "/dev/sdb".to_string();
        live.plan.size = 32 * GB;
        live.plan.model = String::new();
        refused(&mut live, "Sengi OS is running from it");

        let mut mounted = world();
        mounted.fake.disks[0].mounted = vec!["/mnt/data".to_string()];
        refused(&mut mounted, "mounted at /mnt/data");

        let mut swap = world();
        swap.fake.disks[0].swap = true;
        refused(&mut swap, "swap");

        let mut small = world();
        small.fake.disks[0].size = 4 * GB;
        small.plan.size = 4 * GB;
        refused(&mut small, "Too small");

        let mut gone = world();
        gone.plan.device = "/dev/sdq".to_string();
        refused(&mut gone, "no disk called /dev/sdq");
    }

    #[test]
    fn a_disk_that_is_not_the_one_that_was_chosen_is_not_touched() {
        let mut other_size = world();
        other_size.plan.size = 500 * GB;
        refused(&mut other_size, "not the disk that was chosen");
        let mut other_model = world();
        other_model.plan.model = "Some Other Disk".to_string();
        refused(&mut other_model, "not the disk that was chosen");
    }

    #[test]
    fn an_installation_that_cannot_finish_does_not_start() {
        let mut no_tool = world();
        no_tool.fake.missing = Some("mkfs.vfat");
        refused(&mut no_tool, "mkfs.vfat is missing");

        let mut no_grub = world();
        fs::remove_file(no_grub.source.join("usr/sbin/grub-install")).unwrap();
        refused(&mut no_grub, "/usr/sbin/grub-install is missing");

        let mut no_system = world();
        no_system.source.write("etc/os-release", "ID=debian\n");
        refused(&mut no_system, "no Sengi OS system to copy");

        let mut no_kernel = world();
        fs::remove_file(no_kernel.source.join("vmlinuz")).unwrap();
        refused(&mut no_kernel, "no kernel");

        let mut two_kernels = world();
        two_kernels.source.mkdir("lib/modules/6.99.0-amd64");
        refused(&mut two_kernels, "exactly one kernel");

        let mut no_zone = world();
        no_zone.plan.time_zone = "Europe/Atlantis".to_string();
        refused(&mut no_zone, "no time zone called Europe/Atlantis");

        let mut taken = world();
        taken.plan.user = "audio".to_string();
        refused(&mut taken, "already used");
    }

    #[test]
    fn an_empty_password_is_refused_before_anything_else() {
        let mut world = world();
        let paths = Paths {
            target: world.target.path().to_path_buf(),
            log: world.source.join("install.log"),
        };
        let empty = Secret::new(String::new());
        let result = install(&world.plan, &empty, &mut world.fake, &paths, &mut |_| {});
        assert_eq!(result.unwrap_err().0, "check");
        assert!(world.fake.commands.is_empty());
    }

    #[test]
    fn a_step_that_fails_stops_the_rest_and_unmounts_what_was_mounted() {
        for (what, step, never) in [
            ("sfdisk", "partition", "mkfs"),
            ("mkfs.ext4", "format", "mount"),
            ("cp -ax", "copy", "chroot"),
            ("useradd", "account", "update-initramfs"),
            ("chpasswd", "account", "update-initramfs"),
            ("update-initramfs", "boot-image", "grub-install"),
            ("--removable", "boot-loader", "update-grub"),
            ("update-grub", "boot-loader", "sync"),
            ("sync", "finish", "nothing-at-all"),
        ] {
            let mut world = world();
            world.fake.fails = Some(what);
            let (result, events) = run(&mut world);
            let (failed, why) = result.unwrap_err();
            assert_eq!(failed, step, "{what}");
            assert!(why.ends_with("ended with status 1: it broke"), "{why}");
            assert_eq!(events.last(), Some(&Event::Failed(step.to_string(), why)));
            assert!(!events.iter().any(|e| matches!(e, Event::Done(_))));
            assert!(
                world.fake.mounted.is_empty(),
                "{what} left {:?}",
                world.fake.mounted
            );
            let failed_at = world
                .fake
                .commands
                .iter()
                .position(|command| command.contains(what))
                .unwrap();
            for command in &world.fake.commands[failed_at..] {
                let inner = command.split(' ').find(|word| *word == never);
                assert!(inner.is_none(), "after {what} failed, ran {command}");
            }
        }
    }

    #[test]
    fn a_failure_names_the_program_that_ran_inside_the_new_system() {
        let mut world = world();
        world.fake.fails = Some("useradd");
        let (result, _) = run(&mut world);
        assert_eq!(
            result.unwrap_err().1,
            "useradd ended with status 1: it broke"
        );
    }

    #[test]
    fn told_to_stop_it_still_unmounts_what_it_mounted() {
        // A signal arrives while the copy runs. Every command after it is
        // refused, except the ones that put things back.
        let mut world = world();
        world.fake.stops_at = Some("cp -ax");
        let (result, events) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "copy");
        assert!(why.contains("told to stop"), "{why}");
        assert!(
            world.fake.mounted.is_empty(),
            "left {:?}",
            world.fake.mounted
        );
        assert!(world.fake.ran("umount"));
        assert!(
            world.fake.refused.is_empty(),
            "refused {:?}",
            world.fake.refused
        );
        assert!(matches!(events.last(), Some(Event::Failed(..))));
        // And nothing but unmounting was run after the signal.
        let at = world.fake.position("cp -ax");
        for command in &world.fake.commands[at + 1..] {
            assert!(command.starts_with("umount"), "ran {command}");
        }
    }

    #[test]
    fn an_unmount_that_fails_is_tried_again_by_letting_go() {
        let mut world = world();
        world.fake.fails = Some("umount /");
        let (result, _) = run(&mut world);
        // The plain unmounts failed; the lazy ones are what is left to
        // try, and the fake lets those through.
        assert!(result.is_ok());
        assert!(world.fake.mounted.is_empty());
        assert!(world.fake.ran("umount --lazy"));
    }

    #[test]
    fn what_an_earlier_run_left_mounted_is_unmounted_first() {
        let mut world = world();
        let stale = world.target.join("boot/efi");
        world.fake.mounted.push(world.target.path().to_path_buf());
        world.fake.mounted.push(stale.clone());
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let first = world.fake.position("umount");
        assert_eq!(
            world.fake.commands[first],
            format!("umount {}", stale.display())
        );
        assert!(first < world.fake.position("sfdisk"));
    }

    #[test]
    fn on_uefi_the_firmware_is_told_and_old_entries_are_replaced() {
        let mut world = world();
        world.fake.uefi = true;
        world.fake.firmware =
            "BootOrder: 0002,0000\nBoot0000* Windows Boot Manager\nBoot0002* Sengi OS\n";
        let (result, events) = run(&mut world);
        assert!(result.is_ok());
        let fake = &world.fake;
        let listed = fake.position("efibootmgr");
        assert_eq!(fake.commands[listed], "efibootmgr");
        assert_eq!(
            fake.commands[listed + 1],
            "efibootmgr --quiet --bootnum 0002 --delete-bootnum"
        );
        assert_eq!(
            fake.commands[listed + 2],
            "efibootmgr --quiet --create --disk /dev/sda --part 2 --label Sengi OS --loader \\EFI\\sengios\\grubx64.efi"
        );
        assert!(
            listed
                > fake.position(&format!(
                    "chroot {} update-grub",
                    world.target.path().display()
                ))
        );
        assert!(!events.iter().any(|e| matches!(e, Event::Warning(_))));
    }

    #[test]
    fn firmware_that_refuses_an_entry_is_a_warning_not_a_failure() {
        let mut world = world();
        world.fake.uefi = true;
        world.fake.fails = Some("efibootmgr");
        let (result, events) = run(&mut world);
        assert!(result.is_ok());
        let warned = events
            .iter()
            .any(|event| matches!(event, Event::Warning(text) if text.contains("fallback path")));
        assert!(warned, "{events:?}");
        assert!(matches!(events.last(), Some(Event::Done(_))));
    }

    #[test]
    fn a_system_that_is_not_what_was_asked_for_is_a_failure() {
        // update-grub wrote a menu that starts some other disk.
        let mut world = world();
        world.fake.wrong_menu = true;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "boot-loader");
        assert!(why.contains("does not start the system"), "{why}");
        assert!(world.fake.mounted.is_empty());
        // And an account step that left the live user in is caught too.
        let mut left = self::world();
        left.fake.fails = Some("userdel");
        let (result, _) = run(&mut left);
        assert_eq!(result.unwrap_err().0, "account");
        assert!(left.target.read("etc/passwd").contains("live:"));
    }

    #[test]
    fn a_disk_that_something_else_has_open_is_not_written() {
        let mut world = world();
        world.fake.busy = true;
        refused(&mut world, "in use by something else");
    }

    #[test]
    fn the_disk_is_held_while_the_table_is_written_and_let_go_before_it_is_read() {
        let mut world = world();
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let held = |start: &str| {
            world
                .fake
                .held_during
                .iter()
                .find(|(command, _)| command.starts_with(start))
                .map(|(_, held)| *held)
                .unwrap()
        };
        assert!(!held("du "));
        assert!(held("sfdisk"));
        assert!(!held("blockdev --rereadpt"));
        assert!(!held("mkfs.vfat"));
        assert!(!world.fake.held);
        // Held, and then a failure: let go all the same.
        let mut failing = self::world();
        failing.fake.fails = Some("sfdisk");
        let (result, _) = run(&mut failing);
        assert!(result.is_err());
        assert!(!failing.fake.held);
    }

    #[test]
    fn a_kernel_still_on_the_old_table_stops_everything_before_mkfs() {
        let mut world = world();
        world.fake.stale_table = true;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "partition");
        assert!(
            why.contains("does not show the new partition table"),
            "{why}"
        );
        assert!(!world.fake.ran("mkfs"));
        assert!(!world.fake.ran("mount"));
    }

    #[test]
    fn a_disk_with_another_serial_or_name_is_not_the_one_that_was_chosen() {
        let mut serial = world();
        serial.fake.disks[0].serial = "S3Z8NB0K999999Z".to_string();
        refused(&mut serial, "not the disk that was chosen");
        let mut wwn = world();
        wwn.fake.disks[0].wwn = "0x5002538e00000000".to_string();
        refused(&mut wwn, "not the disk that was chosen");
        // The same serial and name on both sides is the same disk.
        let mut same = world();
        same.fake.disks[0].serial = "S3Z8NB0K123456A".to_string();
        same.plan.serial = "S3Z8NB0K123456A".to_string();
        same.fake.disks[0].wwn = "0x5002538e40a1b2c3".to_string();
        same.plan.wwn = "0x5002538e40a1b2c3".to_string();
        let (result, _) = run(&mut same);
        assert!(result.is_ok());
    }

    #[test]
    fn the_install_tests_reporter_stays_only_when_that_test_is_installing() {
        let mut world = world();
        world.fake.under_test = true;
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        for file in INSTALL_TEST {
            assert!(world.target.exists(file), "{file} was removed");
        }
        // The boot test's reporter, with its command hook, goes either way.
        assert!(!world.target.exists("usr/libexec/sengi-boot-report"));
        assert!(!world
            .target
            .exists("etc/systemd/system/graphical.target.wants/sengi-boot-report.service"));
    }

    #[test]
    fn a_nvme_disk_gets_its_partitions_by_their_own_names() {
        let mut world = world();
        world.fake.disks[0].path = "/dev/nvme0n1".to_string();
        world.plan.device = "/dev/nvme0n1".to_string();
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        assert!(world
            .fake
            .ran("mkfs.vfat -F 32 -n SENGI-EFI /dev/nvme0n1p2"));
        assert!(world.fake.ran("mkfs.ext4 -q -F -L sengi-os /dev/nvme0n1p3"));
    }

    #[test]
    fn the_steps_have_ids_the_progress_lines_can_carry() {
        for step in &STEPS {
            let begin = Event::Begin(step.id.to_string());
            assert_eq!(Event::parse(&begin.line()), Some(begin));
            assert!(!step.title.is_empty());
        }
    }
    // Beside another system.

    /// What `sfdisk -d` prints for the 256 GB disk of these tests: an EFI
    /// system partition and a Windows partition of 25 GiB, then free
    /// space to the end.
    const BESIDE_DUMP: &str = "label: gpt\nlabel-id: AAAAAAAA-0000-4000-8000-000000000001\ndevice: /dev/sda\nunit: sectors\nfirst-lba: 2048\nlast-lba: 499999966\nsector-size: 512\n\n/dev/sda1 : start=        2048, size=      204800, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=62516844-55D2-4917-A468-C8A0FBB8B6CD, name=\"EFI System Partition\"\n/dev/sda2 : start=      206848, size=    52428800, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, uuid=DD068BA6-D1DF-4FE6-9721-13FC754E035F, name=\"Windows\"\n";

    fn world_beside() -> World {
        let mut world = world();
        let table = Table::parse_dump(BESIDE_DUMP).unwrap();
        let region = table.free_regions()[0];
        world.plan.mode = Mode::Beside;
        world.plan.beside = Some(beside::Beside {
            start: region.start,
            sectors: region.sectors,
            esp: 1,
            table: table.snapshot(),
            shrink: None,
        });
        world.fake.dump = Some(BESIDE_DUMP.to_string());
        world.fake.kernel_dump = Some(BESIDE_DUMP.to_string());
        world.fake.uefi = true;
        // The stick the live system runs from, with a partition of its own.
        let mut stick = world.fake.disks[0].clone();
        stick.name = "sdz".to_string();
        stick.path = "/dev/sdz".to_string();
        stick.medium = true;
        stick.parts = vec![crate::disks::Part {
            name: "sdz3".to_string(),
            number: 3,
            start: 1,
            size: 1,
            kind: String::new(),
            uuid: String::new(),
            fstype: "hfsplus".to_string(),
            label: String::new(),
        }];
        world.fake.disks.push(stick);
        world.fake.firmware =
            "BootCurrent: 0000\nBootOrder: 0000\nBoot0000* Windows Boot Manager\tHD(1,GPT,x)\n";
        world.fake.created = "BootOrder: 0001,0000\nBoot0001* Sengi OS\tHD(1,GPT,x)\nBoot0000* Windows Boot Manager\tHD(1,GPT,x)\n";
        world
    }

    fn beside_region() -> Region {
        Table::parse_dump(BESIDE_DUMP).unwrap().free_regions()[0]
    }

    #[test]
    fn beside_another_system_one_partition_is_made_and_nothing_else_is_written() {
        let mut world = world_beside();
        let (result, events) = run(&mut world);
        assert_eq!(result.map(|_| ()), Ok(()), "{:#?}", world.fake.commands);
        let target = world.target.path().display().to_string();
        let fake = &world.fake;
        let region = beside_region();

        // The table is read as root, then written with one line.
        assert!(fake.position("sfdisk -d /dev/sda") < fake.position("sfdisk --lock"));
        let write = fake.position("sfdisk --lock");
        assert_eq!(
            fake.commands[write],
            "sfdisk --lock --no-reread --no-tell-kernel --wipe never --wipe-partitions always --append /dev/sda"
        );
        let script = fake
            .inputs
            .iter()
            .find(|(command, _)| command.starts_with("sfdisk --lock"))
            .and_then(|(_, text)| text.clone())
            .unwrap();
        assert_eq!(
            script,
            format!(
                "start={}, size={}, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=01234567-89AB-4DEF-8123-456789ABCDEF, name=\"Sengi OS\"\n",
                region.start, region.sectors
            )
        );
        // Written while the disk was held, and only the new partition
        // told to the kernel; the old ones are not read again.
        let held = |start: &str| {
            fake.held_during
                .iter()
                .find(|(command, _)| command.starts_with(start))
                .map(|(_, held)| *held)
        };
        assert_eq!(held("sfdisk --lock"), Some(true));
        assert_eq!(held("partx"), Some(false));
        assert!(fake.ran("partx --add --nr 3 /dev/sda"));
        assert!(!fake.ran("blockdev"));

        // Only the new partition is formatted. Nothing is wiped.
        assert!(fake.ran("mkfs.ext4 -q -F -L sengi-os /dev/disk/by-partuuid/01234567-89ab-4def-8123-456789abcdef"));
        assert!(!fake.ran("mkfs.vfat"));
        assert!(!fake.ran("wipefs"));
        assert!(fake.position("partx") < fake.position("mkfs.ext4"));
        assert!(fake.ran(&format!(
            "mount -t ext4 /dev/disk/by-partuuid/01234567-89ab-4def-8123-456789abcdef {target}"
        )));
        assert!(fake.ran(&format!(
            "mount -t vfat -o umask=0077 /dev/sda1 {target}/boot/efi"
        )));
        // The old partitions are only ever looked at and mounted.
        for command in &fake.commands {
            for old in ["/dev/sda1", "/dev/sda2"] {
                if command.split(' ').any(|word| word == old) {
                    let program = command.split(' ').next().unwrap();
                    assert!(["blkid", "mount", "umount"].contains(&program), "{command}");
                    if old == "/dev/sda2" {
                        panic!("the other system's partition was named: {command}");
                    }
                }
            }
        }
        assert!(fake.mounted.is_empty());

        // GRUB for UEFI only, in a directory of its own.
        assert!(!fake.commands.iter().any(|c| c.contains("i386-pc")));
        assert!(!fake.commands.iter().any(|c| c.contains("--removable")));
        assert!(fake.ran(&format!("chroot {target} grub-install --target=x86_64-efi --efi-directory=/boot/efi --boot-directory=/boot --bootloader-id=sengios --no-nvram --no-uefi-secure-boot")));
        assert!(!world.target.exists("boot/efi/EFI/BOOT/BOOTX64.EFI"));
        assert!(world.target.exists("boot/efi/EFI/sengios/grubx64.efi"));
        assert!(fake.ran("efibootmgr --quiet --create --disk /dev/sda --part 1 "));
        // The new entry is first already, so the order is left as it is.
        assert!(!fake.ran("efibootmgr --quiet --bootorder"));

        // The EFI system partition is let go before GRUB looks for other
        // systems, which then mount it themselves; the stick is left out.
        assert!(
            fake.position(&format!("umount {target}/boot/efi"))
                < fake.position(&format!("chroot {target} update-grub"))
        );
        assert!(fake.ran(&format!(
            "chroot {target} grub-probe --target=fs_uuid --device /dev/sdz3"
        )));
        // Other systems are looked for, and nothing else is different.
        let grub = world.target.read("etc/default/grub");
        assert_eq!(
            grub,
            edit::with_skip_list(
                &edit::grub_default_beside(),
                &["0000000000000000@/dev/sdz3".to_string()]
            )
        );
        assert!(grub.contains("GRUB_DISABLE_OS_PROBER=false"));
        assert!(fake.ran(&format!("mount --bind /run {target}/run")));
        let fstab = world.target.read("etc/fstab");
        assert!(fstab.contains("/boot/efi vfat"));
        assert!(!fstab.contains("swap"));
        // None was found, and the person is told so.
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Warning(text) if text.contains("No other system"))));
        assert!(!fake
            .commands
            .iter()
            .any(|c| c.starts_with("sfdisk") && c.contains("--delete")));
    }

    #[test]
    fn beside_another_system_the_menu_is_shown_when_one_is_found() {
        let mut world = world_beside();
        world.fake.other_system = true;
        let (result, events) = run(&mut world);
        assert_eq!(result.map(|_| ()), Ok(()));
        let grub = world.target.read("etc/default/grub");
        assert!(grub.contains("GRUB_TIMEOUT_STYLE=menu"));
        assert!(grub.contains("GRUB_TIMEOUT=3"));
        assert!(grub.contains("GRUB_DISABLE_OS_PROBER=false"));
        assert_eq!(
            world
                .fake
                .commands
                .iter()
                .filter(|c| c.ends_with("update-grub"))
                .count(),
            2
        );
        assert!(!events.iter().any(|e| matches!(e, Event::Warning(_))));
    }

    #[test]
    fn beside_another_system_a_disk_on_its_own_keeps_the_hidden_menu() {
        // Whole-disk installs never look for others and hide the menu.
        let mut world = world();
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let grub = world.target.read("etc/default/grub");
        assert!(grub.contains("GRUB_DISABLE_OS_PROBER=true"));
        assert!(grub.contains("GRUB_TIMEOUT_STYLE=hidden"));
        assert!(!world.fake.ran("partx"));
    }

    /// Nothing at all was written: no table, no format, nothing left
    /// mounted.
    fn untouched(world: &World) {
        let fake = &world.fake;
        assert!(!fake.ran("sfdisk --lock"), "{:#?}", fake.commands);
        assert!(!fake.ran("partx"));
        assert!(!fake.ran("mkfs"));
        assert!(!fake.ran("mount -t ext4"));
        assert!(fake.mounted.is_empty());
        assert!(!fake.held);
    }

    #[test]
    fn a_table_that_changed_since_it_was_shown_is_not_written_to() {
        let mut world = world_beside();
        // Somebody made a partition after the person looked.
        world.fake.dump = Some(format!(
            "{BESIDE_DUMP}/dev/sda3 : start=    52635648, size=     2097152, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=CCCC-DDDD\n"
        ));
        let (result, events) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains("changed"), "{why}");
        assert!(events.iter().any(|e| matches!(e, Event::Failed(..))));
        untouched(&world);
        // Or the table is the same but its size is not.
        let mut world = world_beside();
        world.fake.dump = Some(BESIDE_DUMP.replace("size=    52428800", "size=    52428801"));
        let (result, _) = run(&mut world);
        assert_eq!(result.unwrap_err().0, "check");
        untouched(&world);
    }

    #[test]
    fn free_space_that_is_not_free_is_not_written_to() {
        let mut world = world_beside();
        // The right table, and a place that overlaps the system.
        world.plan.beside.as_mut().unwrap().start = 206_848;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains("not free"), "{why}");
        untouched(&world);
        // A place that runs off the end of the disk.
        let mut world = world_beside();
        world.plan.beside.as_mut().unwrap().sectors += 4096 * 1024;
        let (result, _) = run(&mut world);
        assert_eq!(result.unwrap_err().0, "check");
        untouched(&world);
    }

    #[test]
    fn beside_another_system_needs_uefi_and_a_fat_system_partition() {
        let mut world = world_beside();
        world.fake.uefi = false;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains("UEFI"), "{why}");
        untouched(&world);

        let mut world = world_beside();
        world.fake.esp_type = "ntfs";
        let (result, _) = run(&mut world);
        assert!(result.unwrap_err().1.contains("not a FAT volume"));
        untouched(&world);

        for program in ["partx", "sfdisk"] {
            let mut world = world_beside();
            world.fake.missing = Some(program);
            let (result, _) = run(&mut world);
            assert!(result.unwrap_err().1.contains(program));
            untouched(&world);
        }
    }

    #[test]
    fn an_old_partition_that_changed_stops_everything_before_a_format() {
        let mut world = world_beside();
        world.fake.tamper = true;
        let (result, events) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "partition");
        assert!(why.contains("partition 1 is not as it was"), "{why}");
        let fake = &world.fake;
        assert!(fake.ran("sfdisk --lock"));
        assert!(!fake.ran("partx"));
        assert!(!fake.ran("mkfs"));
        assert!(!fake.ran("mount -t ext4"));
        assert!(fake.mounted.is_empty());
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Failed(step, _) if step == "partition")));
    }

    #[test]
    fn a_kernel_that_does_not_show_the_new_partition_stops_everything_before_a_format() {
        let mut world = world_beside();
        world.fake.stale_table = true;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "partition");
        assert!(why.contains("kernel"), "{why}");
        assert!(!world.fake.ran("mkfs"));
    }

    #[test]
    fn a_firmware_that_refuses_the_entry_is_a_failure_beside_another_system() {
        // There is no fallback path to start from there.
        let mut world = world_beside();
        world.fake.fails = Some("efibootmgr --quiet --create");
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "boot-loader");
        assert!(why.contains("firmware did not take a boot entry"), "{why}");
        assert!(world.fake.mounted.is_empty());
    }

    #[test]
    fn our_entry_goes_first_and_windows_stays_in_the_order() {
        let mut world = world_beside();
        world.fake.created = "BootOrder: 0000,0001\nBoot0000* Windows Boot Manager\tHD(1,GPT,x)\nBoot0001* Sengi OS\tHD(1,GPT,x)\n";
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        assert!(world.fake.ran("efibootmgr --quiet --bootorder 0001,0000"));
        // Nothing is deleted but entries that are ours.
        assert!(!world.fake.ran("efibootmgr --quiet --bootnum 0000"));
    }

    #[test]
    fn room_on_the_system_partition_is_looked_up_without_writing() {
        // The probe mounts it read-only.
        let mut world = world_beside();
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let target = world.target.path().display().to_string();
        assert!(world.fake.ran(&format!(
            "mount -t vfat -o ro,noexec,nosuid,nodev /dev/sda1 {target}"
        )));
        assert!(free_bytes(Path::new("/")).is_some());
    }
    #[test]
    fn a_partition_that_is_not_the_one_made_is_not_formatted() {
        // The name moved to another disk's partition: it has another
        // identifier, and mkfs must not run.
        let mut world = world_beside();
        world.fake.wrong_partuuid = true;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "format");
        assert!(why.contains("not the partition that was made"), "{why}");
        assert!(!world.fake.ran("mkfs"));
        assert!(world.fake.mounted.is_empty());
    }

    #[test]
    fn a_disk_that_is_not_the_one_chosen_any_more_is_not_formatted() {
        let mut world = world_beside();
        world.fake.disk_changes = true;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "format");
        assert!(why.contains("no longer the disk that was chosen"), "{why}");
        assert!(!world.fake.ran("mkfs"));
    }

    #[test]
    fn a_table_that_changed_after_it_was_written_is_not_formatted_on() {
        let mut world = world_beside();
        let region = beside_region();
        // Somebody else\'s partition where ours was.
        world.fake.late_dump = Some(format!(
            "{BESIDE_DUMP}/dev/sda3 : start={}, size={}, type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, uuid=CCCC-DDDD\n",
            region.start, region.sectors
        ));
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "format");
        assert!(why.contains("not the one that was written"), "{why}");
        assert!(!world.fake.ran("mkfs"));
    }

    #[test]
    fn the_new_partition_is_said_when_it_exists() {
        let mut world = world_beside();
        let (result, events) = run(&mut world);
        assert!(result.is_ok());
        assert!(events.contains(&Event::Beside));
        assert!(events.contains(&Event::Made(3)));
        // Said once the table is written and not before.
        let made = events.iter().position(|e| *e == Event::Made(3)).unwrap();
        let begun = events
            .iter()
            .position(|e| *e == Event::Begin("partition".to_string()))
            .unwrap();
        assert!(begun < made);
    }

    #[test]
    fn secure_boot_stops_beside_and_only_warns_for_a_whole_disk() {
        let mut beside = world_beside();
        beside.fake.secure_boot = true;
        let (result, _) = run(&mut beside);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains("Secure Boot is on: turn it off"), "{why}");
        untouched(&beside);
        let mut world = world();
        world.fake.uefi = true;
        world.fake.secure_boot = true;
        let (result, events) = run(&mut world);
        assert!(result.is_ok());
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Warning(text) if text.contains("Secure Boot is on"))));
    }

    #[test]
    fn a_warning_from_sfdisk_about_the_table_stops_everything() {
        let mut world = world_beside();
        world.fake.dump_warning =
            "GPT PMBR size mismatch (4194303 != 8388607) will be corrected by write.\n";
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains("PMBR size mismatch"), "{why}");
        untouched(&world);
    }

    #[test]
    fn a_hybrid_mbr_or_a_changed_sector_zero_stops_everything() {
        let mut world = world_beside();
        world.fake.sector0[446 + 16 + 4] = 0x07;
        world.fake.sector0[446 + 16 + 8] = 9;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains("hybrid MBR"), "{why}");
        untouched(&world);
        // Written, and sector 0 is not as it should be: nothing is
        // formatted. A second entry, the disk's signature, the entry's
        // start, and a size that is not the disk's.
        for tamper in [(446 + 20, 0x83), (440, 1), (454, 2), (458, 0x12)] {
            let mut world = world_beside();
            world.fake.tamper_mbr = Some(tamper);
            let (result, _) = run(&mut world);
            let (step, why) = result.unwrap_err();
            assert_eq!(step, "partition", "{tamper:?}");
            assert!(why.contains("sector 0"), "{why}");
            assert!(!world.fake.ran("partx"));
            assert!(!world.fake.ran("mkfs"));
        }
    }

    #[test]
    fn the_size_in_a_windows_protective_mbr_may_become_the_disks_own() {
        // All ones, as Windows writes it, and sfdisk puts the disk's size
        // there with the table.
        let mut world = world_beside();
        assert_eq!(world.fake.sector0[458..462], [0xff; 4]);
        let (result, _) = run(&mut world);
        result.unwrap();
        assert!(world.fake.ran("mkfs.ext4"));
    }

    #[test]
    fn a_protective_mbr_that_would_be_rewritten_is_refused_before_anything_is_written() {
        // The active flag, another start or end in cylinders and heads, an
        // entry elsewhere than the first: sfdisk would change each.
        for (at, byte) in [(446, 0x80), (448, 0), (451, 0xfe), (510, 0)] {
            let mut world = world_beside();
            world.fake.sector0[at] = byte;
            let (result, _) = run(&mut world);
            let (step, why) = result.unwrap_err();
            assert_eq!(step, "check", "byte {at}");
            assert!(why.contains("protective MBR"), "{why}");
            untouched(&world);
        }
    }

    #[test]
    fn only_the_entry_of_ours_on_this_partition_is_replaced() {
        let mut world = world_beside();
        let esp = "62516844-55d2-4917-a468-c8a0fbb8b6cd";
        let other = "11111111-2222-4333-8444-555555555555";
        let entry = |number: &str, name: &str, id: &str, path: &str| {
            format!("Boot{number}* {name}\tHD(1,GPT,{id},0x800,0x32000)/File({path})\n")
        };
        let windows = entry(
            "0000",
            "Windows Boot Manager",
            esp,
            "\\EFI\\Microsoft\\Boot\\bootmgfw.efi",
        );
        let ours = entry("0005", "Sengi OS", esp, "\\EFI\\sengios\\grubx64.efi");
        let elsewhere = entry("0006", "Sengi OS", other, "\\EFI\\sengios\\grubx64.efi");
        world.fake.firmware = Box::leak(
            format!("BootOrder: 0006,0005,0000\n{windows}{ours}{elsewhere}").into_boxed_str(),
        );
        world.fake.created = Box::leak(
            format!("BootOrder: 0005,0006,0000\n{windows}{ours}{elsewhere}").into_boxed_str(),
        );
        let (result, _) = run(&mut world);
        assert!(result.is_ok(), "{result:?}");
        let fake = &world.fake;
        assert!(fake.ran("efibootmgr --quiet --bootnum 0005 --delete-bootnum"));
        assert!(!fake.ran("efibootmgr --quiet --bootnum 0006"));
        assert!(!fake.ran("efibootmgr --quiet --bootnum 0000"));
    }

    #[test]
    fn a_failure_after_the_boot_loader_went_on_takes_it_out_again() {
        // The order cannot be set: the entry and the folder that were
        // made are taken out, and the person is told.
        let mut world = world_beside();
        world.fake.created = "BootOrder: 0000,0001\nBoot0000* Windows Boot Manager\tHD(1,GPT,x)\nBoot0001* Sengi OS\tHD(1,GPT,x)\n";
        world.fake.fails = Some("efibootmgr --quiet --bootorder");
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "boot-loader");
        assert!(
            why.contains("What the installer had put on the EFI system partition"),
            "{why}"
        );
        assert!(why.contains("boot entry it made was removed"), "{why}");
        assert!(why.contains("EFI/sengios was removed"), "{why}");
        assert!(world
            .fake
            .ran("efibootmgr --quiet --bootnum 0001 --delete-bootnum"));
        assert!(!world.target.exists("boot/efi/EFI/sengios"));
        assert!(world.fake.mounted.is_empty());
        // And a folder that was there before is not removed.
        let mut world = world_beside();
        world
            .target
            .write("boot/efi/EFI/sengios/old.txt", "an earlier install");
        world.fake.fails = Some("efibootmgr --quiet --create");
        let (result, _) = run(&mut world);
        assert!(result.is_err());
        assert!(world.target.exists("boot/efi/EFI/sengios/old.txt"));
        let (_, why) = run(&mut world_with_old_folder()).0.unwrap_err();
        assert!(why.contains("was there before and was left"), "{why}");
    }
    fn world_with_old_folder() -> World {
        let mut world = world_beside();
        world
            .target
            .write("boot/efi/EFI/sengios/old.txt", "an earlier install");
        world.fake.fails = Some("efibootmgr --quiet --create");
        world
    }

    #[test]
    fn the_system_partition_is_formatted_and_mounted_by_the_identifier_that_was_checked() {
        let mut world = world_beside();
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let fake = &world.fake;
        let by_id = "/dev/disk/by-partuuid/01234567-89ab-4def-8123-456789abcdef";
        // The identifier is checked, then that very name is formatted.
        assert!(
            fake.position("blkid -s PARTUUID -o value /dev/sda3")
                < fake.position(&format!("mkfs.ext4 -q -F -L sengi-os {by_id}"))
        );
        assert!(!fake.ran("mkfs.ext4 -q -F -L sengi-os /dev/sda3"));
        // And nothing else is a way to the new partition.
        for command in &fake.commands {
            if command.starts_with("mkfs") {
                assert!(command.ends_with(by_id), "{command}");
            }
        }
    }

    #[test]
    fn an_entry_that_was_made_and_cannot_be_found_again_is_said_to_remain() {
        let mut world = world_beside();
        world.fake.relist_fails = true;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "boot-loader");
        assert!(
            why.contains("may have been made and is still in the firmware"),
            "{why}"
        );
        assert!(!why.contains("was removed from the firmware"), "{why}");
        // The folder this installation made is taken out and said so.
        assert!(why.contains("EFI/sengios was removed"), "{why}");
    }

    #[test]
    fn an_earlier_entry_that_was_replaced_and_not_put_back_is_said() {
        let mut world = world_beside();
        let esp = "62516844-55d2-4917-a468-c8a0fbb8b6cd";
        let ours = format!(
            "Boot0005* Sengi OS\tHD(1,GPT,{esp},0x800,0x32000)/File(\\EFI\\sengios\\grubx64.efi)\n"
        );
        world.fake.firmware = Box::leak(format!("BootOrder: 0005\n{ours}").into_boxed_str());
        world.fake.fails = Some("efibootmgr --quiet --create");
        let (result, _) = run(&mut world);
        let (_, why) = result.unwrap_err();
        assert!(why.contains("earlier Sengi OS boot entry on this disk had been removed to be replaced, and was not put back"), "{why}");
        // The same is said where more than one was replaced.
        assert!(why.contains("What the installer had put"), "{why}");
    }

    // Making Windows smaller first.

    /// A Windows that has all of the 256 GB disk but for the EFI system
    /// partition, and uses 80 GB of it.
    const SHRINK_DUMP: &str = "label: gpt\nlabel-id: AAAAAAAA-0000-4000-8000-000000000001\ndevice: /dev/sda\nunit: sectors\nfirst-lba: 2048\nlast-lba: 499999966\nsector-size: 512\n\n/dev/sda1 : start=        2048, size=      204800, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, uuid=62516844-55D2-4917-A468-C8A0FBB8B6CD, name=\"EFI System Partition\"\n/dev/sda2 : start=      206848, size=   499793119, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, uuid=DD068BA6-D1DF-4FE6-9721-13FC754E035F, name=\"Basic data partition\"\n";
    const WINDOWS_NODE: &str = "/dev/sda2";

    fn chosen_shrink() -> (Shrink, Region) {
        let table = Table::parse_dump(SHRINK_DUMP).unwrap();
        crate::shrink::place(table.entry(2).unwrap(), 60).unwrap()
    }

    fn world_shrink() -> World {
        let mut world = world_beside();
        let table = Table::parse_dump(SHRINK_DUMP).unwrap();
        let (shrink, region) = chosen_shrink();
        world.plan.beside = Some(beside::Beside {
            start: region.start,
            sectors: region.sectors,
            esp: 1,
            table: table.snapshot(),
            shrink: Some(shrink),
        });
        world.fake.dump = Some(SHRINK_DUMP.to_string());
        world.fake.kernel_dump = Some(SHRINK_DUMP.to_string());
        let volume = 499_793_119 * 512;
        world.fake.windows.first = crate::shrink::tests::ntfs_sector(499_793_119 - 1);
        world.fake.windows.info = crate::shrink::tests::info_text(volume - 4096, 80_000);
        world
    }

    /// Nothing at all was written: the file system and the table are as
    /// they were.
    fn not_written(world: &World) {
        let fake = &world.fake;
        assert!(!fake.ran("ntfsresize --size"), "{:#?}", fake.commands);
        assert!(!fake.ran("sfdisk --lock"), "{:#?}", fake.commands);
        assert!(!fake.ran("partx"));
        assert!(!fake.ran("mkfs"));
        assert!(fake.mounted.is_empty());
        assert!(!fake.held);
        assert!(fake.whole.is_empty());
    }

    /// A word the failure must say, and what brings it about.
    type Case = (&'static str, Box<dyn Fn(&mut World)>);

    fn resizes(events: &[Event]) -> Vec<Resize> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Resize(stage) => Some(*stage),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn windows_is_made_smaller_file_system_first_then_only_its_size() {
        let mut world = world_shrink();
        let (result, events) = run(&mut world);
        assert_eq!(result.map(|_| ()), Ok(()), "{:#?}", world.fake.commands);
        let (shrink, region) = chosen_shrink();
        let bytes = shrink.file_system_bytes();
        let fake = &world.fake;
        let info = format!("ntfsresize --info --no-progress-bar {WINDOWS_NODE}");
        let dry = format!("ntfsresize --no-action --no-progress-bar --size {bytes} {WINDOWS_NODE}");
        let resize = format!("ntfsresize --size {bytes} {WINDOWS_NODE}");
        let table = "sfdisk --lock --no-reread --no-tell-kernel --wipe never --wipe-partitions never -N 2 /dev/sda";
        let partx = "partx --update --nr 2 /dev/sda";
        // Looked at twice, in the check and right before the write, each
        // time to the end; then the file system, then the table, then the
        // kernel, then the new partition.
        let order = [
            "blkid -s PARTUUID -o value /dev/sda2".to_string(),
            format!("ntfs-3g.probe --readwrite {WINDOWS_NODE}"),
            info.clone(),
            format!("ntfscat {WINDOWS_NODE} hiberfil.sys"),
            dry.clone(),
            format!("ntfs-3g.probe --readwrite {WINDOWS_NODE}"),
            info.clone(),
            format!("ntfscat {WINDOWS_NODE} hiberfil.sys"),
            dry.clone(),
            "sfdisk -d /dev/sda".to_string(),
            resize.clone(),
            format!("ntfs-3g.probe --readwrite {WINDOWS_NODE}"),
            "sfdisk -d /dev/sda".to_string(),
            table.to_string(),
            "sfdisk -d /dev/sda".to_string(),
            partx.to_string(),
            "udevadm settle".to_string(),
            "sfdisk --lock --no-reread --no-tell-kernel --wipe never --wipe-partitions always --append /dev/sda".to_string(),
            "partx --add --nr 3 /dev/sda".to_string(),
            "mkfs.ext4 -q -F -L sengi-os /dev/disk/by-partuuid/".to_string(),
        ];
        let mut at = 0;
        for command in &order {
            let found = fake.commands[at..]
                .iter()
                .position(|ran| ran.starts_with(command.as_str()))
                .unwrap_or_else(|| {
                    panic!(
                        "`{command}` was not run in its place; ran: {:#?}",
                        fake.commands
                    )
                });
            at += found + 1;
        }
        // Each write once, with what it was told; nothing is forced.
        for one in [&resize, &table.to_string(), &partx.to_string()] {
            assert_eq!(
                fake.commands.iter().filter(|c| *c == one).count(),
                1,
                "{one}"
            );
        }
        let input = |command: &str| {
            fake.inputs
                .iter()
                .find(|(ran, _)| ran == command)
                .and_then(|(_, text)| text.clone())
        };
        assert_eq!(input(&resize).as_deref(), Some("y\n"));
        assert_eq!(input(table), Some(format!("size={}\n", shrink.sectors)));
        assert_eq!(fake.whole, [resize.clone(), table.to_string()]);
        for command in &fake.commands {
            for word in command.split(' ') {
                assert!(
                    !["-f", "-ff", "--force", "-b", "--bad-sectors"].contains(&word)
                        || !command.starts_with("ntfs"),
                    "{command}"
                );
            }
            assert!(!command.starts_with("ntfsfix"), "{command}");
            assert!(!command.starts_with("sfdisk --delete"), "{command}");
        }
        // The disk is held through the look and both writes of it.
        let held = |start: &str| {
            fake.held_during
                .iter()
                .find(|(command, _)| command.starts_with(start))
                .map(|(_, held)| *held)
        };
        assert_eq!(held(&resize), Some(true));
        assert_eq!(held(table), Some(true));
        assert_eq!(held(partx), Some(false));
        // The table at the end: Windows shorter, Sengi OS after it.
        let after = Table::parse_dump(fake.dump.as_ref().unwrap()).unwrap();
        let before = Table::parse_dump(SHRINK_DUMP).unwrap();
        assert_eq!(after.entry(1), before.entry(1));
        assert_eq!(after.entry(2).unwrap().sectors, shrink.sectors);
        assert_eq!(
            after.entry(2).unwrap().start,
            before.entry(2).unwrap().start
        );
        assert_eq!(after.entry(3).unwrap().start, region.start);
        assert_eq!(after.entry(3).unwrap().sectors, region.sectors);
        // What is said, so that a window knows the state at every moment.
        assert!(events.contains(&Event::Shrinks(2)));
        assert_eq!(
            resizes(&events),
            [
                Resize::Writing,
                Resize::FileSystem,
                Resize::Table,
                Resize::Done
            ]
        );
        assert!(events.contains(&Event::Percent("shrink".to_string(), 42)));
        let begun: Vec<&str> = events
            .iter()
            .filter_map(|event| match event {
                Event::Begin(step) => Some(step.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(begun[..3], ["check", "shrink", "partition"]);
        assert_eq!(begun.len(), STEPS.len() + 1);
        assert_eq!(fake.holds, ["after-filesystem"]);
    }

    #[test]
    fn nothing_is_written_when_anything_speaks_against_it() {
        let battery = |w: &mut World| w.fake.windows.unplugged_after = Some("blkid");
        let cases: Vec<Case> = vec![
            ("charger", Box::new(battery)),
            (
                "BitLocker",
                Box::new(|w| w.fake.windows.first[3..11].copy_from_slice(b"-FVE-FS-")),
            ),
            (
                "cannot be read",
                Box::new(|w| w.fake.windows.first[3..11].fill(0x9c)),
            ),
            ("hibernated", Box::new(|w| w.fake.windows.probe = 14)),
            (
                "hibernated",
                Box::new(|w| w.fake.windows.hiberfile = Some(b"HIBR")),
            ),
            (
                "not shut down properly",
                Box::new(|w| w.fake.windows.probe = 15),
            ),
            ("errors", Box::new(|w| w.fake.windows.probe = 13)),
            ("status 18", Box::new(|w| w.fake.windows.probe = 18)),
            (
                "disk check",
                Box::new(|w| {
                    w.fake.windows.info = "ERROR: Volume is scheduled for check.".to_string()
                }),
            ),
            (
                "bad sectors",
                Box::new(|w| {
                    w.fake.windows.info = "ERROR: the disk has at least 8 bad sectors.".to_string()
                }),
            ),
            (
                "did not say how much",
                Box::new(|w| w.fake.windows.info = "All is well".to_string()),
            ),
            (
                "enough free space",
                Box::new(|w| {
                    w.fake.windows.info =
                        crate::shrink::tests::info_text(499_793_119 * 512 - 4096, 220_000)
                }),
            ),
            // Room to shrink, but not by as much as was asked.
            (
                "would be left",
                Box::new(|w| {
                    w.fake.windows.info =
                        crate::shrink::tests::info_text(499_793_119 * 512 - 4096, 170_000)
                }),
            ),
            (
                "errors",
                Box::new(|w| {
                    w.fake.windows.dry_run = Some("ERROR: Cluster allocation inconsistency!")
                }),
            ),
            ("identifier", Box::new(|w| w.fake.windows.twin = true)),
            // A dry run that does not say it went through.
            (
                "ntfsresize said",
                Box::new(|w| w.fake.windows.dry_run_silent = true),
            ),
            // Another partition of the disk holds a hibernated Windows,
            // cannot be asked, or may hold one that nobody can look into.
            (
                "hibernated",
                Box::new(|w| {
                    let ntfs = crate::shrink::tests::ntfs_sector(200_000);
                    w.fake.windows.others = vec![("/dev/sda1", ntfs)];
                    w.fake.windows.other_hiberfile = vec![("/dev/sda1", b"hibr", "")];
                }),
            ),
            (
                "could not be asked",
                Box::new(|w| {
                    let ntfs = crate::shrink::tests::ntfs_sector(200_000);
                    w.fake.windows.others = vec![("/dev/sda1", ntfs)];
                    w.fake.windows.other_hiberfile =
                        vec![("/dev/sda1", b"", "Volume is scheduled for check.\n")];
                }),
            ),
            (
                "encrypted with BitLocker",
                Box::new(|w| {
                    let mut locked = crate::shrink::tests::ntfs_sector(200_000);
                    locked[3..11].copy_from_slice(b"-FVE-FS-");
                    w.fake.windows.others = vec![("/dev/sda1", locked)];
                    w.fake.windows.data = true;
                }),
            ),
            (
                "not the partition that was chosen",
                Box::new(|w| w.fake.fails = Some("blkid -s PARTUUID -o value /dev/sda2")),
            ),
            (
                "errors",
                Box::new(|w| {
                    w.fake.windows.first = crate::shrink::tests::ntfs_sector(499_793_119 + 100)
                }),
            ),
            (
                "mounted at /mnt/c",
                Box::new(|w| w.fake.disks[0].mounted = vec!["/mnt/c".to_string()]),
            ),
            ("in use by something else", Box::new(|w| w.fake.busy = true)),
            (
                "ntfsresize is missing",
                Box::new(|w| w.fake.missing = Some("ntfsresize")),
            ),
            (
                "ntfs-3g.probe is missing",
                Box::new(|w| w.fake.missing = Some("ntfs-3g.probe")),
            ),
            ("Secure Boot", Box::new(|w| w.fake.secure_boot = true)),
            (
                "changed",
                Box::new(|w| {
                    w.fake.dump =
                        Some(SHRINK_DUMP.replace("size=   499793119", "size=   499793118"))
                }),
            ),
        ];
        for (word, change) in cases {
            let mut world = world_shrink();
            change(&mut world);
            let (result, events) = run(&mut world);
            let (step, why) = result.unwrap_err();
            assert_eq!(step, "check", "{word}: {why}");
            assert!(why.contains(word), "{word}: {why}");
            assert!(resizes(&events).is_empty(), "{word}");
            not_written(&world);
        }
    }

    #[test]
    fn what_changes_between_the_check_and_the_write_stops_it() {
        let changes: Vec<Case> = vec![
            (
                "charger",
                Box::new(|w| w.fake.windows.unplugged_after = Some("--no-action")),
            ),
            (
                "disk check",
                Box::new(|w| {
                    w.fake.windows.info_later =
                        Some("ERROR: Volume is scheduled for check.".to_string())
                }),
            ),
            (
                "partition table of the disk changed",
                Box::new(|w| {
                    w.fake.windows.table_later = Some((
                        "--no-action",
                        SHRINK_DUMP.replace("name=\"Basic data partition\"", "name=\"x\""),
                    ))
                }),
            ),
        ];
        for (word, change) in changes {
            let mut world = world_shrink();
            change(&mut world);
            let (result, events) = run(&mut world);
            let (step, why) = result.unwrap_err();
            assert_eq!(step, "shrink", "{word}: {why}");
            assert!(why.contains(word), "{word}: {why}");
            assert!(resizes(&events).is_empty());
            not_written(&world);
        }
    }

    /// How many commands with `what` in them were run.
    fn times(world: &World, what: &str) -> usize {
        let commands = &world.fake.commands;
        commands.iter().filter(|c| c.contains(what)).count()
    }

    #[test]
    fn a_disk_that_changes_is_not_opened_for_writing_again() {
        // Between the look at the other partitions and the hold: another
        // disk has the name, and the probe, which opens the volume for
        // writing, is never run on it.
        let mut world = world_shrink();
        world.fake.windows.swapped_after = Some("umount");
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains("no longer the disk"), "{why}");
        assert_eq!(times(&world, "ntfs-3g.probe"), 0);
        not_written(&world);
        // Between the check and the shrink, whose first act is to look
        // that nothing changed: no second probe.
        let mut world = world_shrink();
        let changed = SHRINK_DUMP.replace("name=\"Basic data partition\"", "name=\"x\"");
        world.fake.windows.table_later = Some(("--no-action", changed));
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "shrink", "{why}");
        assert!(why.contains("partition table of the disk changed"), "{why}");
        assert_eq!(times(&world, "ntfs-3g.probe"), 1);
        not_written(&world);
    }

    #[test]
    fn every_command_on_the_volume_names_the_held_disks_own_node() {
        // A copy of the disk that is plugged in takes over the names under
        // /dev/disk. None is used for the partition that is made smaller.
        let mut world = world_shrink();
        let (result, _) = run(&mut world);
        assert_eq!(result.map(|_| ()), Ok(()));
        let on_volume: Vec<&String> = world
            .fake
            .commands
            .iter()
            .filter(|c| c.starts_with("ntfs"))
            .collect();
        assert!(on_volume.len() >= 8, "{on_volume:#?}");
        for command in on_volume {
            assert!(command.contains(" /dev/sda2"), "{command}");
            assert!(!command.contains("/dev/disk/"), "{command}");
        }
    }

    #[test]
    fn immediately_before_each_write_the_target_is_confirmed() {
        // What appears right after the last look: before the resize, before
        // the probe that follows it, and before the table is written. A
        // copy of the disk, the held disk gone from its name, another
        // partition on the node.
        type When = (&'static str, usize);
        let moments: [(When, usize, usize); 3] = [
            // (when, resizes that ran, probes that ran)
            (("--no-action", 2), 0, 2),
            (("ntfsresize --size", 1), 1, 2),
            (("ntfs-3g.probe", 3), 1, 3),
        ];
        for (when, resized, probes) in moments {
            for (word, kind) in [("identifier", 0), ("held", 1), ("not the partition", 2)] {
                let mut world = world_shrink();
                match kind {
                    0 => world.fake.windows.twin_after = Some(when),
                    1 => world.fake.windows.gone_after = Some(when),
                    _ => world.fake.windows.moved_after = Some(when),
                }
                let (result, _) = run(&mut world);
                let (step, why) = result.unwrap_err();
                let at = format!("{word} after {when:?}: {why}");
                assert_eq!(step, "shrink", "{at}");
                assert!(why.contains(word), "{at}");
                assert_eq!(times(&world, "ntfsresize --size"), resized, "{at}");
                assert_eq!(times(&world, "ntfs-3g.probe"), probes, "{at}");
                assert!(!world.fake.ran("sfdisk --lock"), "{at}");
                assert!(!world.fake.ran("mkfs"), "{at}");
            }
        }
    }

    #[test]
    fn sector_zero_is_held_to_after_the_size_is_written_too() {
        // As Windows writes it, the size is all ones, and sfdisk puts the
        // disk's own there: the run goes through (the test of the order).
        // Anything else that changes stops it before the kernel is told.
        for tamper in [(440, 1), (458, 0x12), (446 + 20, 0x83)] {
            let mut world = world_shrink();
            world.fake.tamper_mbr = Some(tamper);
            let (result, events) = run(&mut world);
            let (step, why) = result.unwrap_err();
            assert_eq!(step, "shrink", "{tamper:?}");
            assert!(why.contains("sector 0"), "{why}");
            assert_eq!(resizes(&events).last(), Some(&Resize::Table));
            assert!(!world.fake.ran("partx"));
            assert!(!world.fake.ran("mkfs"));
        }
    }

    #[test]
    fn a_new_partition_that_is_not_where_windows_gives_up_room_is_refused_at_once() {
        let mut world = world_shrink();
        let beside = world.plan.beside.as_mut().unwrap();
        beside.start += 2048;
        let (result, _) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "check");
        assert!(why.contains("does not begin where"), "{why}");
        assert!(!world.fake.ran("ntfs"));
        not_written(&world);
    }

    #[test]
    fn the_table_as_it_was_is_put_in_the_record_before_it_is_changed() {
        let mut world = world_shrink();
        run(&mut world).0.unwrap();
        let log = &world.fake.log;
        let at = log
            .iter()
            .position(|line| line.contains("as sfdisk lists it"))
            .unwrap();
        for line in SHRINK_DUMP.lines() {
            assert!(
                log[at..].iter().any(|said| said.trim() == line.trim()),
                "{line}"
            );
        }
    }

    #[test]
    fn a_resize_that_fails_leaves_the_table_alone() {
        let mut world = world_shrink();
        world.fake.windows.resize = 1;
        let (result, events) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "shrink");
        assert!(why.contains("ntfsresize ended with status 1"), "{why}");
        // Begun, and not said to be done: the file system may be damaged.
        assert_eq!(resizes(&events), [Resize::Writing]);
        assert!(!world.fake.ran("sfdisk --lock"));
        assert!(!world.fake.ran("partx"));
        assert!(!world.fake.ran("mkfs"));
        assert!(!world.fake.held);
    }

    #[test]
    fn a_file_system_that_does_not_check_out_is_not_said_to_be_sound() {
        let changes: Vec<Case> = vec![
            (
                "beyond",
                Box::new(|w| w.fake.windows.resize_does_nothing = true),
            ),
            (
                "copy of its first sector",
                Box::new(|w| w.fake.windows.no_copy = true),
            ),
            (
                "not shut down",
                Box::new(|w| w.fake.windows.probe_after = 15),
            ),
        ];
        for (word, change) in changes {
            let mut world = world_shrink();
            change(&mut world);
            let (result, events) = run(&mut world);
            let (step, why) = result.unwrap_err();
            assert_eq!(step, "shrink", "{word}: {why}");
            assert!(why.contains("did not pass the check"), "{word}: {why}");
            assert!(why.contains(word), "{word}: {why}");
            // As far as anybody is told, the resize was under way: the
            // volume may be damaged. A test is not made to wait there.
            assert_eq!(resizes(&events), [Resize::Writing], "{word}");
            assert!(world.fake.holds.is_empty());
            assert!(!world.fake.ran("sfdisk --lock"), "{word}");
            assert!(!world.fake.ran("partx"));
            assert!(!world.fake.ran("mkfs"));
        }
    }

    #[test]
    fn what_goes_wrong_after_the_check_leaves_the_table_alone() {
        let changes: Vec<Case> = vec![
            (
                "charger",
                Box::new(|w| w.fake.windows.unplugged_after = Some("ntfsresize --size")),
            ),
            (
                "partition table of the disk changed",
                Box::new(|w| {
                    w.fake.windows.table_later = Some((
                        "ntfsresize --size",
                        SHRINK_DUMP.replace("name=\"Basic data partition\"", "name=\"x\""),
                    ))
                }),
            ),
        ];
        for (word, change) in changes {
            let mut world = world_shrink();
            change(&mut world);
            let (result, events) = run(&mut world);
            let (step, why) = result.unwrap_err();
            assert_eq!(step, "shrink", "{word}: {why}");
            assert!(why.contains(word), "{word}: {why}");
            assert_eq!(resizes(&events), [Resize::Writing, Resize::FileSystem]);
            assert!(!world.fake.ran("sfdisk --lock"), "{word}");
            assert!(!world.fake.ran("partx"));
            assert!(!world.fake.ran("mkfs"));
        }
    }

    #[test]
    fn a_table_that_does_not_come_out_as_asked_stops_everything() {
        let mut world = world_shrink();
        world.fake.windows.tamper = true;
        let (result, events) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "shrink");
        assert!(why.contains("partition 1 is not as it should be"), "{why}");
        assert_eq!(
            resizes(&events),
            [Resize::Writing, Resize::FileSystem, Resize::Table]
        );
        assert!(!world.fake.ran("partx"));
        assert!(!world.fake.ran("mkfs"));
        // sfdisk failing says the same stage.
        let mut world = world_shrink();
        world.fake.fails = Some("-N 2");
        let (result, events) = run(&mut world);
        assert_eq!(result.unwrap_err().0, "shrink");
        assert_eq!(resizes(&events).last(), Some(&Resize::Table));
        assert!(!world.fake.ran("partx"));
    }

    #[test]
    fn a_kernel_that_keeps_the_old_size_stops_before_the_new_partition() {
        let mut world = world_shrink();
        world.fake.windows.stale = true;
        let (result, events) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "shrink");
        assert!(why.contains("kernel"), "{why}");
        assert_eq!(resizes(&events).last(), Some(&Resize::Table));
        assert!(!world.fake.ran("partx --add"));
        assert!(!world.fake.ran("mkfs"));
    }

    /// A volume that ends `short` sectors before where a resize to the
    /// chosen size would leave it.
    fn already_resized(world: &mut World, short: u64) {
        let (shrink, _) = chosen_shrink();
        let sectors = (shrink.file_system_bytes() - 512) / 4096 * 8 - short;
        world.fake.windows.first = crate::shrink::tests::ntfs_sector(sectors);
        world.fake.windows.info = crate::shrink::tests::info_text(sectors * 512, 80_000);
    }

    #[test]
    fn a_file_system_that_has_the_size_already_only_has_its_partition_made_to_fit() {
        // As after an attempt that stopped between the two writes, once
        // Windows has checked the disk, with the same size chosen.
        let mut world = world_shrink();
        already_resized(&mut world, 0);
        let (result, events) = run(&mut world);
        assert_eq!(result.map(|_| ()), Ok(()));
        assert!(!world.fake.ran("ntfsresize --size"));
        assert!(!world.fake.ran("ntfsresize --no-action"));
        assert!(world.fake.ran("sfdisk --lock --no-reread --no-tell-kernel --wipe never --wipe-partitions never -N 2 /dev/sda"));
        // Nothing is said of a file system that was made smaller.
        assert_eq!(resizes(&events), [Resize::Table, Resize::Done]);
        assert!(world.fake.holds.is_empty());
    }

    #[test]
    fn a_file_system_that_ends_early_or_lacks_its_copy_is_refused_with_what_to_do() {
        let cases: Vec<Case> = vec![
            ("much smaller", Box::new(|w| already_resized(w, 8000))),
            (
                "copy of its first sector",
                Box::new(|w| {
                    already_resized(w, 0);
                    w.fake.windows.no_copy = true;
                }),
            ),
        ];
        for (word, change) in cases {
            let mut world = world_shrink();
            change(&mut world);
            let (result, events) = run(&mut world);
            let (step, why) = result.unwrap_err();
            assert_eq!(step, "check", "{word}: {why}");
            assert!(why.contains(word), "{word}: {why}");
            assert!(why.contains("Nothing was changed"), "{why}");
            assert!(why.contains("extend filesystem"), "{why}");
            assert!(resizes(&events).is_empty());
            not_written(&world);
        }
    }

    #[test]
    fn told_to_stop_during_the_resize_it_lets_it_end_and_writes_no_more() {
        let mut world = world_shrink();
        world.fake.stops_at = Some("ntfsresize --size");
        let (result, events) = run(&mut world);
        let (step, why) = result.unwrap_err();
        assert_eq!(step, "shrink");
        assert!(why.contains("told to stop"), "{why}");
        // The resize ran to its end. It was not checked, so it is not
        // said to be sound.
        assert!(world.fake.windows.resized);
        assert_eq!(resizes(&events), [Resize::Writing]);
        assert!(!world.fake.ran("sfdisk --lock"));
        assert!(!world.fake.held);
    }

    #[test]
    fn a_failure_after_windows_was_made_smaller_leaves_it_so() {
        let mut world = world_shrink();
        world.fake.fails = Some("cp -ax");
        let (result, events) = run(&mut world);
        assert_eq!(result.unwrap_err().0, "copy");
        assert_eq!(resizes(&events).last(), Some(&Resize::Done));
        // Nothing is grown back or deleted.
        let writes = world
            .fake
            .commands
            .iter()
            .filter(|c| c.starts_with("sfdisk --lock") || c.starts_with("ntfsresize --size"))
            .count();
        assert_eq!(writes, 3);
    }

    #[test]
    fn looking_at_windows_writes_nothing_and_says_what_may_be_chosen() {
        let mut world = world_shrink();
        // What the window's list would show: a full GPT disk.
        let table = Table::parse_dump(SHRINK_DUMP).unwrap();
        let disk = &mut world.fake.disks[0];
        disk.pttype = "gpt".to_string();
        disk.ptuuid = table.id.clone();
        disk.parts = table
            .entries
            .iter()
            .map(|entry| crate::disks::Part {
                name: format!("sda{}", entry.number),
                number: entry.number,
                start: entry.start,
                size: entry.sectors * 512,
                kind: entry.kind.clone(),
                uuid: entry.uuid.clone(),
                fstype: if entry.number == 1 { "vfat" } else { "ntfs" }.to_string(),
                label: String::new(),
            })
            .collect();
        let answer = analyze(&mut world.fake, "/dev/sda", false)
            .unwrap()
            .unwrap();
        assert_eq!(answer.table, table.snapshot());
        assert_eq!((answer.esp, answer.number), (1, 2));
        assert_eq!(answer.used_bytes, 80_000_000_000);
        assert_eq!(answer.room.smallest_gb, 11);
        assert!(answer.room.largest_gb > 150);
        // Nothing that opens the volume for writing: no probe, no resize,
        // not even a dry run.
        for command in &world.fake.commands {
            let program = command.split(' ').next().unwrap();
            assert!(
                ["sfdisk", "blkid", "ntfsresize", "ntfscat"].contains(&program),
                "{command}"
            );
            assert!(!command.contains("--size"), "{command}");
            assert!(!command.contains("--lock"), "{command}");
        }
        assert!(world
            .fake
            .ran(&format!("ntfscat {WINDOWS_NODE} hiberfil.sys")));
        assert!(world.fake.whole.is_empty());
        // A file system that an earlier attempt left ready for 60 GB:
        // less than that is not offered.
        let (first, info) = (
            world.fake.windows.first.clone(),
            world.fake.windows.info.clone(),
        );
        already_resized(&mut world, 0);
        let early = analyze(&mut world.fake, "/dev/sda", false)
            .unwrap()
            .unwrap();
        assert_eq!(early.room.smallest_gb, 60);
        (world.fake.windows.first, world.fake.windows.info) = (first, info);
        // Whether Windows is on the partition, which decides its name.
        assert!(answer.windows);
        world.fake.windows.data = true;
        let data = analyze(&mut world.fake, "/dev/sda", false)
            .unwrap()
            .unwrap();
        assert!(!data.windows);
        // Every NTFS partition of the disk is asked about hibernation.
        let ntfs = crate::shrink::tests::ntfs_sector(200_000);
        world.fake.windows.others = vec![("/dev/sda1", ntfs)];
        world.fake.commands.clear();
        analyze(&mut world.fake, "/dev/sda", false)
            .unwrap()
            .unwrap();
        assert!(world.fake.ran("ntfscat /dev/sda1 hiberfil.sys"));
        world.fake.windows.other_hiberfile = vec![("/dev/sda1", b"HIBR", "")];
        assert_eq!(
            analyze(&mut world.fake, "/dev/sda", false),
            Ok(Err(Refusal::Hibernated))
        );
        world.fake.windows.other_hiberfile.clear();
        // Without the charger nothing is even listed.
        world.fake.windows.unplugged_after = Some("");
        world.fake.commands = vec!["anything".to_string()];
        assert_eq!(
            analyze(&mut world.fake, "/dev/sda", false),
            Ok(Err(Refusal::Battery))
        );
        assert_eq!(world.fake.commands, ["anything"]);
        world.fake.windows.unplugged_after = None;
        // A refusal is an answer, and so is a disk in use.
        world.fake.windows.hiberfile = Some(b"hibr");
        assert_eq!(
            analyze(&mut world.fake, "/dev/sda", false),
            Ok(Err(Refusal::Hibernated))
        );
        world.fake.disks[0].mounted = vec!["/mnt".to_string()];
        assert_eq!(
            analyze(&mut world.fake, "/dev/sda", false),
            Ok(Err(Refusal::Busy))
        );
        assert!(analyze(&mut world.fake, "/dev/sdq", false).is_err());
    }

    #[test]
    fn the_programs_for_windows_stay_on_the_stick() {
        let mut world = world();
        world.target.write("usr/sbin/ntfsresize", "");
        let (result, _) = run(&mut world);
        assert!(result.is_ok());
        let target = world.target.path().display().to_string();
        let purge = format!("chroot {target} dpkg --purge ntfs-3g libntfs-3g89t64");
        let fake = &world.fake;
        assert!(
            fake.position(&purge) < fake.position(&format!("chroot {target} update-initramfs"))
        );
        // A package tool that fails leaves them, and says so.
        let mut world = self::world();
        world.target.write("usr/sbin/ntfsresize", "");
        world.fake.fails = Some("dpkg --purge");
        let (result, events) = run(&mut world);
        assert!(result.is_ok());
        assert!(events
            .iter()
            .any(|e| matches!(e, Event::Warning(text) if text.contains("left on the disk"))));
    }

    #[test]
    fn resize_progress_is_read_from_what_ntfsresize_prints() {
        assert_eq!(resize_percent(""), None);
        assert_eq!(
            resize_percent("Checking filesystem consistency ...\n 55.00 percent completed\r"),
            None
        );
        assert_eq!(
            resize_percent("x\nRelocating needed data ...\n  0.12 percent completed\r"),
            Some(0)
        );
        assert_eq!(
            resize_percent(
                "Relocating needed data ...\n  4.00 percent completed\r 99.99 percent completed\r 12"
            ),
            Some(99)
        );
        assert_eq!(resize_percent("Relocating needed data ...\n"), None);
    }
}
