use std::process::ExitCode;

use sengi_launcher::cli::{self, Action, Invocation};
use sengi_launcher::env::Env;
use sengi_launcher::run;

fn main() -> ExitCode {
    let invocation = match cli::parse(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("sengi-launcher: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(run::FAILED);
        }
    };
    match &invocation.action {
        Action::Help => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Action::Version => {
            println!("sengi-launcher {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Action::List(query) => start(&invocation, Some(query)),
        Action::Show => start(&invocation, None),
    }
}

fn start(invocation: &Invocation, list_query: Option<&str>) -> ExitCode {
    let env = Env::from_process();
    let (source, mut session) = match run::load(invocation, &env, std::io::stdin().lock()) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("sengi-launcher: reading the menu: {error}");
            return ExitCode::from(run::FAILED);
        }
    };

    if let Some(query) = list_query {
        print!("{}", run::list(&source, &mut session, query));
        return ExitCode::SUCCESS;
    }

    // ---------------------------------------------------------------------
    // The layer-shell surface goes here once sengi-layer is in the workspace.
    //
    // Overlay layer, no anchor (so the compositor centres it), exclusive
    // keyboard, namespace "sengi-launcher", sized from the component:
    //
    //     let outcome = Rc::new(Cell::new(Outcome::Dismissed));
    //     sengi_layer::run(config, |ctx| {
    //         let window = LauncherWindow::new()?;
    //         ui::attach(&window, session, invocation.prompt(), {
    //             let outcome = outcome.clone();
    //             move |o| { outcome.set(o); ctx.quit(); }
    //         });
    //         ctx.set_size(window.get_panel_width(), window.get_full_height());
    //         window.invoke_focus_search();
    //         Ok(window)
    //     })?;
    //     let status = run::complete(outcome.get(), source, &env, std::io::stdout());
    //     return ExitCode::from(status);
    // ---------------------------------------------------------------------
    drop((source, session));
    eprintln!(
        "sengi-launcher: this build cannot open its window yet; \
         use --list [QUERY] to see the results"
    );
    ExitCode::from(run::FAILED)
}
