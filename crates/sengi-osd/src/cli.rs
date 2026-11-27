//! The command line.

use sengi_system::{commands, volume};

use crate::level::{Kind, Level};

pub const USAGE: &str = "\
Usage: sengi-osd volume
       sengi-osd show volume|brightness|keyboard PERCENT [muted]

Shows a level for a moment, near the bottom of the screen. `volume` reads
the volume of the default output, so it shows what a key really did. If an
indicator is open already it shows the new level instead of opening another.

  -h, --help       show this help
  -V, --version    show the version
";

#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Show(Level),
    /// Ask PipeWire what the volume is, then show it.
    Volume,
    Help,
    Version,
}

pub fn parse<S: AsRef<str>>(arguments: &[S]) -> Result<Invocation, String> {
    let arguments: Vec<&str> = arguments.iter().map(AsRef::as_ref).collect();
    match arguments.as_slice() {
        ["-h" | "--help"] => Ok(Invocation::Help),
        ["-V" | "--version"] => Ok(Invocation::Version),
        ["volume"] => Ok(Invocation::Volume),
        ["show", kind, percent, rest @ ..] if rest.len() <= 1 => {
            let kind = Kind::parse(kind).ok_or_else(|| format!("unknown kind '{kind}'"))?;
            let percent: u32 = percent
                .parse()
                .map_err(|_| format!("'{percent}' is not a percentage"))?;
            let muted = match rest {
                [] => false,
                ["muted"] => true,
                [other] => return Err(format!("unexpected '{other}'")),
                _ => unreachable!("at most one word is left"),
            };
            Ok(Invocation::Show(Level::new(kind, percent, muted)))
        }
        [] => Err("nothing to show".to_string()),
        [other, ..] => Err(format!("unknown argument '{other}'")),
    }
}

/// The volume of the default output as `wpctl` reports it. With no sound
/// device a volume key still shows something: a crossed speaker, empty.
pub fn read_volume() -> Level {
    let volume = volume::default_output(|args| {
        let output = std::process::Command::new(commands::WPCTL)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    });
    volume_level(volume)
}

fn volume_level(volume: Option<volume::Volume>) -> Level {
    match volume {
        Some(volume) => Level::new(Kind::Volume, volume.percent, volume.muted),
        None => Level::new(Kind::Volume, 0, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_takes_a_kind_a_percent_and_perhaps_muted() {
        assert_eq!(
            parse(&["show", "brightness", "55"]),
            Ok(Invocation::Show(Level::new(Kind::Brightness, 55, false)))
        );
        assert_eq!(
            parse(&["show", "volume", "30", "muted"]),
            Ok(Invocation::Show(Level::new(Kind::Volume, 30, true)))
        );
        assert_eq!(parse(&["volume"]), Ok(Invocation::Volume));
    }

    #[test]
    fn no_sound_device_shows_as_muted_and_empty() {
        let level = volume_level(None);
        assert!(level.muted);
        assert_eq!(level.bar(), 0);
    }

    #[test]
    fn bad_arguments_are_said_so() {
        assert!(parse::<&str>(&[]).is_err());
        assert!(parse(&["show", "colour", "5"]).is_err());
        assert!(parse(&["show", "volume", "loud"]).is_err());
        assert!(parse(&["show", "volume", "5", "loudly"]).is_err());
        assert!(parse(&["show", "volume", "5", "muted", "x"]).is_err());
        assert!(parse(&["hide"]).is_err());
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parse(&["--help"]), Ok(Invocation::Help));
        assert_eq!(parse(&["-V"]), Ok(Invocation::Version));
    }
}
