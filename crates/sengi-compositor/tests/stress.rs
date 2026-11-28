//! Many things done to many windows in a random order, against the running
//! compositor: windows come and go, are minimised, restored, closed and
//! dragged, with menus open, bars appearing, screens plugged and pulled,
//! the session locking and programs dying in the middle of it.
//!
//! After every step the compositor must answer, in good time, and what it
//! remembers must still agree with itself (`inspect` on the control
//! socket). Every so often everything is let go of, and the window in front
//! must take a click and a key. At the end nothing may be left behind.
//!
//! A run is made from a seed. The ordinary run is short, with fixed seeds.
//! For a long one:
//!
//! ```text
//! SENGI_STRESS_SEED=7 SENGI_STRESS_STEPS=20000 cargo test --test stress
//! ```
//!
//! `SENGI_STRESS_ROUNDS` splits the steps into rounds with all programs
//! closed in between; from five rounds on the compositor's memory and open
//! files are watched over them. `SENGI_STRESS_KEEP_GOING=1` notes what is
//! wrong and goes on, to see all of it at once.

mod common;
mod harness;

use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    thread::sleep,
    time::{Duration, Instant},
};

use common::{heads::Op, Client, Seen};
use harness::{Compositor, SOON};
use wayland_client::{
    event_created_child,
    protocol::{
        wl_data_device::WlDataDevice, wl_data_source::WlDataSource, wl_pointer::WlPointer,
        wl_seat::WlSeat, wl_surface::WlSurface,
    },
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::{
    wp::{
        idle_inhibit::zv1::client::zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
        pointer_constraints::zv1::client::{
            zwp_confined_pointer_v1::ZwpConfinedPointerV1,
            zwp_locked_pointer_v1::ZwpLockedPointerV1,
        },
    },
    xdg::shell::client::{
        xdg_popup::{self, XdgPopup},
        xdg_positioner::{self, XdgPositioner},
        xdg_surface::XdgSurface,
        xdg_toplevel::{self, ResizeEdge, XdgToplevel},
        xdg_wm_base::XdgWmBase,
    },
};
use wayland_protocols_wlr::{
    foreign_toplevel::v1::client::{
        zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
        zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
    },
    layer_shell::v1::client::{
        zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
        zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
    },
};
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
        EventMask, PropMode, WindowClass,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT,
};

/// How long one step may take before the compositor counts as stuck. Long,
/// so that a loaded machine is not taken for a frozen one.
const STEP_LIMIT: Duration = Duration::from_secs(40);

/// The seeds of the ordinary run, and how many steps each takes.
const SEEDS: [u64; 4] = [1, 2, 3, 4];
const STEPS: u64 = 350;

/// How often everything is let go of and the desktop is tried.
const SETTLE_EVERY: u64 = 25;

const ACTORS: usize = 5;
const WINDOWS_EACH: usize = 3;
/// The evdev codes of the buttons.
const BUTTONS: [&str; 2] = ["left", "right"];
const MODIFIERS: [&str; 3] = ["super", "alt", "shift"];
/// Keys and chords that start no program outside the compositor.
const KEYS: &[&str] = &[
    "esc",
    "enter",
    "tab",
    "up",
    "down",
    "left",
    "right",
    "delete",
    "a",
    "alt+tab",
    "alt+shift+tab",
    "alt+f4",
    "super+q",
    "super+a",
    "super+up",
    "super+down",
    "super+left",
    "super+right",
    "super+shift+left",
    "super+shift+right",
    "super+shift+up",
    "super+shift+down",
    "super+v",
    "super+/",
];

struct Rng(u64);

impl Rng {
    /// SplitMix64.
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn between(&mut self, low: i32, high: i32) -> i32 {
        low + self.below((high - low).max(1) as usize) as i32
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        &from[self.below(from.len())]
    }
}

/// What was done last, and since when the step in hand has been going.
struct Trail {
    seed: u64,
    step: u64,
    done: VecDeque<String>,
    since: Instant,
    over: bool,
}

impl Trail {
    fn tell(&self, why: &str) {
        // Straight to the descriptor: the harness would hold back what
        // `eprintln` writes if the process is ended from here.
        let mut out = std::io::stderr();
        let _ = writeln!(
            out,
            "\nstress: {why}\nstress: seed {}, step {}; run again with SENGI_STRESS_SEED={}",
            self.seed, self.step, self.seed
        );
        for line in &self.done {
            let _ = writeln!(out, "stress:   {line}");
        }
    }
}

type Shared = Arc<Mutex<Trail>>;

/// Ends the whole test when a step does not finish: a compositor that is
/// stuck leaves the test's own calls waiting for good.
fn watch(trail: Shared, compositor: u32) {
    std::thread::spawn(move || loop {
        sleep(Duration::from_millis(500));
        let held = trail.lock().unwrap_or_else(|held| held.into_inner());
        if held.over {
            return;
        }
        if held.since.elapsed() > STEP_LIMIT {
            let state = std::fs::read_to_string(format!("/proc/{compositor}/stat"))
                .ok()
                .and_then(|stat| stat.rsplit_once(") ")?.1.chars().next());
            held.tell(&format!(
                "STUCK: no end to this step after {STEP_LIMIT:?} (compositor state {state:?})"
            ));
            // SAFETY: kill takes two numbers.
            unsafe { libc::kill(compositor as libc::pid_t, libc::SIGKILL) };
            std::process::exit(101);
        }
    });
}

/// Says where the run was if the test panics.
struct Telling(Shared);

impl Drop for Telling {
    fn drop(&mut self) {
        let mut held = self.0.lock().unwrap_or_else(|held| held.into_inner());
        held.over = true;
        if std::thread::panicking() {
            held.tell("failed");
        }
    }
}

type Flag = Arc<AtomicBool>;

struct Popup {
    popup: XdgPopup,
    shell: XdgSurface,
    surface: WlSurface,
    dismissed: Flag,
}

impl Popup {
    fn destroy(self) {
        self.popup.destroy();
        self.shell.destroy();
        self.surface.destroy();
    }
}

/// What a window was asked by the compositor.
#[derive(Default)]
struct Asked {
    close: AtomicBool,
    width: AtomicU32,
    height: AtomicU32,
}

struct Win {
    title: String,
    surface: WlSurface,
    shell: XdgSurface,
    top: XdgToplevel,
    asked: Arc<Asked>,
    popups: Vec<Popup>,
    shown: bool,
    colour: u32,
    held: Option<ZwpLockedPointerV1>,
    confined: Option<ZwpConfinedPointerV1>,
    inhibitor: Option<ZwpIdleInhibitorV1>,
}

#[derive(Default)]
struct LayerSeen {
    configure: Mutex<Option<(u32, u32, u32)>>,
    answered: AtomicBool,
    closed: AtomicBool,
}

struct Shell {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    seen: Arc<LayerSeen>,
}

struct Actor {
    id: u32,
    client: Client,
    pointer: WlPointer,
    seat: WlSeat,
    wm: XdgWmBase,
    windows: Vec<Win>,
    shells: Vec<Shell>,
    drags: Vec<(WlDataDevice, WlDataSource, WlSurface)>,
}

type Handles = Arc<Mutex<Vec<ZwlrForeignToplevelHandleV1>>>;

fn is_closed(handle: &ZwlrForeignToplevelHandleV1) -> bool {
    handle
        .data::<Flag>()
        .is_some_and(|closed| closed.load(Ordering::Relaxed))
}

struct Dock {
    client: Client,
    seat: WlSeat,
    handles: Handles,
}

struct Locker {
    client: Client,
    lock: wayland_protocols::ext::session_lock::v1::client::ext_session_lock_v1::ExtSessionLockV1,
}

/// Left, top, width and height.
type Rect = (i32, i32, i32, i32);

#[derive(Clone, Debug, Default)]
struct Shown {
    title: String,
    mapped: bool,
    client: Option<(i32, i32, i32, i32)>,
    frame: Option<(i32, i32, i32, i32)>,
    close: Option<(i32, i32)>,
    minimise: Option<(i32, i32)>,
    maximise: Option<(i32, i32)>,
}

/// An answer to `inspect`.
#[derive(Default)]
struct View {
    facts: BTreeMap<String, String>,
    outputs: Vec<(String, (i32, i32, i32, i32))>,
    windows: Vec<Shown>,
    broken: Vec<String>,
}

impl View {
    fn number(&self, name: &str) -> i64 {
        self.facts
            .get(name)
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("no number `{name}` in the report: {:?}", self.facts))
    }

    fn is(&self, name: &str, value: &str) -> bool {
        self.facts.get(name).is_some_and(|held| held == value)
    }
}

/// Whether a line speaks of a window called `x` and a number.
fn names_an_x_window(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        word.strip_prefix('x')
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    })
}

fn numbers(text: &str) -> Vec<i32> {
    text.split(',').filter_map(|n| n.parse().ok()).collect()
}

fn four(text: &str) -> Option<(i32, i32, i32, i32)> {
    match numbers(text)[..] {
        [a, b, c, d] => Some((a, b, c, d)),
        _ => None,
    }
}

fn two(text: &str) -> Option<(i32, i32)> {
    match numbers(text)[..] {
        [a, b] => Some((a, b)),
        _ => None,
    }
}

fn parse(report: &str) -> View {
    let mut view = View::default();
    for line in report.lines() {
        let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
        let pairs = || rest.split(' ').filter_map(|pair| pair.split_once('='));
        match kind {
            "state" | "count" => view
                .facts
                .extend(pairs().map(|(name, value)| (name.to_string(), value.to_string()))),
            "output" => {
                let facts: BTreeMap<_, _> = pairs().collect();
                if let Some(at) = facts.get("at").and_then(|at| four(at)) {
                    view.outputs.push((facts["name"].to_string(), at));
                }
            }
            "window" => {
                let mut shown = Shown::default();
                for (name, value) in pairs() {
                    match name {
                        "title" => shown.title = value.to_string(),
                        "place" => shown.mapped = value == "mapped",
                        "client" => shown.client = four(value),
                        "frame" => shown.frame = four(value),
                        "close" => shown.close = two(value),
                        "minimise" => shown.minimise = two(value),
                        "maximise" => shown.maximise = two(value),
                        _ => {}
                    }
                }
                view.windows.push(shown);
            }
            "broken" => view.broken.push(rest.to_string()),
            _ => {}
        }
    }
    view
}

struct Run {
    rng: Rng,
    trail: Shared,
    compositor: Compositor,
    actors: Vec<Actor>,
    dock: Dock,
    locker: Option<Locker>,
    x: Option<X>,
    with_x: bool,
    held_buttons: Vec<&'static str>,
    held_keys: Vec<&'static str>,
    made: u32,
    keep_going: bool,
    wrong: BTreeMap<String, (u64, u64)>,
    view: View,
}

impl Run {
    fn note(&self, what: String) {
        let mut trail = self.trail.lock().unwrap();
        if trail.done.len() == 60 {
            trail.done.pop_front();
        }
        let step = trail.step;
        trail.done.push_back(format!("{step}: {what}"));
    }

    fn wrong(&mut self, what: String) {
        if !self.keep_going {
            panic!("{what}");
        }
        let (seed, step) = {
            let trail = self.trail.lock().unwrap();
            (trail.seed, trail.step)
        };
        // Names of windows differ from case to case; the case is the same.
        let kind: String = what.chars().filter(|c| !c.is_ascii_digit()).collect();
        if !self.wrong.contains_key(&kind) {
            self.trail
                .lock()
                .unwrap()
                .tell(&format!("noted at seed {seed} step {step}: {what}"));
            self.wrong.insert(kind, (seed, step));
        }
    }

    fn input(&mut self, step: &str) {
        self.note(format!("input {step}"));
        self.compositor.input(step);
    }

    fn inspect(&mut self) {
        let report = self.compositor.control("inspect");
        if !report.starts_with("ok") {
            let log = self.compositor.said_all("");
            let tail = &log[log.len().saturating_sub(30)..];
            panic!("the compositor did not answer (`{report}`); it said: {tail:#?}");
        }
        self.view = parse(&report);
    }

    fn look(&mut self) {
        self.inspect();
        // An X11 program is heard on two connections, and for a moment one
        // may be ahead of the other: a window smaller than what it has
        // drawn, say. What is said about such a window has to stay so.
        if self.view.broken.iter().any(|line| names_an_x_window(line)) {
            sleep(Duration::from_millis(200));
            self.inspect();
        }
        for broken in std::mem::take(&mut self.view.broken) {
            self.wrong(broken);
        }
        for what in ["panicked", "watchdog:", "cannot draw"] {
            if let Some(line) = self.compositor.said(what) {
                panic!("the compositor said: {line}");
            }
        }
    }

    // -- programs ---------------------------------------------------------

    fn new_actor(&mut self) {
        if self.actors.len() >= ACTORS {
            return;
        }
        self.made += 1;
        let id = self.made;
        self.note(format!("program {id} starts"));
        let mut client = self.compositor.connect();
        client.keyboard().expect("a keyboard");
        let pointer = client.pointer().expect("a pointer");
        let seat = client.seat().expect("a seat");
        let wm: XdgWmBase = client
            .globals
            .bind(&client.handle, 1..=2, ())
            .expect("xdg-shell");
        self.actors.push(Actor {
            id,
            client,
            pointer,
            seat,
            wm,
            windows: Vec::new(),
            shells: Vec::new(),
            drags: Vec::new(),
        });
    }

    fn kill_actor(&mut self) {
        if self.actors.is_empty() {
            return;
        }
        let at = self.rng.below(self.actors.len());
        let actor = self.actors.remove(at);
        self.note(format!("program {} is killed", actor.id));
        drop(actor);
    }

    /// Reads what every program was told. One that the compositor threw
    /// out is a fault of one side or the other.
    fn answer_all(&mut self) {
        let mut index = 0;
        while index < self.actors.len() {
            if let Err(err) = self.actors[index].client.roundtrip() {
                let id = self.actors[index].id;
                self.actors.remove(index);
                self.wrong(format!("program {id} lost its connection: {err}"));
                continue;
            }
            index += 1;
        }
        if let Err(err) = self.dock.client.roundtrip() {
            panic!("the dock lost its connection: {err}");
        }
        if let Some(locker) = &mut self.locker {
            if let Err(err) = locker.client.roundtrip() {
                panic!("the locker lost its connection: {err}");
            }
        }
    }

    /// What programs do by themselves: close menus that were dismissed,
    /// take away bars that were told to go.
    fn tidy(&mut self, all: bool) {
        for index in 0..self.actors.len() {
            let eager = all || self.rng.chance(70);
            let actor = &mut self.actors[index];
            if !eager {
                continue;
            }
            for window in &mut actor.windows {
                if let Some(first) = window
                    .popups
                    .iter()
                    .position(|popup| popup.dismissed.load(Ordering::Relaxed))
                {
                    // The ones above it first, as the protocol asks.
                    while window.popups.len() > first {
                        if let Some(popup) = window.popups.pop() {
                            popup.destroy();
                        }
                    }
                }
            }
            actor.shells.retain(|shell| {
                let closed = shell.seen.closed.load(Ordering::Relaxed);
                if closed {
                    shell.layer.destroy();
                    shell.surface.destroy();
                }
                !closed
            });
        }
    }

    fn some_actor(&mut self) -> Option<usize> {
        (!self.actors.is_empty()).then(|| self.rng.below(self.actors.len()))
    }

    fn some_window(&mut self, shown: bool) -> Option<(usize, usize)> {
        let all: Vec<(usize, usize)> = self
            .actors
            .iter()
            .enumerate()
            .flat_map(|(a, actor)| {
                actor
                    .windows
                    .iter()
                    .enumerate()
                    .filter(move |(_, window)| !shown || window.shown)
                    .map(move |(w, _)| (a, w))
            })
            .collect();
        (!all.is_empty()).then(|| all[self.rng.below(all.len())])
    }

    fn map_window(&mut self) {
        let Some(at) = self.some_actor() else { return };
        if self.actors[at].windows.len() >= WINDOWS_EACH {
            return;
        }
        self.made += 1;
        let title = format!("w{}", self.made);
        let colour = 0xff00_0000 | (self.rng.next() as u32 & 0x00ff_ffff);
        let wishes = (0..3).map(|_| self.rng.chance(8)).collect::<Vec<_>>();
        let gives_up = self.rng.chance(6);
        let size = self.some_size();
        let actor = &mut self.actors[at];
        let handle = actor.client.handle.clone();
        let asked = Arc::new(Asked::default());
        let surface = actor.client.compositor.create_surface(&handle, ());
        let shell = actor.wm.get_xdg_surface(&surface, &handle, ());
        let top = shell.get_toplevel(&handle, asked.clone());
        top.set_title(title.clone());
        if wishes[0] {
            top.set_maximized();
        }
        if wishes[1] {
            top.set_fullscreen(None);
        }
        let before = actor.client.seen.window_configures;
        surface.commit();
        if wishes[2] {
            top.set_minimized();
        }
        let window = Win {
            title: title.clone(),
            surface,
            shell,
            top,
            asked,
            popups: Vec::new(),
            shown: false,
            colour,
            held: None,
            confined: None,
            inhibitor: None,
        };
        let configured = actor
            .client
            .wait(SOON, |seen| seen.window_configures > before);
        actor.windows.push(window);
        let id = actor.id;
        self.note(format!(
            "program {id} makes {title} {}x{} (wishes {wishes:?}, never painted: {gives_up})",
            size.0, size.1
        ));
        if !configured {
            self.wrong(format!("{title} was never configured"));
        } else if !gives_up {
            let w = self.actors[at].windows.len() - 1;
            self.paint_window(at, w, size);
        }
    }

    fn some_size(&mut self) -> (i32, i32) {
        if self.rng.chance(8) {
            // Larger than any screen here.
            (self.rng.between(1300, 1700), self.rng.between(700, 900))
        } else if self.rng.chance(8) {
            (self.rng.between(1, 60), self.rng.between(1, 40))
        } else {
            (self.rng.between(90, 700), self.rng.between(60, 450))
        }
    }

    fn paint_window(&mut self, a: usize, w: usize, size: (i32, i32)) {
        let actor = &mut self.actors[a];
        let window = &mut actor.windows[w];
        window.shown = true;
        let (surface, colour) = (window.surface.clone(), window.colour);
        // A dead connection shows when everyone is next heard.
        let _ = actor.client.paint(&surface, size.0, size.1, colour);
    }

    fn repaint(&mut self) {
        let Some((a, w)) = self.some_window(true) else {
            return;
        };
        let window = &self.actors[a].windows[w];
        let told = (
            window.asked.width.load(Ordering::Relaxed) as i32,
            window.asked.height.load(Ordering::Relaxed) as i32,
        );
        let title = window.title.clone();
        let usable = told.0 > 0 && told.1 > 0 && told.0 < 4000 && told.1 < 4000;
        let size = if usable && self.rng.chance(85) {
            told
        } else {
            self.some_size()
        };
        self.note(format!("{title} draws {}x{}", size.0, size.1));
        self.paint_window(a, w, size);
    }

    fn close_popups(window: &mut Win) {
        while let Some(popup) = window.popups.pop() {
            popup.destroy();
        }
    }

    fn destroy_window(&mut self, a: usize, w: usize) {
        let mut window = self.actors[a].windows.remove(w);
        self.note(format!("{} closes", window.title));
        // With its menus still open, or after them: both happen. Surfaces
        // of menus go before their parent's either way.
        Self::close_popups(&mut window);
        if let Some(held) = window.held.take() {
            held.destroy();
        }
        if let Some(confined) = window.confined.take() {
            confined.destroy();
        }
        if let Some(inhibitor) = window.inhibitor.take() {
            inhibitor.destroy();
        }
        window.top.destroy();
        window.shell.destroy();
        window.surface.destroy();
        let _ = self.actors[a].client.queue.flush();
    }

    fn unmap_or_map_again(&mut self) {
        let Some((a, w)) = self.some_window(false) else {
            return;
        };
        let size = self.some_size();
        let actor = &mut self.actors[a];
        let window = &mut actor.windows[w];
        if window.shown {
            Self::close_popups(window);
            window.shown = false;
            window.surface.attach(None, 0, 0);
            window.surface.commit();
            let title = window.title.clone();
            let _ = actor.client.roundtrip();
            self.note(format!("{title} takes its picture away"));
        } else {
            let before = actor.client.seen.window_configures;
            let title = window.title.clone();
            window.surface.commit();
            // Not answered if the first configure is still unused.
            actor.client.wait(Duration::from_millis(50), |seen| {
                seen.window_configures > before
            });
            self.note(format!("{title} shows again"));
            self.paint_window(a, w, size);
        }
    }

    fn ask_as_window(&mut self) {
        let Some((a, w)) = self.some_window(false) else {
            return;
        };
        let what = self.rng.below(6);
        let window = &self.actors[a].windows[w];
        let name = match what {
            0 => {
                window.top.set_minimized();
                "to be minimised"
            }
            1 => {
                window.top.set_maximized();
                "to be maximised"
            }
            2 => {
                window.top.unset_maximized();
                "not to be maximised"
            }
            3 => {
                window.top.set_fullscreen(None);
                "to fill the screen"
            }
            4 => {
                window.top.unset_fullscreen();
                "not to fill the screen"
            }
            _ => {
                let min = self.rng.between(0, 500);
                window.top.set_min_size(min, min / 2);
                window.top.set_max_size(0, 0);
                window.surface.commit();
                "for a smallest size"
            }
        };
        let title = window.title.clone();
        let _ = self.actors[a].client.roundtrip();
        self.note(format!("{title} asks {name}"));
    }

    fn obey_close(&mut self) {
        let asked: Vec<(usize, usize)> = self
            .actors
            .iter()
            .enumerate()
            .flat_map(|(a, actor)| {
                actor
                    .windows
                    .iter()
                    .enumerate()
                    .filter(|(_, window)| window.asked.close.load(Ordering::Relaxed))
                    .map(move |(w, _)| (a, w))
            })
            .collect();
        if let Some(&(a, w)) = asked.last() {
            self.destroy_window(a, w);
        }
    }

    // -- input ------------------------------------------------------------

    /// A place for the pointer. A real one cannot leave the screens, so
    /// neither does this one.
    fn somewhere(&mut self) -> (i32, i32) {
        let (x, y) = self.anywhere();
        let screens: Vec<Rect> = self.view.outputs.iter().map(|output| output.1).collect();
        screens
            .iter()
            .map(|&(left, top, width, height)| {
                (
                    x.clamp(left, left + width.max(1) - 1),
                    y.clamp(top, top + height.max(1) - 1),
                )
            })
            .min_by_key(|&(near_x, near_y)| {
                let (dx, dy) = (i64::from(near_x - x), i64::from(near_y - y));
                dx * dx + dy * dy
            })
            .unwrap_or((x, y))
    }

    fn anywhere(&mut self) -> (i32, i32) {
        if !self.view.windows.is_empty() && self.rng.chance(75) {
            let at = self.rng.below(self.view.windows.len());
            let window = self.view.windows[at].clone();
            let mut spots = Vec::new();
            spots.extend(window.close);
            spots.extend(window.minimise);
            spots.extend(window.maximise);
            if let Some((x, y, w, h)) = window.frame {
                // The title bar, and places a resize starts from.
                spots.push((x + w / 2, y + 12));
                spots.push((x + w - 20, y + 12));
                spots.push((x, y + h / 2));
                spots.push((x + w - 1, y + h / 2));
                spots.push((x + w / 2, y + h - 1));
                spots.push((x + w - 1, y + h - 1));
                spots.push((x, y));
            }
            if let Some((x, y, w, h)) = window.client {
                spots.push((x + w / 2, y + h / 2));
                spots.push((x + 5, y + 5));
            }
            if !spots.is_empty() {
                return *self.rng.pick(&spots);
            }
        }
        let (x, y, w, h) = if self.view.outputs.is_empty() {
            (0, 0, 1280, 720)
        } else {
            self.view.outputs[self.rng.below(self.view.outputs.len())].1
        };
        if self.rng.chance(25) {
            // The edges, where windows snap.
            let spots = [
                (x, y + h / 2),
                (x + w - 1, y + h / 2),
                (x + w / 2, y),
                (x + w / 2, y + h - 1),
                (x, y),
            ];
            *self.rng.pick(&spots)
        } else {
            (
                self.rng.between(x - 20, x + w + 20),
                self.rng.between(y - 20, y + h + 20),
            )
        }
    }

    fn move_pointer(&mut self) {
        let (x, y) = self.somewhere();
        self.input(&format!("move {x} {y}"));
    }

    fn press_or_release(&mut self) {
        let button: &'static str = BUTTONS[usize::from(self.rng.chance(20))];
        if let Some(at) = self.held_buttons.iter().position(|held| *held == button) {
            self.held_buttons.remove(at);
            self.input(&format!("release {button}"));
        } else if self.rng.chance(50) {
            self.input(&format!("click {button}"));
        } else {
            self.held_buttons.push(button);
            self.input(&format!("press {button}"));
        }
    }

    /// A click on a place of a window, as one goes for a button or takes
    /// hold of a title bar.
    fn go_and_press(&mut self) {
        self.move_pointer();
        self.press_or_release();
        if self.rng.chance(60) {
            self.move_pointer();
        }
    }

    fn keys(&mut self) {
        if self.rng.chance(25) {
            let key: &'static str = self.rng.pick(&MODIFIERS);
            if let Some(at) = self.held_keys.iter().position(|held| *held == key) {
                self.held_keys.remove(at);
                self.input(&format!("keyup {key}"));
            } else {
                self.held_keys.push(key);
                self.input(&format!("keydown {key}"));
            }
        } else {
            let key = *self.rng.pick(KEYS);
            self.input(&format!("key {key}"));
        }
    }

    fn other_input(&mut self) {
        match self.rng.below(3) {
            0 => {
                let (dx, dy) = *self.rng.pick(&[(-160, 0), (160, 0), (0, -90), (0, 90)]);
                self.input(&format!("swipe 3 {dx} {dy}"));
            }
            1 => self.input("scroll 30"),
            _ => {
                let (dx, dy) = (self.rng.between(-300, 300), self.rng.between(-300, 300));
                self.input(&format!("nudge {dx} {dy}"));
            }
        }
    }

    /// Moves to the middle of a window of ours, presses, and returns which
    /// program got the press and its serial.
    fn press_on_a_client(&mut self) -> Option<(usize, usize, u32)> {
        let front: Vec<Shown> = self
            .view
            .windows
            .iter()
            .filter(|window| window.mapped && window.client.is_some())
            .cloned()
            .collect();
        if front.is_empty() || !self.held_buttons.is_empty() {
            return None;
        }
        let at = if self.rng.chance(60) {
            front.len() - 1
        } else {
            self.rng.below(front.len())
        };
        let (x, y, w, h) = front[at].client?;
        let (a, i) = self.actors.iter().enumerate().find_map(|(a, actor)| {
            let i = actor
                .windows
                .iter()
                .position(|window| window.title == front[at].title)?;
            Some((a, i))
        })?;
        let before = self.actors[a].client.seen.more.button_serial;
        self.input(&format!("move {} {}", x + w / 2, y + h / 2));
        self.input("press left");
        self.held_buttons.push("left");
        let _ = self.actors[a].client.roundtrip();
        let serial = self.actors[a].client.seen.more.button_serial;
        (serial != before).then_some((a, i, serial?))
    }

    fn open_menu(&mut self) {
        let Some((a, w, serial)) = self.press_on_a_client() else {
            return;
        };
        self.tidy(true);
        let nested = self.rng.chance(40);
        let size = (self.rng.between(20, 300), self.rng.between(20, 300));
        let place = (self.rng.between(0, 90), self.rng.between(0, 60));
        let release_first = self.rng.chance(30);
        // A grab goes to the topmost menu or to a window with none, and a
        // program has one such chain at a time.
        let others = self.actors[a]
            .windows
            .iter()
            .enumerate()
            .any(|(i, window)| i != w && !window.popups.is_empty());
        if others || !self.actors[a].windows[w].shown {
            return;
        }
        if !nested {
            Self::close_popups(&mut self.actors[a].windows[w]);
        }
        if release_first {
            self.release_all_buttons();
        }
        let actor = &mut self.actors[a];
        let handle = actor.client.handle.clone();
        let window = &mut actor.windows[w];
        if window.popups.len() >= 3 {
            return;
        }
        let positioner: XdgPositioner = actor.wm.create_positioner(&handle, ());
        positioner.set_size(size.0, size.1);
        positioner.set_anchor_rect(place.0, place.1, 1, 1);
        let surface = actor.client.compositor.create_surface(&handle, ());
        let shell = actor.wm.get_xdg_surface(&surface, &handle, ());
        let parent = window
            .popups
            .last()
            .map_or(&window.shell, |popup| &popup.shell);
        let dismissed = Flag::default();
        let popup = shell.get_popup(Some(parent), &positioner, &handle, dismissed.clone());
        positioner.destroy();
        popup.grab(&actor.seat, serial);
        let before = actor.client.seen.window_configures;
        surface.commit();
        let title = window.title.clone();
        window.popups.push(Popup {
            popup,
            shell,
            surface: surface.clone(),
            dismissed: dismissed.clone(),
        });
        let depth = window.popups.len();
        self.note(format!("{title} opens menu {depth}"));
        let actor = &mut self.actors[a];
        let configured = actor.client.wait(SOON, |seen| {
            seen.window_configures > before || dismissed.load(Ordering::Relaxed)
        });
        if configured && !dismissed.load(Ordering::Relaxed) {
            let _ = actor.client.paint(&surface, size.0, size.1, 0xff80_8080);
        }
    }

    fn close_menu(&mut self) {
        let Some((a, w)) = self.some_window(true) else {
            return;
        };
        let window = &mut self.actors[a].windows[w];
        if let Some(popup) = window.popups.pop() {
            let title = window.title.clone();
            popup.destroy();
            let _ = self.actors[a].client.roundtrip();
            self.note(format!("{title} closes its top menu"));
        }
    }

    fn client_drag(&mut self) {
        let Some((a, w, serial)) = self.press_on_a_client() else {
            return;
        };
        let what = self.rng.below(4);
        let edge = *self.rng.pick(&[
            ResizeEdge::Left,
            ResizeEdge::Top,
            ResizeEdge::TopLeft,
            ResizeEdge::BottomRight,
            ResizeEdge::Bottom,
        ]);
        let actor = &mut self.actors[a];
        let window = &actor.windows[w];
        let title = window.title.clone();
        let name = match what {
            0 => {
                window.top._move(&actor.seat, serial);
                "move"
            }
            1 => {
                window.top.resize(&actor.seat, serial, edge);
                "resize"
            }
            _ => {
                let surface = window.surface.clone();
                match actor.client.drag(&surface, serial, 0xff10_f0f0) {
                    Ok(drag) => actor.drags.push(drag),
                    Err(_) => return,
                }
                "drag of a thing"
            }
        };
        let _ = actor.client.roundtrip();
        self.note(format!("{title} starts a {name}"));
    }

    fn constrain(&mut self) {
        let Some((a, w)) = self.some_window(true) else {
            return;
        };
        let what = self.rng.below(3);
        let actor = &mut self.actors[a];
        let surface = actor.windows[w].surface.clone();
        let pointer = actor.pointer.clone();
        let title = actor.windows[w].title.clone();
        let window = &mut actor.windows[w];
        if window.held.is_some() || window.confined.is_some() {
            if let Some(held) = window.held.take() {
                held.destroy();
            }
            if let Some(confined) = window.confined.take() {
                confined.destroy();
            }
        } else if what == 0 {
            window.held = actor.client.lock_pointer(&surface, &pointer).ok();
        } else if what == 1 {
            actor.windows[w].confined = actor.client.confine_pointer(&surface, &pointer).ok();
        } else if let Some(inhibitor) = window.inhibitor.take() {
            inhibitor.destroy();
        } else {
            window.inhibitor = actor.client.inhibit_idle(&surface).ok();
        }
        let _ = actor.client.roundtrip();
        self.note(format!("{title} holds or lets go of something ({what})"));
    }

    fn activate(&mut self) {
        let Some((a, w)) = self.some_window(true) else {
            return;
        };
        let Some((b, v)) = self.some_window(false) else {
            return;
        };
        let target = self.actors[b].windows[v].surface.clone();
        let target_title = self.actors[b].windows[v].title.clone();
        let source = self.actors[a].windows[w].surface.clone();
        // A surface of one program cannot be named by another, so the token
        // is used by the program that owns the target.
        let serial = self.actors[a].client.seen.enter_serial;
        let Ok(token) = self.actors[a].client.activation_token(&source, serial) else {
            return;
        };
        self.note(format!("{target_title} asks to come forward"));
        let _ = self.actors[b].client.activate(&token, &target);
    }

    // -- the dock ---------------------------------------------------------

    fn dock_asks(&mut self) {
        let handles: Vec<ZwlrForeignToplevelHandleV1> = self.dock.handles.lock().unwrap().clone();
        if handles.is_empty() {
            return;
        }
        let handle = &handles[self.rng.below(handles.len())];
        let closed = is_closed(handle);
        let what = self.rng.below(9);
        let name = match what {
            0..=2 => {
                handle.activate(&self.dock.seat);
                "activate"
            }
            3 | 4 => {
                handle.set_minimized();
                "minimise"
            }
            5 => {
                handle.unset_minimized();
                "restore"
            }
            6 => {
                handle.set_maximized();
                "maximise"
            }
            7 => {
                handle.unset_maximized();
                "unmaximise"
            }
            _ => {
                handle.close();
                "close"
            }
        };
        let _ = self.dock.client.queue.flush();
        self.note(format!(
            "the dock: {name} on handle {} (closed: {closed})",
            handle.id().protocol_id()
        ));
        // Old handles are thrown away now and then, not at once.
        if self.rng.chance(20) {
            self.forget_closed_handles();
        }
    }

    fn forget_closed_handles(&mut self) {
        self.dock.handles.lock().unwrap().retain(|handle| {
            let closed = is_closed(handle);
            if closed {
                handle.destroy();
            }
            !closed
        });
    }

    // -- bars and menus of the desktop ------------------------------------

    fn layer_surface(&mut self) {
        let Some(a) = self.some_actor() else { return };
        if !self.actors[a].shells.is_empty() && self.rng.chance(55) {
            let at = self.rng.below(self.actors[a].shells.len());
            if self.rng.chance(60) {
                let shell = self.actors[a].shells.remove(at);
                shell.layer.destroy();
                shell.surface.destroy();
                self.note("a layer surface goes".into());
            } else {
                let interactivity = *self.rng.pick(&[
                    KeyboardInteractivity::None,
                    KeyboardInteractivity::Exclusive,
                    KeyboardInteractivity::OnDemand,
                ]);
                let zone = self.rng.between(0, 80);
                let shell = &self.actors[a].shells[at];
                shell.layer.set_keyboard_interactivity(interactivity);
                shell.layer.set_exclusive_zone(zone);
                shell.surface.commit();
                self.note(format!(
                    "a layer surface changes: {interactivity:?}, zone {zone}"
                ));
            }
            let _ = self.actors[a].client.roundtrip();
            return;
        }
        if self.actors[a].shells.len() >= 2 {
            return;
        }
        let kind = self.rng.below(5);
        let actor = &mut self.actors[a];
        let Ok(shells) =
            actor
                .client
                .globals
                .bind::<ZwlrLayerShellV1, _, _>(&actor.client.handle, 1..=4, ())
        else {
            return;
        };
        let seen = Arc::new(LayerSeen::default());
        let surface = actor
            .client
            .compositor
            .create_surface(&actor.client.handle, ());
        let (layer, namespace) = match kind {
            0 => (Layer::Top, "sengi-bar"),
            1 => (Layer::Top, "sengi-dock"),
            2 => (Layer::Bottom, "test desktop"),
            3 => (Layer::Overlay, "test launcher"),
            _ => (Layer::Top, "test panel"),
        };
        let made = shells.get_layer_surface(
            &surface,
            None,
            layer,
            namespace.into(),
            &actor.client.handle,
            seen.clone(),
        );
        match kind {
            0 => {
                made.set_size(0, 28);
                made.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
                made.set_exclusive_zone(28);
            }
            1 => {
                made.set_size(400, 60);
                made.set_anchor(Anchor::Bottom);
                made.set_exclusive_zone(60);
            }
            2 => {
                made.set_size(0, 0);
                made.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
                made.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            }
            3 => {
                made.set_size(300, 200);
                made.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            }
            _ => {
                made.set_size(200, 150);
                made.set_anchor(Anchor::Top | Anchor::Right);
                made.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            }
        }
        surface.commit();
        let waiting = seen.clone();
        actor.client.wait(SOON, move |_| {
            waiting.configure.lock().unwrap().is_some() || waiting.closed.load(Ordering::Relaxed)
        });
        let configure = seen.configure.lock().unwrap().take();
        seen.answered.store(configure.is_some(), Ordering::Relaxed);
        let shell = Shell {
            surface: surface.clone(),
            layer: made,
            seen,
        };
        if let Some((serial, width, height)) = configure {
            shell.layer.ack_configure(serial);
            if width > 0 && height > 0 {
                let _ = actor
                    .client
                    .paint(&surface, width as i32, height as i32, 0xff30_3030);
            }
        }
        actor.shells.push(shell);
        self.note(format!("a layer surface comes: {namespace}"));
    }

    // -- the lock ---------------------------------------------------------

    fn lock_or_unlock(&mut self) {
        match self.locker.take() {
            None => {
                self.note("the session locks".into());
                let client = self.compositor.locker();
                self.take_lock(client);
            }
            Some(locker) => {
                if self.rng.chance(25) {
                    self.note("the locker dies".into());
                    drop(locker);
                    // The compositor starts another.
                    let client = self.compositor.next_locker();
                    self.take_lock(client);
                } else if self.rng.chance(30) {
                    self.locker = Some(locker);
                } else {
                    self.unlock(locker);
                }
            }
        }
    }

    fn unlock(&mut self, mut locker: Locker) {
        // A locker may only unlock once it has been told that every screen
        // is covered, which takes a screen.
        self.look();
        if self.view.outputs.is_empty() {
            self.compositor.plug("1280x720");
            self.note("a screen is plugged in".into());
        }
        if !locker.client.wait(SOON, |seen| seen.locked > 0) {
            self.wrong("the locker was never told that the session is covered".into());
            return;
        }
        self.note("the session unlocks".into());
        locker.lock.unlock_and_destroy();
        locker.client.roundtrip().expect("unlocked");
        drop(locker);
        self.wait_for_lockers();
    }

    fn take_lock(&mut self, mut client: Client) {
        let lock = client.lock().expect("a lock");
        if self.rng.chance(80) {
            for output in client.outputs() {
                // The screen may be gone again by now.
                let _ = client.lock_surface(&lock, &output, 0xff10_1010);
            }
        }
        self.locker = Some(Locker { client, lock });
    }

    /// Until the compositor has collected the lockers it started.
    fn wait_for_lockers(&mut self) {
        let end = Instant::now() + SOON;
        loop {
            self.look();
            if self.view.number("children") == 0 || Instant::now() > end {
                return;
            }
            sleep(Duration::from_millis(10));
        }
    }

    // -- screens ----------------------------------------------------------

    fn screens(&mut self) {
        let names: Vec<String> = self.view.outputs.iter().map(|o| o.0.clone()).collect();
        match self.rng.below(5) {
            0 if names.len() < 3 => {
                let size = *self
                    .rng
                    .pick(&["1280x720", "800x600", "1920x1080", "640x480 panel"]);
                self.note(format!("a screen of {size} is plugged in"));
                self.compositor.plug(size);
            }
            // The last screen goes too, less often.
            1 if names.len() > 1 || (names.len() == 1 && self.rng.chance(30)) => {
                let name = self.rng.pick(&names).clone();
                self.note(format!("{name} is pulled out"));
                self.compositor.unplug(&name);
            }
            2 | 3 if !names.is_empty() => {
                let which = self.rng.below(names.len());
                let scale = *self.rng.pick(&[1.0, 1.25, 1.5, 2.0]);
                let off = names.len() > 1 && self.rng.chance(25);
                let moved = self.rng.chance(30);
                let custom = self.rng.chance(30).then(|| {
                    (
                        self.rng.between(320, 2000),
                        self.rng.between(240, 1200),
                        60_000,
                    )
                });
                let mut manager = self.compositor.connect();
                let Ok(handle) = manager.manager() else {
                    return;
                };
                let ops: Vec<Op> = manager
                    .head_names()
                    .iter()
                    .map(|name| {
                        let mut op = Op::keep(name);
                        if *name == names[which] {
                            if off {
                                return Op::off(name);
                            }
                            op = op.scale(scale);
                            if moved {
                                op = op.at(0, 2000);
                            }
                            if let Some((w, h, r)) = custom {
                                op = op.custom(w, h, r);
                            }
                        }
                        op
                    })
                    .collect();
                let outcome = manager.configure(&handle, None, &ops, true);
                self.note(format!(
                    "screens are set: {} scale {scale} off {off} moved {moved} \
                     size {custom:?}: {outcome:?}",
                    names[which]
                ));
            }
            _ if !names.is_empty() => {
                let mut power = self.compositor.connect();
                if let Ok(controls) = power.output_power() {
                    if let Some(control) = controls.first() {
                        use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::Mode;
                        control.set_mode(Mode::Off);
                        let _ = power.roundtrip();
                        self.note("a screen goes dark".into());
                    }
                }
            }
            _ => {}
        }
    }

    fn reload(&mut self) {
        let settings = format!(
            "[appearance]\nwindow-shadows = {}\ncursor-size = {}\ntheme = \"{}\"\n\
             [clipboard]\nhistory = {}\n[touchpad]\ngestures = true\n",
            self.rng.chance(50),
            self.rng.pick(&[24, 36, 48]),
            self.rng.pick(&["dark", "light"]),
            self.rng.chance(70),
        );
        self.note("the settings change".into());
        self.compositor.write_settings(&settings);
        self.compositor.signal("HUP");
    }

    fn picture(&mut self) {
        if self.view.outputs.is_empty() || self.locker.is_some() {
            return;
        }
        let mut viewer = self.compositor.connect();
        // The screen may go while it is looked at.
        let _ = viewer.capture();
        self.note("a picture is taken".into());
    }

    // -- at once ----------------------------------------------------------

    /// Several things reach the compositor in the same turn of its loop:
    /// it is stopped while they are sent.
    fn burst(&mut self) {
        let pid = self.compositor.pid() as libc::pid_t;
        self.note("burst: the compositor is held".into());
        let _held = Held::new(pid);
        let mut lines: Vec<UnixStream> = Vec::new();
        for _ in 0..self.rng.between(2, 7) {
            match self.rng.below(6) {
                0 | 1 => self.dock_asks(),
                2 => {
                    if let Some((a, w)) = self.some_window(false) {
                        self.destroy_window(a, w);
                    }
                }
                3 => {
                    if let Some((a, w)) = self.some_window(false) {
                        let top = &self.actors[a].windows[w].top;
                        match self.rng.below(3) {
                            0 => top.set_minimized(),
                            1 => top.set_maximized(),
                            _ => top.set_fullscreen(None),
                        }
                        let title = self.actors[a].windows[w].title.clone();
                        let _ = self.actors[a].client.queue.flush();
                        self.note(format!("{title} asks for a change"));
                    }
                }
                4 => self.kill_actor(),
                _ => {
                    let step = match self.rng.below(4) {
                        0 => {
                            let (x, y) = self.somewhere();
                            format!("move {x} {y}")
                        }
                        1 => "click left".into(),
                        2 => format!("key {}", self.rng.pick(KEYS)),
                        _ => {
                            self.held_buttons.retain(|held| *held != "left");
                            "release left".into()
                        }
                    };
                    self.note(format!("input {step}"));
                    let path = format!("{}.control", self.compositor.socket.display());
                    if let Ok(mut stream) = UnixStream::connect(path) {
                        let _ = stream.write_all(format!("do {step}").as_bytes());
                        lines.push(stream);
                    }
                }
            }
        }
        drop(_held);
        for mut line in lines {
            let mut answer = String::new();
            let _ = line.set_read_timeout(Some(SOON));
            let _ = line.read_to_string(&mut answer);
            assert_eq!(answer.trim(), "ok", "input in a burst");
        }
    }

    // -- letting go -------------------------------------------------------

    fn release_all_buttons(&mut self) {
        while let Some(button) = self.held_buttons.pop() {
            self.input(&format!("release {button}"));
        }
    }

    /// Everything is let go of, and then the desktop must work as if new.
    fn settle(&mut self) {
        self.note("-- everything is let go of".into());
        self.release_all_buttons();
        while let Some(key) = self.held_keys.pop() {
            self.input(&format!("keyup {key}"));
        }
        if let Some(locker) = self.locker.take() {
            self.unlock(locker);
        }
        self.look();
        if self.view.outputs.is_empty() {
            self.compositor.plug("1280x720");
            self.note("a screen is plugged in".into());
        }
        // Wakes a dark screen, and ends what a movement ends.
        self.input("move 3 3");
        self.input("move 4 4");
        self.look();
        if !self.view.is("overlay", "none") {
            self.input("key esc");
        }
        self.answer_all();
        self.close_x_menus();
        // Programs close their menus and take their layer surfaces away,
        // without any input after it.
        for actor in &mut self.actors {
            for window in &mut actor.windows {
                Self::close_popups(window);
                if let Some(held) = window.held.take() {
                    held.destroy();
                }
                if let Some(confined) = window.confined.take() {
                    confined.destroy();
                }
            }
            for shell in actor.shells.drain(..) {
                shell.layer.destroy();
                shell.surface.destroy();
            }
            // A drag of a thing that is given up.
            for (device, source, icon) in actor.drags.drain(..) {
                source.destroy();
                icon.destroy();
                drop(device);
            }
        }
        self.answer_all();
        self.input("move 6 6");
        self.answer_all();
        self.look();
        for name in ["pointer_grabbed", "keyboard_grabbed"] {
            if self.view.number(name) != 0 {
                self.wrong(format!("{name} with nothing held and no menu open"));
                return;
            }
        }
        if !self.view.is("drag", "none") || !self.view.is("overlay", "none") {
            self.wrong(format!(
                "still busy with nothing held: {:?}",
                self.view.facts
            ));
            return;
        }
        for name in ["popups", "layers", "held_buttons"] {
            if self.view.number(name) != 0 {
                self.wrong(format!("{name} left over: {}", self.view.number(name)));
            }
        }
        self.listed_as_it_is();
        self.try_the_front_window(true);
    }

    /// The dock is told of every window there is, and of no other.
    fn listed_as_it_is(&mut self) {
        // The list is brought up to date at the end of a turn of the loop.
        self.dock.client.roundtrip().expect("the dock");
        self.dock.client.roundtrip().expect("the dock");
        self.look();
        let open = self
            .dock
            .handles
            .lock()
            .unwrap()
            .iter()
            .filter(|handle| !is_closed(handle))
            .count();
        if open != self.view.windows.len() {
            self.wrong(format!(
                "the dock lists {open} windows, there are {}",
                self.view.windows.len()
            ));
        }
    }

    /// A click on the window in front gives it the keyboard, and a key
    /// then reaches it.
    ///
    /// A window that was asked to shrink is shown a second later if it has
    /// not answered, over what was in front a moment ago: `again` says
    /// whether another one may have come forward meanwhile.
    fn try_the_front_window(&mut self, again: bool) {
        let Some(front) = self
            .view
            .windows
            .iter()
            .rev()
            .find(|window| window.mapped)
            .cloned()
        else {
            return;
        };
        let Some(a) = self.actors.iter().position(|actor| {
            actor
                .windows
                .iter()
                .any(|window| window.title == front.title)
        }) else {
            return;
        };
        let Some((x, y, w, h)) = front.client else {
            return;
        };
        let on_screen = self.view.outputs.iter().find_map(|(_, (ox, oy, ow, oh))| {
            let (left, right) = (x.max(*ox), (x + w).min(ox + ow));
            let (top, bottom) = (y.max(*oy), (y + h).min(oy + oh));
            (right > left && bottom > top).then_some(((left + right) / 2, (top + bottom) / 2))
        });
        let Some((px, py)) = on_screen else { return };
        self.input(&format!("move {px} {py}"));
        let seen = &self.actors[a].client.seen;
        let (presses, keys) = (seen.more.button_serial, seen.keys);
        self.input("click left");
        self.input("key a");
        let reached = self.actors[a].client.wait(SOON, |seen| {
            seen.more.button_serial != presses && seen.keys > keys
        });
        if !reached {
            let seen = &self.actors[a].client.seen;
            let (clicked, typed) = (seen.more.button_serial != presses, seen.keys > keys);
            let report = self.compositor.control("inspect");
            self.note(format!("the click was at {px} {py}; {report}"));
            self.look();
            let now = self.view.windows.iter().rev().find(|window| window.mapped);
            if again && now.is_some_and(|now| now.title != front.title) {
                self.note("another window came forward".into());
                self.try_the_front_window(false);
                return;
            }
            self.wrong(format!(
                "{} in front does not answer: click {clicked}, key {typed}",
                front.title
            ));
        }
    }

    /// Closes every program, and checks that nothing of them stays.
    fn empty(&mut self) {
        self.settle();
        self.note("-- every program closes".into());
        if let Some(mut x) = self.x.take() {
            x.windows.clear();
        }
        while !self.actors.is_empty() {
            self.kill_actor();
        }
        self.dock.client.roundtrip().expect("the dock");
        let none = [
            "mapped",
            "minimised",
            "pending",
            "listed",
            "popups",
            "layers",
            "primary_layers",
            "inhibitors",
            "contexts",
            "held_buttons",
            "children",
        ];
        let end = Instant::now() + SOON;
        loop {
            self.dock.client.roundtrip().expect("the dock");
            self.look();
            let left: Vec<String> = none
                .iter()
                .filter(|name| self.view.number(name) != 0)
                .map(|name| format!("{name}={}", self.view.number(name)))
                .collect();
            if left.is_empty() {
                break;
            }
            if Instant::now() > end {
                self.wrong(format!("left over with no program open: {left:?}"));
                break;
            }
            sleep(Duration::from_millis(20));
        }
        self.forget_closed_handles();
        self.dock.client.roundtrip().expect("the dock");
    }

    fn step(&mut self) {
        if self.actors.len() < 2 {
            self.new_actor();
        }
        let locked = self.locker.is_some();
        let roll = self.rng.below(100);
        match roll {
            0..=2 => self.new_actor(),
            3 => self.kill_actor(),
            4..=11 => self.map_window(),
            12..=14 => {
                if let Some((a, w)) = self.some_window(false) {
                    self.destroy_window(a, w);
                }
            }
            15 | 16 => self.unmap_or_map_again(),
            17..=19 => self.repaint(),
            20..=24 => self.ask_as_window(),
            25..=33 => self.move_pointer(),
            34..=41 => self.press_or_release(),
            42..=49 => self.go_and_press(),
            50..=57 => self.keys(),
            58 | 59 => self.other_input(),
            60..=65 => self.dock_asks(),
            66..=70 => self.open_menu(),
            71 => self.close_menu(),
            72..=75 => self.client_drag(),
            76 | 77 => self.constrain(),
            78 => self.activate(),
            79..=83 => self.layer_surface(),
            // Locked sessions are left again soon.
            84 => self.lock_or_unlock(),
            85 | 86 if locked => self.lock_or_unlock(),
            85 | 86 => self.obey_close(),
            87..=90 => self.screens(),
            91 => self.reload(),
            92 => self.picture(),
            93..=95 => self.burst(),
            _ => {
                if self.with_x {
                    self.x_step();
                } else {
                    self.obey_close();
                }
            }
        }
        if self.with_x && self.rng.chance(8) {
            self.x_step();
        }
    }
}

/// The compositor, stopped for as long as this lives.
struct Held(libc::pid_t);

impl Held {
    fn new(pid: libc::pid_t) -> Self {
        // SAFETY: kill takes two numbers.
        unsafe { libc::kill(pid, libc::SIGSTOP) };
        Self(pid)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe { libc::kill(self.0, libc::SIGCONT) };
    }
}

// -- X11 ------------------------------------------------------------------

struct X {
    conn: RustConnection,
    root: u32,
    windows: Vec<(u32, bool)>,
}

impl X {
    fn atom(&self, name: &str) -> Option<u32> {
        Some(
            self.conn
                .intern_atom(false, name.as_bytes())
                .ok()?
                .reply()
                .ok()?
                .atom,
        )
    }

    fn tell(&self, window: u32, kind: &str, data: [u32; 5]) -> Option<()> {
        let event = ClientMessageEvent::new(32, window, self.atom(kind)?, data);
        self.conn
            .send_event(
                false,
                self.root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .ok()?;
        Some(())
    }

    /// Waits until Xwayland has dealt with what was sent.
    fn sync(&self) -> bool {
        let done = self
            .conn
            .get_input_focus()
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some();
        while let Ok(Some(_)) = self.conn.poll_for_event() {}
        done
    }
}

fn xwayland_of(compositor: u32) -> Option<u32> {
    std::fs::read_dir("/proc").ok()?.find_map(|entry| {
        let pid: u32 = entry.ok()?.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let comm = stat.split_once('(')?.1.split_once(')')?.0;
        let parent: u32 = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        (parent == compositor && comm == "Xwayland").then_some(pid)
    })
}

impl Run {
    fn x_connect(&mut self) {
        let Some(line) = self.compositor.said("Xwayland is up on :") else {
            return;
        };
        let Some(display) = line.rsplit(':').next().map(str::trim) else {
            return;
        };
        let Ok((conn, screen)) = x11rb::connect(Some(&format!(":{display}"))) else {
            return;
        };
        let root = conn.setup().roots[screen].root;
        self.x = Some(X {
            conn,
            root,
            windows: Vec::new(),
        });
        self.note("an X11 program starts".into());
    }

    /// Menus of X11 programs lie over everything and take no part in the
    /// focus; they go before the desktop is tried.
    fn close_x_menus(&mut self) {
        let Some(mut x) = self.x.take() else { return };
        for (window, menu) in &x.windows {
            if *menu {
                let _ = x.conn.destroy_window(*window);
            }
        }
        x.windows.retain(|(_, menu)| !menu);
        if x.sync() {
            self.x = Some(x);
            sleep(Duration::from_millis(50));
        }
    }

    fn x_step(&mut self) {
        if self.x.is_none() {
            if xwayland_of(self.compositor.pid()).is_none() {
                // The next program started brings it back.
                self.input("spawn true");
                let end = Instant::now() + SOON;
                while Instant::now() < end && xwayland_of(self.compositor.pid()).is_none() {
                    sleep(Duration::from_millis(20));
                }
                sleep(Duration::from_millis(500));
            }
            self.x_connect();
            return;
        }
        let roll = self.rng.below(100);
        if roll < 2 {
            if let Some(pid) = xwayland_of(self.compositor.pid()) {
                self.note("Xwayland is killed".into());
                // SAFETY: kill takes two numbers.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
                self.x = None;
                let end = Instant::now() + SOON;
                while Instant::now() < end && xwayland_of(self.compositor.pid()).is_some() {
                    sleep(Duration::from_millis(20));
                }
            }
            return;
        }
        if roll < 5 {
            self.note("the X11 program is killed".into());
            self.x = None;
            return;
        }
        let mut x = self.x.take().expect("just seen");
        if self.x_do(&mut x, roll).is_some() && x.sync() {
            self.x = Some(x);
            // The window manager hears of it on a connection of its own.
            sleep(Duration::from_millis(15));
        } else {
            self.note("the X11 connection is gone".into());
        }
    }

    fn x_do(&mut self, x: &mut X, roll: usize) -> Option<()> {
        if x.windows.is_empty() || (roll < 30 && x.windows.len() < 4) {
            let menu = self.rng.chance(25);
            let id = x.conn.generate_id().ok()?;
            let (w, h) = (self.rng.between(20, 900), self.rng.between(20, 600));
            let (px, py) = (self.rng.between(-50, 1200), self.rng.between(-50, 700));
            let mut aux = CreateWindowAux::new()
                .background_pixel(self.rng.next() as u32 & 0x00ff_ffff)
                .event_mask(EventMask::KEY_PRESS | EventMask::STRUCTURE_NOTIFY);
            if menu {
                aux = aux.override_redirect(1);
            }
            x.conn
                .create_window(
                    COPY_DEPTH_FROM_PARENT,
                    id,
                    x.root,
                    px as i16,
                    py as i16,
                    w as u16,
                    h as u16,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    COPY_FROM_PARENT,
                    &aux,
                )
                .ok()?;
            self.made += 1;
            let title = format!("x{}", self.made);
            x.conn
                .change_property8(
                    PropMode::REPLACE,
                    id,
                    AtomEnum::WM_NAME,
                    AtomEnum::STRING,
                    title.as_bytes(),
                )
                .ok()?;
            x.conn.map_window(id).ok()?;
            x.windows.push((id, menu));
            self.note(format!("{title} {w}x{h} appears (menu: {menu})"));
            return Some(());
        }
        let at = self.rng.below(x.windows.len());
        let (window, menu) = x.windows[at];
        let what = self.rng.below(10);
        match what {
            0 => {
                x.conn.destroy_window(window).ok()?;
                x.windows.remove(at);
            }
            1 => {
                x.conn.unmap_window(window).ok()?;
            }
            2 => {
                x.conn.map_window(window).ok()?;
            }
            3 => x.tell(window, "WM_CHANGE_STATE", [3, 0, 0, 0, 0])?,
            4 => {
                let state = x.atom("_NET_WM_STATE_FULLSCREEN")?;
                x.tell(window, "_NET_WM_STATE", [2, state, 0, 1, 0])?;
            }
            5 => {
                let wide = x.atom("_NET_WM_STATE_MAXIMIZED_HORZ")?;
                let tall = x.atom("_NET_WM_STATE_MAXIMIZED_VERT")?;
                x.tell(window, "_NET_WM_STATE", [2, wide, tall, 1, 0])?;
            }
            6 => {
                let aux = ConfigureWindowAux::new()
                    .x(self.rng.between(-3000, 3000))
                    .y(self.rng.between(-3000, 3000))
                    .width(self.rng.between(1, 3000) as u32)
                    .height(self.rng.between(1, 3000) as u32);
                x.conn.configure_window(window, &aux).ok()?;
            }
            7 => {
                let hints = x.atom("_MOTIF_WM_HINTS")?;
                let decorations = u32::from(self.rng.chance(50));
                x.conn
                    .change_property32(
                        PropMode::REPLACE,
                        window,
                        hints,
                        hints,
                        &[2, 0, decorations, 0, 0],
                    )
                    .ok()?;
            }
            8 => {
                // Gone as soon as it asked for something.
                x.tell(window, "WM_CHANGE_STATE", [3, 0, 0, 0, 0])?;
                x.conn.destroy_window(window).ok()?;
                x.windows.remove(at);
            }
            _ => x.tell(window, "WM_CHANGE_STATE", [1, 0, 0, 0, 0])?,
        }
        self.note(format!(
            "X11 window {window:#x} (menu: {menu}) does thing {what}"
        ));
        Some(())
    }
}

/// How much memory a process holds, in kB, and how many files it has open.
fn footprint(pid: u32) -> (u64, usize) {
    let resident = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
        })
        .unwrap_or(0);
    let files = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map(Iterator::count)
        .unwrap_or(0);
    (resident, files)
}

/// `footprint` once the number of open files holds still: memory that
/// programs shared is let go of on a thread of its own, a little later.
fn settled_footprint(pid: u32) -> (u64, usize) {
    let end = Instant::now() + SOON;
    let mut last = footprint(pid);
    loop {
        sleep(Duration::from_millis(300));
        let now = footprint(pid);
        if now.1 == last.1 || Instant::now() > end {
            return now;
        }
        last = now;
    }
}

fn variable(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn run(seed: u64, steps: u64, rounds: u64, with_x: bool) -> BTreeMap<String, (u64, u64)> {
    let trail: Shared = Arc::new(Mutex::new(Trail {
        seed,
        step: 0,
        done: VecDeque::new(),
        since: Instant::now(),
        over: false,
    }));
    let telling = Telling(trail.clone());
    let compositor = Compositor::start_with(None, Some("[clipboard]\nhistory = true\n"), None);
    watch(trail.clone(), compositor.pid());
    let mut client = compositor.connect();
    let seat = client.seat().expect("a seat");
    let handles = Handles::default();
    let _manager: ZwlrForeignToplevelManagerV1 = client
        .globals
        .bind(&client.handle, 1..=3, handles.clone())
        .expect("the compositor lists its windows");
    client.roundtrip().expect("the dock");
    let mut run = Run {
        rng: Rng(seed),
        trail: trail.clone(),
        compositor,
        actors: Vec::new(),
        dock: Dock {
            client,
            seat,
            handles,
        },
        locker: None,
        x: None,
        with_x,
        held_buttons: Vec::new(),
        held_keys: Vec::new(),
        made: 0,
        keep_going: variable("SENGI_STRESS_KEEP_GOING") == Some(1),
        wrong: BTreeMap::new(),
        view: View::default(),
    };
    run.look();

    let started = Instant::now();
    let mut footprints = Vec::new();
    let per_round = (steps / rounds).max(1);
    let mut step = 0;
    for round in 0..rounds {
        for _ in 0..per_round {
            step += 1;
            {
                let mut trail = trail.lock().unwrap();
                trail.step = step;
                trail.since = Instant::now();
            }
            run.step();
            run.answer_all();
            run.tidy(false);
            run.look();
            if step % SETTLE_EVERY == 0 {
                run.settle();
            }
        }
        trail.lock().unwrap().since = Instant::now();
        run.empty();
        let (resident, files) = settled_footprint(run.compositor.pid());
        footprints.push((resident, files));
        let _ = writeln!(
            std::io::stderr(),
            "stress: seed {seed} round {}: {step} steps in {:?}, {resident} kB resident, \
             {files} files open",
            round + 1,
            started.elapsed()
        );
    }
    // What leaks is more at the end than it ever was early on; what only
    // goes up and down does not. The first round fills what is made once:
    // fonts, cursors, buffers.
    if footprints.len() >= 5 {
        let early = &footprints[1..3];
        let late = &footprints[footprints.len() - 2..];
        let most = early.iter().map(|f| f.1).max();
        if late.iter().map(|f| f.1).min() > most.map(|most| most + 4) {
            run.wrong(format!(
                "open files grow from round to round: {footprints:?}"
            ));
        }
        let most = early.iter().map(|f| f.0).max();
        if late.iter().map(|f| f.0).min() > most.map(|most| most + 24 * 1024) {
            run.wrong(format!("memory grows from round to round: {footprints:?}"));
        }
    }
    let wrong = std::mem::take(&mut run.wrong);
    drop(run);
    drop(telling);
    wrong
}

/// Not run by a plain `cargo test`: the compositor starts this test binary
/// as the screen locker, with `--include-ignored`, see `harness::hand_over`.
#[test]
#[ignore = "started by the compositor as the locker"]
fn locker_stand_in() {
    harness::hand_over();
}

#[test]
fn windows_survive_a_long_run_of_random_things_done_to_them() {
    let seeds = variable("SENGI_STRESS_SEED").map_or(SEEDS.to_vec(), |seed| vec![seed]);
    let steps = variable("SENGI_STRESS_STEPS").unwrap_or(STEPS);
    let rounds = variable("SENGI_STRESS_ROUNDS").unwrap_or(1).max(1);
    let found = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("Xwayland").is_file()));
    let mut wrong = BTreeMap::new();
    for seed in seeds {
        // X11 programs join in on every other seed.
        let with_x = found && variable("SENGI_STRESS_X").map_or(seed % 2 == 0, |x| x == 1);
        wrong.extend(run(seed, steps, rounds, with_x));
    }
    assert!(wrong.is_empty(), "wrong (seed, step): {wrong:#?}");
}

// -- what the compositor tells ----------------------------------------------

impl Dispatch<XdgPositioner, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &XdgPositioner,
        _: xdg_positioner::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<XdgPopup, Flag> for Seen {
    fn event(
        _: &mut Self,
        _: &XdgPopup,
        event: xdg_popup::Event,
        dismissed: &Flag,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_popup::Event::PopupDone = event {
            dismissed.store(true, Ordering::Relaxed);
        }
    }
}

impl Dispatch<XdgToplevel, Arc<Asked>> for Seen {
    fn event(
        _: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        asked: &Arc<Asked>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                asked.width.store(width.max(0) as u32, Ordering::Relaxed);
                asked.height.store(height.max(0) as u32, Ordering::Relaxed);
            }
            xdg_toplevel::Event::Close => asked.close.store(true, Ordering::Relaxed),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, Arc<LayerSeen>> for Seen {
    fn event(
        _: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        seen: &Arc<LayerSeen>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                // The first is answered where it is waited for, the later
                // ones here.
                if seen.answered.load(Ordering::Relaxed) {
                    layer.ack_configure(serial);
                } else {
                    *seen.configure.lock().unwrap() = Some((serial, width, height));
                }
            }
            zwlr_layer_surface_v1::Event::Closed => seen.closed.store(true, Ordering::Relaxed),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, Handles> for Seen {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        handles: &Handles,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            handles.lock().unwrap().push(toplevel);
        }
    }

    event_created_child!(Seen, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, Flag::default())
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, Flag> for Seen {
    fn event(
        _: &mut Self,
        _: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        closed: &Flag,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_handle_v1::Event::Closed = event {
            closed.store(true, Ordering::Relaxed);
        }
    }
}
