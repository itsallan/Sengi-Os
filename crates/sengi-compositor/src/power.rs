//! What happens when nobody has touched the computer for a while: the
//! screen is dimmed, then switched off, and the session may lock. And what
//! the lid does when it is closed, if Settings has it do nothing.
//!
//! The times are the user's, from `power.*` in the settings. Nothing here
//! polls: one timer is set for the next time something is due, it is
//! moved on when it fires, and input only notes the time it came at.
//! `IdleClock` knows nothing of the compositor or of real time, so the
//! tests give it the time themselves.

use std::{
    process::{Command, Stdio},
    time::Duration,
};

use calloop::timer::{TimeoutAction, Timer};

use crate::state::State;

/// When each step comes, as the time without input. `None` is never.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Plan {
    pub dim: Option<Duration>,
    pub off: Option<Duration>,
    pub lock: Option<Duration>,
}

impl Plan {
    /// From the settings, whole seconds with zero for never. Dimming only
    /// makes sense before the screen goes off, so a time that is not
    /// earlier than that is dropped.
    pub fn from_seconds(dim: i64, off: i64, lock: i64) -> Plan {
        let time = |seconds: i64| (seconds > 0).then(|| Duration::from_secs(seconds as u64));
        let (dim, off) = (time(dim), time(off));
        Plan {
            dim: dim.filter(|dim| off.is_none_or(|off| *dim < off)),
            off,
            lock: time(lock),
        }
    }
}

/// What a step does to the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Dim,
    Off,
    Lock,
}

/// How far the screen has been taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Awake,
    Dimmed,
    Off,
}

pub struct IdleClock {
    plan: Plan,
    /// When the last input came, on the clock the caller keeps.
    last_input: Duration,
    stage: Stage,
    /// The lock step has come in this stretch of idleness.
    locked: bool,
    /// Something, a video say, asks for the computer not to be taken for
    /// idle.
    held: bool,
}

impl IdleClock {
    pub fn new(plan: Plan, now: Duration) -> Self {
        Self {
            plan,
            last_input: now,
            stage: Stage::Awake,
            locked: false,
            held: false,
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn plan(&self) -> Plan {
        self.plan
    }

    /// Another set of times. What has already happened stays: a screen
    /// that is off stays off until input, and one that is dimmed under a
    /// plan that has no dimming is brought back by the caller, see
    /// `State::set_idle_plan`.
    pub fn set_plan(&mut self, plan: Plan) {
        self.plan = plan;
    }

    /// Input came at `now`. Returns how far the screen had gone, so that
    /// the caller can bring it back.
    pub fn activity(&mut self, now: Duration) -> Stage {
        let before = self.stage;
        self.last_input = now;
        self.stage = Stage::Awake;
        self.locked = false;
        before
    }

    /// Whether something holds idleness off, from `now`. Holding counts
    /// as input; so does letting go, since the time the hold lasted was
    /// not idle time. Returns how far the screen had gone.
    pub fn hold(&mut self, held: bool, now: Duration) -> Stage {
        if held == self.held {
            return Stage::Awake;
        }
        self.held = held;
        self.activity(now)
    }

    /// The steps whose time has come at `now`, each of them once in a
    /// stretch of idleness, the first one first. A late timer, as after
    /// the computer slept, does not dim a screen that is to be off.
    pub fn due(&mut self, now: Duration) -> Vec<Step> {
        if self.held {
            return Vec::new();
        }
        let idle = now.saturating_sub(self.last_input);
        let reached = |time: Option<Duration>| time.is_some_and(|time| idle >= time);
        let mut steps = Vec::new();
        let off = reached(self.plan.off) && self.stage != Stage::Off;
        let dim = reached(self.plan.dim) && self.stage == Stage::Awake && !off;
        let lock = reached(self.plan.lock) && !self.locked;
        let mut order = Vec::new();
        if dim {
            order.push((self.plan.dim, Step::Dim));
        }
        if off {
            order.push((self.plan.off, Step::Off));
        }
        if lock {
            order.push((self.plan.lock, Step::Lock));
        }
        order.sort_by_key(|(time, _)| *time);
        for (_, step) in order {
            match step {
                Step::Dim => self.stage = Stage::Dimmed,
                Step::Off => self.stage = Stage::Off,
                Step::Lock => self.locked = true,
            }
            steps.push(step);
        }
        steps
    }

    /// When the next step is due, on the caller's clock: `None` when
    /// nothing is left to happen until there is input.
    pub fn next_deadline(&self) -> Option<Duration> {
        if self.held {
            return None;
        }
        let mut times = Vec::new();
        if self.stage == Stage::Awake {
            times.extend(self.plan.dim);
        }
        if self.stage != Stage::Off {
            times.extend(self.plan.off);
        }
        if !self.locked {
            times.extend(self.plan.lock);
        }
        times.into_iter().min().map(|idle| self.last_input + idle)
    }

    /// One line for the SIGUSR1 report.
    pub fn describe(&self) -> String {
        let stage = match self.stage {
            Stage::Awake => "awake",
            Stage::Dimmed => "dimmed",
            Stage::Off => "off",
        };
        let seconds = |time: Option<Duration>| {
            time.map_or("never".to_string(), |time| format!("{}s", time.as_secs()))
        };
        format!(
            "idle: {stage}{}, dim {}, off {}, lock {}",
            if self.locked {
                ", locked by idleness"
            } else {
                ""
            },
            seconds(self.plan.dim),
            seconds(self.plan.off),
            seconds(self.plan.lock),
        )
    }
}

/// The program that dims the screen and brings it back, as the brightness
/// keys' own. The compositor has no bus to ask logind on.
const DIM: &str = "exec sengi-brightness dim";
const UNDIM: &str = "exec sengi-brightness restore";

impl State {
    /// The time on the idle clock: since the compositor started.
    fn idle_now(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Input came, or something that counts as input. Brings the screen
    /// back to full brightness if it was dimmed, or off; lighting an output
    /// that is off is `note_activity`'s.
    pub(crate) fn idle_input(&mut self) {
        let now = self.idle_now();
        let before = self.idle_clock.activity(now);
        self.after_idle(before);
    }

    fn after_idle(&mut self, before: Stage) {
        if before != Stage::Awake {
            self.spawn_plain(UNDIM);
            crate::say!("idle: back from {before:?}");
        }
        self.ensure_idle_timer();
    }

    /// Something holds idleness off, or has stopped to.
    pub(crate) fn idle_hold(&mut self, held: bool) {
        let now = self.idle_now();
        let before = self.idle_clock.hold(held, now);
        self.after_idle(before);
    }

    /// Makes sure a timer is set for the next time something is due.
    pub(crate) fn ensure_idle_timer(&mut self) {
        if self.idle_timer.is_some() {
            return;
        }
        let now = self.idle_now();
        let Some(at) = self.idle_clock.next_deadline() else {
            return;
        };
        let timer = Timer::from_duration(at.saturating_sub(now).max(Duration::from_millis(1)));
        self.idle_timer = self
            .loop_handle
            .insert_source(timer, |_, _, state| match state.idle_tick() {
                Some(wait) => TimeoutAction::ToDuration(wait),
                None => {
                    state.idle_timer = None;
                    TimeoutAction::Drop
                }
            })
            .ok();
    }

    /// The timer fired: does what is due and says how long to wait for
    /// the next, or `None` when there is no next.
    fn idle_tick(&mut self) -> Option<Duration> {
        let now = self.idle_now();
        for step in self.idle_clock.due(now) {
            self.idle_step(step);
        }
        let at = self.idle_clock.next_deadline()?;
        Some(at.saturating_sub(now).max(Duration::from_millis(1)))
    }

    fn idle_step(&mut self, step: Step) {
        match step {
            Step::Dim => {
                crate::say!("idle: dimming the screen");
                self.spawn_plain(DIM);
            }
            Step::Off => {
                crate::say!("idle: switching the screen off");
                let outputs: Vec<_> = self.space.outputs().cloned().collect();
                for output in outputs {
                    self.set_output_power(&output, false);
                }
            }
            Step::Lock => {
                if !self.is_locked() && !self.login_screen {
                    crate::say!("idle: locking the session");
                    self.spawn_locker();
                }
            }
        }
    }

    /// Takes up the times in `config`: at start and when the settings are
    /// read again.
    pub(crate) fn set_idle_plan(&mut self, plan: Plan) {
        if self.idle_clock.plan() == plan {
            return;
        }
        // A screen dimmed for a time that is no more is brought back, and
        // the timer set for a time that may now be earlier is set again.
        let before = self.idle_clock.stage();
        self.idle_clock.set_plan(plan);
        if let Some(token) = self.idle_timer.take() {
            self.loop_handle.remove(token);
        }
        if before == Stage::Dimmed && plan.dim.is_none() {
            self.idle_input();
        } else {
            self.ensure_idle_timer();
        }
    }

    /// Closing the lid does nothing, if Settings says so: logind is asked
    /// not to suspend for it, for as long as the compositor wants. The
    /// request is a program, `systemd-inhibit`, kept running over a pipe;
    /// it ends when the pipe closes, which is also what happens if the
    /// compositor dies, so the lid cannot stay ignored by mistake.
    pub(crate) fn set_lid_does_nothing(&mut self, nothing: bool) {
        if nothing == self.lid_hold.is_some() || self.login_screen {
            return;
        }
        if !nothing {
            // Its input closes, and it ends.
            self.lid_hold = None;
            return;
        }
        let spawned = Command::new("systemd-inhibit")
            .args([
                "--what=handle-lid-switch",
                "--who=Sengi OS",
                "--why=Closing the lid does nothing, as set in Settings",
                "--mode=block",
                "cat",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                self.lid_hold = child.stdin.take();
                self.children.push(child);
                crate::say!("lid: closing it does nothing");
            }
            Err(err) => crate::say!("lid: cannot hold the lid switch: {err}"),
        }
    }

    /// One line for the SIGUSR1 report.
    pub fn power_status(&self) -> String {
        let lid = if self.lid_hold.is_some() {
            "closing it does nothing"
        } else {
            "left to the system"
        };
        format!("{}; lid: {lid}", self.idle_clock.describe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn s(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    /// The defaults: dim after 2 minutes, off after 5, no lock.
    fn defaults() -> Plan {
        Plan::from_seconds(120, 300, 0)
    }

    #[test]
    fn zero_is_never_and_dimming_must_come_before_the_screen_goes_off() {
        let plan = Plan::from_seconds(0, 0, 0);
        assert_eq!((plan.dim, plan.off, plan.lock), (None, None, None));
        let plan = Plan::from_seconds(120, 300, 600);
        assert_eq!(
            (plan.dim, plan.off, plan.lock),
            (Some(s(120)), Some(s(300)), Some(s(600)))
        );
        // Dimming at or after the time it goes off is pointless.
        assert_eq!(Plan::from_seconds(300, 300, 0).dim, None);
        assert_eq!(Plan::from_seconds(600, 300, 0).dim, None);
        // With no time for off, dimming stands alone.
        assert_eq!(Plan::from_seconds(60, 0, 0).dim, Some(s(60)));
        assert_eq!(Plan::from_seconds(-5, 300, -1).dim, None);
    }

    #[test]
    fn the_steps_come_in_order_at_their_times() {
        let mut clock = IdleClock::new(Plan::from_seconds(120, 300, 600), s(1000));
        assert_eq!(clock.next_deadline(), Some(s(1120)));
        assert!(clock.due(s(1119)).is_empty());
        assert_eq!(clock.due(s(1120)), [Step::Dim]);
        assert_eq!(clock.stage(), Stage::Dimmed);
        assert_eq!(clock.next_deadline(), Some(s(1300)));
        assert_eq!(clock.due(s(1300)), [Step::Off]);
        assert_eq!(clock.stage(), Stage::Off);
        assert_eq!(clock.next_deadline(), Some(s(1600)));
        assert_eq!(clock.due(s(1600)), [Step::Lock]);
        // Nothing is left until there is input.
        assert_eq!(clock.next_deadline(), None);
        assert!(clock.due(s(9999)).is_empty());
    }

    #[test]
    fn each_step_happens_once_however_often_it_is_asked() {
        let mut clock = IdleClock::new(defaults(), s(0));
        assert_eq!(clock.due(s(130)), [Step::Dim]);
        assert!(clock.due(s(131)).is_empty());
        assert!(clock.due(s(200)).is_empty());
    }

    #[test]
    fn input_starts_the_wait_again_and_says_how_far_it_had_gone() {
        let mut clock = IdleClock::new(defaults(), s(0));
        assert_eq!(clock.activity(s(10)), Stage::Awake);
        assert_eq!(clock.next_deadline(), Some(s(130)));
        clock.due(s(130));
        assert_eq!(clock.activity(s(140)), Stage::Dimmed);
        assert_eq!(clock.stage(), Stage::Awake);
        assert_eq!(clock.next_deadline(), Some(s(260)));
        clock.due(s(260));
        clock.due(s(440));
        assert_eq!(clock.stage(), Stage::Off);
        assert_eq!(clock.activity(s(500)), Stage::Off);
        // The whole way again.
        assert_eq!(clock.due(s(620)), [Step::Dim]);
        assert_eq!(clock.due(s(800)), [Step::Off]);
    }

    #[test]
    fn a_late_timer_does_not_dim_a_screen_that_is_to_be_off() {
        // As after the computer slept: the first look is past both times.
        let mut clock = IdleClock::new(Plan::from_seconds(120, 300, 0), s(0));
        assert_eq!(clock.due(s(4000)), [Step::Off]);
        assert_eq!(clock.stage(), Stage::Off);
        // And with a lock time, it comes in order after.
        let mut clock = IdleClock::new(Plan::from_seconds(120, 300, 200), s(0));
        assert_eq!(clock.due(s(4000)), [Step::Lock, Step::Off]);
    }

    #[test]
    fn a_lock_that_comes_before_the_screen_goes_off_is_its_own_step() {
        let mut clock = IdleClock::new(Plan::from_seconds(0, 300, 60), s(0));
        assert_eq!(clock.next_deadline(), Some(s(60)));
        assert_eq!(clock.due(s(60)), [Step::Lock]);
        assert_eq!(clock.stage(), Stage::Awake);
        assert_eq!(clock.next_deadline(), Some(s(300)));
        assert_eq!(clock.due(s(300)), [Step::Off]);
        // After input the lock comes again.
        clock.activity(s(400));
        assert_eq!(clock.due(s(460)), [Step::Lock]);
    }

    #[test]
    fn nothing_set_means_nothing_is_ever_due() {
        let mut clock = IdleClock::new(Plan::from_seconds(0, 0, 0), s(0));
        assert_eq!(clock.next_deadline(), None);
        assert!(clock.due(s(100_000)).is_empty());
    }

    #[test]
    fn something_that_holds_idleness_off_stops_the_clock_and_restarts_it() {
        let mut clock = IdleClock::new(defaults(), s(0));
        assert_eq!(clock.due(s(130)), [Step::Dim]);
        // A video starts: the screen is brought back and nothing is due.
        assert_eq!(clock.hold(true, s(140)), Stage::Dimmed);
        assert_eq!(clock.stage(), Stage::Awake);
        assert_eq!(clock.next_deadline(), None);
        assert!(clock.due(s(10_000)).is_empty());
        // It stops: the wait begins from then, not from before.
        assert_eq!(clock.hold(false, s(5000)), Stage::Awake);
        assert_eq!(clock.next_deadline(), Some(s(5120)));
        assert!(clock.due(s(5100)).is_empty());
        assert_eq!(clock.due(s(5120)), [Step::Dim]);
        // Saying the same thing twice changes nothing.
        assert_eq!(clock.hold(false, s(5200)), Stage::Awake);
        assert_eq!(clock.stage(), Stage::Dimmed);
    }

    #[test]
    fn new_times_take_effect_from_the_last_input() {
        let mut clock = IdleClock::new(defaults(), s(0));
        clock.activity(s(100));
        clock.set_plan(Plan::from_seconds(30, 60, 0));
        assert_eq!(clock.next_deadline(), Some(s(130)));
        assert_eq!(clock.due(s(160)), [Step::Off]);
        // A plan with no off: a screen already off stays so until input.
        clock.set_plan(Plan::from_seconds(30, 0, 0));
        assert_eq!(clock.next_deadline(), None);
        assert_eq!(clock.stage(), Stage::Off);
    }

    #[test]
    fn the_report_says_where_things_stand() {
        let mut clock = IdleClock::new(defaults(), s(0));
        assert_eq!(
            clock.describe(),
            "idle: awake, dim 120s, off 300s, lock never"
        );
        clock.due(s(130));
        assert!(clock.describe().starts_with("idle: dimmed"));
    }
}
