//! Runs a second Wayland event queue on a worker thread next to the UI loop
//! and checks that neither gets in the other's way.
//!
//! The worker takes the connection from `Handle::connection`, binds every
//! `wl_output` through a registry of its own and keeps a chain of
//! `wl_display.sync` round trips going, reporting each one to the UI over a
//! channel. Meanwhile the UI animates without pause and resizes its surface
//! four times a second. At the end it prints what it saw and fails if a
//! report went missing, the worker stopped, or the UI loop stalled.
//!
//!     cargo run -p sengi-layer --example soak -- 120

use std::cell::RefCell;
use std::process::ExitCode;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use sengi_layer::wayland_client::protocol::{wl_callback, wl_display, wl_output, wl_registry};
use sengi_layer::wayland_client::{Connection, Dispatch, QueueHandle};
use sengi_layer::{App, Config, Layer, Sender};
use slint::{ComponentHandle, Timer, TimerMode};

slint::slint! {
    import { Tokens, SengiSurface, SengiText } from "../../../ui/components.slint";

    export component Soak inherits Window {
        in property <string> status;
        // Read by the program to size the layer surface.
        out property <length> narrow: Tokens.launcher-width / 2;
        out property <length> wide: Tokens.launcher-width;
        out property <length> surface-height: Tokens.bar-height + Tokens.space-xl;

        background: transparent;
        default-font-family: Tokens.font-family;

        SengiSurface {
            raised: true;

            SengiText {
                text: root.status;
                horizontal-alignment: center;
                vertical-alignment: center;
            }

            // Never still, so the loop is always drawing.
            Rectangle {
                width: Tokens.space-xl;
                height: Tokens.space-xs;
                y: parent.height - self.height - Tokens.space-xs;
                x: (parent.width - self.width) * (0.5 + 0.5 * sin(animation-tick() / 2s * 360deg));
                border-radius: Tokens.radius;
                background: Tokens.color-accent;
            }
        }
    }
}

/// What the worker tells the UI.
enum Report {
    /// A round trip finished; they are numbered from 1.
    RoundTrip(u64),
    /// An event arrived for one of the worker's own `wl_output` objects.
    OutputEvent,
}

struct Worker {
    display: wl_display::WlDisplay,
    reports: Sender<Report>,
    round_trips: u64,
    /// The UI loop has ended; stop asking for more.
    finished: bool,
}

impl Worker {
    fn report(&mut self, report: Report) {
        if self.reports.send(report).is_err() {
            self.finished = true;
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Worker {
    fn event(
        _: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == "wl_output" {
                registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), queue, ());
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Worker {
    fn event(
        worker: &mut Self,
        _: &wl_output::WlOutput,
        _: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        worker.report(Report::OutputEvent);
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Worker {
    fn event(
        worker: &mut Self,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        worker.round_trips += 1;
        worker.report(Report::RoundTrip(worker.round_trips));
        if !worker.finished {
            // Not inside a read: nothing is held while this thread sleeps.
            thread::sleep(Duration::from_millis(2));
            worker.display.sync(queue, ());
        }
    }
}

fn run_worker(connection: Connection, reports: Sender<Report>) {
    let mut queue = connection.new_event_queue::<Worker>();
    let handle = queue.handle();
    let display = connection.display();
    display.get_registry(&handle, ());
    display.sync(&handle, ());
    let mut worker = Worker {
        display,
        reports,
        round_trips: 0,
        finished: false,
    };
    while !worker.finished {
        if let Err(error) = queue.blocking_dispatch(&mut worker) {
            eprintln!("soak: the worker's queue failed: {error}");
            return;
        }
    }
}

#[derive(Default)]
struct Seen {
    round_trips: u64,
    out_of_order: u64,
    output_events: u64,
    resizes: u64,
    /// The longest the UI loop went without running its 50 ms timer.
    longest_gap: Duration,
    last_tick: Option<Instant>,
}

fn pixels(length: f32) -> u32 {
    length.round().max(1.0) as u32
}

fn run(seconds: u64) -> Result<Seen, sengi_layer::Error> {
    let app = App::new()?;
    let handle = app.handle();
    let seen = Rc::new(RefCell::new(Seen::default()));

    let reports = {
        let seen = seen.clone();
        handle.channel(move |report: Report| {
            let mut seen = seen.borrow_mut();
            match report {
                Report::RoundTrip(number) => {
                    if number != seen.round_trips + 1 {
                        seen.out_of_order += 1;
                    }
                    seen.round_trips = number;
                }
                Report::OutputEvent => seen.output_events += 1,
            }
        })?
    };
    let connection = handle.connection();
    thread::spawn(move || run_worker(connection, reports));

    let ticker = Timer::default();
    {
        let seen = seen.clone();
        ticker.start(TimerMode::Repeated, Duration::from_millis(50), move || {
            let mut seen = seen.borrow_mut();
            let now = Instant::now();
            if let Some(last) = seen.last_tick {
                seen.longest_gap = seen.longest_gap.max(now - last);
            }
            seen.last_tick = Some(now);
        });
    }
    let stop = Timer::default();
    {
        let handle = handle.clone();
        stop.start(
            TimerMode::SingleShot,
            Duration::from_secs(seconds),
            move || handle.quit(),
        );
    }

    let resizer = Timer::default();
    let status = Timer::default();
    let started = Instant::now();
    let for_build = seen.clone();
    app.run(
        Config::new("sengi-layer-soak").layer(Layer::Top),
        move |context| {
            let soak = Soak::new()?;
            let surface = context.surface();
            let sizes = [pixels(soak.get_narrow()), pixels(soak.get_wide())];
            let height = pixels(soak.get_surface_height());
            surface.set_size(sizes[0], height)?;

            let seen = for_build.clone();
            resizer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
                let mut seen = seen.borrow_mut();
                seen.resizes += 1;
                let _ = surface.set_size(sizes[(seen.resizes % 2) as usize], height);
            });

            let seen = for_build.clone();
            let weak = soak.as_weak();
            status.start(TimerMode::Repeated, Duration::from_millis(500), move || {
                if let Some(soak) = weak.upgrade() {
                    let seen = seen.borrow();
                    soak.set_status(
                        format!(
                            "{} s, {} round trips, {} resizes",
                            started.elapsed().as_secs(),
                            seen.round_trips,
                            seen.resizes
                        )
                        .into(),
                    );
                }
            });
            Ok(soak)
        },
    )?;

    drop(ticker);
    drop(stop);
    let seen = std::mem::take(&mut *seen.borrow_mut());
    Ok(seen)
}

fn main() -> ExitCode {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|argument| argument.parse().ok())
        .unwrap_or(120);
    let seen = match run(seconds) {
        Ok(seen) => seen,
        Err(error) => {
            eprintln!("soak: {error}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "soak: {seconds} s, {} round trips on the worker queue ({} out of order), \
         {} output events, {} resizes, longest UI gap {} ms",
        seen.round_trips,
        seen.out_of_order,
        seen.output_events,
        seen.resizes,
        seen.longest_gap.as_millis()
    );

    // The worker sleeps 2 ms a round trip, so a healthy run manages well
    // over a hundred a second.
    let healthy = seen.out_of_order == 0
        && seen.output_events > 0
        && seen.round_trips > seconds * 50
        && seen.resizes >= seconds * 3
        && seen.longest_gap < Duration::from_millis(500);
    if healthy {
        println!("soak: ok");
        ExitCode::SUCCESS
    } else {
        println!("soak: FAILED");
        ExitCode::FAILURE
    }
}
