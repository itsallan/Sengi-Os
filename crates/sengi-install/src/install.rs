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

use crate::disks::{self, partition_path, Disk};
use crate::edit;
use crate::names;
use crate::plan::Plan;
use crate::progress::Event;

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
        title: "Erasing the disk",
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
/// password, and the start-up scripts that find the live medium.
pub const LIVE_ONLY: [&str; 7] = [
    "usr/bin/sengi-install",
    "usr/bin/sengi-installer",
    "usr/share/applications/sengi-installer.desktop",
    "usr/share/icons/hicolor/scalable/apps/sengi-installer.svg",
    "etc/sudoers.d/live",
    "etc/initramfs-tools/hooks/sengi",
    "etc/initramfs-tools/scripts/sengi",
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
}

impl Command<'_> {
    pub fn new(argv: &[&str]) -> Command<'static> {
        Command {
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            input: Input::None,
            cleanup: false,
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

/// The machine, as far as an installation needs it.
pub trait System {
    /// Run a program to its end. `tick` is called now and then while it
    /// runs. An error is a program that could not be run at all, or an
    /// installation that was told to stop.
    fn run(&mut self, command: &Command, tick: &mut dyn FnMut()) -> Result<Output, String>;
    fn disks(&mut self, allow_loop: bool) -> Vec<Disk>;
    fn has_program(&mut self, name: &str) -> bool;
    /// Whether a device node is there.
    fn exists(&mut self, path: &Path) -> bool;
    /// Whether this machine was started by UEFI firmware.
    fn uefi(&mut self) -> bool;
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
        let output = self.sys.run(command, &mut || {})?;
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
        if self.source_bytes.saturating_add(HEADROOM_BYTES) > plan.size {
            return Err(format!(
                "the disk is too small: the system alone is {}",
                disks::size_words(self.source_bytes)
            ));
        }
        // Last, and kept until the new table is written: the disk, opened
        // so that nothing else can open it to mount it or build on it.
        // If something already has, this is where it shows, whatever the
        // tables read above said.
        self.sys.hold(&plan.device)?;
        Ok(())
    }

    /// From here on the disk is no longer what it was.
    fn partition(&mut self) -> Result<(), String> {
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
        });
        self.sys.release();
        written?;
        // Now the kernel reads it. If anything took the disk or one of
        // its old partitions in the moment between, this fails.
        self.run(&["blockdev", "--rereadpt", &device])?;
        self.run(&["udevadm", "settle", "--timeout=30"])?;
        // The kernel has the new table; the nodes follow in a moment.
        for number in [edit::BIOS_PART, edit::EFI_PART, edit::ROOT_PART] {
            let node = PathBuf::from(self.part(number));
            let mut tries = 0;
            while !self.sys.exists(&node) {
                tries += 1;
                if tries > 50 {
                    return Err(format!("{} did not appear", node.display()));
                }
                self.sys.pause(Duration::from_millis(200));
            }
        }
        // And it is the table that was asked for, as the kernel sees it,
        // before anything is formatted by partition number: a kernel
        // still on the old table would have mkfs write somewhere else.
        let seen = self.sys.partitions(&device);
        edit::table_is_as_asked(&seen, self.plan.size / 512).map_err(|why| {
            format!("the kernel does not show the new partition table on {device}: {why}")
        })
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
        let (efi, root) = (self.part(edit::EFI_PART), self.part(edit::ROOT_PART));
        self.run(&["mkfs.vfat", "-F", "32", "-n", "SENGI-EFI", &efi])?;
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
        let root = self.part(edit::ROOT_PART);
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
        let output = sys.run(&command, &mut || {
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
        let efi = self.part(edit::EFI_PART);
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
        // Host keys would be the same on every machine. The image has
        // no SSH server, and if one ever gets in, its keys stay behind.
        if let Ok(entries) = fs::read_dir(self.at("etc/ssh")) {
            for name in entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()) {
                if name.starts_with("ssh_host_") {
                    self.remove(&format!("etc/ssh/{name}"))?;
                }
            }
        }

        self.write("etc/default/grub", edit::GRUB_DEFAULT)?;
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
        for (argv, at) in [
            (vec!["mount", "--bind", "/dev"], "dev"),
            (vec!["mount", "-t", "proc", "proc"], "proc"),
            (vec!["mount", "-t", "sysfs", "sysfs"], "sys"),
        ] {
            let place = target.join(at);
            let place_text = place.to_string_lossy().into_owned();
            let mut argv = argv;
            argv.push(&place_text);
            self.mount(&argv, place.clone())?;
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

    fn boot_loader(&mut self) -> Result<(), String> {
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
        if !menu.contains(&self.root_uuid) || !menu.contains(&format!("vmlinuz-{}", self.kernel)) {
            return Err("GRUB's menu does not start the system that was installed".to_string());
        }
        for file in [
            "boot/grub/i386-pc/core.img",
            "boot/efi/EFI/BOOT/BOOTX64.EFI",
            "boot/efi/EFI/sengios/grubx64.efi",
        ] {
            if !self.at(file).is_file() {
                return Err(format!("grub-install left no /{file}"));
            }
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
        let listing = self.run(&["efibootmgr"])?;
        for entry in edit::boot_entries(&listing, edit::EFI_LABEL) {
            self.run(&[
                "efibootmgr",
                "--quiet",
                "--bootnum",
                &entry,
                "--delete-bootnum",
            ])?;
        }
        let device = self.plan.device.clone();
        let part = edit::EFI_PART.to_string();
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
                if let Err(why) = self.sys.run(&command, &mut || {}) {
                    self.sys.log(&format!("umount {place}: {why}"));
                }
            }
        }
        self.mounts.clear();
        self.sys.mounts_under(&self.paths.target)
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
    };
    installer.tell(Event::Log(paths.log.to_string_lossy().into_owned()));
    for step in &STEPS {
        installer.tell(Event::Begin(step.id.to_string()));
        let result = match step.id {
            "check" => installer.check(),
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

    /// A machine that runs nothing and remembers what it was asked.
    struct Fake {
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
        /// Each command, and whether the disk was held while it ran.
        held_during: Vec<(String, bool)>,
        held: bool,
        /// The kernel did not take the new partition table.
        stale_table: bool,
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

        fn position(&self, start: &str) -> usize {
            self.commands
                .iter()
                .position(|command| command.starts_with(start))
                .unwrap_or_else(|| panic!("{start} was not run; ran: {:#?}", self.commands))
        }
    }

    impl System for Fake {
        fn run(&mut self, command: &Command, tick: &mut dyn FnMut()) -> Result<Output, String> {
            let line = command.argv.join(" ");
            self.commands.push(line.clone());
            match &command.input {
                Input::None => {}
                Input::Text(text) => self.inputs.push((line.clone(), Some(text.to_string()))),
                Input::Secret(secret) => {
                    self.inputs.push((line.clone(), None));
                    self.secrets.push(secret.expose().to_string());
                }
            }
            tick();
            self.held_during.push((line.clone(), self.held));
            if self.stops_at.is_some_and(|what| line.contains(what)) {
                self.stopped = true;
            }
            if self.stopped && !command.cleanup {
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
            match inner[0] {
                "du" => stdout = "1500000\t/source\n".to_string(),
                "blkid" => {
                    let node = inner.last().unwrap();
                    stdout = if node.ends_with('2') {
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
                    self.touch(
                        "boot/grub/grub.cfg",
                        &format!("linux /boot/vmlinuz-{KERNEL} root=UUID={root} ro quiet\n"),
                    );
                }
                "efibootmgr" if inner.len() == 1 => stdout = self.firmware.to_string(),
                _ => {}
            }
            Ok(Output {
                code: Some(0),
                stdout,
                stderr: String::new(),
            })
        }

        fn disks(&mut self, _allow_loop: bool) -> Vec<Disk> {
            self.disks.clone()
        }

        fn has_program(&mut self, name: &str) -> bool {
            self.missing != Some(name)
        }

        fn exists(&mut self, _path: &Path) -> bool {
            true
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
        for file in TARGET_FILES.iter().chain(LIVE_ONLY.iter()) {
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
            held_during: Vec::new(),
            held: false,
            stale_table: false,
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
        assert!(!target.exists("home/live"));
        assert_eq!(target.read("etc/default/grub"), edit::GRUB_DEFAULT);
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
}
