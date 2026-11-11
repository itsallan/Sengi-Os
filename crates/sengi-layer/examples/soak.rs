//! Runs a second Wayland event queue on a worker thread next to the UI loop
//! and checks that neither gets in the other's way.
//!
//! The worker takes the connection from `Handle::connection`, binds every
//! `wl_output` through a registry of its own and keeps a chain of
//! `wl_display.sync` round trips going with `sengi_layer::dispatch_queue`,
//! reporting each one to the UI over a channel.
//!
//! The run has two halves. In the first the UI animates without pause and
//! resizes its surface four times a second. In the second it does nothing
//! at all: no animation, no timers, no traffic of its own. The idle half is
//! the hard one for the worker, because nothing but its own events can wake
//! it. At the end the example prints what it saw and fails if a report went
//! missing, the worker fell behind in either half, or the UI loop stalled.
//!
//!     cargo run -p sengi-layer --example soak -- 120
//!
//! With `--blocking-dispatch` the worker uses `EventQueue::blocking_dispatch`
//! instead, which is the obvious way and the wrong one: it stalls in the
//! idle half within seconds, and the run fails. It is kept to show why.
//!
//!     cargo run -p sengi-layer --example soak -- 20 --blocking-dispatch

use std::cell::RefCell;
use std::ops::ControlFlow;
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
        in property <bool> busy: true;
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

            // Never still while busy, so the loop is always drawing.
            Rectangle {
                width: Tokens.space-xl;
                height: Tokens.space-xs;
                y: parent.height - self.height - Tokens.space-xs;
                x: root.busy
                    ? (parent.width - self.width) * (0.5 + 0.5 * sin(animation-tick() / 2s * 360deg))
                    : 0;
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

/// How long the worker rests between round trips. Without it the two
/// threads would do nothing but talk to the compositor.
const PAUSE: Duration = Duration::from_millis(2);

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
            thread::sleep(PAUSE);
            worker.display.sync(queue, ());
        }
    }
}

fn run_worker(connection: Connection, reports: Sender<Report>, blocking: bool) {
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

    let result = if blocking {
        // The wrong way, kept to show the stall. This reads the socket in
        // competition with the UI loop and can go to sleep with its own
        // event already waiting on its queue.
        loop {
            if worker.finished {
                break Ok(());
            }
            if let Err(error) = queue.blocking_dispatch(&mut worker) {
                break Err(error);
            }
        }
    } else {
        sengi_layer::dispatch_queue(&mut queue, &mut worker, |worker| {
            if worker.finished {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
    };
    if let Err(error) = result {
        eprintln!("soak: the worker's queue failed: {error}");
    }
}

#[derive(Default)]
struct Seen {
    round_trips: u64,
    out_of_order: u64,
    output_events: u64,
    resizes: u64,
    /// Round trips reported by the time the UI went idle.
    round_trips_while_busy: u64,
    /// The longest the UI loop went without running its 50 ms timer.
    longest_gap: Duration,
    last_tick: Option<Instant>,
    /// When the worker last reported a round trip.
    last_report: Option<Instant>,
    /// How long the worker had been silent when the run ended.
    silence: Option<Duration>,
}

fn pixels(length: f32) -> u32 {
    length.round().max(1.0) as u32
}

fn run(seconds: u64, blocking: bool) -> Result<Seen, sengi_layer::Error> {
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
                    seen.last_report = Some(Instant::now());
                }
                Report::OutputEvent => seen.output_events += 1,
            }
        })?
    };
    let connection = handle.connection();
    thread::spawn(move || run_worker(connection, reports, blocking));

    // The timers of the busy half. They are dropped, and so stopped, when
    // the UI goes idle.
    let busy_timers = Rc::new(RefCell::new(Vec::<Timer>::new()));

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
    busy_timers.borrow_mut().push(ticker);

    // The one timer of the idle half is its end; until it fires the loop
    // has nothing of its own to wake for.
    let stop = Timer::default();
    {
        let handle = handle.clone();
        let seen = seen.clone();
        stop.start(
            TimerMode::SingleShot,
            Duration::from_secs(seconds),
            move || {
                let mut seen = seen.borrow_mut();
                seen.silence = seen.last_report.map(|last| last.elapsed());
                handle.quit();
            },
        );
    }

    let go_idle = Timer::default();
    let started = Instant::now();
    let for_build = seen.clone();
    let config = Config::new("sengi-layer-soak").layer(Layer::Top);
    app.run(config, move |context| {
        let soak = Soak::new()?;
        let surface = context.surface();
        let sizes = [pixels(soak.get_narrow()), pixels(soak.get_wide())];
        let height = pixels(soak.get_surface_height());
        surface.set_size(sizes[0], height)?;

        let resizer = Timer::default();
        let seen = for_build.clone();
        resizer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
            let mut seen = seen.borrow_mut();
            seen.resizes += 1;
            let _ = surface.set_size(sizes[(seen.resizes % 2) as usize], height);
        });

        let status = Timer::default();
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
        busy_timers.borrow_mut().extend([resizer, status]);

        let seen = for_build.clone();
        let weak = soak.as_weak();
        let busy_timers = busy_timers.clone();
        go_idle.start(
            TimerMode::SingleShot,
            Duration::from_secs(seconds / 2),
            move || {
                busy_timers.borrow_mut().clear();
                let mut seen = seen.borrow_mut();
                seen.round_trips_while_busy = seen.round_trips;
                if let Some(soak) = weak.upgrade() {
                    soak.set_busy(false);
                    soak.set_status("idle".into());
                }
            },
        );
        Ok(soak)
    })?;

    drop(stop);
    let seen = std::mem::take(&mut *seen.borrow_mut());
    Ok(seen)
}

fn main() -> ExitCode {
    let mut seconds = 120;
    let mut blocking = false;
    for argument in std::env::args().skip(1) {
        if argument == "--blocking-dispatch" {
            blocking = true;
        } else if let Ok(number) = argument.parse() {
            seconds = number;
        } else {
            eprintln!("usage: soak [seconds] [--blocking-dispatch]");
            return ExitCode::FAILURE;
        }
    }

    let seen = match run(seconds, blocking) {
        Ok(seen) => seen,
        Err(error) => {
            eprintln!("soak: {error}");
            return ExitCode::FAILURE;
        }
    };

    let half = seconds / 2;
    let silence = seen.silence.unwrap_or(Duration::MAX);
    let while_idle = seen.round_trips - seen.round_trips_while_busy;
    println!(
        "soak: {} round trips in {half} s with a busy UI, {while_idle} in {} s with an idle UI \
         ({} out of order, last heard {} ms before the end), {} output events, {} resizes, \
         longest UI gap {} ms",
        seen.round_trips_while_busy,
        seconds - half,
        seen.out_of_order,
        silence.as_millis(),
        seen.output_events,
        seen.resizes,
        seen.longest_gap.as_millis()
    );

    // With a 2 ms pause a healthy worker manages well over a hundred round
    // trips a second, whatever the UI is doing, and is still at it when the
    // run ends. A stalled one has gone quiet.
    let healthy = seen.out_of_order == 0
        && silence < Duration::from_millis(500)
        && seen.output_events > 0
        && seen.round_trips_while_busy > half * 50
        && while_idle > (seconds - half) * 50
        && seen.resizes >= half * 3
        && seen.longest_gap < Duration::from_millis(500);
    if healthy {
        println!("soak: ok");
        ExitCode::SUCCESS
    } else {
        println!("soak: FAILED");
        ExitCode::FAILURE
    }
}
