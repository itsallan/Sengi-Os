//! sengi-focus keeps the focused app ahead of the others.
//!
//! It listens for window focus changes from the compositor and raises the
//! CPU and I/O weight of the focused app's systemd scope, putting the
//! previous one back to the default. Weights only matter when the machine is
//! busy, so this costs nothing when it is idle.

mod cgroup;
mod policy;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;

use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self as handle, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self as manager, ZwlrForeignToplevelManagerV1,
};

#[derive(Default)]
struct Window {
    app_id: String,
    activated: bool,
}

struct Focus {
    app_slice: PathBuf,
    windows: HashMap<ObjectId, Window>,
    boosted: Vec<String>,
    manager_found: bool,
    finished: bool,
}

impl Focus {
    fn update(&mut self) {
        let focused = self
            .windows
            .values()
            .find(|w| w.activated)
            .map(|w| w.app_id.as_str());
        let wanted = match focused {
            Some(app_id) => policy::scopes_for(app_id, &cgroup::scopes(&self.app_slice)),
            None => Vec::new(),
        };
        let change = policy::plan(&self.boosted, &wanted);
        for unit in &change.restore {
            cgroup::set_weight(unit, cgroup::DEFAULT_WEIGHT);
        }
        for unit in &change.boost {
            cgroup::set_weight(unit, cgroup::FOCUSED_WEIGHT);
        }
        self.boosted = wanted;
    }
}

impl Dispatch<WlRegistry, ()> for Focus {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
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
            if interface == ZwlrForeignToplevelManagerV1::interface().name {
                registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(
                    name,
                    version.min(3),
                    queue,
                    (),
                );
                state.manager_found = true;
            }
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Focus {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            manager::Event::Toplevel { toplevel } => {
                state.windows.insert(toplevel.id(), Window::default());
            }
            manager::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(Focus, ZwlrForeignToplevelManagerV1, [
        manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Focus {
    fn event(
        state: &mut Self,
        toplevel: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            handle::Event::AppId { app_id } => {
                if let Some(window) = state.windows.get_mut(&toplevel.id()) {
                    window.app_id = app_id;
                }
            }
            handle::Event::State { state: flags } => {
                // The state arrives as a packed array of 32-bit values.
                let activated = flags.chunks(4).any(|bytes| {
                    <[u8; 4]>::try_from(bytes)
                        .is_ok_and(|b| u32::from_ne_bytes(b) == handle::State::Activated as u32)
                });
                if let Some(window) = state.windows.get_mut(&toplevel.id()) {
                    window.activated = activated;
                }
            }
            handle::Event::Done => state.update(),
            handle::Event::Closed => {
                state.windows.remove(&toplevel.id());
                toplevel.destroy();
                state.update();
            }
            _ => {}
        }
    }
}

fn run() -> Result<(), String> {
    let app_slice = cgroup::app_slice().map_err(|e| format!("cannot find the app slice: {e}"))?;
    let connection =
        Connection::connect_to_env().map_err(|e| format!("cannot reach the compositor: {e}"))?;
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());

    let mut focus = Focus {
        app_slice,
        windows: HashMap::new(),
        boosted: Vec::new(),
        manager_found: false,
        finished: false,
    };

    queue.roundtrip(&mut focus).map_err(|e| e.to_string())?;
    if !focus.manager_found {
        return Err("the compositor does not offer wlr-foreign-toplevel-management".into());
    }

    while !focus.finished {
        queue
            .blocking_dispatch(&mut focus)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sengi-focus: {error}");
            ExitCode::FAILURE
        }
    }
}
