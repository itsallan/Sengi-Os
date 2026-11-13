//! The Display page in the window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Timer, TimerMode};

use super::{scale_words, Change, Command, DisplayService, Event, Output, Reading, SCALES, TRIAL};
use crate::app::{self, App, Choice, Controller};
use crate::cli::Page;
use crate::ui::{DialogSpec, DisplayData, DisplayOutput};
use crate::worker::Handle;

/// How long leaving waits for the worker to put a change on trial back.
/// It may have to ask the compositor more than once.
const LAST_REVERT: Duration = Duration::from_secs(8);

type Worker = Arc<Mutex<Option<Handle<Command>>>>;

/// Stop the worker, which undoes a change still on trial as it goes,
/// and wait for that. From any thread, and from two at once: a signal
/// and the window closing can both come here, and neither may go on,
/// the one to end the process, while the other still waits. So the
/// handle is left where it is, for the second to wait on as well, and
/// the lock is not held over the wait. The process is ending: nothing
/// will want a new worker.
fn stop_and_wait(worker: &Worker) {
    let handle = worker.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if let Some(handle) = handle {
        if !handle.stop_and_wait(LAST_REVERT) {
            eprintln!("sengi-settings: the display worker had not finished at exit");
        }
    }
}

pub struct DisplayPage {
    service: Arc<dyn DisplayService>,
    /// Running while the page shows or a change is on trial.
    worker: Worker,
    /// Which worker this is, counting from the first. What an earlier
    /// one still has to say is not listened to.
    generation: Cell<u64>,
    reading: RefCell<Reading>,
    /// When the change on trial is undone, while one is.
    deadline: Cell<Option<Instant>>,
    /// Ticks the countdown in the dialog. Runs only while it is shown.
    clock: Timer,
}

/// What the dialog says with `left` to go.
pub fn countdown(left: Duration) -> String {
    match left.as_secs_f32().ceil() as u64 {
        0 | 1 => "Going back to the previous settings in 1 second.".to_string(),
        seconds => format!("Going back to the previous settings in {seconds} seconds."),
    }
}

fn shown(output: &Output) -> DisplayOutput {
    let size = output.current.map(|mode| mode.size());
    DisplayOutput {
        name: output.name.as_str().into(),
        title: output.title.as_str().into(),
        connector: output.name.as_str().into(),
        enabled: output.enabled,
        resolution: output
            .current
            .map_or("Unknown".to_string(), |mode| mode.size_words())
            .into(),
        refresh: output
            .current
            .map_or("Unknown".to_string(), |mode| mode.refresh_words())
            .into(),
        scale: scale_words(output.scale).into(),
        can_resolution: output.enabled && output.sizes().len() > 1,
        can_refresh: output.enabled && size.is_some_and(|size| output.rates(size).len() > 1),
    }
}

impl DisplayPage {
    pub fn attach(app: &Rc<App>, service: Arc<dyn DisplayService>) -> Rc<DisplayPage> {
        let page = Rc::new(DisplayPage {
            service,
            worker: Worker::default(),
            generation: Cell::new(0),
            reading: RefCell::default(),
            deadline: Cell::new(None),
            clock: Timer::default(),
        });
        // Told to go while a change is on trial: it is undone first.
        app::on_terminate({
            let worker = page.worker.clone();
            move || stop_and_wait(&worker)
        });
        let data = app.window.global::<DisplayData>();
        let (weak, weak_app) = (Rc::downgrade(&page), Rc::downgrade(app));
        data.on_choose(move |name, what| {
            if let (Some(page), Some(app)) = (weak.upgrade(), weak_app.upgrade()) {
                page.choose(&app, &name, &what);
            }
        });
        let weak = Rc::downgrade(&page);
        data.on_brightness(move |percent| {
            if let Some(page) = weak.upgrade() {
                page.send(Command::Brightness(percent.clamp(0, 100) as u32));
            }
        });
        page
    }

    fn send(&self, command: Command) {
        let worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(worker) = worker.as_ref() {
            worker.send(command);
        }
    }

    /// End the worker without waiting for it. If it had just put a
    /// change on trial that this side has not heard of, it undoes it.
    fn stop_worker(&self) {
        let worker = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(worker) = worker {
            worker.stop();
        }
    }

    fn output(&self, name: &str) -> Option<Output> {
        let reading = self.reading.borrow();
        reading
            .outputs
            .iter()
            .flatten()
            .find(|output| output.name == name)
            .cloned()
    }

    /// One of an output's buttons was pressed: offer the choices.
    fn choose(self: &Rc<Self>, app: &Rc<App>, name: &str, what: &str) {
        let Some(output) = self.output(name) else {
            return;
        };
        let weak = Rc::downgrade(self);
        let apply = move |change: Option<Change>| {
            if let (Some(page), Some(change)) = (weak.upgrade(), change) {
                page.send(Command::Apply(change));
            }
        };
        let now = output.current;
        match what {
            "resolution" => {
                let sizes = output.sizes();
                let choices = sizes
                    .iter()
                    .enumerate()
                    .map(|(index, size)| Choice {
                        id: index.to_string(),
                        title: format!("{} × {}", size.0, size.1),
                        detail: String::new(),
                        current: now.is_some_and(|mode| mode.size() == *size),
                    })
                    .collect();
                app.choose("Resolution", false, choices, move |_, id| {
                    let size = id.parse::<usize>().ok().and_then(|i| sizes.get(i).copied());
                    let mode = size.and_then(|size| output.mode_for(size));
                    apply(mode.filter(|mode| Some(*mode) != now).map(|mode| Change {
                        output: output.name.clone(),
                        mode: Some(mode),
                        scale: None,
                    }));
                });
            }
            "refresh" => {
                let Some(now) = now else { return };
                let rates = output.rates(now.size());
                let choices = rates
                    .iter()
                    .enumerate()
                    .map(|(index, mode)| Choice {
                        id: index.to_string(),
                        title: mode.refresh_words(),
                        detail: String::new(),
                        current: mode.refresh == now.refresh,
                    })
                    .collect();
                app.choose("Refresh rate", false, choices, move |_, id| {
                    let mode = id.parse::<usize>().ok().and_then(|i| rates.get(i).copied());
                    apply(mode.filter(|mode| *mode != now).map(|mode| Change {
                        output: output.name.clone(),
                        mode: Some(mode),
                        scale: None,
                    }));
                });
            }
            "scale" => {
                let choices = SCALES
                    .iter()
                    .map(|scale| Choice {
                        id: scale.to_string(),
                        title: scale_words(*scale),
                        detail: String::new(),
                        current: *scale == output.scale,
                    })
                    .collect();
                app.choose("Scale", false, choices, move |_, id| {
                    let scale = id.parse::<u32>().ok().filter(|s| SCALES.contains(s));
                    apply(
                        scale
                            .filter(|scale| *scale != output.scale)
                            .map(|scale| Change {
                                output: output.name.clone(),
                                mode: None,
                                scale: Some(scale),
                            }),
                    );
                });
            }
            _ => {}
        }
    }

    pub fn on_event(self: &Rc<Self>, app: &Rc<App>, generation: u64, event: Event) {
        if generation != self.generation.get() {
            return;
        }
        match event {
            Event::Reading(reading) => {
                *self.reading.borrow_mut() = reading;
                self.render(app);
            }
            Event::Trying => self.ask_to_keep(app),
            Event::Kept => self.end_trial(app),
            Event::Reverted { timed_out } => {
                self.end_trial(app);
                if timed_out {
                    app.tell(
                        "The previous display settings are back",
                        "The new settings were not confirmed in time, so they were undone.",
                    );
                }
            }
            Event::RevertFailed(error) => {
                self.end_trial(app);
                app.tell(
                    "The previous display settings could not be restored",
                    &format!(
                        "{} The display may still be using the settings that were being tried.",
                        error.words()
                    ),
                );
            }
            Event::Failed(error) => {
                app.tell("The display setting was not applied", error.words());
            }
        }
    }

    fn end_trial(&self, app: &Rc<App>) {
        self.clock.stop();
        if self.deadline.take().is_some() {
            app.close_dialog();
        }
        // The trial was all that kept the worker of a hidden page.
        if app.current() != Some(Page::Display) {
            self.stop_worker();
        }
    }

    /// A change is in effect. Ask whether to keep it, and count down.
    fn ask_to_keep(self: &Rc<Self>, app: &Rc<App>) {
        let deadline = Instant::now() + TRIAL;
        self.deadline.set(Some(deadline));
        let (keep, revert) = (Rc::downgrade(self), Rc::downgrade(self));
        app.open_dialog(
            DialogSpec {
                title: "Keep these display settings?".into(),
                body: "If the picture looks wrong, or you cannot read this, wait and the \
                       previous settings will come back by themselves."
                    .into(),
                confirm: "Keep".into(),
                cancel: "Go back".into(),
                // Go back is the button a stray Return should press.
                danger: true,
                ..DialogSpec::default()
            },
            move |_, _| {
                if let Some(page) = keep.upgrade() {
                    page.send(Command::Keep);
                }
            },
            move |_| {
                if let Some(page) = revert.upgrade() {
                    // Closed already; the worker's answer must not close
                    // some other dialog.
                    page.deadline.set(None);
                    page.clock.stop();
                    page.send(Command::Revert);
                }
            },
        );
        app.set_dialog_countdown(&countdown(TRIAL));
        let weak_app = Rc::downgrade(app);
        self.clock
            .start(TimerMode::Repeated, Duration::from_millis(250), move || {
                if let Some(app) = weak_app.upgrade() {
                    let left = deadline.saturating_duration_since(Instant::now());
                    app.set_dialog_countdown(&countdown(left));
                }
            });
    }

    fn render(&self, app: &Rc<App>) {
        let data = app.window.global::<DisplayData>();
        let reading = self.reading.borrow();
        data.set_note(
            match &reading.outputs {
                None => {
                    "The compositor does not offer display settings, so resolution and \
                     scale cannot be changed here."
                }
                Some(outputs) if outputs.is_empty() => "No display is connected.",
                Some(_) => "",
            }
            .into(),
        );
        let outputs: Vec<DisplayOutput> = reading.outputs.iter().flatten().map(shown).collect();
        data.set_outputs(app::model(outputs));
        data.set_has_backlight(reading.backlight.is_some());
        if let (Some(light), false) = (&reading.backlight, data.get_brightness_held()) {
            data.set_brightness_value(light.percent() as i32);
        }
    }
}

impl Controller for DisplayPage {
    fn show(&self, _app: &Rc<App>) {
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        match worker.as_ref() {
            Some(worker) => worker.refresh(),
            None => {
                let generation = self.generation.get() + 1;
                self.generation.set(generation);
                *worker = Some(super::start(self.service.clone(), move |event| {
                    app::post(move |app| {
                        if let Some(page) = app.page::<DisplayPage>() {
                            page.on_event(app, generation, event);
                        }
                    });
                }));
            }
        }
    }

    /// Nobody is looking, so nothing needs reading: the worker goes,
    /// unless a change is on trial, which only it can undo in time.
    fn hide(&self, _app: &Rc<App>) {
        if self.deadline.get().is_none() {
            self.stop_worker();
        }
    }

    fn shutdown(&self) {
        self.clock.stop();
        // The worker undoes a change still on trial as it stops. Wait for
        // that: the process must not exit with it half done.
        stop_and_wait(&self.worker);
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::testing::{laptop, mode};

    #[test]
    fn the_countdown_counts_whole_seconds_down_to_one() {
        assert!(countdown(TRIAL).contains("15 seconds"));
        assert!(countdown(Duration::from_millis(14_200)).contains("15 seconds"));
        assert!(countdown(Duration::from_millis(1_800)).contains("2 seconds"));
        assert!(countdown(Duration::from_millis(900)).contains("1 second."));
        assert!(countdown(Duration::ZERO).contains("1 second."));
    }

    #[test]
    fn an_output_is_shown_with_what_can_be_changed() {
        let shown_laptop = shown(&laptop());
        assert_eq!(shown_laptop.resolution, "1920 × 1080");
        assert_eq!(shown_laptop.refresh, "60 Hz");
        assert_eq!(shown_laptop.scale, "100%");
        assert!(shown_laptop.can_resolution && shown_laptop.can_refresh);

        let mut one_rate = laptop();
        one_rate.current = Some(mode(1600, 900, 60));
        assert!(!shown(&one_rate).can_refresh);

        let mut off = laptop();
        off.enabled = false;
        off.current = None;
        let shown_off = shown(&off);
        assert!(!shown_off.can_resolution && !shown_off.can_refresh);
        assert_eq!(shown_off.resolution, "Unknown");
    }
}
