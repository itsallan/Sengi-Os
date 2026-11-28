//! Talking to the `flatpak` command. Every call has a time limit: a
//! network that half works makes flatpak wait for a long time, and the
//! store must stay usable meanwhile.
//!
//! Everything is changed in the user's own installation (`--user`), which
//! needs no root and no polkit. What the system has installed is only read.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use sengi_text::progress::{Progress, Reader};

use crate::catalog::{self, App, Installed};

/// The remote the image ships a description of.
const REMOTE: &str = "flathub";
const REPO_FILE: &str = "/etc/flatpak/remotes.d/flathub.flatpakrepo";
const REPO_URL_BASE: &str = "https://dl.flathub.org/repo/";
const REPO_URL: &str = "https://dl.flathub.org/repo/flathub.flatpakrepo";

/// No call goes on longer than this, however much it prints.
const HARD_LIMIT: Duration = Duration::from_secs(4 * 3600);

/// How often progress is looked at, which is also how often it is shown.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// How long the output of a call that has ended is waited for. A helper
/// it left behind may hold the pipes open.
const DRAIN_WAIT: Duration = Duration::from_secs(2);

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
    pub remove: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            quick: Duration::from_secs(20),
            appstream: Duration::from_secs(10 * 60),
            listing: Duration::from_secs(60),
            // How long an install may go without a word from flatpak, not
            // how long it may take: it prints as long as data arrives, and
            // gives up on a dead connection by itself.
            install: Duration::from_secs(10 * 60),
            // Flatpak says little while it removes and tidies its store.
            remove: Duration::from_secs(5 * 60),
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
    /// The user's Flathub source is not the real one: nothing is fetched.
    Refused(String),
    /// The user stopped it, or the window was closed.
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Missing => f.write_str("Flatpak is not installed"),
            Error::TimedOut => f.write_str("Flatpak took too long and was stopped"),
            Error::Failed(text) | Error::Refused(text) => f.write_str(text),
            Error::Cancelled => f.write_str("Flatpak was stopped"),
        }
    }
}

/// Asks one call to stop. Clones ask the same call.
#[derive(Clone, Debug, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// How a call may be stopped before it ends.
#[derive(Clone, Copy)]
enum Stop<'a> {
    /// By closing the window.
    WithWindow,
    /// By closing the window, or by the user.
    OnRequest(&'a Cancel),
    /// Only by its time limit: a removal is left to finish.
    Never,
}

/// The calls under way that closing the window stops.
#[derive(Debug, Default)]
struct Running {
    /// Their process groups.
    groups: Vec<libc::pid_t>,
    /// The window is closed: nothing of the kind is started any more.
    closed: bool,
}

#[derive(Clone, Debug)]
pub struct Flatpak {
    program: OsString,
    limits: Limits,
    running: Arc<Mutex<Running>>,
}

impl Default for Flatpak {
    fn default() -> Self {
        Self::new("flatpak", Limits::default())
    }
}

/// Whether the user's `flathub` remote is Flathub, with its signatures
/// checked. `listing` is `flatpak remotes --columns=name,url,options`.
pub fn check_remote(listing: &str) -> Result<(), String> {
    for line in listing.lines() {
        let mut columns = line.split('\t');
        if columns.next().map(str::trim) != Some(REMOTE) {
            continue;
        }
        let url = columns.next().unwrap_or("").trim().trim_end_matches('/');
        let options = columns.next().unwrap_or("");
        if url != REPO_URL_BASE.trim_end_matches('/') {
            return Err(
                "This user's \"flathub\" source is not Flathub. Fix it with flatpak remotes"
                    .to_string(),
            );
        }
        if options.contains("no-gpg-verify") {
            return Err(
                "This user's Flathub source does not check signatures. Fix it with flatpak remotes"
                    .to_string(),
            );
        }
        return Ok(());
    }
    Err("The Flathub source is missing".to_string())
}

/// The last thing a failed command said, without its `error:` label.
fn last_line(stderr: &[u8], stdout: &[u8]) -> String {
    for bytes in [stderr, stdout] {
        let text = String::from_utf8_lossy(bytes);
        if let Some(line) = text.lines().rev().map(str::trim).find(|l| !l.is_empty()) {
            let line = line.strip_prefix("error:").unwrap_or(line).trim();
            // The reason comes last, after an address that can be long.
            return sengi_text::line(line, 600);
        }
    }
    "Flatpak failed".to_string()
}

/// What a reader thread has got so far, and whether it has reached the end.
#[derive(Default)]
struct Pipe {
    bytes: Mutex<Vec<u8>>,
    done: AtomicBool,
}

/// Reads a stream to its end on a thread, noting when something came, and
/// shows it to the `reader` of progress if there is one.
fn drain(
    mut source: impl Read + Send + 'static,
    last_word: Arc<Mutex<Instant>>,
    reader: Option<Arc<Mutex<Reader>>>,
) -> (Arc<Pipe>, std::thread::JoinHandle<()>) {
    let pipe = Arc::new(Pipe::default());
    let mine = pipe.clone();
    let handle = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(n) = source.read(&mut chunk) {
            if n == 0 {
                break;
            }
            *last_word.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
            if let Some(reader) = &reader {
                reader
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .feed(&chunk[..n]);
            }
            let mut bytes = mine.bytes.lock().unwrap_or_else(|e| e.into_inner());
            if (bytes.len() as u64) < MAX_OUTPUT {
                bytes.extend_from_slice(&chunk[..n]);
            }
        }
        mine.done.store(true, Ordering::Release);
    });
    (pipe, handle)
}

fn text(pipe: &Option<(Arc<Pipe>, std::thread::JoinHandle<()>)>) -> Vec<u8> {
    pipe.as_ref()
        .map(|(p, _)| p.bytes.lock().unwrap_or_else(|e| e.into_inner()).clone())
        .unwrap_or_default()
}

impl Flatpak {
    pub fn new(program: impl Into<OsString>, limits: Limits) -> Self {
        Self {
            program: program.into(),
            limits,
            running: Arc::default(),
        }
    }

    fn spawn(&self, args: &[&str]) -> std::io::Result<std::process::Child> {
        let mut tries = 0;
        loop {
            let spawned = Command::new(&self.program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // Its own group, so that its helpers go when it does.
                .process_group(0)
                .spawn();
            // A script that was written a moment ago may still be open for
            // writing in another thread's fork.
            match spawned {
                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 20 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(25));
                }
                other => return other,
            }
        }
    }

    fn running(&self) -> MutexGuard<'_, Running> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Kills every call under way, and what they started, and starts no
    /// more. Used when the window is closed. A removal is not touched.
    pub fn stop_all(&self) {
        let mut running = self.running();
        running.closed = true;
        for pid in &running.groups {
            // SAFETY: killpg only sends a signal.
            unsafe { libc::killpg(*pid, libc::SIGKILL) };
        }
    }

    /// Runs flatpak with `args` and gives what it wrote, or says why not.
    /// It is stopped, with anything it started, if it says nothing for
    /// `limit` (or has gone on for hours).
    pub fn run(&self, args: &[&str], limit: Duration) -> Result<String, Error> {
        self.run_with(args, limit, None)
    }

    /// As `run`, and tells `on_progress` how far flatpak says it has got,
    /// at most a few times a second and only when that has changed.
    pub fn run_with(
        &self,
        args: &[&str],
        limit: Duration,
        on_progress: Option<&dyn Fn(Progress)>,
    ) -> Result<String, Error> {
        self.call(args, limit, on_progress, Stop::WithWindow)
    }

    fn call(
        &self,
        args: &[&str],
        limit: Duration,
        on_progress: Option<&dyn Fn(Progress)>,
        stop: Stop<'_>,
    ) -> Result<String, Error> {
        let cancelled = || matches!(stop, Stop::OnRequest(cancel) if cancel.cancelled());
        let stoppable = !matches!(stop, Stop::Never);
        let (mut child, pid) = {
            // Held while it starts, so that `stop_all` misses nothing.
            let mut running = self.running();
            if cancelled() || (stoppable && running.closed) {
                return Err(Error::Cancelled);
            }
            let child = self.spawn(args).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => Error::Missing,
                _ => Error::Failed(sengi_text::line(&error.to_string(), 200)),
            })?;
            let pid = child.id() as libc::pid_t;
            if stoppable {
                running.groups.push(pid);
            }
            (child, pid)
        };
        let started = Instant::now();
        let last_word = Arc::new(Mutex::new(started));
        let reader = on_progress.map(|_| Arc::new(Mutex::new(Reader::default())));
        let out = child
            .stdout
            .take()
            .map(|s| drain(s, last_word.clone(), reader.clone()));
        let err = child
            .stderr
            .take()
            .map(|s| drain(s, last_word.clone(), None));

        let mut reported = Progress::default();
        let mut last_look = Instant::now()
            .checked_sub(PROGRESS_EVERY)
            .unwrap_or_else(Instant::now);
        let mut asked = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if let (Some(report), Some(reader)) = (on_progress, &reader) {
                        if last_look.elapsed() >= PROGRESS_EVERY {
                            last_look = Instant::now();
                            let now = reader
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .now()
                                .clone();
                            if !now.is_empty() && now != reported {
                                reported = now.clone();
                                report(now);
                            }
                        }
                    }
                    let quiet = last_word
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .elapsed();
                    asked = cancelled();
                    if !asked && quiet < limit && started.elapsed() < HARD_LIMIT {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                }
                Err(_) => {}
            }
            // The whole group at once, while the parent is still there.
            // SAFETY: killpg only sends a signal.
            unsafe { libc::killpg(pid, libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
            break None;
        };
        // Whatever it left behind goes too, so the pipes close.
        // SAFETY: killpg only sends a signal.
        unsafe { libc::killpg(pid, libc::SIGKILL) };
        let closed = {
            let mut running = self.running();
            running.groups.retain(|p| *p != pid);
            running.closed
        };

        // A helper that left its group may still hold the pipes: the output
        // is waited for a little, and then taken as it is.
        let waited = Instant::now();
        while waited.elapsed() < DRAIN_WAIT
            && [&out, &err].iter().any(|p| {
                p.as_ref()
                    .is_some_and(|(p, _)| !p.done.load(Ordering::Acquire))
            })
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        let stdout = text(&out);
        let stderr = text(&err);
        match status {
            None if asked => Err(Error::Cancelled),
            None => Err(Error::TimedOut),
            Some(status) if status.success() => Ok(String::from_utf8_lossy(&stdout).into_owned()),
            // `stop_all` got to it first.
            Some(status) if stoppable && closed && status.signal() == Some(libc::SIGKILL) => {
                Err(Error::Cancelled)
            }
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
        )?;
        // --if-not-exists keeps whatever is there under that name.
        let remotes = self.run(
            &["--user", "remotes", "--columns=name,url,options"],
            self.limits.quick,
        )?;
        check_remote(&remotes).map_err(Error::Refused)
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
    pub fn installed(&self) -> Result<Installed, Error> {
        self.run(
            &["list", "--app", "--columns=application,installation"],
            self.limits.quick,
        )
        .map(|text| catalog::parse_installed(&text))
    }

    pub fn install(&self, id: &str) -> Result<(), Error> {
        self.install_with(id, None, &Cancel::default())
    }

    /// Installs, and reports progress while it goes. `cancel` ends flatpak
    /// and whatever it started; the next install carries on from what had
    /// arrived.
    pub fn install_with(
        &self,
        id: &str,
        on_progress: Option<&dyn Fn(Progress)>,
        cancel: &Cancel,
    ) -> Result<(), Error> {
        if !catalog::valid_id(id) {
            return Err(Error::Failed("That is not an app ID".to_string()));
        }
        // -y: every question is answered yes. With --noninteractive as
        // well, flatpak would not say how far it has got.
        let target = format!("app/{id}");
        self.call(
            &["--user", "install", "-y", REMOTE, &target],
            self.limits.install,
            on_progress,
            Stop::OnRequest(cancel),
        )
        .map(|_| ())
    }

    /// Removes an app, with all its branches, from the user's own
    /// installation. What the app has saved (`~/.var/app/<id>`) is kept.
    /// Closing the window does not stop this: half a removal is worse than
    /// a late exit.
    pub fn uninstall(&self, id: &str) -> Result<(), Error> {
        if !catalog::valid_id(id) {
            return Err(Error::Failed("That is not an app ID".to_string()));
        }
        // By its ID alone flatpak asks which branch is meant, even with
        // -y, so each is named.
        let listed = self.call(
            &["--user", "list", "--app", "--columns=ref"],
            self.limits.quick,
            None,
            Stop::Never,
        )?;
        let refs = catalog::parse_refs(&listed, id);
        if refs.is_empty() {
            return Err(Error::Failed(
                "It is not installed for this user".to_string(),
            ));
        }
        let mut args = vec!["--user", "uninstall", "-y", "--noninteractive", "--app"];
        args.extend(refs.iter().map(String::as_str));
        self.call(&args, self.limits.remove, None, Stop::Never)
            .map(|_| ())
    }

    /// Removes the runtimes and extensions of the user's installation that
    /// flatpak itself says nothing uses. One the user installed by name is
    /// pinned by flatpak and stays.
    pub fn remove_unused(&self) -> Result<(), Error> {
        self.call(
            &["--user", "uninstall", "--unused", "-y", "--noninteractive"],
            self.limits.remove,
            None,
            Stop::Never,
        )
        .map(|_| ())
    }

    /// Whether the app is open now.
    pub fn is_running(&self, id: &str) -> Result<bool, Error> {
        self.run(&["ps", "--columns=application"], self.limits.quick)
            .map(|text| text.lines().any(|line| line.trim() == id))
    }

    /// The command that starts an installed app, for the user's own
    /// installation, with nothing connected to the Store.
    fn open_command(&self, id: &str) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(["run", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // Its own group, so that closing the Store does not take it along.
            .process_group(0);
        command
    }

    /// Starts an installed app and returns at once. The app is not
    /// waited for, and is not among the calls `stop_all` kills.
    pub fn open(&self, id: &str) -> Result<(), Error> {
        if !catalog::valid_id(id) {
            return Err(Error::Failed("That is not an app ID".to_string()));
        }
        let mut child = self
            .open_command(id)
            .spawn()
            .map_err(|e| Error::Failed(format!("could not start it: {e}")))?;
        // Reaped on a thread of its own, so it does not stay a zombie.
        let _ = std::thread::Builder::new()
            .name("store-open".into())
            .spawn(move || {
                let _ = child.wait();
            });
        Ok(())
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
            quick: Duration::from_secs(5),
            appstream: Duration::from_secs(5),
            listing: Duration::from_secs(5),
            install: Duration::from_secs(5),
            remove: Duration::from_secs(5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::problem::Problem;
    use std::fs;

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
            ["--user install -y flathub app/org.gimp.GIMP"]
        );
    }

    #[test]
    fn progress_is_reported_but_not_at_every_line() {
        let fake = Fake::new(
            "progress-throttle",
            "echo 'Installing 2/4…'\n\
             i=1; while [ $i -le 30 ]; do printf 'Installing 2/4… ##  %s%%  2.0 MB/s\\n' $i; i=$((i+1)); done\n\
             sleep 1",
        );
        let seen = std::sync::Mutex::new(Vec::new());
        let report = |p: Progress| seen.lock().unwrap().push(p);
        fake.flatpak(quick())
            .run_with(&["x"], Duration::from_secs(5), Some(&report))
            .unwrap();
        let seen = seen.into_inner().unwrap();
        assert!(
            !seen.is_empty() && seen.len() <= 5,
            "{} reports",
            seen.len()
        );
        let last = seen.last().unwrap();
        assert_eq!(last.percent, Some(30));
        assert_eq!(last.step, Some((2, 4)));
    }

    /// The lines are the ones a real flatpak ended with.
    #[test]
    fn a_full_disk_and_a_lost_network_are_still_told_apart() {
        for (file, expected) in [
            ("disk-minimum", Problem::Space),
            ("disk-full", Problem::Space),
            ("network-gone", Problem::Offline),
            ("network-stall", Problem::Slow),
        ] {
            let fake = Fake::new(
                file,
                &format!(
                    "cat '{}/../sengi-text/tests/fixtures/{file}.err' >&2; exit 1",
                    env!("CARGO_MANIFEST_DIR")
                ),
            );
            let report = |_: Progress| {};
            let error = fake
                .flatpak(quick())
                .install_with("org.gimp.GIMP", Some(&report), &Cancel::default())
                .unwrap_err();
            assert_eq!(Problem::of(&error), expected, "{file}: {error}");
        }
    }

    /// The address of an object on Flathub is long, and the reason comes
    /// after it.
    #[test]
    fn a_long_error_keeps_its_reason() {
        let object = "a".repeat(62);
        let fake = Fake::new(
            "long-error",
            &format!(
                "echo 'error: Failed to install org.freedesktop.Platform.GL.default: While pulling runtime/org.freedesktop.Platform.GL.default/x86_64/24.08extra from remote flathub: While fetching https://dl.flathub.org/repo/objects/aa/{object}.filez: [6] Could not resolve hostname' >&2; exit 1"
            ),
        );
        let error = fake.flatpak(quick()).installed().unwrap_err();
        assert_eq!(Problem::of(&error), Problem::Offline, "{error}");
    }

    #[test]
    fn open_runs_the_app_by_its_id_with_nothing_attached() {
        let flatpak = Flatpak::new("flatpak", quick());
        let command = flatpak.open_command("org.gimp.GIMP");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["run", "org.gimp.GIMP"]);
        assert_eq!(command.get_program(), "flatpak");
    }

    #[test]
    fn open_does_not_wait_for_the_app() {
        let fake = Fake::new("open-detached", "sleep 3");
        let flatpak = fake.flatpak(quick());
        let started = std::time::Instant::now();
        flatpak.open("org.gimp.GIMP").unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        for _ in 0..100 {
            if fake.calls().iter().any(|c| c == "run org.gimp.GIMP") {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("flatpak run was not called");
    }

    #[test]
    fn open_refuses_what_is_not_an_app_id() {
        let flatpak = Flatpak::new("flatpak", quick());
        assert!(flatpak.open("-x").is_err());
        assert!(flatpak.open("../x").is_err());
    }

    #[test]
    fn a_bad_id_never_reaches_flatpak() {
        let fake = Fake::new("badid", "exit 0");
        assert!(fake.flatpak(quick()).install("--system").is_err());
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn installed_apps_are_listed() {
        let fake = Fake::new("installed", "printf 'org.gimp.GIMP\\tuser\\n'");
        let installed = fake.flatpak(quick()).installed().unwrap();
        assert!(installed.user.contains("org.gimp.GIMP"));
        assert_eq!(
            fake.calls(),
            ["list --app --columns=application,installation"]
        );
    }

    #[test]
    fn removal_is_for_the_user_installation_and_keeps_the_data() {
        let fake = Fake::new(
            "remove",
            "case \"$*\" in *--columns=ref) printf 'org.gimp.GIMP/x86_64/stable\\n';; esac",
        );
        let flatpak = fake.flatpak(quick());
        flatpak.uninstall("org.gimp.GIMP").unwrap();
        flatpak.remove_unused().unwrap();
        assert!(flatpak.uninstall("--system").is_err());
        // Not in the user's installation: nothing is run to remove it.
        assert!(flatpak.uninstall("org.x.Y").is_err());
        assert_eq!(
            fake.calls(),
            [
                "--user list --app --columns=ref",
                "--user uninstall -y --noninteractive --app org.gimp.GIMP/x86_64/stable",
                "--user uninstall --unused -y --noninteractive",
                "--user list --app --columns=ref",
            ]
        );
    }

    /// The helper stays in the group, as flatpak's own do.
    #[test]
    fn cancel_ends_the_whole_group() {
        let fake = Fake::new(
            "cancel-group",
            "sleep 30 &\necho $! >\"$(dirname \"$0\")/helper\"\nwait",
        );
        let flatpak = fake.flatpak(quick());
        let cancel = Cancel::default();
        let worker = {
            let (flatpak, cancel) = (flatpak.clone(), cancel.clone());
            std::thread::spawn(move || flatpak.install_with("org.gimp.GIMP", None, &cancel))
        };
        let helper = (0..200)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(25));
                fs::read_to_string(fake.dir.join("helper"))
                    .ok()?
                    .trim()
                    .parse::<u32>()
                    .ok()
            })
            .expect("the helper started");
        let started = Instant::now();
        cancel.cancel();
        assert_eq!(worker.join().unwrap(), Err(Error::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2));
        // Gone, or dead and not yet reaped by whoever has it now.
        let gone = (0..200).any(|_| {
            std::thread::sleep(Duration::from_millis(10));
            fs::read_to_string(format!("/proc/{helper}/stat"))
                .map_or(true, |stat| stat.contains(") Z "))
        });
        assert!(gone, "the helper is alive");
    }

    #[test]
    fn a_call_cancelled_before_it_starts_is_not_made() {
        let fake = Fake::new("cancel-early", "exit 0");
        let cancel = Cancel::default();
        cancel.cancel();
        let result = fake
            .flatpak(quick())
            .install_with("org.gimp.GIMP", None, &cancel);
        assert_eq!(result, Err(Error::Cancelled));
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn after_closing_only_a_removal_is_started() {
        let fake = Fake::new("closed", "exit 0");
        let flatpak = fake.flatpak(quick());
        flatpak.stop_all();
        assert_eq!(flatpak.install("org.gimp.GIMP"), Err(Error::Cancelled));
        assert_eq!(flatpak.installed(), Err(Error::Cancelled));
        flatpak.remove_unused().unwrap();
        assert_eq!(fake.calls().len(), 1);
    }

    /// What Flatpak 1.16 printed with one app open.
    #[test]
    fn an_open_app_is_told_from_a_closed_one() {
        let fake = Fake::new(
            "ps",
            &format!("cat '{}/tests/fixtures/ps.out'", env!("CARGO_MANIFEST_DIR")),
        );
        let flatpak = fake.flatpak(quick());
        assert_eq!(flatpak.is_running("org.sengi.test.Hello"), Ok(true));
        assert_eq!(flatpak.is_running("org.sengi.test.Other"), Ok(false));
        assert_eq!(fake.calls()[0], "ps --columns=application");
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
    fn a_helper_left_behind_after_flatpak_ends_does_not_hang_it() {
        // The parent says its piece and exits; a helper in another group
        // keeps both pipes open for a long time.
        let fake = Fake::new(
            "left",
            "echo 'org.gimp.GIMP'\nsetsid sleep 30 </dev/null &\nexit 0",
        );
        let started = Instant::now();
        let set = fake.flatpak(quick()).installed().unwrap();
        assert!(set.all.contains("org.gimp.GIMP"));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_call_that_keeps_talking_is_not_stopped_for_being_slow() {
        let fake = Fake::new(
            "talk",
            "for i in 1 2 3 4 5 6 7 8; do echo $i; sleep 0.15; done",
        );
        let limits = Limits {
            install: Duration::from_millis(500),
            ..quick()
        };
        assert!(fake.flatpak(limits).install("org.gimp.GIMP").is_ok());
    }

    #[test]
    fn closing_the_window_stops_what_is_running() {
        let fake = Fake::new("stop", "sleep 30");
        let flatpak = fake.flatpak(quick());
        let worker = {
            let flatpak = flatpak.clone();
            std::thread::spawn(move || flatpak.install("org.gimp.GIMP"))
        };
        std::thread::sleep(Duration::from_millis(500));
        let started = Instant::now();
        flatpak.stop_all();
        assert_eq!(worker.join().unwrap(), Err(Error::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_user_remote_that_is_not_flathub_is_refused() {
        let good = "flathub\thttps://dl.flathub.org/repo/\tuser\nother\thttp://x\t\n";
        assert!(check_remote(good).is_ok());
        assert!(check_remote("flathub\thttps://dl.flathub.org/repo\t\n").is_ok());
        assert!(check_remote("flathub\thttps://evil.example/repo/\tuser\n").is_err());
        assert!(
            check_remote("flathub\thttps://dl.flathub.org/repo/\tuser,no-gpg-verify\n").is_err()
        );
        assert!(check_remote("other\thttp://x\t\n").is_err());
    }

    #[test]
    fn the_remote_is_added_and_then_checked() {
        let fake = Fake::new(
            "remote",
            "case \"$*\" in *remotes*) printf 'flathub\\thttps://dl.flathub.org/repo/\\tuser\\n';; esac",
        );
        fake.flatpak(quick()).ensure_remote().unwrap();
        let calls = fake.calls();
        assert!(calls[0].starts_with("--user remote-add --if-not-exists flathub "));
        assert_eq!(calls[1], "--user remotes --columns=name,url,options");
    }

    #[test]
    fn no_flatpak_is_said_plainly() {
        let flatpak = Flatpak::new("/nonexistent/flatpak", quick());
        assert_eq!(flatpak.installed(), Err(Error::Missing));
    }
}
