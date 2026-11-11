//! sengi-bar, the Sengi OS top bar.
//!
//! The bar is a Slint window on a wlr-layer-shell surface anchored to the
//! top of the screen. sengi-layer does the Wayland work; this file fills
//! the bar in and keeps it up to date.

mod status;
mod tray;

use std::cell::RefCell;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use sengi_layer::{Anchor, App, Config, Instances, Layer, Outputs};
use slint::{Timer, TimerMode};

slint::include_modules!();

/// How often the network and battery are read. Both are a few small files
/// under /sys.
const SYS_INTERVAL: Duration = Duration::from_secs(10);

/// How often the volume and Wi-Fi name are read. These run other programs,
/// so they are polled slowly on their own thread, away from drawing.
const SLOW_INTERVAL: Duration = Duration::from_secs(15);

/// Readings that take a subprocess to get.
#[derive(Clone, PartialEq, Eq)]
struct Slow {
    volume: Option<status::Volume>,
    wifi_name: Option<String>,
}

/// Runs a shell command without waiting for it. The extra shell exits at
/// once, so the command is adopted by the session and never left a zombie.
fn spawn(command: &str) {
    let result = Command::new("sh")
        .arg("-c")
        .arg(format!("{command} >/dev/null 2>&1 &"))
        .stdin(Stdio::null())
        .status();
    if let Err(error) = result {
        eprintln!("sengi-bar: could not run {command}: {error}");
    }
}

fn refresh_clock(bar: &Bar) {
    let now = chrono::Local::now();
    bar.set_clock(now.format("%a %-e %b  %H:%M").to_string().into());
}

/// Seconds from `second` of the current minute to the start of the next.
/// Never zero, so a timer that fires a touch early cannot spin.
fn seconds_until_next_minute(second: u32) -> u64 {
    u64::from(60 - second.min(59))
}

/// Shows the time now and again at the start of each minute, so the bar
/// wakes once a minute for the clock instead of once a second.
fn run_clock(timer: Rc<Timer>, bars: Instances<Bar>) {
    bars.for_each(refresh_clock);
    let second = chrono::Timelike::second(&chrono::Local::now());
    let until_next = Duration::from_secs(seconds_until_next_minute(second));
    let again = timer.clone();
    timer.start(TimerMode::SingleShot, until_next, move || {
        run_clock(again.clone(), bars.clone())
    });
}

/// Readings that are a few small files under /sys.
struct Sys {
    network: status::Network,
    battery: Option<status::Battery>,
}

fn read_sys() -> Sys {
    Sys {
        network: status::network(&status::read_interfaces(Path::new("/sys/class/net"))),
        battery: status::read_battery(Path::new("/sys/class/power_supply")),
    }
}

fn apply_sys(bar: &Bar, sys: &Sys) {
    bar.set_network(sys.network.label().into());
    bar.set_online(sys.network != status::Network::Offline);
    bar.set_wifi(sys.network == status::Network::Wifi);

    match &sys.battery {
        Some(battery) => {
            bar.set_battery(battery.label().into());
            bar.set_battery_low(battery.low());
            bar.set_battery_warning(battery.warning());
        }
        None => bar.set_battery("".into()),
    }
}

fn apply_slow(bar: &Bar, slow: &Slow) {
    match &slow.volume {
        Some(volume) => {
            bar.set_volume(volume.label().into());
            bar.set_muted(volume.muted);
        }
        None => bar.set_volume("".into()),
    }
    bar.set_wifi_name(slow.wifi_name.clone().unwrap_or_default().into());
}

fn read_slow() -> Slow {
    let wifi_up = status::read_interfaces(Path::new("/sys/class/net"))
        .iter()
        .any(|interface| interface.wireless && interface.up);
    Slow {
        volume: status::read_volume(),
        wifi_name: if wifi_up {
            status::read_wifi_name()
        } else {
            None
        },
    }
}

fn run() -> Result<(), sengi_layer::Error> {
    let app = App::new()?;
    let bars: Instances<Bar> = Instances::new();

    // The slow readings come from a worker thread. The mute button sends
    // on `mute`, and the worker toggles and reads the result back in order.
    // The latest reading is kept for bars that are built later.
    let latest: Rc<RefCell<Option<Slow>>> = Rc::default();
    let slow_sender = {
        let bars = bars.clone();
        let latest = latest.clone();
        app.handle().channel(move |slow: Slow| {
            bars.for_each(|bar| apply_slow(bar, &slow));
            *latest.borrow_mut() = Some(slow);
        })?
    };
    let (mute, mute_requests) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut last = None;
        let mut toggle = false;
        loop {
            if toggle {
                status::toggle_mute();
            }
            let slow = read_slow();
            if last.as_ref() != Some(&slow) {
                if slow_sender.send(slow.clone()).is_err() {
                    return;
                }
                last = Some(slow);
            }
            toggle = match mute_requests.recv_timeout(SLOW_INTERVAL) {
                Ok(()) => true,
                Err(mpsc::RecvTimeoutError::Timeout) => false,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
        }
    });

    let clock_timer = Rc::new(Timer::default());
    run_clock(clock_timer.clone(), bars.clone());
    let sys_timer = Timer::default();
    {
        let bars = bars.clone();
        sys_timer.start(TimerMode::Repeated, SYS_INTERVAL, move || {
            let sys = read_sys();
            bars.for_each(|bar| {
                apply_sys(bar, &sys);
                // The minute timer does not run while the machine is
                // suspended, so this catches the clock up after a resume.
                refresh_clock(bar);
            });
        });
    }

    let config = Config::new("sengi-bar")
        .layer(Layer::Top)
        .anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)
        // A bar on every monitor.
        .outputs(Outputs::All);

    app.run(config, move |context| {
        let bar = Bar::new()?;

        // The height comes from the design tokens, by way of the component.
        let height = (bar.get_bar_height().round() as u32).max(1);
        let handle = context.handle();
        handle.set_size(0, height)?;
        handle.set_exclusive_zone(height as i32);

        bar.on_open_launcher(|| spawn("sengi-launcher"));
        bar.on_open_network(|| spawn("sengi-run foot -e iwctl"));
        bar.on_show_notifications(|| spawn("makoctl restore"));
        bar.on_dismiss_notifications(|| spawn("makoctl dismiss --all"));
        bar.on_open_session(|| spawn("sengi-power"));
        let mute = mute.clone();
        bar.on_toggle_mute(move || {
            let _ = mute.send(());
        });

        apply_sys(&bar, &read_sys());
        refresh_clock(&bar);
        if let Some(slow) = latest.borrow().as_ref() {
            apply_slow(&bar, slow);
        }
        bars.add(&bar);
        Ok(bar)
    })
}

fn main() -> ExitCode {
    // The bar runs itself again to decode an icon file, which it will not
    // do in its own process.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = tray::icons::run_decoder(&args) {
        return code;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-bar: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_timer_always_waits_at_least_a_second() {
        assert_eq!(seconds_until_next_minute(0), 60);
        assert_eq!(seconds_until_next_minute(20), 40);
        assert_eq!(seconds_until_next_minute(59), 1);
        // A leap second reads as 60.
        assert_eq!(seconds_until_next_minute(60), 1);
    }
}
