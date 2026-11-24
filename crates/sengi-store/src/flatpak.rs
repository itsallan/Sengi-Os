//! Talking to the `flatpak` command. Every call has a time limit: a
//! network that half works makes flatpak wait for a long time, and the
//! store must stay usable meanwhile.
//!
//! Everything is done for the user's own installation (`--user`), which
//! needs no root and no polkit.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::catalog::{self, App};

/// The remote the image ships a description of.
const REMOTE: &str = "flathub";
const REPO_FILE: &str = "/etc/flatpak/remotes.d/flathub.flatpakrepo";
const REPO_URL: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// The most output of one call that is kept. The whole of Flathub's
/// listing is about 400 kB.
const MAX_OUTPUT: u64 = 16 * 1024 * 1024;

/// How long each kind of call may take.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub quick: Duration,
    pub appstream: Duration,
    pub listing: Duration,
    pub install: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            quick: Duration::from_secs(20),
            appstream: Duration::from_secs(120),
            listing: Duration::from_secs(60),
            install: Duration::from_secs(30 * 60),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// There is no `flatpak` to run.
    Missing,
    /// It did not finish in time and was stopped.
    TimedOut,
    /// It ended with an error; this is what it said, on one line.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Missing => f.write_str("Flatpak is not installed"),
            Error::TimedOut => f.write_str("Flatpak took too long and was stopped"),
            Error::Failed(text) => f.write_str(text),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Flatpak {
    program: OsString,
    limits: Limits,
}

impl Default for Flatpak {
    fn default() -> Self {
        Self::new("flatpak", Limits::default())
    }
}

/// The last thing a failed command said, without its `error:` label.
fn last_line(stderr: &[u8], stdout: &[u8]) -> String {
    for bytes in [stderr, stdout] {
        let text = String::from_utf8_lossy(bytes);
        if let Some(line) = text.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
            let line = line.strip_prefix("error:").unwrap_or(line).trim();
            return sengi_text::line(line, 200);
        }
    }
    "Flatpak failed".to_string()
}

fn drain(source: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = source.take(MAX_OUTPUT).read_to_end(&mut bytes);
        bytes
    })
}

impl Flatpak {
    pub fn new(program: impl Into<OsString>, limits: Limits) -> Self {
        Self {
            program: program.into(),
            limits,
        }
    }

    /// Runs flatpak with `args` and gives what it wrote, or says why not.
    /// After `limit` the command and anything it started are killed.
    pub fn run(&self, args: &[&str], limit: Duration) -> Result<String, Error> {
        let mut child = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Its own group, so that its helpers go when it does.
            .process_group(0)
            .spawn()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Error::Missing,
                _ => Error::Failed(sengi_text::line(&error.to_string(), 200)),
            })?;
        let pid = child.id() as libc::pid_t;
        let out = child.stdout.take().map(drain);
        let err = child.stderr.take().map(drain);

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() < limit => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => {
                    // SAFETY: killpg only sends a signal.
                    unsafe { libc::killpg(pid, libc::SIGKILL) };
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let stdout = out.and_then(|h| h.join().ok()).unwrap_or_default();
        let stderr = err.and_then(|h| h.join().ok()).unwrap_or_default();
        match status {
            None => Err(Error::TimedOut),
            Some(status) if status.success() => Ok(String::from_utf8_lossy(&stdout).into_owned()),
            Some(_) => Err(Error::Failed(last_line(&stderr, &stdout))),
        }
    }

    /// Makes sure the user has the Flathub remote.
    pub fn ensure_remote(&self) -> Result<(), Error> {
        let source = if std::path::Path::new(REPO_FILE).exists() {
            REPO_FILE
        } else {
            REPO_URL
        };
        let limit = if source == REPO_URL {
            self.limits.listing
        } else {
            self.limits.quick
        };
        self.run(
            &["--user", "remote-add", "--if-not-exists", REMOTE, source],
            limit,
        )
        .map(|_| ())
    }

    /// Fetches the newest descriptions and icons of the apps.
    pub fn refresh_appstream(&self) -> Result<(), Error> {
        self.run(&["--user", "update", "--appstream"], self.limits.appstream)
            .map(|_| ())
    }

    /// The apps of the remote. With `cached`, from what is already on
    /// the disk and without the network.
    pub fn list_remote(&self, cached: bool) -> Result<Vec<App>, Error> {
        let mut args = vec!["--user", "remote-ls"];
        if cached {
            args.push("--cached");
        }
        args.extend(["--app", "--columns=application,name,description", REMOTE]);
        self.run(&args, self.limits.listing)
            .map(|text| catalog::parse_listing(&text))
    }

    /// The apps installed, for the user or for the whole system.
    pub fn installed(&self) -> Result<HashSet<String>, Error> {
        self.run(
            &["list", "--app", "--columns=application"],
            self.limits.quick,
        )
        .map(|text| catalog::parse_installed(&text))
    }

    pub fn install(&self, id: &str) -> Result<(), Error> {
        if !catalog::valid_id(id) {
            return Err(Error::Failed("That is not an app ID".to_string()));
        }
        self.run(
            &["--user", "install", "-y", "--noninteractive", REMOTE, id],
            self.limits.install,
        )
        .map(|_| ())
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// A directory with a `flatpak` script in it that runs `body`. It
    /// writes the arguments it was called with to `calls` beside it.
    pub struct Fake {
        pub dir: PathBuf,
    }

    impl Fake {
        pub fn new(tag: &str, body: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("sengi-store-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let script = dir.join("flatpak");
            fs::write(
                &script,
                format!(
                    "#!/bin/sh\necho \"$*\" >>\"{}/calls\"\n{body}\n",
                    dir.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir }
        }

        pub fn flatpak(&self, limits: Limits) -> Flatpak {
            Flatpak::new(self.dir.join("flatpak"), limits)
        }

        pub fn calls(&self) -> Vec<String> {
            fs::read_to_string(self.dir.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    pub fn quick() -> Limits {
        Limits {
            quick: Duration::from_millis(500),
            appstream: Duration::from_millis(500),
            listing: Duration::from_millis(500),
            install: Duration::from_millis(500),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn listing_is_read_from_the_output() {
        let fake = Fake::new(
            "list",
            "printf 'org.gimp.GIMP\\tGIMP\\tImages\\norg.x.Y\\tY\\t\\n'",
        );
        let apps = fake.flatpak(quick()).list_remote(true).unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, "GIMP");
        assert_eq!(
            fake.calls(),
            ["--user remote-ls --cached --app --columns=application,name,description flathub"]
        );
    }

    #[test]
    fn install_asks_for_the_user_installation_without_questions() {
        let fake = Fake::new("install", "exit 0");
        fake.flatpak(quick()).install("org.gimp.GIMP").unwrap();
        assert_eq!(
            fake.calls(),
            ["--user install -y --noninteractive flathub org.gimp.GIMP"]
        );
    }

    #[test]
    fn a_bad_id_never_reaches_flatpak() {
        let fake = Fake::new("badid", "exit 0");
        assert!(fake.flatpak(quick()).install("--system").is_err());
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn installed_apps_are_listed() {
        let fake = Fake::new("installed", "printf 'org.gimp.GIMP\\n'");
        let set = fake.flatpak(quick()).installed().unwrap();
        assert!(set.contains("org.gimp.GIMP"));
    }

    #[test]
    fn an_error_is_one_clean_line() {
        let fake = Fake::new(
            "fail",
            "echo 'Looking for matches' >&2; echo 'error: Unable to load summary: Could not connect' >&2; exit 1",
        );
        let error = fake.flatpak(quick()).list_remote(false).unwrap_err();
        assert_eq!(
            error,
            Error::Failed("Unable to load summary: Could not connect".to_string())
        );
    }

    #[test]
    fn a_call_that_hangs_is_stopped() {
        let fake = Fake::new("hang", "sleep 30");
        let limits = Limits {
            install: Duration::from_millis(200),
            ..quick()
        };
        let started = Instant::now();
        let error = fake.flatpak(limits).install("org.gimp.GIMP").unwrap_err();
        assert_eq!(error, Error::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_helper_that_keeps_the_pipe_open_does_not_hang_it() {
        let fake = Fake::new("helper", "sleep 30 &\nsleep 30");
        let limits = Limits {
            quick: Duration::from_millis(200),
            ..quick()
        };
        let started = Instant::now();
        assert_eq!(fake.flatpak(limits).installed(), Err(Error::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn no_flatpak_is_said_plainly() {
        let flatpak = Flatpak::new("/nonexistent/flatpak", quick());
        assert_eq!(flatpak.installed(), Err(Error::Missing));
    }
}
