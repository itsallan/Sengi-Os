//! A screen whose front window is maximised belongs to that window: the
//! window is as large as the screen, and the bar and the dock slide out of
//! its way. Resting the pointer against the top or the bottom edge brings
//! one back, over the window, until the pointer leaves it.
//!
//! The bar and the dock are not told. Their layer surfaces keep their size
//! and the room they reserve; they are only drawn, and pointed at, higher
//! or lower than where the layout has them. While the bar shows, the title
//! bar of the maximised window moves down below it, so its buttons stay in
//! reach.
//!
//! [`State::update_autohide`] runs at the end of every turn of the loop and
//! settles what should show. Time passes through a single timer, armed only
//! while something is waited for or moving, so an idle screen costs nothing.

use std::time::{Duration, Instant};

use smithay::{
    desktop::{layer_map_for_output, LayerSurface, Window, WindowSurfaceType},
    output::Output,
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            RegistrationToken,
        },
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point},
    wayland::compositor::{with_states, RectangleKind, SurfaceAttributes},
};

use crate::{frame, layout::Rect, state::State, window};

/// How close to an edge, in logical pixels, the pointer rests to ask for
/// what is hidden there. The pointer stops at the edge of the screen, so a
/// mouse or a finger on a touchpad pushed that way ends up inside this.
const EDGE: f64 = 2.0;

/// How often a sliding panel is drawn.
const FRAME: Duration = Duration::from_millis(16);

/// The edges of a screen something hides at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
}

pub const SIDES: [Side; 2] = [Side::Top, Side::Bottom];

impl Side {
    /// The edge the layer surfaces named `namespace` hide at, if they are
    /// the bar or the dock.
    pub fn of(namespace: &str) -> Option<Side> {
        match namespace {
            "sengi-bar" => Some(Side::Top),
            "sengi-dock" => Some(Side::Bottom),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Side::Top => 0,
            Side::Bottom => 1,
        }
    }
}

/// A cubic Bézier curve from (0, 0) to (1, 1), as the tokens write easing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Easing {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

impl Easing {
    const LINEAR: Easing = Easing {
        x1: 0.0,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };

    /// Reads `cubic-bezier(x1, y1, x2, y2)`.
    pub fn parse(text: &str) -> Option<Easing> {
        let inside = text
            .trim()
            .strip_prefix("cubic-bezier(")?
            .strip_suffix(')')?;
        let mut parts = inside.split(',').map(|part| part.trim().parse::<f64>());
        let mut next = || parts.next()?.ok().filter(|value| value.is_finite());
        let easing = Easing {
            x1: next()?.clamp(0.0, 1.0),
            y1: next()?,
            x2: next()?.clamp(0.0, 1.0),
            y2: next()?,
        };
        parts.next().is_none().then_some(easing)
    }

    /// How far along the movement is when `t` of its time has passed.
    pub fn at(&self, t: f64) -> f64 {
        let curve = |a: f64, b: f64, s: f64| {
            let u = 1.0 - s;
            3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
        };
        let t = t.clamp(0.0, 1.0);
        // x only rises along the curve, so halving finds where it is `t`.
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..32 {
            let middle = (low + high) / 2.0;
            if curve(self.x1, self.x2, middle) < t {
                low = middle;
            } else {
                high = middle;
            }
        }
        curve(self.y1, self.y2, (low + high) / 2.0)
    }
}

/// How long things take, from the tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    /// The pointer rests at an edge this long before the panel comes.
    pub dwell: Duration,
    /// It is away this long before the panel goes.
    pub grace: Duration,
    /// A panel that was clicked stays this long at least, for what the
    /// click asked for to appear.
    pub hold: Duration,
    /// A panel takes this long to slide in or out.
    pub slide: Duration,
    pub easing: Easing,
}

impl Timing {
    pub fn from_tokens(tokens: &sengi_tokens::Tokens, reduce_motion: bool) -> Timing {
        let millis = |name: &str| {
            let value = tokens.px(name);
            Duration::from_millis(u64::from(value.expect("the tokens define the timings")))
        };
        Timing {
            dwell: millis("timing.edge-reveal-delay"),
            grace: millis("timing.edge-hide-delay"),
            hold: millis("timing.edge-click-hold"),
            slide: if reduce_motion {
                Duration::ZERO
            } else {
                millis("motion.base")
            },
            easing: tokens
                .get("motion.easing")
                .and_then(Easing::parse)
                .unwrap_or(Easing::LINEAR),
        }
    }
}

/// What happens at one edge of one screen.
#[derive(Debug)]
struct Edge {
    /// Brought back over a maximised window with the pointer.
    revealed: bool,
    /// Where the panel is going: on to the screen, or off it.
    shown: bool,
    /// The slide under way: from how far shown, and since when.
    sliding: Option<(f64, Instant)>,
    /// How much of the panel is on the screen as it is drawn and pointed
    /// at: 0 is none, 1 all of it. It moves a frame at a time.
    at: f64,
    /// When `at` last moved.
    stepped: Instant,
    /// When to bring it back, or to send it away.
    due: Option<Instant>,
    /// A click on it holds it until then.
    held_until: Option<Instant>,
    /// The pointer was already at the edge when the window took the screen.
    /// It has to leave before resting there counts.
    blocked: bool,
}

impl Default for Edge {
    fn default() -> Edge {
        Edge {
            revealed: false,
            shown: true,
            sliding: None,
            at: 1.0,
            stepped: Instant::now(),
            due: None,
            held_until: None,
            blocked: false,
        }
    }
}

impl Edge {
    /// Turns the panel round, or sends it off, from where it is.
    fn head_for(&mut self, shown: bool, now: Instant) {
        if self.shown != shown {
            self.shown = shown;
            self.sliding = Some((self.at, now));
            self.stepped = now;
        }
    }

    /// Moves the panel on, if a frame has passed since it last moved or
    /// it has arrived. Whether it moved. With `at_once` it arrives now.
    fn step(&mut self, timing: &Timing, now: Instant, at_once: bool) -> bool {
        if self
            .sliding
            .is_some_and(|(_, since)| at_once || now >= since + timing.slide)
        {
            self.sliding = None;
        }
        if self.sliding.is_some() && now < self.stepped + FRAME {
            return false;
        }
        let there = self.progress(timing, now);
        self.stepped = now;
        std::mem::replace(&mut self.at, there) != there
    }

    /// How much of the panel should be on the screen at `now`.
    fn progress(&self, timing: &Timing, now: Instant) -> f64 {
        let target = f64::from(u8::from(self.shown));
        let Some((from, since)) = self.sliding else {
            return target;
        };
        let passed = now.saturating_duration_since(since);
        if passed >= timing.slide {
            return target;
        }
        let t = passed.as_secs_f64() / timing.slide.as_secs_f64();
        from + (target - from) * timing.easing.at(t)
    }
}

/// Where the panels of one output are, see `State::panel_shifts`.
#[derive(Debug, Clone, Copy)]
pub struct Shifts([Option<i32>; 2]);

impl Shifts {
    /// How far down the layer surface named `namespace` is, or nothing if
    /// it is out of sight. Anything but the bar and the dock stays put.
    pub fn of(&self, namespace: &str) -> Option<i32> {
        Side::of(namespace).map_or(Some(0), |side| self.0[side.index()])
    }
}

#[derive(Debug)]
struct Screen {
    output: Output,
    /// The maximised window that has the screen to itself.
    cover: Option<Window>,
    edges: [Edge; 2],
}

#[derive(Debug, Default)]
pub struct AutoHide {
    screens: Vec<Screen>,
    /// A button went down on a panel and has not come up.
    pressed: Option<(Output, Side)>,
    /// A program is dragging something about, which may be on its way to
    /// the dock.
    pub carrying: bool,
    /// The one timer, and when it fires.
    timer: Option<(Instant, RegistrationToken)>,
}

impl State {
    fn timing(&self) -> Timing {
        Timing::from_tokens(&self.tokens, self.config.appearance.reduce_motion)
    }

    /// The window that has `output` to itself: the front one there is
    /// maximised. Dialogs of a window further back are not looked at, so a
    /// maximised window keeps the screen under its own questions.
    pub fn covering_window(&self, output: &Output) -> Option<Window> {
        let front_to_back: Vec<&Window> = self
            .space
            .elements()
            .rev()
            .filter(|window| !window::is_override(window))
            .filter(|window| self.output_of_window(window).as_ref() == Some(output))
            .collect();
        for (at, window) in front_to_back.iter().enumerate() {
            let behind = &front_to_back[at + 1..];
            if self
                .parent_of(window)
                .is_some_and(|parent| behind.contains(&&parent))
            {
                continue;
            }
            let data = window::data(window);
            let covers = data.restore.get().is_some() && data.fullscreen.get().is_none();
            return covers.then(|| (*window).clone());
        }
        None
    }

    /// The window `window` is a dialog of, if it is on the desktop.
    fn parent_of(&self, window: &Window) -> Option<Window> {
        let parent = if let Some(x11) = window::x11(window) {
            let id = x11.is_transient_for()?;
            self.space
                .elements()
                .find(|other| window::x11(other).is_some_and(|other| other.window_id() == id))
        } else {
            let surface = window.toplevel()?.parent()?;
            self.space
                .elements()
                .find(|other| other.toplevel().is_some_and(|t| *t.wl_surface() == surface))
        };
        parent.filter(|parent| *parent != window).cloned()
    }

    /// The bar or the dock surfaces of `output`.
    fn panels(&self, output: &Output, side: Side) -> Vec<LayerSurface> {
        layer_map_for_output(output)
            .layers()
            .filter(|layer| Side::of(layer.namespace()) == Some(side))
            .cloned()
            .collect()
    }

    /// How far from its edge of `output` a panel reaches: the bar's lower
    /// side, or from the dock's upper side, where it takes the pointer, to
    /// the bottom of the screen. Zero without a panel.
    fn reach(&self, output: &Output, side: Side) -> i32 {
        let Some(screen) = self.space.output_geometry(output) else {
            return 0;
        };
        let map = layer_map_for_output(output);
        map.layers()
            .filter(|layer| Side::of(layer.namespace()) == Some(side))
            .filter_map(|layer| {
                let geometry = map.layer_geometry(layer)?;
                Some(match side {
                    Side::Top => geometry.loc.y + geometry.size.h,
                    Side::Bottom => screen.size.h - geometry.loc.y - input_top(layer.wl_surface()),
                })
            })
            .max()
            .unwrap_or(0)
            .max(0)
    }

    /// Whether something that belongs to the panel at `side` of `output`
    /// is open or has the keyboard, so that it must not go.
    fn panel_in_use(&self, output: &Output, side: Side) -> bool {
        let ours = |surface: &WlSurface| {
            self.panels(output, side)
                .iter()
                .any(|layer| layer.wl_surface() == surface)
        };
        // Its own menu, which holds the keyboard.
        self.exclusive_layer().is_some_and(|surface| ours(&surface))
            || self.layer_focus.as_ref().is_some_and(ours)
            || self.menu.as_ref().is_some_and(|(root, _)| ours(root))
    }

    /// As `panel_in_use`, or something opened from it covers the screen:
    /// quick settings, the session menu, the calendar, the notification
    /// centre, a tray menu, or one of the compositor's own menus.
    fn panel_wanted(&self, output: &Output, side: Side) -> bool {
        let pressed = self
            .autohide
            .pressed
            .as_ref()
            .is_some_and(|(on, at)| on == output && *at == side);
        pressed
            || self.panel_in_use(output, side)
            || self.overlay_open()
            || self
                .exclusive_layer()
                .is_some_and(|surface| self.output_for_layer(&surface).as_ref() == Some(output))
    }

    /// Where the pointer is on `output`, if it is on it.
    fn pointer_on(&self, output: &Output) -> Option<Point<f64, Logical>> {
        if !self.pointer_seen {
            return None;
        }
        let screen = self.space.output_geometry(output)?;
        let at = self.seat.get_pointer()?.current_location();
        screen
            .to_f64()
            .contains(at)
            .then(|| at - screen.loc.to_f64())
    }

    /// Whether the pointer is in the rows that ask for the panel.
    fn at_edge(&self, output: &Output, side: Side) -> bool {
        let (Some(at), Some(screen)) =
            (self.pointer_on(output), self.space.output_geometry(output))
        else {
            return false;
        };
        match side {
            Side::Top => at.y < EDGE,
            Side::Bottom => at.y >= f64::from(screen.size.h) - EDGE,
        }
    }

    /// Whether the pointer is still where a panel that came back is kept:
    /// at the edge, on the panel, or between it and the edge. Under the bar
    /// the title bar that moved down for it counts too.
    fn over_panel(&self, output: &Output, side: Side, cover: Option<&Window>) -> bool {
        if self.at_edge(output, side) {
            return true;
        }
        let Some(at) = self.pointer_on(output) else {
            return false;
        };
        match side {
            Side::Top => {
                let title = cover
                    .filter(|window| window::data(window).decorated.get())
                    .map_or(0, |_| self.theme.metrics.titlebar);
                at.y < f64::from(self.reach(output, side) + title)
            }
            Side::Bottom => {
                let map = layer_map_for_output(output);
                let found = map
                    .layers()
                    .filter(|layer| Side::of(layer.namespace()) == Some(side))
                    .filter_map(|layer| Some((layer, map.layer_geometry(layer)?)))
                    .any(|(layer, geometry)| {
                        // Under the surface counts as on its last row.
                        let mut local = at - geometry.loc.to_f64();
                        local.y = local.y.min(f64::from(geometry.size.h) - 1.0);
                        layer.surface_under(local, WindowSurfaceType::ALL).is_some()
                    });
                found
            }
        }
    }

    /// Settles what shows at the edges of every screen. Runs at the end of
    /// each turn of the loop.
    pub fn update_autohide(&mut self) {
        let now = Instant::now();
        let timing = self.timing();
        let locked = self.is_locked();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if !pointer.is_grabbed() {
            self.autohide.carrying = false;
        }
        if locked {
            self.autohide.pressed = None;
        }
        // A held button belongs to what it went down on, unless something
        // is being carried to the dock.
        let busy = pointer.is_grabbed() && !self.autohide.carrying;

        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        let mut screens = std::mem::take(&mut self.autohide.screens);
        screens.retain(|screen| outputs.contains(&screen.output));
        for output in &outputs {
            if !screens.iter().any(|screen| screen.output == *output) {
                screens.push(Screen {
                    output: output.clone(),
                    cover: None,
                    edges: Default::default(),
                });
            }
        }

        let mut next: Option<Instant> = None;
        let mut moved = false;
        for screen in &mut screens {
            let cover = self.covering_window(&screen.output);
            let taken = screen.cover.is_none() && cover.is_some();
            // The title bar that moves with the bar is another window's now.
            if screen.cover != cover {
                moved = true;
            }
            screen.cover = cover;
            for side in SIDES {
                let output = &screen.output;
                let edge = &mut screen.edges[side.index()];
                let cover = screen.cover.as_ref();
                if taken {
                    edge.revealed = false;
                    edge.blocked = self.at_edge(output, side);
                }
                if cover.is_none() || locked || self.panels(output, side).is_empty() {
                    edge.revealed = false;
                    edge.due = None;
                    edge.held_until = None;
                } else if self.panel_in_use(output, side) {
                    // A menu of a panel cannot be left without it.
                    edge.revealed = true;
                    edge.due = None;
                } else if edge.revealed {
                    if self.panel_wanted(output, side) || self.over_panel(output, side, cover) {
                        edge.due = None;
                    } else {
                        let due = *edge.due.get_or_insert_with(|| {
                            let soon = now + timing.grace;
                            edge.held_until.map_or(soon, |held| held.max(soon))
                        });
                        if now >= due {
                            edge.revealed = false;
                            edge.due = None;
                        }
                    }
                } else if !self.at_edge(output, side) {
                    edge.blocked = false;
                    edge.due = None;
                } else if edge.blocked || busy || self.overlay_open() {
                    edge.due = None;
                } else {
                    let due = *edge.due.get_or_insert(now + timing.dwell);
                    if now >= due {
                        edge.revealed = true;
                        edge.due = None;
                    }
                }

                edge.head_for(cover.is_none() || edge.revealed, now);
                // Nobody watches a slide behind the lock.
                moved |= edge.step(&timing, now, locked);
                let wake = edge.sliding.map(|_| edge.stepped + FRAME);
                for when in [edge.due, wake].into_iter().flatten() {
                    next = Some(next.map_or(when, |soonest| soonest.min(when)));
                }
            }
        }
        self.autohide.screens = screens;

        if moved {
            self.pointer_stale = true;
            self.request_redraw();
        }
        self.arm_autohide(next);
    }

    /// Wakes the loop at `next`, which is all that is needed: it settles
    /// everything at the end of the turn.
    fn arm_autohide(&mut self, next: Option<Instant>) {
        if let Some((at, token)) = self.autohide.timer {
            // One that fires early enough will do; it is armed again then.
            if next.is_some_and(|next| at <= next && at > Instant::now()) {
                return;
            }
            self.loop_handle.remove(token);
            self.autohide.timer = None;
        }
        let Some(next) = next else {
            return;
        };
        let timer = Timer::from_deadline(next);
        match self.loop_handle.insert_source(timer, |_, _, state| {
            state.autohide.timer = None;
            TimeoutAction::Drop
        }) {
            Ok(token) => self.autohide.timer = Some((next, token)),
            Err(err) => crate::say!("cannot time the bar and the dock: {err}"),
        }
    }

    /// A button went down on `surface`, or one came up anywhere. A panel
    /// that was clicked stays while what the click asked for appears.
    pub fn autohide_pressed(&mut self, surface: Option<&WlSurface>) {
        let hit = surface.and_then(|surface| {
            let output = self.output_for_layer(surface)?;
            let side = SIDES.into_iter().find(|side| {
                self.panels(&output, *side)
                    .iter()
                    .any(|layer| layer.wl_surface() == surface)
            })?;
            Some((output, side))
        });
        self.autohide.pressed = hit;
    }

    pub fn autohide_released(&mut self) {
        let Some((output, side)) = self.autohide.pressed.take() else {
            return;
        };
        let until = Instant::now() + self.timing().hold;
        if let Some(screen) = self
            .autohide
            .screens
            .iter_mut()
            .find(|screen| screen.output == output)
        {
            screen.edges[side.index()].held_until = Some(until);
        }
    }

    fn edge(&self, output: &Output, side: Side) -> Option<&Edge> {
        self.autohide
            .screens
            .iter()
            .find(|screen| screen.output == *output)
            .map(|screen| &screen.edges[side.index()])
    }

    /// How far down the bar and the dock of `output` are drawn from where
    /// the layout has them, nothing for one that is out of sight. For the
    /// pointer one that is on its way out is gone already. Taken before
    /// the layer map is borrowed, since this borrows it.
    pub fn panel_shifts(&self, output: &Output, for_pointer: bool) -> Shifts {
        Shifts(SIDES.map(|side| {
            let (progress, leaving) = self
                .edge(output, side)
                .map_or((1.0, false), |edge| (edge.at, !edge.shown));
            if progress >= 1.0 && !(for_pointer && leaving) {
                return Some(0);
            }
            if progress <= 0.0 || (for_pointer && leaving) {
                return None;
            }
            let away = (f64::from(self.reach(output, side)) * (1.0 - progress)).round() as i32;
            Some(match side {
                Side::Top => -away,
                Side::Bottom => away,
            })
        }))
    }

    /// How far the title bar of `window` is below its place, to be clear of
    /// the bar that came back over it.
    pub fn titlebar_drop(&self, window: &Window) -> i32 {
        if !window::data(window).decorated.get() {
            return 0;
        }
        let Some(screen) = self
            .autohide
            .screens
            .iter()
            .find(|screen| screen.cover.as_ref() == Some(window))
        else {
            return 0;
        };
        let progress = screen.edges[Side::Top.index()].at;
        if progress <= 0.0 {
            return 0;
        }
        (f64::from(self.reach(&screen.output, Side::Top)) * progress).round() as i32
    }

    /// The frame of `window` as its title bar is drawn and clicked.
    pub fn titlebar_frame(&self, window: &Window) -> Option<Rect> {
        let frame = self.frame_rect(window)?;
        Some(Rect::new(
            frame.x,
            frame.y + self.titlebar_drop(window),
            frame.w,
            frame.h,
        ))
    }

    /// What a title bar that moved down has at `(x, y)`, if it moved and is
    /// there.
    pub fn dropped_titlebar_hit(&self, window: &Window, x: i32, y: i32) -> Option<frame::Hit> {
        if self.titlebar_drop(window) <= 0 {
            return None;
        }
        let frame = self.titlebar_frame(window)?;
        let titlebar = Rect::new(frame.x, frame.y, frame.w, self.theme.metrics.titlebar);
        if !titlebar.contains(x, y) {
            return None;
        }
        self.theme.metrics.hit(frame, x, y, false)
    }

    /// For the tests: one line for each screen, then whatever must never
    /// be so.
    #[cfg(feature = "test-hooks")]
    pub fn autohide_report(&self) -> (Vec<String>, Vec<String>) {
        let (mut lines, mut broken) = (Vec::new(), Vec::new());
        for screen in &self.autohide.screens {
            let name = screen.output.name();
            let cover = screen.cover.as_ref();
            let title = cover.map(|window| window::title(window).replace(' ', "_"));
            let word = |side: Side| {
                let edge = &screen.edges[side.index()];
                if self.panels(&screen.output, side).is_empty() {
                    "none"
                } else if edge.sliding.is_some() {
                    "moving"
                } else if edge.at >= 1.0 {
                    "shown"
                } else {
                    "hidden"
                }
            };
            lines.push(format!(
                "screen name={name} cover={} top={} bottom={} drop={}",
                title.as_deref().unwrap_or("-"),
                word(Side::Top),
                word(Side::Bottom),
                cover.map_or(0, |window| self.titlebar_drop(window)),
            ));
            if cover.cloned() != self.covering_window(&screen.output) {
                broken.push(format!("{name} is not settled on who covers it"));
            }
            for side in SIDES {
                let edge = &screen.edges[side.index()];
                if cover.is_none() && (edge.revealed || !edge.shown || edge.due.is_some()) {
                    broken.push(format!("{name} {side:?} waits or hides with no cover"));
                }
                if self.is_locked() && (edge.revealed || edge.due.is_some()) {
                    broken.push(format!(
                        "{name} {side:?} is revealed or waits behind the lock"
                    ));
                }
                if cover.is_some() && edge.shown != edge.revealed {
                    broken.push(format!("{name} {side:?} shows without being asked for"));
                }
                if (edge.due.is_some() || edge.sliding.is_some()) && self.autohide.timer.is_none() {
                    broken.push(format!("{name} {side:?} waits with no timer"));
                }
            }
        }
        if self.autohide.screens.len() != self.space.outputs().count() {
            broken.push("the screens and the outputs differ".into());
        }
        (lines, broken)
    }
}

/// The first row of `surface` that takes the pointer.
fn input_top(surface: &WlSurface) -> i32 {
    with_states(surface, |states| {
        let mut attributes = states.cached_state.get::<SurfaceAttributes>();
        attributes
            .current()
            .input_region
            .as_ref()
            .and_then(|region| {
                region
                    .rects
                    .iter()
                    .filter(|(kind, rect)| matches!(kind, RectangleKind::Add) && !rect.is_empty())
                    .map(|(_, rect)| rect.loc.y)
                    .min()
            })
            .map_or(0, |top| top.max(0))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_easing_of_the_tokens_is_read_and_goes_from_start_to_end() {
        let tokens = sengi_tokens::Tokens::builtin();
        let easing = Easing::parse(tokens.get("motion.easing").unwrap()).unwrap();
        assert!(easing.at(0.0) < 0.001);
        assert!(easing.at(1.0) > 0.999);
        let mut last = 0.0;
        for step in 1..=20 {
            let now = easing.at(f64::from(step) / 20.0);
            assert!(now >= last, "it goes back at step {step}");
            last = now;
        }
        // It starts fast and settles: well past half way at half time.
        assert!(easing.at(0.5) > 0.7);
    }

    #[test]
    fn a_straight_curve_is_linear() {
        for t in [0.1, 0.25, 0.5, 0.9] {
            assert!((Easing::LINEAR.at(t) - t).abs() < 0.001);
        }
    }

    #[test]
    fn only_a_whole_curve_is_read() {
        assert_eq!(Easing::parse("ease-out"), None);
        assert_eq!(Easing::parse("cubic-bezier(0, 0, 1)"), None);
        assert_eq!(Easing::parse("cubic-bezier(0, 0, 1, 1, 1)"), None);
        assert_eq!(Easing::parse("cubic-bezier(0, 0, x, 1)"), None);
    }

    #[test]
    fn the_timings_come_from_the_tokens_and_reduced_motion_slides_in_no_time() {
        let tokens = sengi_tokens::Tokens::builtin();
        let timing = Timing::from_tokens(&tokens, false);
        assert!(timing.dwell >= Duration::from_millis(50));
        assert!(timing.grace < timing.hold);
        assert_eq!(timing.slide, Duration::from_millis(200));
        assert_eq!(Timing::from_tokens(&tokens, true).slide, Duration::ZERO);
    }

    #[test]
    fn a_slide_that_turns_back_starts_where_it_was() {
        let timing = Timing::from_tokens(&sengi_tokens::Tokens::builtin(), false);
        let start = Instant::now();
        let mut edge = Edge {
            shown: false,
            sliding: Some((1.0, start)),
            ..Edge::default()
        };
        let half = start + timing.slide / 2;
        let there = edge.progress(&timing, half);
        assert!(there > 0.0 && there < 1.0);
        edge.shown = true;
        edge.sliding = Some((there, half));
        assert!((edge.progress(&timing, half) - there).abs() < 0.001);
        assert_eq!(edge.progress(&timing, half + timing.slide), 1.0);
    }

    #[test]
    fn the_bar_and_the_dock_are_known_by_name() {
        assert_eq!(Side::of("sengi-bar"), Some(Side::Top));
        assert_eq!(Side::of("sengi-dock"), Some(Side::Bottom));
        assert_eq!(Side::of("sengi-dock-backdrop"), None);
        assert_eq!(Side::of("sengi-quick"), None);
    }
}
