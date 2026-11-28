//! sengi-bar, the Sengi OS top bar.
//!
//! The bar is a Slint window on a wlr-layer-shell surface anchored to the
//! top of the screen. sengi-layer does the Wayland work; this file fills
//! the bar in and keeps it up to date.

mod status;
mod tray;
mod tray_ui;
mod watch;

use std::cell::{OnceCell, RefCell};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use sengi_clock::Clock;
use sengi_layer::{Anchor, App, Config, Instances, Layer, Outputs};
use sengi_system::volume;
use slint::{Timer, TimerMode};
use watch::{Ask, Readers, Slow};

slint::include_modules!();

/// How often the network and battery are read. Both are a few small files
/// under /sys.
const SYS_INTERVAL: Duration = Duration::from_secs(10);

/// How often the volume and Wi-Fi name are read. These run other programs,
/// so they are polled slowly on their own thread, away from drawing. A
/// volume change that the shell makes is announced and read at once.
const SLOW_INTERVAL: Duration = Duration::from_secs(15);

/// The thread that hears those announcements only sleeps and forwards.
const LISTENER_STACK: usize = 64 * 1024;

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

fn refresh_clock(bar: &Bar, clock: &Clock) {
    bar.set_clock(clock.text(chrono::Utc::now()).into());
}

/// Seconds from `second` of the current minute to the start of the next.
/// Never zero, so a timer that fires a touch early cannot spin.
fn seconds_until_next_minute(second: u32) -> u64 {
    u64::from(60 - second.min(59))
}

/// Shows the time now and again at the start of each minute, so the bar
/// wakes once a minute for the clock instead of once a second. That
/// minute is also when it looks whether the clock's settings changed.
fn run_clock(timer: Rc<Timer>, bars: Instances<Bar>, clock: Rc<RefCell<Clock>>) {
    clock.borrow_mut().follow_settings();
    bars.for_each(|bar| refresh_clock(bar, &clock.borrow()));
    let second = chrono::Timelike::second(&chrono::Utc::now());
    let until_next = Duration::from_secs(seconds_until_next_minute(second));
    let again = timer.clone();
    timer.start(TimerMode::SingleShot, until_next, move || {
        run_clock(again.clone(), bars.clone(), clock.clone())
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
            bar.set_volume(status::volume_label(volume).into());
            bar.set_muted(volume.muted);
        }
        None => bar.set_volume("".into()),
    }
    bar.set_wifi_name(slow.wifi_name.clone().unwrap_or_default().into());
}

fn read_wifi() -> Option<String> {
    let wifi_up = status::read_interfaces(Path::new("/sys/class/net"))
        .iter()
        .any(|interface| interface.wireless && interface.up);
    if wifi_up {
        status::read_wifi_name()
    } else {
        None
    }
}

/// Passes on every announced volume change. Without the socket the bar
/// still catches up at the next slow reading.
fn listen_for_volume(asks: mpsc::Sender<Ask>) {
    let started = volume::Notices::listen().and_then(|notices| {
        thread::Builder::new()
            .stack_size(LISTENER_STACK)
            .spawn(move || {
                while notices.wait().is_ok() {
                    if asks.send(Ask::Volume).is_err() {
                        return;
                    }
                }
            })
    });
    if let Err(error) = started {
        eprintln!("sengi-bar: volume changes are only polled: {error}");
    }
}

fn run() -> Result<(), sengi_layer::Error> {
    let app = App::new()?;
    let bars: Instances<Bar> = Instances::new();

    // The tray starts with the first bar, which is when the size icons are
    // drawn at is known. None inside means it could not be started.
    let tray: OnceCell<Option<Rc<tray_ui::TrayUi>>> = OnceCell::new();

    // The slow readings come from a worker thread. The mute button and
    // announced volume changes send on `asks`, and the worker toggles and
    // reads in order. The latest reading is kept for bars built later.
    let latest: Rc<RefCell<Option<Slow>>> = Rc::default();
    let slow_sender = {
        let bars = bars.clone();
        let latest = latest.clone();
        app.handle().channel(move |slow: Slow| {
            bars.for_each(|bar| apply_slow(bar, &slow));
            *latest.borrow_mut() = Some(slow);
        })?
    };
    let (mute, asks) = mpsc::channel::<Ask>();
    listen_for_volume(mute.clone());
    thread::spawn(move || {
        let machine = Readers {
            has_output: status::has_output,
            volume: status::read_volume,
            wifi_name: read_wifi,
            toggle_mute: status::toggle_mute,
        };
        watch::run(&asks, SLOW_INTERVAL, &machine, |slow| {
            slow_sender.send(slow).is_ok()
        });
    });

    let clock_timer = Rc::new(Timer::default());
    let clock = Rc::new(RefCell::new(Clock::new()));
    run_clock(clock_timer.clone(), bars.clone(), clock.clone());
    let sys_timer = Timer::default();
    {
        let bars = bars.clone();
        let clock = clock.clone();
        sys_timer.start(TimerMode::Repeated, SYS_INTERVAL, move || {
            let sys = read_sys();
            bars.for_each(|bar| {
                apply_sys(bar, &sys);
                // The minute timer does not run while the machine is
                // suspended, so this catches the clock up after a resume.
                refresh_clock(bar, &clock.borrow());
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
        sengi_config::apply_theme!(slint::ComponentHandle::global::<ThemeSwitch>(&bar));

        // The height comes from the design tokens, by way of the component.
        let height = (bar.get_bar_height().round() as u32).max(1);
        let handle = context.handle();
        handle.set_size(0, height)?;
        handle.set_exclusive_zone(height as i32);

        bar.on_open_launcher(|| spawn("sengi-launcher"));
        // The network, volume and battery items open the quick settings
        // panel; the same command closes it again.
        bar.on_open_quick(|| spawn("sengi-quick"));
        bar.on_show_notifications(|| spawn("sengi-notify --toggle-centre"));
        bar.on_dismiss_notifications(|| spawn("sengi-notify --dismiss-all"));
        bar.on_open_session(|| spawn("sengi-power"));
        bar.on_open_calendar(|| spawn("sengi-calendar"));
        let mute = mute.clone();
        bar.on_toggle_mute(move || {
            let _ = mute.send(Ask::ToggleMute);
        });

        let started = tray.get_or_init(|| {
            let scale = context.output().map_or(1, |output| output.scale);
            tray_ui::TrayUi::start(&handle, &bar, scale, bars.clone())
                .inspect_err(|error| eprintln!("sengi-bar: no system tray: {error}"))
                .ok()
        });
        if let Some(tray) = started {
            tray.attach(&bar, context);
        }

        apply_sys(&bar, &read_sys());
        refresh_clock(&bar, &clock.borrow());
        if let Some(slow) = latest.borrow().as_ref() {
            apply_slow(&bar, slow);
        }
        bars.add(&bar);
        Ok(bar)
    })
}

fn main() -> ExitCode {
    // The bar runs itself again for two jobs it will not do in its own
    // process: decoding an icon file and drawing a tray menu.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = sengi_apps::icons::run_decoder(&args) {
        return code;
    }
    if let Some(code) = tray::menu_view::run(&args) {
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
