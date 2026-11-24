//! sengi-brightness, run by the compositor for the brightness keys:
//!
//!     sengi-brightness up|down|kbd-up|kbd-down|kbd-toggle
//!
//! It reads the device, works out the next step, has logind set it, and
//! tells sengi-osd what to show. It runs for a few milliseconds and is not
//! running otherwise.

use std::process::{Command, ExitCode, Stdio};

use sengi_brightness::{next_keyboard, next_screen, osd_arguments, read_keyboard, Key};
use sengi_system::backlight::{self, Backlight};
use sengi_system::bus::Bus;
use sengi_system::machine::Sysfs;

const USAGE: &str = "Usage: sengi-brightness up|down|kbd-up|kbd-down|kbd-toggle";

fn main() -> ExitCode {
    sengi_system::set_program_name("sengi-brightness");
    let mut arguments = std::env::args().skip(1);
    let key = match (arguments.next().as_deref(), arguments.next()) {
        (Some("-h" | "--help"), None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
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

fn run(key: Key) -> Result<(), String> {
    let class = sengi_system::sys_class();
    let screen = matches!(key, Key::Up | Key::Down);
    let (light, group, raw): (Backlight, &str, u32) = if screen {
        let light = backlight::read(&class.join("backlight")).ok_or("no backlight")?;
        let raw = next_screen(&light, key == Key::Up);
        (light, "backlight", raw)
    } else {
        let light = read_keyboard(&class.join("leds")).ok_or("no keyboard light")?;
        let raw = next_keyboard(&light, key);
        (light, "leds", raw)
    };
    if raw != light.brightness {
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
