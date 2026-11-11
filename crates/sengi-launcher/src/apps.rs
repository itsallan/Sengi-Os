//! Application mode: the installed applications as search candidates, and
//! starting the one that was picked.

use std::fmt;
use std::path::Path;

use crate::desktop::{self, DesktopEntry};
use crate::env::Env;
use crate::exec::{self, ExecError};
use crate::matcher::Candidate;
use crate::usage::Usage;

pub struct Apps {
    entries: Vec<DesktopEntry>,
    usage: Usage,
}

#[derive(Debug)]
pub enum LaunchError {
    Exec(ExecError),
    Spawn(std::io::Error),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::Exec(e) => write!(f, "{e}"),
            LaunchError::Spawn(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LaunchError {}

impl Apps {
    pub fn load(env: &Env) -> Self {
        Apps {
            entries: desktop::load(env),
            usage: Usage::load(&env.usage_file()),
        }
    }

    pub fn entries(&self) -> &[DesktopEntry] {
        &self.entries
    }

    /// One candidate per entry, in the same order as `entries`.
    pub fn candidates(&self, now: u64) -> Vec<Candidate> {
        self.entries
            .iter()
            .map(|entry| candidate(entry, &self.usage, now))
            .collect()
    }

    /// Start entry `index` and remember that it was used.
    pub fn launch(&mut self, index: usize, env: &Env, now: u64) -> Result<(), LaunchError> {
        let entry = &self.entries[index];
        let argv = exec::command_line(entry, env).map_err(LaunchError::Exec)?;
        exec::spawn(&argv, entry.working_dir.as_deref()).map_err(LaunchError::Spawn)?;
        self.usage.record(&entry.id, now);
        // Losing the history is not worth failing a launch that worked.
        let _ = self.usage.save(&env.usage_file(), now);
        Ok(())
    }
}

fn candidate(entry: &DesktopEntry, usage: &Usage, now: u64) -> Candidate {
    let mut keywords = entry.keywords.clone();
    // Let people type the command they know, e.g. "foot" for Terminal.
    if let Some(program) = program_name(&entry.exec) {
        keywords.push(program);
    }
    let detail = if entry.generic_name == entry.name {
        ""
    } else {
        &entry.generic_name
    };
    Candidate::new(&entry.name)
        .detail(detail)
        .keywords(keywords)
        .comment(&entry.comment)
        .weight(usage.weight(&entry.id, now))
}

fn program_name(exec: &str) -> Option<String> {
    let first = exec::split(exec).ok()?.into_iter().next()?;
    let name = Path::new(&first)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matcher::{rank, Query, TieBreak};
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

    fn write_entry(tmp: &TempDir, id: &str, body: &str) {
        tmp.write(
            &format!("system/applications/{id}"),
            format!("[Desktop Entry]\nType=Application\n{body}"),
        );
    }

    fn order(apps: &Apps, query: &str, now: u64) -> Vec<String> {
        let candidates = apps.candidates(now);
        rank(&candidates, &Query::new(query), TieBreak::Alphabetical)
            .iter()
            .map(|hit| apps.entries()[hit.index].name.clone())
            .collect()
    }

    #[test]
    fn entries_become_searchable_candidates() {
        let tmp = TempDir::new();
        write_entry(
            &tmp,
            "foot.desktop",
            "Name=Terminal\nGenericName=Terminal\nExec=/usr/bin/foot --server\nKeywords=shell;prompt;\n",
        );
        write_entry(
            &tmp,
            "browser.desktop",
            "Name=Firefox\nGenericName=Web Browser\nComment=Browse the internet\nExec=firefox %u\n",
        );
        let apps = Apps::load(&env_in(&tmp));

        assert_eq!(order(&apps, "", 0), vec!["Firefox", "Terminal"]);
        assert_eq!(order(&apps, "foot", 0), vec!["Terminal"]);
        assert_eq!(order(&apps, "shell", 0), vec!["Terminal"]);
        assert_eq!(order(&apps, "browser", 0), vec!["Firefox"]);
        assert_eq!(order(&apps, "internet", 0), vec!["Firefox"]);

        let candidates = apps.candidates(0);
        let terminal = candidates.iter().find(|c| c.name == "Terminal").unwrap();
        assert_eq!(
            terminal.detail, "",
            "a generic name equal to the name is not repeated"
        );
        let firefox = candidates.iter().find(|c| c.name == "Firefox").unwrap();
        assert_eq!(firefox.detail, "Web Browser");
    }

    #[test]
    fn launching_runs_the_app_and_moves_it_up() {
        let tmp = TempDir::new();
        let marker = tmp.path().join("ran");
        tmp.write(
            "bin/toucher",
            "#!/bin/sh\nprintf '%s' \"$1\" > \"$2.tmp\" && mv \"$2.tmp\" \"$2\"\n",
        );
        tmp.make_executable("bin/toucher");
        write_entry(&tmp, "alpha.desktop", "Name=Alpha\nExec=true\n");
        write_entry(
            &tmp,
            "zulu.desktop",
            &format!(
                "Name=Zulu\nExec={} \"two words\" {} %U\n",
                tmp.path().join("bin/toucher").display(),
                marker.display()
            ),
        );
        let env = env_in(&tmp);
        let now = 1_800_000_000;

        let mut apps = Apps::load(&env);
        assert_eq!(order(&apps, "", now), vec!["Alpha", "Zulu"]);
        let zulu = apps
            .entries()
            .iter()
            .position(|e| e.name == "Zulu")
            .unwrap();
        apps.launch(zulu, &env, now).unwrap();

        let mut ran = None;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&marker) {
                ran = Some(text);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ran.as_deref(), Some("two words"));

        // A fresh start reads the usage file back.
        let apps = Apps::load(&env);
        assert_eq!(order(&apps, "", now + 5), vec!["Zulu", "Alpha"]);
        assert!(env.usage_file().starts_with(tmp.path().join("state")));
    }

    #[test]
    fn a_failed_launch_is_reported_and_not_counted() {
        let tmp = TempDir::new();
        write_entry(&tmp, "broken.desktop", "Name=Broken\nExec=\"unclosed\n");
        write_entry(
            &tmp,
            "missing.desktop",
            "Name=Missing\nExec=/no/such/binary\n",
        );
        let env = env_in(&tmp);
        let mut apps = Apps::load(&env);
        for index in 0..apps.entries().len() {
            assert!(apps.launch(index, &env, 100).is_err());
        }
        assert!(!env.usage_file().exists());
    }
}
