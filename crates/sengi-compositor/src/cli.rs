//! Command line parsing.

use std::{path::PathBuf, time::Duration};

pub const USAGE: &str = "\
Usage: sengi-compositor [options]

  -s, --startup CMD     run CMD with sh once the compositor is ready
      --backend NAME    headless, winit or auto (default auto: a window when
                        started from a desktop session, headless otherwise)
      --size WxH        output size in pixels (default 1280x720)
      --screenshot PNG  save the last frame to PNG on exit (headless only)
      --exit-after SEC  stop after SEC seconds
      --script FILE     play scripted input and screenshots (headless only)
  -h, --help            show this help
  -V, --version         show the version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Headless,
    Winit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub backend: Backend,
    pub size: (i32, i32),
    pub startup: Vec<String>,
    pub screenshot: Option<PathBuf>,
    pub exit_after: Option<Duration>,
    pub script: Option<PathBuf>,
}

impl Options {
    /// The option given that only the headless backend supports, if any.
    fn headless_only(&self) -> Option<&'static str> {
        match (&self.screenshot, &self.script) {
            (Some(_), _) => Some("--screenshot"),
            (_, Some(_)) => Some("--script"),
            _ => None,
        }
    }

    /// Settles `Auto`. `has_display` says whether we were started from
    /// inside a desktop session that a window could be opened on.
    pub fn resolve_backend(&self, has_display: bool) -> Backend {
        match self.backend {
            Backend::Auto if self.headless_only().is_none() && has_display => Backend::Winit,
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
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "-s" | "--startup" => options.startup.push(value()?),
            "--backend" => {
                options.backend = match value()?.as_str() {
                    "auto" => Backend::Auto,
                    "headless" => Backend::Headless,
                    "winit" => Backend::Winit,
                    other => return Err(format!("unknown backend: {other}")),
                }
            }
            "--size" => options.size = parse_size(&value()?)?,
            "--screenshot" => options.screenshot = Some(PathBuf::from(value()?)),
            "--script" => options.script = Some(PathBuf::from(value()?)),
            "--exit-after" => {
                let text = value()?;
                let seconds: f64 = text
                    .parse()
                    .ok()
                    .filter(|s: &f64| s.is_finite() && *s >= 0.0)
                    .ok_or_else(|| format!("invalid duration: {text}"))?;
                options.exit_after = Some(Duration::from_secs_f64(seconds));
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    if options.screenshot.is_some() && options.exit_after.is_none() {
        return Err("--screenshot needs --exit-after".into());
    }
    if let (Some(option), Backend::Winit) = (options.headless_only(), options.backend) {
        return Err(format!("{option} only works with the headless backend"));
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
            }))
        );
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(run(&["--size", "1x1", "--help"]), Ok(Command::Help));
        assert_eq!(run(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn bad_input_is_rejected() {
        assert!(run(&["--size"]).is_err());
        assert!(run(&["--size", "1280"]).is_err());
        assert!(run(&["--size", "0x720"]).is_err());
        assert!(run(&["--size", "-5x720"]).is_err());
        assert!(run(&["--exit-after", "soon"]).is_err());
        assert!(run(&["--exit-after", "-1"]).is_err());
        assert!(run(&["--frobnicate"]).is_err());
        assert!(run(&["--backend", "drm"]).is_err());
    }

    #[test]
    fn screenshot_needs_an_exit_time() {
        assert!(run(&["--screenshot", "out.png"]).is_err());
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
    fn auto_picks_a_window_only_inside_a_session() {
        let mut options = Options::default();
        assert_eq!(options.resolve_backend(true), Backend::Winit);
        assert_eq!(options.resolve_backend(false), Backend::Headless);

        options.screenshot = Some("a.png".into());
        assert_eq!(options.resolve_backend(true), Backend::Headless);

        options.screenshot = None;
        options.script = Some("steps.txt".into());
        assert_eq!(options.resolve_backend(true), Backend::Headless);

        options.backend = Backend::Winit;
        options.script = None;
        assert_eq!(options.resolve_backend(false), Backend::Winit);
    }
}
