//! Cancelling and removing with a real flatpak, through the Store's own
//! `Flatpak`. It installs apps, so it runs only when asked for, against the
//! repository `make-repo.sh` builds, which the test serves itself:
//!
//!     sh crates/sengi-store/tests/make-repo.sh /tmp/sengi-repo
//!     SENGI_TEST_REPO=/tmp/sengi-repo/repo \
//!         cargo test -p sengi-store --test cancel_remove -- \
//!         --ignored --nocapture --test-threads=1
//!
//! Each test has a user installation of its own, in a throwaway directory
//! under TMPDIR that takes the place of HOME. Opening an app needs a
//! session with XDG_RUNTIME_DIR and unprivileged user namespaces.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sengi_store::flatpak::{Cancel, Error, Flatpak, Limits};
use sengi_text::progress::Progress;

const HELLO: &str = "org.sengi.test.Hello";
const OTHER: &str = "org.sengi.test.Other";
/// The app with a stable and a beta branch.
const BRANCHED: &str = "org.sengi.test.Branched";
const PLATFORM: &str = "org.sengi.test.Platform";
/// The small files make-repo.sh puts in the runtime and in each app.
const MANY: usize = 1500;

/// Bytes a second, shared by every connection, so that an install takes
/// long enough to be watched.
const SLOW: u64 = 6_000_000;
const FAST: u64 = 400_000_000;

fn answer(stream: TcpStream, repo: &Path, rate: u64, next: &Mutex<Instant>) {
    // Thousands of small files are asked for: each answer goes at once.
    let _ = stream.set_nodelay(true);
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut requests = BufReader::new(stream);
    loop {
        let mut first = String::new();
        if requests.read_line(&mut first).unwrap_or(0) == 0 {
            return;
        }
        let mut line = String::new();
        while requests.read_line(&mut line).unwrap_or(0) > 2 {
            line.clear();
        }
        let path = first.split_whitespace().nth(1).unwrap_or("/");
        let path = path.split('?').next().unwrap_or("").trim_start_matches('/');
        let body = (!path.contains(".."))
            .then(|| fs::read(repo.join(path)).ok())
            .flatten();
        let Some(body) = body else {
            if out
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .is_err()
            {
                return;
            }
            continue;
        };
        let mut reply =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        reply.extend_from_slice(&body);
        for piece in reply.chunks(32 * 1024) {
            let at = {
                let mut next = next.lock().unwrap();
                let at = (*next).max(Instant::now());
                *next = at + Duration::from_secs_f64(piece.len() as f64 / rate as f64);
                at
            };
            std::thread::sleep(at.saturating_duration_since(Instant::now()));
            if out.write_all(piece).is_err() {
                return;
            }
        }
    }
}

/// Serves the repository on this machine, and gives its address.
fn serve(repo: PathBuf, rate: u64) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/", listener.local_addr().unwrap());
    let next = Arc::new(Mutex::new(Instant::now()));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (repo, next) = (repo.clone(), next.clone());
            std::thread::spawn(move || answer(stream, &repo, rate, &next));
        }
    });
    address
}

/// A user installation of its own, with the test repository as `flathub`.
struct User {
    dir: PathBuf,
}

impl User {
    fn new(tag: &str, rate: u64) -> Option<Self> {
        let Some(repo) = std::env::var_os("SENGI_TEST_REPO") else {
            eprintln!("skipped: SENGI_TEST_REPO is not set");
            return None;
        };
        let dir = std::env::temp_dir().join(format!("sengi-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        let script = dir.join("flatpak");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nunset XDG_DATA_HOME XDG_CACHE_HOME XDG_CONFIG_HOME\n\
                 HOME='{}' exec flatpak \"$@\"\n",
                dir.join("home").display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let user = Self { dir };
        let address = serve(repo.into(), rate);
        user.command(&[
            "--user",
            "remote-add",
            "--no-gpg-verify",
            "flathub",
            &address,
        ]);
        Some(user)
    }

    /// The Store's own way to flatpak, as a Store just started has it.
    fn store(&self) -> Flatpak {
        Flatpak::new(self.dir.join("flatpak"), Limits::default())
    }

    fn command(&self, args: &[&str]) -> String {
        let output = Command::new(self.dir.join("flatpak"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "flatpak {args:?}: {text}");
        text
    }

    fn installation(&self) -> PathBuf {
        self.dir.join("home/.local/share/flatpak")
    }

    fn desktop_file(&self, id: &str) -> PathBuf {
        self.installation()
            .join(format!("exports/share/applications/{id}.desktop"))
    }

    /// Every ref installed, runtimes too.
    fn refs(&self) -> String {
        self.command(&["--user", "list", "--all", "--columns=ref"])
    }

    /// The processes that have this user's HOME: whatever flatpak started.
    fn processes(&self) -> Vec<String> {
        let home = format!("HOME={}", self.dir.join("home").display());
        fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter(|entry| {
                fs::read(entry.path().join("environ"))
                    .is_ok_and(|env| env.split(|b| *b == 0).any(|var| var == home.as_bytes()))
            })
            .filter_map(|entry| fs::read_to_string(entry.path().join("cmdline")).ok())
            .map(|line| line.replace('\0', " "))
            .collect()
    }

    /// What a half done install may leave: directories that are not whole
    /// deployments, and staged downloads.
    fn leftovers(&self) -> Vec<String> {
        let mut found = Vec::new();
        for kind in ["app", "runtime"] {
            let pattern = self.installation().join(kind);
            for name in list(&pattern) {
                for arch in list(&name) {
                    for branch in list(&arch) {
                        let active = branch.join("active").exists();
                        for entry in list(&branch) {
                            let file = entry.file_name().unwrap().to_string_lossy().into_owned();
                            if file.starts_with('.') || (!active && file != "active") {
                                found.push(entry.display().to_string());
                            }
                        }
                        if !active && list(&branch).is_empty() {
                            found.push(format!("{} (empty)", branch.display()));
                        }
                    }
                }
            }
        }
        for dir in ["repo/tmp", "repo/state", ".removed"] {
            for entry in list(&self.installation().join(dir)) {
                if entry.file_name().is_some_and(|name| name != "cache") {
                    found.push(entry.display().to_string());
                }
            }
        }
        let prefix = format!("{}/", self.installation().display());
        found.iter().map(|f| f.replace(&prefix, "")).collect()
    }

    /// The app is whole: listed as the user's, in the menus, with all its
    /// files, and it starts.
    fn assert_works(&self, id: &str) {
        let installed = self.store().installed().unwrap();
        assert!(installed.user.contains(id), "{installed:?}");
        assert!(self.desktop_file(id).exists(), "no desktop file");
        for (kind, name) in [("app", id), ("runtime", PLATFORM)] {
            let arch = list(&self.installation().join(kind).join(name));
            let files = arch[0].join("stable/active/files");
            let many = files.join("share/filler/many");
            let count = fs::read_dir(many).map_or(0, |entries| entries.count());
            assert_eq!(count, MANY, "{kind} {name}");
        }
        self.command(&["run", id, "0"]);
    }

    fn remove_everything(&self) {
        let store = self.store();
        for id in store.installed().unwrap().user {
            store.uninstall(&id).unwrap();
        }
        store.remove_unused().unwrap();
        assert!(!self.refs().contains("org.sengi"), "{}", self.refs());
    }
}

impl Drop for User {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// What is in a directory, without the link to an app's current branch.
/// A directory that is not there, or is a link, has nothing in it.
fn list(dir: &Path) -> Vec<PathBuf> {
    if dir.is_symlink() {
        return Vec::new();
    }
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    found.retain(|path| path.file_name().is_some_and(|name| name != "current"));
    found.sort();
    found
}

/// Installs on a thread and cancels when `when` first says so. Gives what
/// the call came to and how long it took to end after the cancel.
fn install_and_cancel(
    user: &User,
    id: &'static str,
    when: impl Fn(&Progress, Duration) -> bool,
) -> (Result<(), Error>, Option<Duration>) {
    let store = user.store();
    let cancel = Cancel::default();
    let latest = Arc::new(Mutex::new(Progress::default()));
    let worker = {
        let (store, cancel, latest) = (store.clone(), cancel.clone(), latest.clone());
        std::thread::spawn(move || {
            let report = |progress: Progress| *latest.lock().unwrap() = progress;
            store.install_with(id, Some(&report), &cancel)
        })
    };
    let started = Instant::now();
    let mut at = None;
    while !worker.is_finished() {
        if at.is_none() && when(&latest.lock().unwrap(), started.elapsed()) {
            cancel.cancel();
            at = Some(Instant::now());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let result = worker.join().unwrap();
    (result, at.map(|at| at.elapsed()))
}

/// Waits a moment for processes that were killed to be gone.
fn assert_nothing_is_left_running(user: &User) {
    for _ in 0..300 {
        if user.processes().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("still running: {:?}", user.processes());
}

fn cancel_in_step(step: u32, tag: &str) {
    let Some(user) = User::new(tag, SLOW) else {
        return;
    };
    let (result, took) = install_and_cancel(&user, HELLO, |progress, _| {
        progress.step == Some((step, 2)) && progress.percent >= Some(30)
    });
    assert_eq!(result, Err(Error::Cancelled));
    println!("step {step}: ended {:?} after the cancel", took.unwrap());
    assert!(took.unwrap() < Duration::from_secs(2));
    assert_nothing_is_left_running(&user);
    println!("refs after the cancel:\n{}", user.refs());
    println!("left behind: {:#?}", user.leftovers());
    assert!(!user.store().installed().unwrap().all.contains(HELLO));
    assert!(!user.desktop_file(HELLO).exists());

    // As the Store does it again: all of it, with progress.
    let seen = Mutex::new(Vec::new());
    let report = |progress: Progress| seen.lock().unwrap().push(progress);
    let started = Instant::now();
    user.store()
        .install_with(HELLO, Some(&report), &Cancel::default())
        .unwrap();
    let seen = seen.into_inner().unwrap();
    println!(
        "installed again in {:.1}s, steps {:?}",
        started.elapsed().as_secs_f32(),
        seen.last().and_then(|p| p.step)
    );
    user.assert_works(HELLO);
    println!("left after it: {:#?}", user.leftovers());
    user.remove_everything();
}

#[test]
#[ignore = "installs from a real repository"]
fn cancel_while_the_runtime_arrives_then_install_again() {
    cancel_in_step(1, "cancel-runtime");
}

#[test]
#[ignore = "installs from a real repository"]
fn cancel_while_the_app_arrives_then_install_again() {
    cancel_in_step(2, "cancel-app");
}

/// Cancelled at moments spread over the whole of an install, and at the
/// moments a deployment appears on the disk, one after the other in the
/// same installation. SENGI_TEST_ROUNDS says how many moments.
#[test]
#[ignore = "installs from a real repository"]
fn cancel_at_any_moment_never_stops_the_next_install() {
    let Some(user) = User::new("cancel-anywhere", FAST) else {
        return;
    };
    let started = Instant::now();
    user.store().install(HELLO).unwrap();
    let whole = started.elapsed();
    user.remove_everything();
    println!("a whole install takes {:.2}s", whole.as_secs_f32());

    let rounds: u32 = std::env::var("SENGI_TEST_ROUNDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(40);
    let installation = user.installation();
    let runtime = installation.join(format!("runtime/{PLATFORM}"));
    let app = installation.join(format!("app/{HELLO}"));
    // A file or directory of a deployment under way, or done.
    let branch = |root: &Path| list(root).first().map(|arch| arch.join("stable"));
    let marks: [(&str, &dyn Fn() -> bool); 5] = [
        ("the runtime is being checked out", &|| {
            branch(&runtime).is_some_and(|b| !list(&b).is_empty())
        }),
        ("the runtime is active", &|| {
            branch(&runtime).is_some_and(|b| b.join("active").exists())
        }),
        ("the app is being checked out", &|| {
            branch(&app).is_some_and(|b| !list(&b).is_empty())
        }),
        ("the app is active", &|| {
            branch(&app).is_some_and(|b| b.join("active").exists())
        }),
        ("the app is in the menus", &|| {
            user.desktop_file(HELLO).is_symlink()
        }),
    ];

    for round in 0..rounds + marks.len() as u32 * 4 {
        let (what, result) = if round < rounds {
            let delay = whole * round / rounds;
            let (result, _) = install_and_cancel(&user, HELLO, |_, passed| passed >= delay);
            (format!("after {:.2}s", delay.as_secs_f32()), result)
        } else {
            let (name, mark) = marks[(round - rounds) as usize % marks.len()];
            let (result, _) = install_and_cancel(&user, HELLO, |_, _| mark());
            (format!("when {name}"), result)
        };
        assert_nothing_is_left_running(&user);
        let there = user.store().installed().unwrap().all.contains(HELLO);
        println!(
            "round {round}, {what}: {result:?}, installed {there}, left {:?}",
            user.leftovers()
        );
        // If the Store says it is installed it had better be whole; if
        // not, installing it again has to work.
        if !there {
            let again = user.store().install(HELLO);
            assert_eq!(again, Ok(()), "{what}");
        }
        user.assert_works(HELLO);
        let left = user.leftovers();
        if !left.is_empty() {
            println!("round {round}: still there once installed {left:?}");
        }
        user.remove_everything();
    }
    println!("left at the end: {:#?}", user.leftovers());
    println!("{}", user.command(&["--user", "repair", "--dry-run"]));
}

#[test]
#[ignore = "installs from a real repository"]
fn closing_the_store_mid_install_leaves_a_next_one_free_to_run() {
    let Some(user) = User::new("close", SLOW) else {
        return;
    };
    let store = user.store();
    let downloading = Arc::new(Mutex::new(false));
    let worker = {
        let (store, downloading) = (store.clone(), downloading.clone());
        std::thread::spawn(move || {
            let report = |progress: Progress| {
                if progress.percent >= Some(20) {
                    *downloading.lock().unwrap() = true;
                }
            };
            store.install_with(HELLO, Some(&report), &Cancel::default())
        })
    };
    while !*downloading.lock().unwrap() {
        assert!(!worker.is_finished());
        std::thread::sleep(Duration::from_millis(10));
    }
    store.stop_all();
    assert_eq!(worker.join().unwrap(), Err(Error::Cancelled));
    assert_nothing_is_left_running(&user);
    println!("left behind: {:#?}", user.leftovers());
    assert_eq!(store.install(HELLO), Err(Error::Cancelled));
    user.store().install(HELLO).unwrap();
    user.assert_works(HELLO);
}

#[test]
#[ignore = "installs from a real repository"]
fn remove_keeps_the_data_and_what_others_use() {
    let Some(user) = User::new("remove", FAST) else {
        return;
    };
    let store = user.store();
    store.install(HELLO).unwrap();
    store.install(OTHER).unwrap();
    let data = user
        .dir
        .join(format!("home/.var/app/{HELLO}/data/mine.txt"));
    fs::create_dir_all(data.parent().unwrap()).unwrap();
    fs::write(&data, "kept").unwrap();

    store.uninstall(HELLO).unwrap();
    store.remove_unused().unwrap();
    let installed = store.installed().unwrap();
    assert!(!installed.all.contains(HELLO) && installed.user.contains(OTHER));
    assert!(!user.desktop_file(HELLO).exists());
    assert!(user.desktop_file(OTHER).exists());
    assert_eq!(fs::read_to_string(&data).unwrap(), "kept");
    // The other app still uses the runtime.
    assert!(user.refs().contains(PLATFORM), "{}", user.refs());
    user.assert_works(OTHER);

    store.uninstall(OTHER).unwrap();
    assert!(user.refs().contains(PLATFORM), "{}", user.refs());
    store.remove_unused().unwrap();
    assert!(!user.refs().contains("org.sengi"), "{}", user.refs());
    assert_eq!(fs::read_to_string(&data).unwrap(), "kept");

    // Removing what is not there is an error, in flatpak's words.
    let error = store.uninstall(HELLO).unwrap_err();
    println!("removing what is not there: {error}");

    // And it can come back, to its data.
    store.install(HELLO).unwrap();
    user.assert_works(HELLO);
    assert_eq!(fs::read_to_string(&data).unwrap(), "kept");
}

#[test]
#[ignore = "installs from a real repository"]
fn every_branch_of_an_app_is_removed() {
    let Some(user) = User::new("remove-branches", FAST) else {
        return;
    };
    let store = user.store();
    for branch in ["stable", "beta"] {
        user.command(&[
            "--user",
            "install",
            "-y",
            "--noninteractive",
            "flathub",
            &format!("app/{BRANCHED}//{branch}"),
        ]);
    }
    assert!(user.refs().contains("/beta") && user.refs().contains("/stable"));
    store.uninstall(BRANCHED).unwrap();
    store.remove_unused().unwrap();
    assert!(!user.refs().contains("org.sengi"), "{}", user.refs());
}

/// Why the Store does not remove an open app: flatpak does, without a word,
/// and removing the same app again while that copy is open fails half way.
#[test]
#[ignore = "installs from a real repository"]
fn flatpak_removes_an_open_app_and_then_trips_over_it() {
    let Some(user) = User::new("remove-open", FAST) else {
        return;
    };
    let store = user.store();
    store.install(HELLO).unwrap();
    // Flatpak goes on listing an app for a few seconds after it has ended,
    // as the tests before this one may just have.
    let comes_to = |open: bool| {
        (0..300).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            store.is_running(HELLO) == Ok(open)
        })
    };
    assert!(comes_to(false), "an app is listed that is not there");
    let mut app = Command::new(user.dir.join("flatpak"))
        .args(["run", HELLO, "60"])
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    assert!(comes_to(true), "the app did not start");

    let removed = store.uninstall(HELLO);
    println!("uninstall while open: {removed:?}");
    removed.unwrap();
    store.remove_unused().unwrap();
    assert!(!store.installed().unwrap().all.contains(HELLO));
    assert!(!user.desktop_file(HELLO).exists());
    std::thread::sleep(Duration::from_millis(500));
    assert!(app.try_wait().unwrap().is_none(), "the app was closed");
    println!("still listed as running: {:?}", store.is_running(HELLO));
    println!("refs: {:?}", user.refs());
    println!("left behind: {:#?}", user.leftovers());

    // Installed again while the old one is still open, and removed again.
    store.install(HELLO).unwrap();
    user.assert_works(HELLO);
    let again = store.uninstall(HELLO);
    println!("uninstall again while the first copy is open: {again:?}");
    println!("left behind: {:#?}", user.leftovers());
    assert!(
        matches!(&again, Err(Error::Failed(text)) if text.contains("Directory not empty")),
        "flatpak has changed: {again:?}"
    );

    // Ending the process that started it leaves the sandbox running.
    user.command(&["kill", HELLO]);
    app.wait().unwrap();
}
