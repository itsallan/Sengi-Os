//! Command line parsing.

use std::{path::PathBuf, time::Duration};

pub const USAGE: &str = "\
Usage: sengi-compositor [options]

  -s, --startup CMD     run CMD with sh once the compositor is ready
      --backend NAME    headless, winit, drm or auto (default auto: a window
                        when started from a desktop session, the screen when
                        started on a terminal, headless otherwise)
      --size WxH        output size in pixels (default 1280x720)
      --screenshot PNG  save the last frame to PNG on exit (headless, drm);
                        with --backend drm also on every SIGUSR2
      --exit-after SEC  stop after SEC seconds
      --script FILE     play scripted input and screenshots (headless only)
      --locker CMD      what Super+L runs to lock (default: swaylock -f)
      --renderer NAME   drm backend: gles, pixman or auto (default auto: gles
                        with a GPU, pixman without; SENGI_COMPOSITOR_RENDERER
                        sets the same)
      --exit            ask the running compositor to quit (log out)
  -h, --help            show this help
  -V, --version         show the version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Headless,
    Winit,
    Drm,
}

/// Which renderer the drm backend should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    /// GLES with a GPU, pixman without.
    Auto,
    Gles,
    Pixman,
}

pub fn parse_renderer(name: &str) -> Option<Wanted> {
    match name {
        "auto" => Some(Wanted::Auto),
        "gles" => Some(Wanted::Gles),
        "pixman" => Some(Wanted::Pixman),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub backend: Backend,
    pub size: (i32, i32),
    pub startup: Vec<String>,
    pub screenshot: Option<PathBuf>,
    pub exit_after: Option<Duration>,
    pub script: Option<PathBuf>,
    pub locker: Option<String>,
    #[cfg_attr(not(feature = "drm"), allow(dead_code))]
    pub renderer: Wanted,
}

impl Options {
    /// Settles `Auto`. `has_display` says whether we were started from
    /// inside a desktop session that a window could be opened on,
    /// `on_terminal` whether we were started on a virtual terminal with a
    /// build that can drive a screen.
    pub fn resolve_backend(&self, has_display: bool, on_terminal: bool) -> Backend {
        match self.backend {
            Backend::Auto if self.script.is_some() || self.screenshot.is_some() => {
                Backend::Headless
            }
            Backend::Auto if has_display => Backend::Winit,
            Backend::Auto if on_terminal => Backend::Drm,
            Backend::Auto => Backend::Headless,
            chosen => chosen,
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            backend: Backend::Auto,
            size: (1280, 720),
            startup: Vec::new(),
            screenshot: None,
            exit_after: None,
            script: None,
            locker: None,
            renderer: Wanted::Auto,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
    /// Ask the running compositor to end the session.
    Exit,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--exit" => return Ok(Command::Exit),
            "-s" | "--startup" => options.startup.push(value()?),
            "--backend" => {
                options.backend = match value()?.as_str() {
                    "auto" => Backend::Auto,
                    "headless" => Backend::Headless,
                    "winit" => Backend::Winit,
                    "drm" => Backend::Drm,
                    other => return Err(format!("unknown backend: {other}")),
                }
            }
            "--size" => options.size = parse_size(&value()?)?,
            "--screenshot" => options.screenshot = Some(PathBuf::from(value()?)),
            "--script" if cfg!(feature = "test-hooks") => {
                options.script = Some(PathBuf::from(value()?))
            }
            "--locker" => options.locker = Some(value()?),
            "--renderer" => {
                let name = value()?;
                options.renderer =
                    parse_renderer(&name).ok_or_else(|| format!("unknown renderer: {name}"))?;
            }
            "--exit-after" => {
                let text = value()?;
                let seconds: f64 = text
                    .parse()
                    .ok()
                    .filter(|s: &f64| s.is_finite() && *s >= 0.0)
                    .ok_or_else(|| format!("invalid duration: {text}"))?;
                let after = Duration::try_from_secs_f64(seconds)
                    .map_err(|_| format!("invalid duration: {text}"))?;
                options.exit_after = Some(after);
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    // The drm backend also saves one whenever it gets SIGUSR2.
    if options.screenshot.is_some()
        && options.exit_after.is_none()
        && options.backend != Backend::Drm
    {
        return Err("--screenshot needs --exit-after".into());
    }
    // Winit draws into someone else's window and has no script player;
    // the drm backend can save what it drew but takes input from devices.
    let unsupported = match options.backend {
        Backend::Winit if options.screenshot.is_some() => Some("--screenshot"),
        Backend::Winit | Backend::Drm if options.script.is_some() => Some("--script"),
        _ => None,
    };
    if let Some(option) = unsupported {
        return Err(format!("{option} does not work with that backend"));
    }
    Ok(Command::Run(options))
}

fn parse_size(text: &str) -> Result<(i32, i32), String> {
    let err = || format!("invalid size: {text} (expected WIDTHxHEIGHT)");
    let (w, h) = text.split_once('x').ok_or_else(err)?;
    let w: i32 = w.parse().map_err(|_| err())?;
    let h: i32 = h.parse().map_err(|_| err())?;
    // 16384 is far beyond any panel we target and keeps buffers sane.
    if !(1..=16384).contains(&w) || !(1..=16384).contains(&h) {
        return Err(err());
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_arguments_gives_the_defaults() {
        assert_eq!(run(&[]), Ok(Command::Run(Options::default())));
    }

    #[test]
    #[cfg(not(feature = "test-hooks"))]
    fn a_build_for_the_image_has_no_script_player() {
        assert!(run(&["--backend", "headless", "--script", "steps.txt"]).is_err());
    }

    #[test]
    #[cfg(feature = "test-hooks")]
    fn all_options_are_read() {
        let parsed = run(&[
            "--backend",
            "headless",
            "--size",
            "1920x1080",
            "-s",
            "foot",
            "--startup",
            "sengi-bar",
            "--screenshot",
            "out.png",
            "--exit-after",
            "2.5",
            "--script",
            "steps.txt",
            "--locker",
            "swaylock",
            "--renderer",
            "pixman",
        ]);
        assert_eq!(
            parsed,
            Ok(Command::Run(Options {
                backend: Backend::Headless,
                size: (1920, 1080),
                startup: vec!["foot".into(), "sengi-bar".into()],
                screenshot: Some("out.png".into()),
                exit_after: Some(Duration::from_millis(2500)),
                script: Some("steps.txt".into()),
                locker: Some("swaylock".into()),
                renderer: Wanted::Pixman,
            }))
        );
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(run(&["--size", "1x1", "--help"]), Ok(Command::Help));
        assert_eq!(run(&["-V"]), Ok(Command::Version));
        assert_eq!(run(&["--exit"]), Ok(Command::Exit));
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(run(&["--size"]).is_err());
        assert!(run(&["--size", "1280"]).is_err());
        assert!(run(&["--size", "0x720"]).is_err());
        assert!(run(&["--size", "-5x720"]).is_err());
        assert!(run(&["--exit-after", "soon"]).is_err());
        assert!(run(&["--exit-after", "-1"]).is_err());
        assert!(run(&["--exit-after", "1e30"]).is_err());
        assert!(run(&["--frobnicate"]).is_err());
        assert!(run(&["--backend", "x11"]).is_err());
        assert!(run(&["--renderer", "vulkan"]).is_err());
        assert_eq!(parse_renderer("gles"), Some(Wanted::Gles));
        assert_eq!(parse_renderer("auto"), Some(Wanted::Auto));
    }

    #[test]
    fn screenshot_needs_an_exit_time() {
        assert!(run(&["--screenshot", "out.png"]).is_err());
    }

    #[test]
    fn the_drm_backend_takes_a_screenshot_but_no_script() {
        let shot = [
            "--backend",
            "drm",
            "--screenshot",
            "a.png",
            "--exit-after",
            "1",
        ];
        assert!(run(&shot).is_ok());
        // Without an exit time it still saves one on SIGUSR2.
        assert!(run(&shot[..4]).is_ok());
        assert!(run(&["--backend", "drm", "--script", "steps.txt"]).is_err());
    }

    #[test]
    fn screenshot_and_winit_do_not_mix() {
        let args = [
            "--backend",
            "winit",
            "--screenshot",
            "a.png",
            "--exit-after",
            "1",
        ];
        assert!(run(&args).is_err());
    }

    #[test]
    fn auto_picks_the_backend_from_where_it_was_started() {
        let mut options = Options::default();
        assert_eq!(options.resolve_backend(true, false), Backend::Winit);
        assert_eq!(options.resolve_backend(false, true), Backend::Drm);
        // A terminal inside a desktop session is still that session.
        assert_eq!(options.resolve_backend(true, true), Backend::Winit);
        assert_eq!(options.resolve_backend(false, false), Backend::Headless);

        options.screenshot = Some("a.png".into());
        assert_eq!(options.resolve_backend(true, true), Backend::Headless);

        options.screenshot = None;
        options.script = Some("steps.txt".into());
        assert_eq!(options.resolve_backend(true, true), Backend::Headless);

        options.backend = Backend::Winit;
        options.script = None;
        assert_eq!(options.resolve_backend(false, false), Backend::Winit);
        options.backend = Backend::Drm;
        assert_eq!(options.resolve_backend(true, false), Backend::Drm);
    }
}
