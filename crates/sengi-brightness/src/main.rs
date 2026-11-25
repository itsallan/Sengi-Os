//! sengi-brightness, run by the compositor for the brightness keys:
//!
//!     sengi-brightness up|down|kbd-up|kbd-down|kbd-toggle
//!     sengi-brightness dim|restore
//!
//! It reads the device, works out the next step, has logind set it, and
//! tells sengi-osd what to show. It runs for a few milliseconds and is not
//! running otherwise.
//!
//! `dim` and `restore` are for the compositor when the computer is left
//! alone: `dim` turns the screen down and remembers what it was, `restore`
//! puts it back. The brightness keys count a dimmed screen as it was.

use std::process::{Command, ExitCode, Stdio};

use sengi_brightness::{
    dim_target, next_keyboard, next_screen, osd_arguments, read_keyboard, saved_path, take_saved,
    Key,
};
use sengi_system::backlight::{self, Backlight};
use sengi_system::bus::Bus;
use sengi_system::machine::Sysfs;

const USAGE: &str = "Usage: sengi-brightness up|down|kbd-up|kbd-down|kbd-toggle|dim|restore";

fn main() -> ExitCode {
    sengi_system::set_program_name("sengi-brightness");
    let mut arguments = std::env::args().skip(1);
    let key = match (arguments.next().as_deref(), arguments.next()) {
        (Some("-h" | "--help"), None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        (Some(word @ ("dim" | "restore")), None) => {
            return match idle(word == "dim") {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("sengi-brightness: {message}");
                    ExitCode::FAILURE
                }
            };
        }
        (Some(word), None) => match Key::parse(word) {
            Some(key) => key,
            None => {
                eprintln!("sengi-brightness: unknown key '{word}'\n{USAGE}");
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(key) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("sengi-brightness: {message}");
            ExitCode::FAILURE
        }
    }
}

fn runtime_dir() -> Result<std::path::PathBuf, String> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set".to_string())
}

/// Dims the screen, or brings it back from dimming. Nothing is shown.
fn idle(dim: bool) -> Result<(), String> {
    let class = sengi_system::sys_class();
    let runtime = runtime_dir()?;
    let light = backlight::read(&class.join("backlight")).ok_or("no backlight")?;
    let raw = if dim {
        // Dimmed already: what was saved is the brightness to go back to.
        if saved_path(&runtime).exists() {
            return Ok(());
        }
        let Some(target) = dim_target(&light) else {
            return Ok(());
        };
        std::fs::write(saved_path(&runtime), light.brightness.to_string())
            .map_err(|error| format!("cannot remember the brightness: {error}"))?;
        target
    } else {
        match take_saved(&runtime) {
            Some(before) => before.min(light.max),
            None => return Ok(()),
        }
    };
    let sysfs = Sysfs::new(std::sync::Arc::new(Bus::default()), class);
    sysfs
        .set_brightness("backlight", &light.name, raw)
        .map_err(|_| "logind would not set it".to_string())
}

fn run(key: Key) -> Result<(), String> {
    let class = sengi_system::sys_class();
    let screen = matches!(key, Key::Up | Key::Down);
    let mut dimmed = false;
    let (light, group, raw): (Backlight, &str, u32) = if screen {
        let mut light = backlight::read(&class.join("backlight")).ok_or("no backlight")?;
        // A dimmed screen is stepped from where it was before.
        if let Some(before) = runtime_dir().ok().and_then(|dir| take_saved(&dir)) {
            light.brightness = before.min(light.max);
            dimmed = true;
        }
        let raw = next_screen(&light, key == Key::Up);
        (light, "backlight", raw)
    } else {
        let light = read_keyboard(&class.join("leds")).ok_or("no keyboard light")?;
        let raw = next_keyboard(&light, key);
        (light, "leds", raw)
    };
    // A dimmed screen is set even if the key changes nothing: it is not
    // at the value the light says it is.
    if raw != light.brightness || dimmed {
        let sysfs = Sysfs::new(std::sync::Arc::new(Bus::default()), class);
        sysfs
            .set_brightness(group, &light.name, raw)
            .map_err(|_| "logind would not set it".to_string())?;
    }
    // Shown even when nothing changed, so the key at either end still
    // answers.
    let shown = Backlight {
        brightness: raw,
        ..light
    };
    let _ = Command::new("sengi-osd")
        .args(osd_arguments(key, shown.percent()))
        .stdin(Stdio::null())
        .spawn();
    Ok(())
}
