//! sengi-open: opens each file, folder or file:// address it is given in the
//! program that suits it, or says that nothing does. It starts the program
//! and leaves; it never stays running.

use std::path::Path;
use std::process::{Command, ExitCode};

use sengi_apps::{exec, Env};
use sengi_open::{decide_path, resolve};

const USAGE: &str = "usage: sengi-open FILE_OR_URI...\n\nOpens each file in the Sengi OS program for it: text in Text Editor, PNG and JPEG\nimages in Image Viewer, folders in Files. When nothing can open one, a\nnotification says so.\n";

/// Tells the user, through the notification service, or on standard error
/// when there is none to ask.
fn notify(env: &Env, summary: &str, body: &str) {
    eprintln!("sengi-open: {summary}: {body}");
    if env.find_program("sengi-notify").is_none() {
        return;
    }
    let _ = Command::new("sengi-notify")
        .args(["--post", summary, body])
        .status();
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Opens one argument. The error is what to tell the user, as a summary and
/// a line under it.
fn open_one(env: &Env, arg: &str, cwd: &Path) -> Result<(), (String, String)> {
    let path = resolve(arg, cwd).map_err(|why| ("Cannot open this".to_string(), why))?;
    let target = decide_path(&path).map_err(|why| ("Cannot open this file".to_string(), why))?;
    let Some(program) = target.program() else {
        return Err((
            "Nothing can open this file".to_string(),
            format!("\"{}\" is not text, a PNG or a JPEG.", name_of(&path)),
        ));
    };
    if env.find_program(program).is_none() {
        return Err((
            "Cannot open this file".to_string(),
            format!(
                "{program} is not installed, so \"{}\" stays closed.",
                name_of(&path)
            ),
        ));
    }
    let mut argv = vec![program.to_string(), path.to_string_lossy().into_owned()];
    if env.find_program(exec::RUNNER).is_some() {
        argv.insert(0, exec::RUNNER.to_string());
    }
    exec::spawn(&argv, None).map_err(|error| {
        (
            "Cannot open this file".to_string(),
            format!("Cannot start {program}: {error}"),
        )
    })
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        Some("-h" | "--help") => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("-V" | "--version") => {
            println!("sengi-open {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) if other.starts_with('-') && other != "-" => {
            eprintln!("sengi-open: unknown option {other}\n\n{USAGE}");
            return ExitCode::from(2);
        }
        Some(_) => {}
    }
    let env = Env::from_process();
    let cwd = std::env::current_dir().unwrap_or_else(|_| env.home.clone());
    let mut failed = false;
    for arg in &args {
        if let Err((summary, body)) = open_one(&env, arg, &cwd) {
            notify(&env, &summary, &body);
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
