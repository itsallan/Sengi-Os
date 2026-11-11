//! Turning an Exec value into the argument list to run. Nothing here goes
//! through a shell: the value is split and expanded by the rules of the
//! Desktop Entry specification and executed directly.

use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::desktop::DesktopEntry;
use crate::env::Env;

/// Terminal entries run inside this.
pub const TERMINAL: [&str; 2] = ["foot", "-e"];
/// Puts the app in its own systemd scope so the focused app can be prioritised.
pub const RUNNER: &str = "sengi-run";

/// Why an Exec line cannot be run.
#[derive(Debug, PartialEq, Eq)]
pub enum ExecError {
    /// There is no command: no Exec line, or nothing left of it.
    Empty,
    /// A quote was opened and never closed.
    UnclosedQuote,
    /// The program is a path that does not start at the root.
    RelativePath,
    /// The named program is not installed.
    NotFound(String),
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecError::Empty => write!(f, "the Exec line has no command"),
            ExecError::UnclosedQuote => write!(f, "the Exec line has an unclosed quote"),
            ExecError::RelativePath => write!(f, "the Exec line names a relative path"),
            ExecError::NotFound(program) => write!(f, "{program} is not installed"),
        }
    }
}

impl std::error::Error for ExecError {}

/// Split an Exec value into arguments.
///
/// Double quotes follow the spec: inside them a backslash escapes `"`, `` ` ``,
/// `$` and `\`. Single quotes and a backslash outside quotes are not in the
/// spec but appear in real files, and are read the way GLib reads them.
pub fn split(exec: &str) -> Result<Vec<String>, ExecError> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = exec.chars();

    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        None => return Err(ExecError::UnclosedQuote),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            None => return Err(ExecError::UnclosedQuote),
                            Some(e @ ('"' | '`' | '$' | '\\')) => current.push(e),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                        },
                        Some(other) => current.push(other),
                    }
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        None => return Err(ExecError::UnclosedQuote),
                        Some('\'') => break,
                        Some(other) => current.push(other),
                    }
                }
            }
            '\\' => {
                started = true;
                current.push(chars.next().unwrap_or('\\'));
            }
            other => {
                started = true;
                current.push(other);
            }
        }
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

/// Expand the field codes in `args`. There are never files or URLs to pass,
/// so `%f %F %u %U` vanish, as do the deprecated codes, and an argument
/// that was nothing but such codes disappears. `%%` is a literal percent.
///
/// Codes are expanded in every argument, whether or not it was quoted in
/// the Exec line, as GLib does. Real files depend on it: `app "%U"`,
/// `--name="%c"`. [`command_line`] passes every argument through here.
pub fn expand(args: &[String], entry: &DesktopEntry) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    for arg in args {
        if arg == "%i" {
            if !entry.icon.is_empty() {
                out.push("--icon".to_owned());
                out.push(entry.icon.clone());
            }
            continue;
        }
        let mut expanded = String::with_capacity(arg.len());
        let mut chars = arg.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                expanded.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => expanded.push('%'),
                Some('c') => expanded.push_str(&entry.name),
                Some('k') => expanded.push_str(&entry.path.to_string_lossy()),
                // File and URL codes, %i inside a longer argument, deprecated
                // and unknown codes all expand to nothing.
                Some(_) => {}
                None => expanded.push('%'),
            }
        }
        // An argument that was only a field code disappears; `--name=%c`
        // style arguments stay even if they end up short.
        if !expanded.is_empty() || !is_only_field_codes(arg) {
            out.push(expanded);
        }
    }
    out
}

fn is_only_field_codes(arg: &str) -> bool {
    let mut chars = arg.chars();
    let mut any = false;
    while let Some(c) = chars.next() {
        if c != '%' || chars.next().is_none() {
            return false;
        }
        any = true;
    }
    any
}

/// The entry's own command: its Exec line split and expanded.
fn own_command(entry: &DesktopEntry) -> Result<Vec<String>, ExecError> {
    let command = expand(&split(&entry.exec)?, entry);
    if command.first().is_none_or(|program| program.is_empty()) {
        return Err(ExecError::Empty);
    }
    // Absolute paths and bare names only, as the spec says: anything else
    // would run whatever sits at that path under the working directory.
    if command[0].contains('/') && !Path::new(&command[0]).is_absolute() {
        return Err(ExecError::RelativePath);
    }
    Ok(command)
}

/// The full command for an entry: its Exec line, inside a terminal if it asks
/// for one, started through `sengi-run` when that is installed.
pub fn command_line(entry: &DesktopEntry, env: &Env) -> Result<Vec<String>, ExecError> {
    let command = own_command(entry)?;
    let mut argv = Vec::with_capacity(command.len() + 3);
    if let Some(runner) = env.find_program(RUNNER) {
        argv.push(runner.to_string_lossy().into_owned());
    }
    if entry.terminal {
        argv.extend(TERMINAL.iter().map(|s| s.to_string()));
    }
    argv.extend(command);
    Ok(argv)
}

/// `command_line`, after making sure the program it names exists.
///
/// Starting goes through `sengi-run` and perhaps a terminal, and those start
/// fine whatever they are asked to run. Without this check a desktop file
/// pointing at a removed program would look like a successful launch.
pub fn launchable(entry: &DesktopEntry, env: &Env) -> Result<Vec<String>, ExecError> {
    let command = own_command(entry)?;
    let mut needed = vec![command[0].as_str()];
    if entry.terminal {
        needed.push(TERMINAL[0]);
    }
    for program in needed {
        if env.find_program(program).is_none() {
            return Err(ExecError::NotFound(program.to_owned()));
        }
    }
    command_line(entry, env)
}

/// The name of the program an Exec line starts, without its directory:
/// `/usr/bin/env FOO=1 /opt/app/bin/app --flag` gives `app`.
pub fn program_name(exec: &str) -> Option<String> {
    let words = split(exec).ok()?;
    let mut words = words.iter().map(String::as_str);
    let base = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();

    let first = words.next()?;
    if base(first) != "env" {
        return Some(base(first)).filter(|name| !name.is_empty());
    }
    words
        .find(|word| !word.contains('=') && !word.starts_with('-'))
        .map(base)
        .filter(|name| !name.is_empty())
}

/// Start `argv` and let go of it. Does not wait for the program.
///
/// The program is forked twice, so it is never the caller's child: when it
/// exits, init collects it and a long-lived caller such as the dock is left
/// with no zombie to reap. It gets a session of its own, so nothing aimed at
/// the caller reaches it and it outlives the caller. It starts in
/// `working_dir` if that is a directory, with no standard input or output.
pub fn spawn(argv: &[String], working_dir: Option<&Path>) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;

    let (program, args) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = working_dir.filter(|dir| dir.is_dir()) {
        command.current_dir(dir);
    }
    // SAFETY: the closure runs in the forked child before exec and only
    // makes async-signal-safe calls: fork, setsid and _exit.
    unsafe {
        command.pre_exec(|| match libc::fork() {
            -1 => Err(std::io::Error::last_os_error()),
            0 => {
                libc::setsid();
                Ok(())
            }
            _ => libc::_exit(0),
        });
    }
    // The child seen from here is the short-lived middle process. A failed
    // exec in the grandchild is still reported through spawn().
    let mut middle = command.spawn()?;
    let _ = middle.wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::ffi::OsString;

    fn entry(exec: &str) -> DesktopEntry {
        let mut entry = DesktopEntry::named("app", "My App", exec);
        entry.icon = "app-icon".into();
        entry
    }

    fn env_with_path(path: &str) -> Env {
        let path = OsString::from(path);
        Env::from_vars(move |name| (name == "PATH").then(|| path.clone()))
    }

    fn no_runner() -> Env {
        env_with_path("/nonexistent/bin")
    }

    fn split_ok(exec: &str) -> Vec<String> {
        split(exec).expect("splits")
    }

    #[test]
    fn splits_on_whitespace() {
        assert_eq!(split_ok("app"), ["app"]);
        assert_eq!(
            split_ok("  app   --flag\tvalue  "),
            ["app", "--flag", "value"]
        );
        assert!(split_ok("").is_empty());
        assert!(split_ok("   ").is_empty());
    }

    #[test]
    fn double_quotes_group_and_unescape() {
        assert_eq!(split_ok(r#"app "two words" x"#), ["app", "two words", "x"]);
        assert_eq!(split_ok(r#"app "say \"hi\"""#), ["app", r#"say "hi""#]);
        assert_eq!(
            split_ok(r#"app "\$HOME \`id\` \\ \n""#),
            ["app", r"$HOME `id` \ \n"]
        );
        assert_eq!(split_ok(r#"app """#), ["app", ""]);
        assert_eq!(
            split_ok(r#"app pre"mid dle"post"#),
            ["app", "premid dlepost"]
        );
        assert_eq!(
            split_ok(r#""/opt/My App/run" --go"#),
            ["/opt/My App/run", "--go"]
        );
    }

    #[test]
    fn single_quotes_and_bare_backslashes_are_tolerated() {
        assert_eq!(
            split_ok(r#"sh -c 'echo "a b" $X'"#),
            ["sh", "-c", r#"echo "a b" $X"#]
        );
        assert_eq!(split_ok(r"app one\ arg"), ["app", "one arg"]);
        assert_eq!(split_ok(r"app trailing\"), ["app", r"trailing\"]);
    }

    #[test]
    fn shell_syntax_is_never_interpreted() {
        assert_eq!(
            split_ok("app; rm -rf ~ && $(reboot) | `id` > /tmp/x"),
            [
                "app;",
                "rm",
                "-rf",
                "~",
                "&&",
                "$(reboot)",
                "|",
                "`id`",
                ">",
                "/tmp/x"
            ]
        );
    }

    #[test]
    fn unclosed_quotes_are_errors() {
        assert_eq!(split(r#"app "open"#), Err(ExecError::UnclosedQuote));
        assert_eq!(split("app 'open"), Err(ExecError::UnclosedQuote));
        assert_eq!(split(r#"app "ends with \"#), Err(ExecError::UnclosedQuote));
    }

    fn expanded(exec: &str) -> Vec<String> {
        expand(&split_ok(exec), &entry(exec))
    }

    #[test]
    fn file_and_url_codes_are_dropped() {
        assert_eq!(expanded("app %f"), ["app"]);
        assert_eq!(expanded("app %F %u %U"), ["app"]);
        assert_eq!(
            expanded("app --open %U --flag"),
            ["app", "--open", "--flag"]
        );
        assert_eq!(expanded("app --file=%f"), ["app", "--file="]);
        assert_eq!(expanded("app %d %D %n %N %v %m %z"), ["app"]);
    }

    #[test]
    fn icon_name_path_and_percent_codes_expand() {
        assert_eq!(expanded("app %i"), ["app", "--icon", "app-icon"]);
        assert_eq!(expanded("app %c"), ["app", "My App"]);
        assert_eq!(expanded("app --title=%c"), ["app", "--title=My App"]);
        assert_eq!(
            expanded("app %k"),
            ["app", "/usr/share/applications/app.desktop"]
        );
        assert_eq!(expanded("app 100%% %%f"), ["app", "100%", "%f"]);
        assert_eq!(expanded("app 50%"), ["app", "50%"]);
        assert_eq!(expanded(r#"app "" x"#), ["app", "", "x"]);
    }

    #[test]
    fn icon_code_without_an_icon_adds_nothing() {
        let mut e = entry("app %i --x");
        e.icon.clear();
        assert_eq!(expand(&split_ok(&e.exec), &e), ["app", "--x"]);
    }

    #[test]
    fn expansion_results_are_not_split_again() {
        let mut e = entry("app %c");
        e.name = "Name with \"quotes\" and spaces".into();
        assert_eq!(
            expand(&split_ok(&e.exec), &e),
            ["app", "Name with \"quotes\" and spaces"]
        );
    }

    #[test]
    fn command_line_without_runner_runs_directly() {
        let argv = command_line(&entry("app --new-window %U"), &no_runner()).unwrap();
        assert_eq!(argv, ["app", "--new-window"]);
    }

    #[test]
    fn terminal_entries_run_inside_foot() {
        let mut e = entry("htop -d 10");
        e.terminal = true;
        assert_eq!(
            command_line(&e, &no_runner()).unwrap(),
            ["foot", "-e", "htop", "-d", "10"]
        );
    }

    #[test]
    fn runner_wraps_everything_when_installed() {
        let tmp = TempDir::new();
        tmp.write("bin/sengi-run", "#!/bin/sh\n");
        tmp.make_executable("bin/sengi-run");
        let bin = tmp.path().join("bin");
        let env = env_with_path(bin.to_str().unwrap());
        let runner = bin.join("sengi-run").display().to_string();

        let mut e = entry("vim %F");
        assert_eq!(command_line(&e, &env).unwrap(), [runner.as_str(), "vim"]);
        e.terminal = true;
        assert_eq!(
            command_line(&e, &env).unwrap(),
            [runner.as_str(), "foot", "-e", "vim"]
        );
    }

    #[test]
    fn empty_commands_are_errors() {
        assert_eq!(
            command_line(&entry(""), &no_runner()),
            Err(ExecError::Empty)
        );
        assert_eq!(
            command_line(&entry("%U"), &no_runner()),
            Err(ExecError::Empty)
        );
        assert_eq!(
            command_line(&entry(r#""" arg"#), &no_runner()),
            Err(ExecError::Empty)
        );
        assert_eq!(
            command_line(&entry(r#"app "x"#), &no_runner()),
            Err(ExecError::UnclosedQuote)
        );
    }

    #[test]
    fn spawn_starts_the_program_with_its_arguments() {
        let tmp = TempDir::new();
        let out = tmp.path().join("out");
        tmp.write(
            "write-args",
            "#!/bin/sh\nout=$1\nshift\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$out.tmp\"\nmv \"$out.tmp\" \"$out\"\n",
        );
        tmp.make_executable("write-args");
        let argv = vec![
            tmp.path().join("write-args").display().to_string(),
            out.display().to_string(),
            "two words".to_string(),
            "$HOME;".to_string(),
        ];
        spawn(&argv, Some(tmp.path())).unwrap();

        let mut contents = None;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&out) {
                contents = Some(text);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let contents = contents.expect("the program ran");
        let lines: Vec<&str> = contents.lines().collect();
        let cwd = std::fs::canonicalize(tmp.path()).unwrap();
        assert_eq!(std::fs::canonicalize(lines[0]).unwrap(), cwd);
        assert_eq!(&lines[1..], ["two words", "$HOME;"]);
    }

    #[test]
    fn spawn_reports_missing_programs() {
        assert!(spawn(&["/no/such/program".to_string()], None).is_err());
        assert!(spawn(&[], None).is_err());
    }

    #[test]
    fn relative_program_paths_are_refused() {
        for exec in ["./app", "bin/app --flag", "../app", "\"sub dir/app\""] {
            assert_eq!(
                command_line(&entry(exec), &no_runner()),
                Err(ExecError::RelativePath),
                "{exec}"
            );
        }
        // Slashes in later arguments are none of our business.
        assert!(command_line(&entry("app ./file a/b"), &no_runner()).is_ok());
        assert!(command_line(&entry("/usr/bin/app"), &no_runner()).is_ok());
    }

    #[test]
    fn launchable_needs_the_program_and_the_terminal_to_exist() {
        let tmp = TempDir::new();
        for program in ["sengi-run", "app"] {
            tmp.write(&format!("bin/{program}"), "#!/bin/sh\n");
            tmp.make_executable(&format!("bin/{program}"));
        }
        tmp.write("bin/not-executable", "");
        let bin = tmp.path().join("bin");
        let env = env_with_path(bin.to_str().unwrap());
        let runner = bin.join("sengi-run").display().to_string();
        let absolute = bin.join("app").display().to_string();

        assert_eq!(
            launchable(&entry("app --x %U"), &env).unwrap(),
            [runner.as_str(), "app", "--x"]
        );
        assert_eq!(
            launchable(&entry(&absolute), &env).unwrap(),
            [runner.as_str(), absolute.as_str()]
        );
        // The runner being there does not make a missing program startable.
        for (exec, missing) in [
            ("gone --x", "gone"),
            ("/no/such/binary", "/no/such/binary"),
            ("not-executable", "not-executable"),
        ] {
            assert_eq!(
                launchable(&entry(exec), &env),
                Err(ExecError::NotFound(missing.into())),
                "{exec}"
            );
        }
        let mut in_terminal = entry("app");
        in_terminal.terminal = true;
        assert_eq!(
            launchable(&in_terminal, &env),
            Err(ExecError::NotFound("foot".into()))
        );
        assert_eq!(
            launchable(&entry("\"open"), &env),
            Err(ExecError::UnclosedQuote)
        );
        assert_eq!(
            launchable(&entry("./app"), &env),
            Err(ExecError::RelativePath)
        );
    }

    // The tests below came with the dock's parser.

    fn run(exec: &str) -> Result<Vec<String>, ExecError> {
        let mut entry = crate::testutil::entry("web", "Web Browser", exec);
        entry.icon = "web-icon".into();
        command_line(&entry, &no_runner())
    }

    fn args(list: &[&str]) -> Result<Vec<String>, ExecError> {
        Ok(list.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn plain_words_are_split_on_whitespace() {
        assert_eq!(run("foot"), args(&["foot"]));
        assert_eq!(
            run("  foot   --server\t-x "),
            args(&["foot", "--server", "-x"])
        );
    }

    #[test]
    fn file_and_url_codes_are_dropped_from_a_command() {
        assert_eq!(run("web %u"), args(&["web"]));
        assert_eq!(
            run("web --new-window %U --tail"),
            args(&["web", "--new-window", "--tail"])
        );
        assert_eq!(run("edit %f %F"), args(&["edit"]));
        assert_eq!(run("web --url=%u"), args(&["web", "--url="]));
    }

    #[test]
    fn name_icon_and_path_codes_are_filled_in() {
        assert_eq!(
            run("web --title %c"),
            args(&["web", "--title", "Web Browser"])
        );
        assert_eq!(run("web %i"), args(&["web", "--icon", "web-icon"]));
        assert_eq!(
            run("web --desktop=%k"),
            args(&["web", "--desktop=/usr/share/applications/web.desktop"])
        );
    }

    #[test]
    fn the_icon_code_vanishes_when_there_is_no_icon() {
        let mut entry = crate::testutil::entry("web", "Web", "web %i");
        entry.icon.clear();
        assert_eq!(command_line(&entry, &no_runner()), args(&["web"]));
    }

    #[test]
    fn percent_and_deprecated_codes() {
        assert_eq!(run("calc 100%%"), args(&["calc", "100%"]));
        assert_eq!(run("old %d %n %v %m x"), args(&["old", "x"]));
    }

    #[test]
    fn quoted_arguments_keep_spaces_and_escapes() {
        assert_eq!(
            run(r#"sh -c "echo \"a  b\" \$HOME \\ \`x\`""#),
            args(&["sh", "-c", r#"echo "a  b" $HOME \ `x`"#])
        );
        assert_eq!(
            run(r#""/opt/My App/run" --flag"#),
            args(&["/opt/My App/run", "--flag"])
        );
        assert_eq!(run(r#"app """#), args(&["app", ""]));
    }

    #[test]
    fn field_codes_inside_quotes_are_expanded_too() {
        // What GLib does, and what `Exec=/opt/App/app "%U"` relies on.
        assert_eq!(run(r#"app "%u""#), args(&["app"]));
        assert_eq!(
            run("app '%c' --name=\"%c\""),
            args(&["app", "Web Browser", "--name=Web Browser"])
        );
        assert_eq!(run(r#"app "100%%""#), args(&["app", "100%"]));
        assert_eq!(
            run(r#"bash -c "exec tool --desktop %k""#),
            args(&[
                "bash",
                "-c",
                "exec tool --desktop /usr/share/applications/web.desktop"
            ])
        );
    }

    #[test]
    fn broken_and_empty_commands_are_errors() {
        assert_eq!(run(r#"app "unfinished"#), Err(ExecError::UnclosedQuote));
        assert_eq!(run(r#"app "bad\"#), Err(ExecError::UnclosedQuote));
        assert_eq!(run(""), Err(ExecError::Empty));
        assert_eq!(run("%u"), Err(ExecError::Empty));
    }

    #[test]
    fn the_program_name_skips_paths_and_env() {
        assert_eq!(program_name("foot").as_deref(), Some("foot"));
        assert_eq!(
            program_name("/usr/lib/firefox-esr/firefox-esr %u").as_deref(),
            Some("firefox-esr")
        );
        assert_eq!(
            program_name("env GDK_BACKEND=wayland /opt/app/bin/app --flag").as_deref(),
            Some("app")
        );
        assert_eq!(
            program_name(r#""/opt/My App/run" %f"#).as_deref(),
            Some("run")
        );
        assert_eq!(program_name(""), None);
        assert_eq!(program_name("env A=1"), None);
    }

    #[test]
    fn a_started_program_is_not_our_child() {
        let tmp = TempDir::new();
        let out = tmp.path().join("out");
        tmp.write(
            "report",
            "#!/bin/sh\nprintf '%s|%s|%s' \"$1\" \"$2\" \"$PPID\" > \"$3\"\n",
        );
        tmp.make_executable("report");
        spawn(
            &[
                tmp.path().join("report").display().to_string(),
                "two words".into(),
                "$HOME".into(),
                out.display().to_string(),
            ],
            None,
        )
        .unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let text = loop {
            match std::fs::read_to_string(&out) {
                Ok(text) if text.matches('|').count() == 2 => break text,
                _ if std::time::Instant::now() > deadline => panic!("the program did not run"),
                _ => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        };
        let fields: Vec<&str> = text.split('|').collect();
        // Arguments arrive untouched: no shell has split or expanded them.
        assert_eq!(fields[0], "two words");
        assert_eq!(fields[1], "$HOME");
        // Its parent is whoever adopts orphans, not this process.
        assert_ne!(fields[2], std::process::id().to_string());
    }

    #[test]
    fn the_runner_is_used_only_when_installed() {
        let tmp = TempDir::new();
        let env = env_with_path(&format!("/nonexistent:{}", tmp.path().display()));
        let foot = crate::testutil::entry("foot", "Foot", "foot -x");
        assert_eq!(command_line(&foot, &env), args(&["foot", "-x"]));

        // Present but not executable does not count.
        tmp.write(RUNNER, "");
        assert_eq!(command_line(&foot, &env), args(&["foot", "-x"]));

        tmp.write(RUNNER, "#!/bin/sh\nexec \"$@\"\n");
        tmp.make_executable(RUNNER);
        let runner = tmp.path().join(RUNNER).display().to_string();
        assert_eq!(command_line(&foot, &env), args(&[&runner, "foot", "-x"]));
        assert_eq!(env.find_program(RUNNER), Some(tmp.path().join(RUNNER)));
        assert_eq!(env_with_path("").find_program(RUNNER), None);
    }

    #[test]
    fn terminal_apps_are_wrapped_in_the_terminal() {
        let mut vim = crate::testutil::entry("vim", "Vim", "vim %F");
        assert_eq!(command_line(&vim, &no_runner()), args(&["vim"]));
        vim.terminal = true;
        assert_eq!(
            command_line(&vim, &no_runner()),
            args(&["foot", "-e", "vim"])
        );
    }
}
