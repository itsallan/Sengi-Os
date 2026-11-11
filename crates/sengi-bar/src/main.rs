//! sengi-bar, the Sengi OS top bar.
//!
//! The bar is a Slint window on a wlr-layer-shell surface anchored to the
//! top of the screen. sengi-layer does the Wayland work; this file fills
//! the bar in and keeps it up to date.

mod status;
mod tray;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use sengi_layer::{Anchor, App, Config, Instances, Layer, Outputs};
use slint::{Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel};

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

/// The tray as the bars draw it. Every bar shows the same model.
#[derive(Default)]
struct TrayIcons {
    model: Rc<VecModel<TrayEntry>>,
    /// The image made from each item's pixels, so an update that leaves an
    /// icon alone does not make a new one.
    images: Vec<(u32, Arc<tray::Pixels>, Image)>,
}

impl TrayIcons {
    fn image(&self, id: u32, pixels: &Arc<tray::Pixels>) -> Option<Image> {
        let held = self.images.iter().find(|(held, ..)| *held == id);
        if let Some((_, _, image)) = held.filter(|(_, held, _)| held == pixels) {
            return Some(image.clone());
        }
        let side = pixels.size;
        if side == 0 || pixels.data.len() != (side * side * 4) as usize {
            return None;
        }
        let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels.data, side, side);
        Some(Image::from_rgba8_premultiplied(buffer))
    }

    /// Shows these items. Returns whether the set of items changed, as
    /// opposed to how some of them look.
    fn show(&mut self, views: Vec<tray::View>) -> bool {
        let mut images = Vec::new();
        let entries: Vec<TrayEntry> = views
            .into_iter()
            .map(|view| {
                let image = view.icon.as_ref().and_then(|pixels| {
                    let image = self.image(view.id, pixels)?;
                    images.push((view.id, pixels.clone(), image.clone()));
                    Some(image)
                });
                TrayEntry {
                    id: view.id as i32,
                    has_icon: image.is_some(),
                    icon: image.unwrap_or_default(),
                    letter: view.letter.into(),
                    label: view.label.into(),
                    attention: view.attention,
                }
            })
            .collect();
        self.images = images;

        let held: Vec<i32> = self.model.iter().map(|entry| entry.id).collect();
        let wanted: Vec<i32> = entries.iter().map(|entry| entry.id).collect();
        if held != wanted {
            self.model.set_vec(entries);
            return true;
        }
        // The same items: change rows in place, so the icon under the
        // pointer stays the icon under the pointer.
        for (row, entry) in entries.into_iter().enumerate() {
            if self.model.row_data(row).as_ref() != Some(&entry) {
                self.model.set_row_data(row, entry);
            }
        }
        false
    }
}

fn run() -> Result<(), sengi_layer::Error> {
    let app = App::new()?;
    let bars: Instances<Bar> = Instances::new();

    // The tray starts with the first bar, which is when the size icons are
    // drawn at is known. Its threads feed `icons` through a channel.
    let icons: Rc<RefCell<TrayIcons>> = Rc::default();
    let tray: Rc<OnceCell<Option<tray::Tray>>> = Rc::default();
    let tray_scale = Rc::new(Cell::new(1u32));

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
        bar.on_show_notifications(|| spawn("sengi-notify --toggle-centre"));
        bar.on_dismiss_notifications(|| spawn("sengi-notify --dismiss-all"));
        bar.on_open_session(|| spawn("sengi-power"));
        let mute = mute.clone();
        bar.on_toggle_mute(move || {
            let _ = mute.send(());
        });

        let icon_size = (bar.get_tray_icon_size().round() as u32).max(1);
        let scale = context.output().map_or(1, |output| output.scale.max(1));
        let started = tray.get_or_init(|| {
            tray_scale.set(scale);
            let (icons, bars) = (icons.clone(), bars.clone());
            let show = move |views| {
                if icons.borrow_mut().show(views) {
                    bars.for_each(|bar| bar.invoke_tray_changed());
                }
            };
            tray::Tray::start(&handle, icon_size * scale, show)
                .inspect_err(|error| eprintln!("sengi-bar: no system tray: {error}"))
                .ok()
        });
        if started.is_some() {
            bar.set_tray(ModelRc::from(icons.borrow().model.clone()));
            // Icons are decoded for the sharpest output there is.
            let note_scale = {
                let (tray, tray_scale) = (tray.clone(), tray_scale.clone());
                move |scale: u32| {
                    if scale > tray_scale.get() {
                        tray_scale.set(scale);
                        if let Some(Some(tray)) = tray.get() {
                            tray.set_pixels(icon_size * scale);
                        }
                    }
                }
            };
            note_scale(scale);
            context
                .surface()
                .on_output_changed(move |output| note_scale(output.scale));

            let output = context.output_name().unwrap_or_default().to_string();
            bar.on_tray_pressed({
                let tray = tray.clone();
                move |id, button, x| {
                    let button = match button {
                        0 => tray::Button::Left,
                        1 => tray::Button::Middle,
                        _ => tray::Button::Right,
                    };
                    if let (Some(Some(tray)), Ok(id)) = (tray.get(), u32::try_from(id)) {
                        tray.pressed(id, button, &output, x.round() as i32);
                    }
                }
            });
            bar.on_tray_scrolled({
                let tray = tray.clone();
                move |id, dx, dy| {
                    let (delta, horizontal) = if dx.abs() > dy.abs() {
                        (dx, true)
                    } else {
                        (dy, false)
                    };
                    // A wheel notch is 60 to Slint and 120 to the apps.
                    if let (Some(Some(tray)), Ok(id)) = (tray.get(), u32::try_from(id)) {
                        tray.scrolled(id, (delta * 2.0).round() as i32, horizontal);
                    }
                }
            });
        }

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
    // The bar runs itself again for two jobs it will not do in its own
    // process: decoding an icon file and drawing a tray menu.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = tray::icons::run_decoder(&args) {
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
