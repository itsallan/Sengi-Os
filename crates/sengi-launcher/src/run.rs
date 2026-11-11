//! What happens before the panel opens and after it closes, for both modes.

use std::io::{Read, Write};

use crate::apps::Apps;
use crate::cli::{Invocation, Mode};
use crate::env::Env;
use crate::matcher::TieBreak;
use crate::session::Session;
use crate::ui::{segments, Outcome};
use crate::{exec, menu, usage};

/// Exit status after a choice was made and carried out.
pub const CHOSEN: u8 = 0;
/// Exit status when the panel was dismissed, as dmenu-style tools use.
pub const DISMISSED: u8 = 1;
pub const FAILED: u8 = 2;

/// Rows of a session before a panel has sized it. How many fit is a matter
/// of the design tokens, which only the panel knows.
const UNSIZED: usize = 1;

/// Where the candidates of a session came from. Candidate indices are indices
/// into the application entries or the menu lines.
pub enum Source {
    Apps(Apps),
    Menu(Vec<String>),
}

/// Gather the candidates: installed applications, or the lines on `input`.
pub fn load(
    invocation: &Invocation,
    env: &Env,
    input: impl Read,
) -> std::io::Result<(Source, Session)> {
    Ok(match invocation.mode {
        Mode::Apps => {
            let apps = Apps::load(env);
            let candidates = apps.candidates(usage::now());
            let session = Session::new(candidates, TieBreak::Alphabetical, UNSIZED);
            (Source::Apps(apps), session)
        }
        Mode::Menu => {
            let items = menu::read_items(input)?;
            let candidates = menu::candidates(&items);
            let session = Session::new(candidates, TieBreak::InputOrder, UNSIZED);
            (Source::Menu(items), session)
        }
    })
}

/// The check a choice must pass before the panel closes on it. For an
/// application that is whether it can be started at all; the answer is the
/// message to show in the panel if not. Menu lines always pass.
pub fn checker(source: &Source, env: &Env) -> Box<dyn Fn(usize) -> Result<(), String>> {
    match source {
        Source::Apps(apps) => {
            let entries = apps.entries().to_vec();
            let env = env.clone();
            Box::new(move |index| {
                let entry = entries.get(index).ok_or("That application is gone")?;
                exec::launchable(entry, &env)
                    .map(drop)
                    .map_err(|error| format!("Cannot start {}: {error}", entry.name))
            })
        }
        Source::Menu(_) => Box::new(|_| Ok(())),
    }
}

/// Act on how the panel was closed: start the application, or write the menu
/// line to `output`. Returns the exit status.
pub fn complete(outcome: Outcome, source: Source, env: &Env, mut output: impl Write) -> u8 {
    let Outcome::Accepted(index) = outcome else {
        return DISMISSED;
    };
    match source {
        Source::Apps(mut apps) => {
            if index >= apps.entries().len() {
                return FAILED;
            }
            match apps.launch(index, env, usage::now()) {
                Ok(()) => CHOSEN,
                Err(error) => {
                    let name = &apps.entries()[index].name;
                    eprintln!("sengi-launcher: could not start {name}: {error}");
                    FAILED
                }
            }
        }
        Source::Menu(items) => match items.get(index) {
            Some(item) if writeln!(output, "{item}").is_ok() => CHOSEN,
            _ => FAILED,
        },
    }
}

/// Every result for `query`, best first, one per line, with the matched
/// characters in brackets. Applications are followed by a tab and their
/// desktop file ID. This is `--list`, for debugging and tests.
pub fn list(source: &Source, session: &mut Session, query: &str) -> String {
    session.set_query(query);
    let mut out = String::new();
    for hit in session.hits() {
        let name = &session.candidate(hit.index).name;
        for (text, matched) in segments(name, &hit.ranges) {
            let text = text.replace('\u{a0}', " ");
            out.push_str(&if matched { format!("[{text}]") } else { text });
        }
        if let Source::Apps(apps) = source {
            out.push('\t');
            out.push_str(&apps.entries()[hit.index].id);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli;
    use crate::testutil::TempDir;
    use std::ffi::OsString;

    fn env_in(tmp: &TempDir) -> Env {
        let root = tmp.path().to_path_buf();
        Env::from_vars(move |name| {
            let value = match name {
                "HOME" => root.join("home"),
                "XDG_DATA_HOME" => root.join("data"),
                "XDG_DATA_DIRS" => root.join("system"),
                "XDG_STATE_HOME" => root.join("state"),
                "PATH" => root.join("bin"),
                _ => return None,
            };
            Some(OsString::from(value))
        })
    }

    const SESSION_MENU: &[u8] = b"Lock\nLog out\nRestart\nShut down\n";

    fn menu(input: &[u8]) -> (Source, Session, Env, TempDir) {
        let tmp = TempDir::new();
        let env = env_in(&tmp);
        let invocation = cli::parse(["--menu", "--lines", "4"]).unwrap();
        let (source, session) = load(&invocation, &env, input).unwrap();
        (source, session, env, tmp)
    }

    #[test]
    fn menu_prints_the_chosen_line_and_exits_zero() {
        let (source, mut session, env, _tmp) = menu(SESSION_MENU);
        session.set_query("rest");
        let chosen = session.chosen().unwrap();
        let mut out = Vec::new();
        let status = complete(Outcome::Accepted(chosen), source, &env, &mut out);
        assert_eq!(status, 0);
        assert_eq!(out, b"Restart\n");
    }

    #[test]
    fn menu_dismissed_prints_nothing_and_exits_one() {
        let (source, _session, env, _tmp) = menu(SESSION_MENU);
        let mut out = Vec::new();
        assert_eq!(complete(Outcome::Dismissed, source, &env, &mut out), 1);
        assert!(out.is_empty());
    }

    #[test]
    fn menu_keeps_input_order_and_filters() {
        let (source, mut session, _env, _tmp) = menu(SESSION_MENU);
        assert_eq!(
            list(&source, &mut session, ""),
            "Lock\nLog out\nRestart\nShut down\n"
        );
        assert_eq!(list(&source, &mut session, "lo"), "[Lo]ck\n[Lo]g out\n");
        assert_eq!(list(&source, &mut session, "sd"), "[S]hut [d]own\n");
        assert_eq!(list(&source, &mut session, "nothing"), "");
    }

    #[test]
    fn an_empty_menu_can_only_be_dismissed() {
        let (source, session, env, _tmp) = menu(b"");
        assert_eq!(session.chosen(), None);
        let mut out = Vec::new();
        assert_eq!(
            complete(Outcome::Accepted(0), source, &env, &mut out),
            FAILED
        );
        assert!(out.is_empty());
    }

    #[test]
    fn app_mode_lists_ids_and_launches() {
        let tmp = TempDir::new();
        let marker = tmp.path().join("ran");
        tmp.write("bin/mark", "#!/bin/sh\n: > \"$1\"\n");
        tmp.make_executable("bin/mark");
        tmp.write(
            "system/applications/org.example.Mark.desktop",
            format!(
                "[Desktop Entry]\nType=Application\nName=Marker Pen\nExec={} {}\n",
                tmp.path().join("bin/mark").display(),
                marker.display()
            ),
        );
        tmp.write(
            "system/applications/hidden.desktop",
            "[Desktop Entry]\nType=Application\nName=Marked Hidden\nExec=true\nNoDisplay=true\n",
        );
        let env = env_in(&tmp);
        let invocation = cli::parse(Vec::<String>::new()).unwrap();
        let (source, mut session) = load(&invocation, &env, &b""[..]).unwrap();

        assert_eq!(
            list(&source, &mut session, "pen"),
            "Marker [Pen]\torg.example.Mark.desktop\n"
        );
        let chosen = session.chosen().unwrap();
        assert_eq!(
            complete(Outcome::Accepted(chosen), source, &env, Vec::new()),
            0
        );
        for _ in 0..200 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(marker.exists());
        assert!(env.usage_file().exists());
    }

    #[test]
    fn a_stale_index_fails_instead_of_panicking() {
        let tmp = TempDir::new();
        let env = env_in(&tmp);
        let invocation = cli::parse(Vec::<String>::new()).unwrap();
        let (source, _session) = load(&invocation, &env, &b""[..]).unwrap();
        assert_eq!(
            complete(Outcome::Accepted(3), source, &env, Vec::new()),
            FAILED
        );
    }

    #[test]
    fn the_checker_explains_why_an_application_cannot_start() {
        let tmp = TempDir::new();
        tmp.write("bin/present", "#!/bin/sh\n");
        tmp.make_executable("bin/present");
        tmp.write(
            "system/applications/a.desktop",
            "[Desktop Entry]\nType=Application\nName=Alpha\nExec=present --go\n",
        );
        tmp.write(
            "system/applications/b.desktop",
            "[Desktop Entry]\nType=Application\nName=Beta\nExec=/no/such/binary\n",
        );
        let env = env_in(&tmp);
        let invocation = cli::parse(Vec::<String>::new()).unwrap();
        let (source, _session) = load(&invocation, &env, &b""[..]).unwrap();
        let check = checker(&source, &env);
        assert_eq!(check(0), Ok(()));
        assert_eq!(
            check(1),
            Err("Cannot start Beta: /no/such/binary is not installed".to_string())
        );
        assert!(check(7).is_err());

        let (source, _session, env, _tmp) = menu(SESSION_MENU);
        assert_eq!(checker(&source, &env)(2), Ok(()));
    }
}
