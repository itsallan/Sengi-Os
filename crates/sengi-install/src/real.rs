//! The machine for real: programs are run, the disks are the disks.
//!
//! Every program is started with a short fixed PATH and an environment
//! of nothing else, in a process group of its own so that it and what it
//! starts can be ended together. What it prints is kept for the log,
//! which never sees what was written to it.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::disks::{self, Disk};
use crate::install::{Command, Input, Output, System};

/// Where programs are looked for, and all the programs started here are
/// told.
pub const PATH: &str = "/usr/sbin:/usr/bin:/sbin:/bin";

/// Set by a signal that means "stop". The program that is running is
/// ended and the installation fails where it is, which unmounts.
pub static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// The most that is kept of what one program prints, per stream.
const KEPT_BYTES: usize = 64 * 1024;

pub struct Real {
    log: Option<File>,
    started: Instant,
}

impl Real {
    /// `log` is started afresh, readable by everyone: nothing secret is
    /// written to it, and the person whose installation failed is not
    /// root.
    pub fn new(log: &Path) -> Real {
        if let Some(parent) = log.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o644)
            .open(log);
        if let Err(error) = &file {
            eprintln!("sengi-install: no log at {}: {error}", log.display());
        }
        Real {
            log: file.ok(),
            started: Instant::now(),
        }
    }
}

/// Everything `stream` says, of which the first `KEPT_BYTES` are kept.
/// Read to the end either way, so the program never blocks on a full
/// pipe.
fn drain(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let room = KEPT_BYTES.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..count.min(room)]);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}

/// A mount point as the kernel's table writes it, with its escapes for
/// space, tab, newline and backslash undone.
fn unescape(field: &str) -> String {
    let mut out = String::new();
    let mut rest = field;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        let code = rest.get(at + 1..at + 4);
        match code.and_then(|code| u8::from_str_radix(code, 8).ok()) {
            Some(byte) => {
                out.push(byte as char);
                rest = &rest[at + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The mount points at or under `path` in a table of the form of
/// /proc/self/mountinfo, the last mounted first.
pub fn mounts_under(mountinfo: &str, path: &Path) -> Vec<PathBuf> {
    let mut under: Vec<PathBuf> = mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(|place| PathBuf::from(unescape(place)))
        .filter(|place| place.starts_with(path))
        .collect();
    under.reverse();
    under
}

impl System for Real {
    fn run(&mut self, command: &Command, tick: &mut dyn FnMut()) -> Result<Output, String> {
        let Some((program, args)) = command.argv.split_first() else {
            return Err("there is no program to run".to_string());
        };
        self.log(&format!("$ {}", command.argv.join(" ")));
        if INTERRUPTED.load(Ordering::Relaxed) && !command.cleanup {
            return Err("the installation was told to stop".to_string());
        }
        let has_input = !matches!(command.input, Input::None);
        let mut child = std::process::Command::new(program)
            .args(args)
            .env_clear()
            .env("PATH", PATH)
            .env("LC_ALL", "C")
            .stdin(if has_input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|error| format!("{program} could not be started: {error}"))?;
        let stdout = child.stdout.take().map(drain);
        let stderr = child.stderr.take().map(drain);
        if let Some(mut stdin) = child.stdin.take() {
            let bytes = match &command.input {
                Input::None => &[][..],
                Input::Text(text) => text.as_bytes(),
                Input::Secret(secret) => secret.expose().as_bytes(),
            };
            // A program that ends without reading is found out by its
            // status, not by this.
            let _ = stdin.write_all(bytes);
        }

        let mut waited = 0u32;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => {}
                Err(error) => break Err(format!("{program} was lost: {error}")),
            }
            if INTERRUPTED.load(Ordering::Relaxed) && !command.cleanup {
                // The whole group: a script's children too.
                // SAFETY: kill takes a process group and a signal.
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                let _ = child.wait();
                break Err("the installation was told to stop".to_string());
            }
            std::thread::sleep(Duration::from_millis(50));
            waited += 1;
            if waited.is_multiple_of(10) {
                tick();
            }
        };
        let collect = |handle: Option<std::thread::JoinHandle<String>>| {
            handle
                .and_then(|handle| handle.join().ok())
                .unwrap_or_default()
        };
        let output = Output {
            code: status.as_ref().ok().and_then(|status| status.code()),
            stdout: collect(stdout),
            stderr: collect(stderr),
        };
        for line in output.stdout.lines().chain(output.stderr.lines()) {
            self.log(&format!("  {line}"));
        }
        let status = status?;
        if !status.success() {
            self.log(&format!("  ({status})"));
        }
        Ok(output)
    }

    fn disks(&mut self, allow_loop: bool) -> Vec<Disk> {
        disks::scan_system(allow_loop)
    }

    fn has_program(&mut self, name: &str) -> bool {
        PATH.split(':')
            .any(|dir| Path::new(dir).join(name).is_file())
    }

    fn exists(&mut self, path: &Path) -> bool {
        path.exists()
    }

    fn uefi(&mut self) -> bool {
        Path::new("/sys/firmware/efi").is_dir()
    }

    fn mounts_under(&mut self, path: &Path) -> Vec<PathBuf> {
        let table = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
        mounts_under(&table, path)
    }

    fn random_id(&mut self) -> Result<String, String> {
        let mut bytes = [0u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut source| source.read_exact(&mut bytes))
            .map_err(|error| format!("no random numbers for a machine identifier: {error}"))?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    fn pause(&mut self, time: Duration) {
        std::thread::sleep(time);
    }

    fn log(&mut self, line: &str) {
        if let Some(log) = &mut self.log {
            let _ = writeln!(log, "[{:7.1}] {line}", self.started.elapsed().as_secs_f32());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use sengi_system::secret::Secret;

    fn real(tmp: &TempDir) -> Real {
        Real::new(&tmp.join("install.log"))
    }

    #[test]
    fn a_program_is_run_and_what_it_says_comes_back() {
        let tmp = TempDir::new();
        let mut sys = real(&tmp);
        let command = Command::new(&["sh", "-c", "echo out; echo err >&2; exit 3"]);
        let output = sys.run(&command, &mut || {}).unwrap();
        assert_eq!(output.code, Some(3));
        assert_eq!(output.stdout, "out\n");
        assert_eq!(output.stderr, "err\n");
        let log = tmp.read("install.log");
        assert!(log.contains("$ sh -c echo out"));
        assert!(log.contains("  out\n") && log.contains("  err\n"));
        assert!(log.contains("exit status: 3"));
    }

    #[test]
    fn what_is_written_to_a_program_reaches_it_and_not_the_log() {
        let tmp = TempDir::new();
        let mut sys = real(&tmp);
        let secret = Secret::new("allan:hunter2\n".to_string());
        let command = Command {
            argv: vec!["sh".to_string(), "-c".to_string(), "wc -c".to_string()],
            input: Input::Secret(&secret),
            cleanup: false,
        };
        let output = sys.run(&command, &mut || {}).unwrap();
        assert_eq!(output.stdout.trim(), "14");
        assert!(!tmp.read("install.log").contains("hunter2"));
        let command = Command {
            argv: vec!["cat".to_string()],
            input: Input::Text("label: gpt\n"),
            cleanup: false,
        };
        assert_eq!(
            sys.run(&command, &mut || {}).unwrap().stdout,
            "label: gpt\n"
        );
    }

    #[test]
    fn programs_get_a_fixed_path_and_nothing_else() {
        let tmp = TempDir::new();
        let mut sys = real(&tmp);
        let output = sys.run(&Command::new(&["env"]), &mut || {}).unwrap();
        let mut lines: Vec<&str> = output.stdout.lines().collect();
        lines.sort();
        assert_eq!(lines, ["LC_ALL=C", "PATH=/usr/sbin:/usr/bin:/sbin:/bin"]);
    }

    #[test]
    fn a_program_that_is_not_there_is_an_error() {
        let tmp = TempDir::new();
        let mut sys = real(&tmp);
        let problem = sys
            .run(&Command::new(&["/no/such/program"]), &mut || {})
            .unwrap_err();
        assert!(problem.contains("could not be started"));
        assert!(sys.has_program("sh"));
        assert!(!sys.has_program("no-such-program-anywhere"));
    }

    #[test]
    fn a_long_program_ticks_and_a_noisy_one_does_not_block() {
        let tmp = TempDir::new();
        let mut sys = real(&tmp);
        let mut ticks = 0;
        let command = Command::new(&[
            "sh",
            "-c",
            "head -c 300000 /dev/zero | tr '\\0' x; sleep 1.2",
        ]);
        let output = sys.run(&command, &mut || ticks += 1).unwrap();
        assert_eq!(output.code, Some(0));
        assert!(ticks >= 1);
        assert_eq!(output.stdout.len(), KEPT_BYTES);
    }

    #[test]
    fn mount_points_under_a_directory_are_found_deepest_first() {
        let table = "\
22 1 0:20 / /sys rw - sysfs sysfs rw
40 30 8:3 / /run/sengi-install/target rw - ext4 /dev/sda3 rw
41 40 8:2 / /run/sengi-install/target/boot/efi rw - vfat /dev/sda2 rw
42 40 0:5 / /run/sengi-install/target/dev rw - devtmpfs udev rw
43 30 8:17 / /run/sengi-install/target-other rw - ext4 /dev/sdb1 rw
44 30 8:18 / /mnt/with\\040space rw - ext4 /dev/sdb2 rw
";
        let under = mounts_under(table, Path::new("/run/sengi-install/target"));
        assert_eq!(
            under,
            [
                PathBuf::from("/run/sengi-install/target/dev"),
                PathBuf::from("/run/sengi-install/target/boot/efi"),
                PathBuf::from("/run/sengi-install/target"),
            ]
        );
        assert_eq!(
            mounts_under(table, Path::new("/mnt")),
            [PathBuf::from("/mnt/with space")]
        );
        assert!(mounts_under("", Path::new("/")).is_empty());
        assert_eq!(unescape("a\\134b\\zz"), "a\\b\\zz");
    }

    #[test]
    fn a_machine_identifier_is_thirty_two_hexadecimal_digits() {
        let tmp = TempDir::new();
        let mut sys = real(&tmp);
        let id = sys.random_id().unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, sys.random_id().unwrap());
    }
}
