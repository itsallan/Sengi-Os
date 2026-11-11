//! Opens the launcher panel in an ordinary window, for looking at the design
//! and trying the keyboard handling without a layer-shell surface.
//!
//!     cargo run -p sengi-launcher --example preview -- [--menu] [--real]
//!         [--query TEXT] [--down N]
//!
//! By default it shows made-up applications. `--real` reads the installed
//! ones (or, with `--menu`, standard input) exactly as the launcher does, and
//! carries out the choice.

use std::cell::Cell;
use std::rc::Rc;

use sengi_launcher::cli::{self, Mode};
use sengi_launcher::env::Env;
use sengi_launcher::matcher::{Candidate, TieBreak};
use sengi_launcher::run;
use sengi_launcher::session::Session;
use sengi_launcher::ui::{self, LauncherWindow, Outcome};
use slint::ComponentHandle;

fn sample_apps() -> Vec<Candidate> {
    [
        ("Terminal", "", (9, 9)),
        ("Files", "File Manager", (7, 7)),
        ("Firefox", "Web Browser", (5, 5)),
        ("Settings", "", (3, 3)),
        ("Text Editor", "", (0, 0)),
        ("Calculator", "", (0, 0)),
        ("Document Viewer", "", (0, 0)),
        ("Image Viewer", "", (0, 0)),
        ("LibreOffice Writer", "Word Processor", (0, 0)),
        ("LibreOffice Calc", "Spreadsheet", (0, 0)),
        ("Software", "Install and remove applications", (0, 0)),
        ("System Monitor", "", (0, 0)),
        (
            "An Application With A Remarkably Long Name That Cannot Possibly Fit In The Row",
            "And an equally long description of what it is for",
            (0, 0),
        ),
    ]
    .into_iter()
    .map(|(name, detail, weight)| Candidate::new(name).detail(detail).weight(weight))
    .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut real = false;
    let mut query = String::new();
    let mut down = 0;
    let mut rest = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--real" => real = true,
            "--query" => query = args.next().unwrap_or_default(),
            "--down" => down = args.next().and_then(|n| n.parse().ok()).unwrap_or(0),
            _ => rest.push(arg),
        }
    }
    let invocation = cli::parse(rest)?;
    let env = Env::from_process();

    let (source, mut session) = if real {
        let (source, session) = run::load(&invocation, &env, std::io::stdin().lock())?;
        (Some(source), session)
    } else {
        let session = match invocation.mode {
            Mode::Apps => Session::new(sample_apps(), TieBreak::Alphabetical, invocation.lines),
            Mode::Menu => {
                let items = ["Lock", "Log out", "Restart", "Shut down"].map(String::from);
                let candidates = sengi_launcher::menu::candidates(&items);
                Session::new(candidates, TieBreak::InputOrder, invocation.lines)
            }
        };
        (None, session)
    };
    session.set_query(&query);
    session.move_selection(down);

    let window = LauncherWindow::new()?;
    let outcome = Rc::new(Cell::new(Outcome::Dismissed));
    let prompt = match (invocation.prompt(), invocation.mode) {
        ("", Mode::Menu) if !real => "Session",
        (prompt, _) => prompt,
    };
    ui::attach(&window, session, prompt, {
        let outcome = outcome.clone();
        move |o| {
            outcome.set(o);
            let _ = slint::quit_event_loop();
        }
    });
    window.invoke_focus_search();
    window.run()?;

    println!("{:?}", outcome.get());
    if let Some(source) = source {
        let status = run::complete(outcome.get(), source, &env, std::io::stdout());
        std::process::exit(status.into());
    }
    Ok(())
}
