//! The Display page's rules: what each connected output can do, how a
//! change of mode or scale is tried, and how it is taken back if nobody
//! says to keep it. `wlr` talks to the compositor, `backlight` reads the
//! panel's brightness, and `page` puts it all in the window.
//!
//! A mode the panel cannot show leaves the user looking at nothing, with
//! no way to press a button they cannot see. So no change is final until
//! it is confirmed, and the worker, not the window, holds the clock: if
//! the window hangs, or is closed, the old settings still come back. The
//! clock is looked at on every turn of the worker's loop, so news from
//! the compositor, however steady, cannot put the moment off.

pub mod page;
pub mod wlr;

use std::sync::Arc;

/// The backlight under `/sys/class/backlight`, as sengi-system reads it.
pub use sengi_system::backlight;
use std::time::{Duration, Instant};

use crate::worker::{self, Handle, Inbox, Notify, Waited};

/// How long a change waits to be confirmed.
pub const TRIAL: Duration = Duration::from_secs(15);
/// How many times going back is tried when the compositor says the
/// outputs changed under it.
pub const REVERT_TRIES: u32 = 4;
pub const MAX_OUTPUTS: usize = 16;
pub const MAX_MODES: usize = 256;

/// The scales offered, in thousandths.
pub const SCALES: [u32; 5] = [1000, 1250, 1500, 1750, 2000];

/// Sizes offered on an output that lists no modes of its own and takes
/// any size: a virtual machine's display, a nested or headless session.
pub const CUSTOM_SIZES: [(i32, i32); 6] = [
    (1024, 768),
    (1280, 720),
    (1280, 800),
    (1366, 768),
    (1600, 900),
    (1920, 1080),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: i32,
    pub height: i32,
    /// In thousandths of a hertz, as the compositor gives it. Zero when
    /// it does not say.
    pub refresh: i32,
    /// Not one of the output's own modes: set by size alone.
    pub custom: bool,
}

impl Mode {
    pub fn size(&self) -> (i32, i32) {
        (self.width, self.height)
    }

    pub fn size_words(&self) -> String {
        format!("{} × {}", self.width, self.height)
    }

    pub fn refresh_words(&self) -> String {
        if self.refresh <= 0 {
            return "Automatic".to_string();
        }
        let hertz = f64::from(self.refresh) / 1000.0;
        if (hertz - hertz.round()).abs() < 0.005 {
            format!("{hertz:.0} Hz")
        } else {
            format!("{hertz:.2} Hz")
        }
    }

    fn sane(&self) -> bool {
        (1..=16384).contains(&self.width)
            && (1..=16384).contains(&self.height)
            && (0..=1_000_000).contains(&self.refresh)
    }
}

pub fn scale_words(scale: u32) -> String {
    format!("{}%", (scale + 5) / 10)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The connector's name, such as `eDP-1`: how the service knows it.
    pub name: String,
    /// What to call it: the maker's description, cleaned.
    pub title: String,
    pub enabled: bool,
    /// The output's own modes. Empty for one that takes any size.
    pub modes: Vec<Mode>,
    pub current: Option<Mode>,
    /// In thousandths.
    pub scale: u32,
}

impl Output {
    /// The sizes that can be chosen, largest first, each once.
    pub fn sizes(&self) -> Vec<(i32, i32)> {
        let mut sizes: Vec<(i32, i32)> = if self.modes.is_empty() {
            let mut sizes = CUSTOM_SIZES.to_vec();
            sizes.extend(self.current.map(|mode| mode.size()));
            sizes
        } else {
            self.modes.iter().map(Mode::size).collect()
        };
        sizes.sort_by_key(|size| std::cmp::Reverse((size.0 * size.1, size.0)));
        sizes.dedup();
        sizes
    }

    /// The modes of one size, fastest first, each rate once.
    pub fn rates(&self, size: (i32, i32)) -> Vec<Mode> {
        let mut rates: Vec<Mode> = self
            .modes
            .iter()
            .filter(|mode| mode.size() == size)
            .copied()
            .collect();
        rates.sort_by_key(|mode| std::cmp::Reverse(mode.refresh));
        rates.dedup_by_key(|mode| mode.refresh);
        rates
    }

    /// The mode to use for `size`: the current rate if that size has it,
    /// else its fastest; a custom mode when the output lists none.
    pub fn mode_for(&self, size: (i32, i32)) -> Option<Mode> {
        if self.modes.is_empty() {
            let known = CUSTOM_SIZES.contains(&size)
                || self.current.is_some_and(|mode| mode.size() == size);
            return known.then_some(Mode {
                width: size.0,
                height: size.1,
                refresh: 0,
                custom: true,
            });
        }
        let rates = self.rates(size);
        let same = self
            .current
            .and_then(|now| rates.iter().find(|mode| mode.refresh == now.refresh));
        same.or(rates.first()).copied()
    }

    /// Whether `change` would leave this output different.
    pub fn changed_by(&self, change: &Change) -> bool {
        change.mode.is_some_and(|mode| Some(mode) != self.current)
            || change.scale.is_some_and(|scale| scale != self.scale)
    }

    /// The change that puts things back as they are now.
    pub fn restore(&self) -> Change {
        Change {
            output: self.name.clone(),
            mode: self.current,
            scale: Some(self.scale),
        }
    }
}

/// Something to set on one output. What is `None` is left as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub output: String,
    pub mode: Option<Mode>,
    pub scale: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyError {
    /// The compositor turned the configuration down.
    Refused,
    /// The outputs changed while it was being made. Nothing was applied.
    Outdated,
    /// The output is not there, or there is no compositor to ask.
    Unavailable,
    /// The output does not have the mode that was asked for.
    NoSuchMode,
    /// The compositor did not answer in time.
    Silent,
}

impl ApplyError {
    pub fn words(self) -> &'static str {
        match self {
            ApplyError::Refused => "The display did not accept that setting.",
            ApplyError::Outdated => {
                "The displays changed while the setting was being applied. Try again."
            }
            ApplyError::Unavailable => "The display is no longer there.",
            ApplyError::NoSuchMode => "The display does not offer that mode any more.",
            ApplyError::Silent => "The compositor did not answer.",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reading {
    /// `None` when the compositor does not offer output management.
    pub outputs: Option<Vec<Output>>,
    pub backlight: Option<backlight::Backlight>,
}

pub trait DisplayService: Send + Sync + 'static {
    fn read(&self) -> Reading;
    /// Returns when the compositor has taken the change or refused it.
    fn apply(&self, change: &Change) -> Result<(), ApplyError>;
    fn set_brightness(&self, name: &str, raw: u32);
    /// Have `changed` called when a later read could differ. A second
    /// call replaces the first.
    fn watch(&self, changed: Notify);
    /// Catch up with the compositor after it said the outputs changed,
    /// so that the next `apply` is made against what is there now.
    fn settle(&self) {}
}

/// A change that is in effect but not yet kept.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pending {
    restore: Change,
    deadline: Instant,
}

/// The trial of one change at a time.
#[derive(Debug)]
pub struct Trial {
    pending: Option<Pending>,
    length: Duration,
}

impl Default for Trial {
    fn default() -> Trial {
        Trial::lasting(TRIAL)
    }
}

impl Trial {
    pub fn lasting(length: Duration) -> Trial {
        Trial {
            pending: None,
            length,
        }
    }

    /// A change has been applied; `restore` undoes it. If another was
    /// still on trial, going back means going back to before that one.
    pub fn begin(&mut self, restore: Change, now: Instant) {
        let restore = match self.pending.take() {
            Some(earlier) if earlier.restore.output == restore.output => earlier.restore,
            _ => restore,
        };
        self.pending = Some(Pending {
            restore,
            deadline: now + self.length,
        });
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    /// The user said to keep it.
    pub fn keep(&mut self) -> bool {
        self.pending.take().is_some()
    }

    /// The user said to go back, or the window is closing: what to apply.
    pub fn abandon(&mut self) -> Option<Change> {
        self.pending.take().map(|pending| pending.restore)
    }

    /// What to apply if the time is up.
    pub fn expire(&mut self, now: Instant) -> Option<Change> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.abandon()
        } else {
            None
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Apply(Change),
    Keep,
    Revert,
    /// A slider position, in percent.
    Brightness(u32),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Reading(Reading),
    /// A change is in effect and will be undone unless kept in time.
    Trying,
    Kept,
    /// The old settings are back. `timed_out` when nobody answered.
    Reverted {
        timed_out: bool,
    },
    Failed(ApplyError),
    /// The old settings could not be put back.
    RevertFailed(ApplyError),
}

/// Put things back. An answer that the outputs changed meanwhile, or no
/// answer, is not the end of it: nobody chose the settings on trial.
fn revert(service: &dyn DisplayService, restore: &Change) -> Result<(), ApplyError> {
    let mut last = ApplyError::Outdated;
    for _ in 0..REVERT_TRIES {
        match service.apply(restore) {
            Ok(()) => return Ok(()),
            Err(error @ (ApplyError::Outdated | ApplyError::Silent)) => {
                last = error;
                service.settle();
            }
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

fn undo(service: &dyn DisplayService, restore: &Change, timed_out: bool, emit: &impl Fn(Event)) {
    match revert(service, restore) {
        Ok(()) => emit(Event::Reverted { timed_out }),
        Err(error) => {
            eprintln!(
                "sengi-settings: could not put {} back as it was: {}",
                restore.output,
                error.words()
            );
            emit(Event::RevertFailed(error));
        }
    }
}

pub fn run(
    service: Arc<dyn DisplayService>,
    handle: Handle<Command>,
    inbox: Inbox<Command>,
    emit: impl Fn(Event) + Send + Sync + 'static,
) {
    run_with(service, handle, inbox, emit, Trial::default());
}

fn run_with(
    service: Arc<dyn DisplayService>,
    handle: Handle<Command>,
    mut inbox: Inbox<Command>,
    emit: impl Fn(Event) + Send + Sync + 'static,
    mut trial: Trial,
) {
    service.watch(handle.notifier());
    emit(Event::Reading(service.read()));
    loop {
        // Before anything else, whatever woke the loop.
        if let Some(restore) = trial.expire(Instant::now()) {
            undo(&*service, &restore, true, &emit);
            emit(Event::Reading(service.read()));
        }
        let command = match inbox.wait_until(trial.deadline()) {
            Waited::Stopped => break,
            Waited::TimedOut => continue,
            Waited::Refresh => None,
            Waited::Command(command) => Some(command),
        };
        match command {
            None => {}
            Some(Command::Apply(change)) => {
                let before = service.read();
                let output = before
                    .outputs
                    .iter()
                    .flatten()
                    .find(|output| output.name == change.output);
                match output {
                    None => emit(Event::Failed(ApplyError::Unavailable)),
                    // Nothing would change, so there is nothing to keep.
                    Some(output) if !output.changed_by(&change) => {}
                    // Without the mode it runs at now there is no way back.
                    Some(output) if change.mode.is_some() && output.current.is_none() => {
                        emit(Event::Failed(ApplyError::Unavailable));
                    }
                    // No answer in time is not a no: the compositor may
                    // yet do as it was asked. So that is a trial too, to
                    // be kept or undone like any other, and never a
                    // change that arrives later with nobody to undo it.
                    Some(output) => match service.apply(&change) {
                        Ok(()) | Err(ApplyError::Silent) => {
                            trial.begin(output.restore(), Instant::now());
                            emit(Event::Trying);
                        }
                        Err(error) => emit(Event::Failed(error)),
                    },
                }
            }
            Some(Command::Keep) => {
                if trial.keep() {
                    emit(Event::Kept);
                }
            }
            Some(Command::Revert) => {
                if let Some(restore) = trial.abandon() {
                    undo(&*service, &restore, false, &emit);
                }
            }
            Some(Command::Brightness(percent)) => {
                if let Some(light) = service.read().backlight {
                    service.set_brightness(&light.name, light.raw(percent));
                }
            }
        }
        emit(Event::Reading(service.read()));
    }
    // The window is going away with a change still on trial. Nobody can
    // say to keep it now, and there is no window to report to.
    if let Some(restore) = trial.abandon() {
        if let Err(error) = revert(&*service, &restore) {
            eprintln!(
                "sengi-settings: could not put {} back as it was: {}",
                restore.output,
                error.words()
            );
        }
    }
}

pub fn start(
    service: Arc<dyn DisplayService>,
    emit: impl Fn(Event) + Send + Sync + 'static,
) -> Handle<Command> {
    worker::spawn("display", move |handle, inbox| {
        run(service, handle, inbox, emit)
    })
}

/// A made-up compositor, for the tests and, in debug builds only, for
/// looking at the page with displays the machine at hand does not have.
#[cfg(any(test, debug_assertions))]
pub mod testing {
    use super::*;
    use std::sync::Mutex;

    pub fn mode(width: i32, height: i32, hertz: i32) -> Mode {
        Mode {
            width,
            height,
            refresh: hertz * 1000,
            custom: false,
        }
    }

    pub fn laptop() -> Output {
        Output {
            name: "eDP-1".to_string(),
            title: "Built-in display".to_string(),
            enabled: true,
            modes: vec![
                mode(1920, 1080, 60),
                mode(1920, 1080, 48),
                mode(1600, 900, 60),
                mode(1280, 720, 60),
            ],
            current: Some(mode(1920, 1080, 60)),
            scale: 1000,
        }
    }

    #[derive(Default)]
    pub struct Fake {
        pub outputs: Mutex<Vec<Output>>,
        pub backlight: Mutex<Option<backlight::Backlight>>,
        pub log: Mutex<Vec<String>>,
        pub refuse: Mutex<bool>,
        /// This many of the next changes are answered as made against
        /// outputs that have changed since.
        pub stale: Mutex<u32>,
        /// How often it was asked to catch up.
        pub settled: Mutex<u32>,
        /// This many of the next changes are made, but the answer comes
        /// too late to be heard.
        pub slow: Mutex<u32>,
    }

    impl Fake {
        pub fn with(outputs: Vec<Output>) -> Arc<Fake> {
            Arc::new(Fake {
                outputs: Mutex::new(outputs),
                ..Fake::default()
            })
        }

        pub fn log(&self) -> Vec<String> {
            self.log.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }
    }

    impl DisplayService for Fake {
        fn read(&self) -> Reading {
            Reading {
                outputs: Some(
                    self.outputs
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone(),
                ),
                backlight: self
                    .backlight
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone(),
            }
        }

        fn apply(&self, change: &Change) -> Result<(), ApplyError> {
            if *self.refuse.lock().unwrap_or_else(|p| p.into_inner()) {
                return Err(ApplyError::Refused);
            }
            {
                let mut stale = self.stale.lock().unwrap_or_else(|p| p.into_inner());
                if *stale > 0 {
                    *stale -= 1;
                    return Err(ApplyError::Outdated);
                }
            }
            let mut outputs = self.outputs.lock().unwrap_or_else(|p| p.into_inner());
            let output = outputs
                .iter_mut()
                .find(|output| output.name == change.output)
                .ok_or(ApplyError::Unavailable)?;
            if let Some(mode) = change.mode {
                output.current = Some(mode);
            }
            if let Some(scale) = change.scale {
                output.scale = scale;
            }
            let now = output.current.map(|m| m.size_words()).unwrap_or_default();
            self.log
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(format!(
                    "{} {now} {}",
                    output.name,
                    scale_words(output.scale)
                ));
            let mut slow = self.slow.lock().unwrap_or_else(|p| p.into_inner());
            if *slow > 0 {
                *slow -= 1;
                return Err(ApplyError::Silent);
            }
            Ok(())
        }

        fn set_brightness(&self, name: &str, raw: u32) {
            self.log
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(format!("brightness {name} {raw}"));
            if let Some(light) = self
                .backlight
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_mut()
            {
                light.brightness = raw;
            }
        }

        fn watch(&self, _changed: Notify) {}

        fn settle(&self) {
            *self.settled.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::sync::{mpsc, Mutex};

    fn change(width: i32, height: i32) -> Change {
        Change {
            output: "eDP-1".to_string(),
            mode: Some(mode(width, height, 60)),
            scale: None,
        }
    }

    #[test]
    fn modes_in_words() {
        assert_eq!(mode(1920, 1080, 60).size_words(), "1920 × 1080");
        assert_eq!(mode(1920, 1080, 60).refresh_words(), "60 Hz");
        let odd = Mode {
            refresh: 59_940,
            ..mode(1920, 1080, 0)
        };
        assert_eq!(odd.refresh_words(), "59.94 Hz");
        assert_eq!(mode(800, 600, 0).refresh_words(), "Automatic");
        assert_eq!(scale_words(1000), "100%");
        assert_eq!(scale_words(1250), "125%");
        assert!(mode(1920, 1080, 60).sane());
        assert!(!mode(0, 1080, 60).sane());
        assert!(!mode(1920, -1, 60).sane());
        assert!(!mode(100_000, 1080, 60).sane());
    }

    #[test]
    fn sizes_and_rates_are_listed_once_largest_first() {
        let output = laptop();
        assert_eq!(output.sizes(), [(1920, 1080), (1600, 900), (1280, 720)]);
        let rates: Vec<i32> = output
            .rates((1920, 1080))
            .iter()
            .map(|m| m.refresh)
            .collect();
        assert_eq!(rates, [60_000, 48_000]);
        assert!(output.rates((640, 480)).is_empty());
    }

    #[test]
    fn a_size_keeps_the_current_rate_when_it_can() {
        let mut output = laptop();
        output.current = Some(mode(1920, 1080, 48));
        // 1600 × 900 has no 48 Hz: its fastest.
        assert_eq!(output.mode_for((1600, 900)), Some(mode(1600, 900, 60)));
        assert_eq!(output.mode_for((1920, 1080)), Some(mode(1920, 1080, 48)));
        // A size the output does not have cannot be asked for.
        assert_eq!(output.mode_for((640, 480)), None);
    }

    #[test]
    fn an_output_without_modes_is_offered_common_sizes() {
        let virtual_output = Output {
            modes: Vec::new(),
            current: Some(Mode {
                custom: true,
                refresh: 0,
                ..mode(1024, 700, 0)
            }),
            ..laptop()
        };
        let sizes = virtual_output.sizes();
        assert_eq!(sizes[0], (1920, 1080));
        assert!(sizes.contains(&(1024, 700)));
        let chosen = virtual_output.mode_for((1280, 720)).unwrap();
        assert!(chosen.custom && chosen.refresh == 0);
        assert_eq!(virtual_output.mode_for((123, 45)), None);
    }

    #[test]
    fn a_trial_ends_by_keeping_reverting_or_running_out() {
        let start = Instant::now();
        let restore = laptop().restore();

        let mut trial = Trial::default();
        assert_eq!(trial.deadline(), None);
        assert!(!trial.keep());
        assert_eq!(trial.abandon(), None);

        trial.begin(restore.clone(), start);
        assert_eq!(trial.deadline(), Some(start + TRIAL));
        assert_eq!(trial.expire(start + TRIAL - Duration::from_millis(1)), None);
        assert!(trial.keep());
        assert_eq!(trial.expire(start + TRIAL * 2), None);

        trial.begin(restore.clone(), start);
        assert_eq!(trial.abandon(), Some(restore.clone()));

        trial.begin(restore.clone(), start);
        assert_eq!(trial.expire(start + TRIAL), Some(restore));
        // Only once.
        assert_eq!(trial.expire(start + TRIAL * 2), None);
    }

    #[test]
    fn a_second_change_on_trial_still_goes_back_to_the_start() {
        let start = Instant::now();
        let original = laptop().restore();
        let mut after_first = laptop();
        after_first.current = Some(mode(1600, 900, 60));

        let mut trial = Trial::default();
        trial.begin(original.clone(), start);
        // The user changes again without answering: the clock restarts,
        // but what is restored is what they started from.
        let later = start + Duration::from_secs(10);
        trial.begin(after_first.restore(), later);
        assert_eq!(trial.deadline(), Some(later + TRIAL));
        assert_eq!(trial.abandon(), Some(original));
    }

    fn worked(fake: &Arc<Fake>, commands: Vec<Command>) -> Vec<Event> {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        for command in commands {
            handle.send(command);
        }
        handle.stop();
        run(fake.clone(), handle, inbox, move |event| {
            let _ = tx.lock().unwrap().send(event);
        });
        rx.try_iter()
            .filter(|event| !matches!(event, Event::Reading(_)))
            .collect()
    }

    #[test]
    fn a_change_nobody_answers_is_undone_by_the_worker_itself() {
        let fake = Fake::with(vec![laptop()]);
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        handle.send(Command::Apply(change(1600, 900)));
        let thread = std::thread::spawn({
            let (fake, handle) = (fake.clone(), handle.clone());
            move || {
                let trial = Trial::lasting(Duration::from_millis(60));
                run_with(
                    fake,
                    handle,
                    inbox,
                    move |event| {
                        let _ = tx.lock().unwrap().send(event);
                    },
                    trial,
                )
            }
        });
        // Nothing is sent: no keep, no revert, no window at all.
        let mut seen = Vec::new();
        while !seen.contains(&Event::Reverted { timed_out: true }) {
            seen.push(rx.recv_timeout(Duration::from_secs(5)).expect("the revert"));
        }
        assert!(seen.contains(&Event::Trying));
        handle.stop();
        thread.join().unwrap();
        // Undone once, not again on the way out.
        assert_eq!(
            fake.log(),
            ["eDP-1 1600 × 900 100%", "eDP-1 1920 × 1080 100%"]
        );
    }

    /// Runs the worker on a thread of its own with a trial of `length`.
    fn working(
        fake: &Arc<Fake>,
        length: Duration,
    ) -> (
        Handle<Command>,
        mpsc::Receiver<Event>,
        std::thread::JoinHandle<()>,
    ) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let (handle, inbox) = worker::channel();
        let thread = std::thread::spawn({
            let (fake, handle) = (fake.clone(), handle.clone());
            move || {
                run_with(
                    fake,
                    handle,
                    inbox,
                    move |event| {
                        let _ = tx.lock().unwrap().send(event);
                    },
                    Trial::lasting(length),
                )
            }
        });
        (handle, rx, thread)
    }

    /// Events up to and including the first that `last` picks out.
    fn until(rx: &mpsc::Receiver<Event>, last: impl Fn(&Event) -> bool) -> Vec<Event> {
        let mut seen = Vec::new();
        loop {
            let event = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the event waited for");
            let done = last(&event);
            if !matches!(event, Event::Reading(_)) {
                seen.push(event);
            }
            if done {
                return seen;
            }
        }
    }

    #[test]
    fn steady_news_from_the_compositor_does_not_put_the_revert_off() {
        let fake = Fake::with(vec![laptop()]);
        *fake.backlight.lock().unwrap() = backlight::parse("intel_backlight", "100", "1000");
        let (handle, rx, thread) = working(&fake, Duration::from_millis(150));
        handle.send(Command::Apply(change(1600, 900)));
        // An output that flaps and a hand on the brightness keys: the
        // worker is never left to wait in peace.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let noise = std::thread::spawn({
            let (handle, stop) = (handle.clone(), stop.clone());
            move || {
                let mut turn = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    handle.refresh();
                    if turn.is_multiple_of(8) {
                        handle.send(Command::Brightness(40 + turn % 50));
                    }
                    turn += 1;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        let started = Instant::now();
        let seen = until(&rx, |event| matches!(event, Event::Reverted { .. }));
        let took = started.elapsed();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        noise.join().unwrap();
        handle.stop();
        thread.join().unwrap();
        assert_eq!(seen, [Event::Trying, Event::Reverted { timed_out: true }]);
        // The trial and a burst of news waited out, not for as long as
        // the news keeps coming.
        assert!(took < Duration::from_secs(5), "{took:?}");
        let sizes: Vec<String> = fake
            .log()
            .into_iter()
            .filter(|line| line.starts_with("eDP-1"))
            .collect();
        assert_eq!(sizes, ["eDP-1 1600 × 900 100%", "eDP-1 1920 × 1080 100%"]);
    }

    #[test]
    fn a_revert_the_compositor_cancels_is_tried_again() {
        let fake = Fake::with(vec![laptop()]);
        let (handle, rx, thread) = working(&fake, Duration::from_secs(60));
        handle.send(Command::Apply(change(1600, 900)));
        until(&rx, |event| *event == Event::Trying);
        *fake.stale.lock().unwrap() = 2;
        handle.send(Command::Revert);
        let seen = until(&rx, |event| matches!(event, Event::Reverted { .. }));
        assert_eq!(seen, [Event::Reverted { timed_out: false }]);
        // It caught up with the compositor before each new try.
        assert_eq!(*fake.settled.lock().unwrap(), 2);
        assert_eq!(
            fake.log(),
            ["eDP-1 1600 × 900 100%", "eDP-1 1920 × 1080 100%"]
        );
        handle.stop();
        thread.join().unwrap();
    }

    #[test]
    fn a_revert_that_never_goes_through_is_reported() {
        let fake = Fake::with(vec![laptop()]);
        let (handle, rx, thread) = working(&fake, Duration::from_secs(60));
        handle.send(Command::Apply(change(1600, 900)));
        until(&rx, |event| *event == Event::Trying);
        *fake.stale.lock().unwrap() = 1000;
        handle.send(Command::Revert);
        let seen = until(&rx, |event| matches!(event, Event::RevertFailed(_)));
        assert_eq!(seen, [Event::RevertFailed(ApplyError::Outdated)]);
        assert_eq!(*fake.stale.lock().unwrap(), 1000 - REVERT_TRIES);
        // The new mode is still there, and nothing says otherwise.
        assert_eq!(fake.log(), ["eDP-1 1600 × 900 100%"]);
        handle.stop();
        thread.join().unwrap();
        // A refusal is final: asking again would get the same answer.
        let fake = Fake::with(vec![laptop()]);
        *fake.refuse.lock().unwrap() = true;
        assert_eq!(
            revert(&*fake, &laptop().restore()),
            Err(ApplyError::Refused)
        );
        assert_eq!(*fake.settled.lock().unwrap(), 0);
    }

    #[test]
    fn a_change_that_changes_nothing_is_not_put_on_trial() {
        let fake = Fake::with(vec![laptop()]);
        let same = Change {
            output: "eDP-1".to_string(),
            mode: Some(mode(1920, 1080, 60)),
            scale: Some(1000),
        };
        assert!(worked(&fake, vec![Command::Apply(same)]).is_empty());
        assert!(fake.log().is_empty());
    }

    #[test]
    fn a_mode_is_not_tried_without_one_to_go_back_to() {
        let mut blind = laptop();
        blind.current = None;
        let fake = Fake::with(vec![blind]);
        let events = worked(&fake, vec![Command::Apply(change(1600, 900))]);
        assert_eq!(events, [Event::Failed(ApplyError::Unavailable)]);
        assert!(fake.log().is_empty());
        // A scale can still be: the scale it has now is known.
        let scale = Change {
            output: "eDP-1".to_string(),
            mode: None,
            scale: Some(2000),
        };
        assert_eq!(worked(&fake, vec![Command::Apply(scale)]), [Event::Trying]);
    }

    #[test]
    fn a_kept_change_stays() {
        let fake = Fake::with(vec![laptop()]);
        let events = worked(
            &fake,
            vec![Command::Apply(change(1600, 900)), Command::Keep],
        );
        assert_eq!(events, [Event::Trying, Event::Kept]);
        assert_eq!(fake.log(), ["eDP-1 1600 × 900 100%"]);
    }

    #[test]
    fn a_change_the_user_takes_back_is_undone() {
        let fake = Fake::with(vec![laptop()]);
        let events = worked(
            &fake,
            vec![Command::Apply(change(1600, 900)), Command::Revert],
        );
        assert_eq!(
            events,
            [Event::Trying, Event::Reverted { timed_out: false }]
        );
        assert_eq!(
            fake.log(),
            ["eDP-1 1600 × 900 100%", "eDP-1 1920 × 1080 100%"]
        );
    }

    #[test]
    fn closing_the_window_with_a_change_on_trial_undoes_it() {
        let fake = Fake::with(vec![laptop()]);
        let scale = Change {
            output: "eDP-1".to_string(),
            mode: None,
            scale: Some(2000),
        };
        let events = worked(&fake, vec![Command::Apply(scale)]);
        assert_eq!(events, [Event::Trying]);
        assert_eq!(
            fake.log(),
            ["eDP-1 1920 × 1080 200%", "eDP-1 1920 × 1080 100%"]
        );
    }

    #[test]
    fn a_refused_change_starts_no_trial() {
        let fake = Fake::with(vec![laptop()]);
        *fake.refuse.lock().unwrap() = true;
        let events = worked(
            &fake,
            vec![Command::Apply(change(1600, 900)), Command::Keep],
        );
        assert_eq!(events, [Event::Failed(ApplyError::Refused)]);
        let missing = Change {
            output: "HDMI-9".to_string(),
            ..change(1600, 900)
        };
        *fake.refuse.lock().unwrap() = false;
        let events = worked(&fake, vec![Command::Apply(missing)]);
        assert_eq!(events, [Event::Failed(ApplyError::Unavailable)]);
        assert!(fake.log().is_empty());
    }

    #[test]
    fn a_change_the_compositor_was_slow_to_answer_is_on_trial_all_the_same() {
        let fake = Fake::with(vec![laptop()]);
        *fake.slow.lock().unwrap() = 1;
        // The worker is stopped with the trial still running.
        let events = worked(&fake, vec![Command::Apply(change(1600, 900))]);
        assert_eq!(events, [Event::Trying]);
        // It took effect after all, and was undone on the way out.
        let log = fake.log();
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(log[0].contains("1600") && log[1].contains("1920"), "{log:?}");

        // And it can be kept.
        *fake.slow.lock().unwrap() = 1;
        let events = worked(
            &fake,
            vec![Command::Apply(change(1600, 900)), Command::Keep],
        );
        assert_eq!(events, [Event::Trying, Event::Kept]);
        assert_eq!(fake.log().len(), 3);
    }

    #[test]
    fn brightness_goes_to_the_backlight_there_is() {
        let fake = Fake::with(vec![laptop()]);
        assert!(worked(&fake, vec![Command::Brightness(50)]).is_empty());
        assert!(fake.log().is_empty());
        *fake.backlight.lock().unwrap() = backlight::parse("intel_backlight", "100", "1000");
        worked(&fake, vec![Command::Brightness(50), Command::Brightness(0)]);
        // Never all the way to dark.
        assert_eq!(
            fake.log(),
            [
                "brightness intel_backlight 500",
                "brightness intel_backlight 50"
            ]
        );
    }
}
