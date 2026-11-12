//! Outputs through the compositor's `wlr-output-management-unstable-v1`.
//!
//! The protocol is a conversation: the compositor describes every head
//! and its modes and then says `done` with a serial; a client that wants
//! a change builds a configuration against that serial, naming every head,
//! and is told whether it succeeded, failed, or was cancelled because the
//! heads changed meanwhile.
//!
//! This has a Wayland connection of its own, apart from the window's. A
//! small thread sleeps in `poll` on its socket and wakes the page's worker
//! when the compositor has said something, such as a monitor being plugged
//! in; nothing here wakes up on a timer.

use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use wayland_client::backend::{ObjectId, WaylandError};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols_wlr::output_management::v1::client::zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1;
use wayland_protocols_wlr::output_management::v1::client::zwlr_output_configuration_v1::{
    self, ZwlrOutputConfigurationV1,
};
use wayland_protocols_wlr::output_management::v1::client::zwlr_output_head_v1::{
    self, ZwlrOutputHeadV1,
};
use wayland_protocols_wlr::output_management::v1::client::zwlr_output_manager_v1::{
    self, ZwlrOutputManagerV1,
};
use wayland_protocols_wlr::output_management::v1::client::zwlr_output_mode_v1::{
    self, ZwlrOutputModeV1,
};

use super::{
    backlight, ApplyError, Change, DisplayService, Mode, Output, Reading, MAX_MODES, MAX_OUTPUTS,
};
use crate::bus::Bus;
use crate::text;
use crate::worker::Notify;

/// The newest version of the protocol this was written against.
const VERSION: u32 = 4;

struct ModeEntry {
    proxy: ZwlrOutputModeV1,
    mode: Mode,
}

struct Head {
    proxy: ZwlrOutputHeadV1,
    name: String,
    description: String,
    enabled: bool,
    modes: Vec<ModeEntry>,
    current: Option<ObjectId>,
    scale: f64,
}

/// An output as `wl_output` describes it. The management protocol has
/// no way to say what size an output with no mode list is running at;
/// this does.
struct Plain {
    proxy: WlOutput,
    name: String,
    size: Option<(i32, i32, i32)>,
}

#[derive(Default)]
struct State {
    manager: Option<ZwlrOutputManagerV1>,
    heads: Vec<Head>,
    plain: Vec<Plain>,
    /// From the last `done`: what a configuration must be made against.
    serial: Option<u32>,
    /// How the configuration being applied ended.
    outcome: Option<Result<(), ApplyError>>,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut State,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<State>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == ZwlrOutputManagerV1::interface().name && state.manager.is_none() {
                state.manager = Some(registry.bind(name, version.min(VERSION), queue, ()));
            }
            // Version 4 is the first that names the output.
            if interface == WlOutput::interface().name
                && version >= 4
                && state.plain.len() < MAX_OUTPUTS
            {
                state.plain.push(Plain {
                    proxy: registry.bind(name, 4, queue, ()),
                    name: String::new(),
                    size: None,
                });
            }
        }
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        state: &mut State,
        proxy: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(plain) = state.plain.iter_mut().find(|plain| plain.proxy == *proxy) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => plain.name = name,
            wl_output::Event::Mode {
                flags,
                width,
                height,
                refresh,
            } => {
                let current = flags
                    .into_result()
                    .is_ok_and(|flags| flags.contains(wl_output::Mode::Current));
                if current {
                    plain.size = Some((width, height, refresh));
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for State {
    fn event(
        state: &mut State,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                if state.heads.len() < MAX_OUTPUTS {
                    state.heads.push(Head {
                        proxy: head,
                        name: String::new(),
                        description: String::new(),
                        enabled: false,
                        modes: Vec::new(),
                        current: None,
                        scale: 1.0,
                    });
                }
            }
            zwlr_output_manager_v1::Event::Done { serial } => state.serial = Some(serial),
            zwlr_output_manager_v1::Event::Finished => {
                state.manager = None;
                state.heads.clear();
                state.serial = None;
            }
            _ => {}
        }
    }

    event_created_child!(State, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for State {
    fn event(
        state: &mut State,
        proxy: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let zwlr_output_head_v1::Event::Finished = event {
            state.heads.retain(|head| head.proxy != *proxy);
            return;
        }
        let Some(head) = state.heads.iter_mut().find(|head| head.proxy == *proxy) else {
            return;
        };
        match event {
            zwlr_output_head_v1::Event::Name { name } => head.name = name,
            zwlr_output_head_v1::Event::Description { description } => {
                head.description = description;
            }
            zwlr_output_head_v1::Event::Mode { mode } => {
                if head.modes.len() < MAX_MODES {
                    head.modes.push(ModeEntry {
                        proxy: mode,
                        mode: Mode {
                            width: 0,
                            height: 0,
                            refresh: 0,
                            custom: false,
                        },
                    });
                }
            }
            zwlr_output_head_v1::Event::Enabled { enabled } => head.enabled = enabled != 0,
            zwlr_output_head_v1::Event::CurrentMode { mode } => head.current = Some(mode.id()),
            zwlr_output_head_v1::Event::Scale { scale } => head.scale = scale,
            _ => {}
        }
    }

    event_created_child!(State, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for State {
    fn event(
        state: &mut State,
        proxy: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let zwlr_output_mode_v1::Event::Finished = event {
            for head in state.heads.iter_mut() {
                head.modes.retain(|entry| entry.proxy != *proxy);
            }
            return;
        }
        let mut modes = state
            .heads
            .iter_mut()
            .flat_map(|head| head.modes.iter_mut());
        let Some(entry) = modes.find(|entry| entry.proxy == *proxy) else {
            return;
        };
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => {
                entry.mode.width = width;
                entry.mode.height = height;
            }
            zwlr_output_mode_v1::Event::Refresh { refresh } => entry.mode.refresh = refresh,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for State {
    fn event(
        state: &mut State,
        _: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        state.outcome = Some(match event {
            zwlr_output_configuration_v1::Event::Succeeded => Ok(()),
            zwlr_output_configuration_v1::Event::Failed => Err(ApplyError::Refused),
            zwlr_output_configuration_v1::Event::Cancelled => Err(ApplyError::Outdated),
            _ => return,
        });
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ()> for State {
    fn event(
        _: &mut State,
        _: &ZwlrOutputConfigurationHeadV1,
        _: <ZwlrOutputConfigurationHeadV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

/// A connector name as compositors write them: `eDP-1`, `HDMI-A-2`.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// A scale as the protocol gives it, in thousandths, within reason.
pub fn scale_thousandths(scale: f64) -> u32 {
    if scale.is_finite() {
        (scale * 1000.0).round().clamp(250.0, 8000.0) as u32
    } else {
        1000
    }
}

fn outputs(state: &State) -> Vec<Output> {
    let mut outputs: Vec<Output> = state
        .heads
        .iter()
        .filter(|head| plain_name(&head.name))
        .map(|head| {
            let modes: Vec<Mode> = head
                .modes
                .iter()
                .map(|entry| entry.mode)
                .filter(Mode::sane)
                .collect();
            let listed = head.current.as_ref().and_then(|id| {
                head.modes
                    .iter()
                    .find(|entry| entry.proxy.id() == *id)
                    .map(|entry| entry.mode)
                    .filter(Mode::sane)
            });
            // An output with no modes of its own runs at a size it was
            // given, which only wl_output tells.
            let given = state
                .plain
                .iter()
                .find(|plain| plain.name == head.name)
                .and_then(|plain| plain.size)
                .map(|(width, height, refresh)| Mode {
                    width,
                    height,
                    refresh,
                    custom: true,
                })
                .filter(Mode::sane);
            // A virtual display, as in a virtual machine or a headless
            // session, lists nothing or just the size it was last given,
            // with no refresh rate, and takes any size. A real panel's
            // modes always have a rate.
            let takes_any_size = modes.is_empty() || (modes.len() == 1 && modes[0].refresh == 0);
            let (modes, current) = if takes_any_size {
                let as_given = listed.map(|mode| Mode {
                    custom: true,
                    ..mode
                });
                (Vec::new(), given.or(as_given))
            } else {
                (modes, listed)
            };
            let title = text::clean(&head.description, 48);
            Output {
                name: head.name.clone(),
                title: if title.is_empty() {
                    head.name.clone()
                } else {
                    title
                },
                enabled: head.enabled,
                current,
                modes,
                scale: scale_thousandths(head.scale),
            }
        })
        .collect();
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    outputs
}

struct Inner {
    queue: EventQueue<State>,
    state: State,
}

pub struct Wlr {
    /// Made the first time the page asks for anything, not before: most
    /// times Settings is opened the Display page is never looked at.
    connection: OnceLock<Option<Connection>>,
    inner: Mutex<Option<Inner>>,
    bus: Arc<Bus>,
    /// `/sys/class`, or in a debug build a directory laid out like it.
    class: PathBuf,
}

impl Wlr {
    pub fn new(bus: Arc<Bus>, class: PathBuf) -> Wlr {
        Wlr {
            connection: OnceLock::new(),
            inner: Mutex::new(None),
            bus,
            class,
        }
    }

    /// The connection to the compositor named by the environment. With
    /// none to connect to the page has no outputs and says why.
    fn connection(&self) -> Option<&Connection> {
        self.connection
            .get_or_init(|| {
                let connection = Connection::connect_to_env().ok()?;
                let mut queue = connection.new_event_queue();
                let mut state = State::default();
                connection.display().get_registry(&queue.handle(), ());
                // Once for the globals, once for the heads, once for
                // their modes.
                for _ in 0..3 {
                    queue.roundtrip(&mut state).ok()?;
                }
                *self.inner.lock().unwrap_or_else(|p| p.into_inner()) =
                    Some(Inner { queue, state });
                Some(connection)
            })
            .as_ref()
    }

    fn inner(&self) -> MutexGuard<'_, Option<Inner>> {
        self.connection();
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl DisplayService for Wlr {
    fn read(&self) -> Reading {
        let mut guard = self.inner();
        let outputs = guard.as_mut().and_then(|inner| {
            // Only what the watcher has already read off the socket: a
            // read must cause no traffic, or it would wake the watcher,
            // which would ask for another read.
            inner.queue.dispatch_pending(&mut inner.state).ok()?;
            inner.state.manager.as_ref()?;
            Some(outputs(&inner.state))
        });
        Reading {
            outputs,
            backlight: backlight::read(&self.class.join("backlight")),
        }
    }

    fn apply(&self, change: &Change) -> Result<(), ApplyError> {
        let mut guard = self.inner();
        let inner = guard.as_mut().ok_or(ApplyError::Unavailable)?;
        let Inner { queue, state } = inner;
        queue
            .dispatch_pending(state)
            .map_err(|_| ApplyError::Unavailable)?;
        let (Some(manager), Some(serial)) = (state.manager.clone(), state.serial) else {
            return Err(ApplyError::Unavailable);
        };
        if !state.heads.iter().any(|head| head.name == change.output) {
            return Err(ApplyError::Unavailable);
        }
        let handle = queue.handle();
        let configuration = manager.create_configuration(serial, &handle, ());
        // Every head has to be spoken for, changed or not.
        for head in &state.heads {
            if !head.enabled {
                configuration.disable_head(&head.proxy);
                continue;
            }
            let settings = configuration.enable_head(&head.proxy, &handle, ());
            if head.name != change.output {
                continue;
            }
            if let Some(mode) = change.mode.filter(Mode::sane) {
                let listed = head.modes.iter().find(|entry| {
                    !mode.custom
                        && entry.mode.size() == mode.size()
                        && entry.mode.refresh == mode.refresh
                });
                match listed {
                    Some(entry) => settings.set_mode(&entry.proxy),
                    None if mode.custom => {
                        settings.set_custom_mode(mode.width, mode.height, mode.refresh);
                    }
                    // A mode the head does not have: leave it as it is,
                    // and let the compositor's answer stand for the rest.
                    None => {}
                }
            }
            if let Some(scale) = change.scale {
                settings.set_scale(f64::from(scale.clamp(250, 8000)) / 1000.0);
            }
        }
        state.outcome = None;
        configuration.apply();
        let outcome = loop {
            if let Some(outcome) = state.outcome.take() {
                break outcome;
            }
            if queue.blocking_dispatch(state).is_err() {
                break Err(ApplyError::Unavailable);
            }
        };
        configuration.destroy();
        let _ = queue.flush();
        outcome
    }

    fn set_brightness(&self, name: &str, raw: u32) {
        use sengi_system::services::Machine;
        // logind sets it: the one route open to an ordinary user.
        sengi_system::machine::Sysfs::new(self.bus.clone(), self.class.clone())
            .set_backlight(name, raw);
    }

    fn watch(&self, changed: Notify) {
        let Some(connection) = self.connection().cloned() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("settings-display-watch".to_string())
            .spawn(move || loop {
                let Some(guard) = connection.prepare_read() else {
                    // Events are waiting to be dispatched: say so, and
                    // give the worker a moment to do it.
                    changed();
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                };
                let mut fds = [libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                }];
                // SAFETY: poll is given one valid pollfd and waits; it
                // touches nothing else.
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, -1) };
                if ready < 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                match guard.read() {
                    Ok(_) => changed(),
                    Err(WaylandError::Io(error))
                        if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    // The compositor has gone.
                    Err(_) => break,
                }
            });
        if spawned.is_err() {
            eprintln!("sengi-settings: could not start a thread to watch the displays");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_names() {
        for good in [
            "eDP-1",
            "HDMI-A-2",
            "DP-3",
            "HEADLESS-1",
            "Virtual-1",
            "DVI-I-1",
        ] {
            assert!(plain_name(good), "{good}");
        }
        for bad in ["", "a b", "x/y", "é", "new\nline", &"n".repeat(65)] {
            assert!(!plain_name(bad), "{bad}");
        }
    }

    #[test]
    fn scales_are_held_within_reason() {
        assert_eq!(scale_thousandths(1.0), 1000);
        assert_eq!(scale_thousandths(1.25), 1250);
        assert_eq!(scale_thousandths(1.7499), 1750);
        assert_eq!(scale_thousandths(0.0), 250);
        assert_eq!(scale_thousandths(1e9), 8000);
        assert_eq!(scale_thousandths(f64::NAN), 1000);
        assert_eq!(scale_thousandths(f64::INFINITY), 1000);
    }

    #[test]
    fn without_a_compositor_there_are_no_outputs_and_nothing_applies() {
        // A socket name nothing listens on.
        let before = (
            std::env::var_os("WAYLAND_DISPLAY"),
            std::env::var_os("WAYLAND_SOCKET"),
        );
        std::env::set_var("WAYLAND_DISPLAY", "sengi-settings-test-no-such-socket");
        std::env::remove_var("WAYLAND_SOCKET");
        let wlr = Wlr::new(Arc::default(), PathBuf::from("/nonexistent"));
        match before.0 {
            Some(value) => std::env::set_var("WAYLAND_DISPLAY", value),
            None => std::env::remove_var("WAYLAND_DISPLAY"),
        }
        if let Some(value) = before.1 {
            std::env::set_var("WAYLAND_SOCKET", value);
        }
        assert_eq!(wlr.read(), Reading::default());
        let change = Change {
            output: "eDP-1".to_string(),
            mode: None,
            scale: Some(2000),
        };
        assert_eq!(wlr.apply(&change), Err(ApplyError::Unavailable));
        wlr.watch(Box::new(|| {}));
    }
}
