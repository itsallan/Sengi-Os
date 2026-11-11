//! wlr-foreign-toplevel-management: tells the dock and sengi-focus which
//! windows exist and lets them activate, close, maximise and minimise them.
//!
//! Smithay does not ship this protocol, so it is implemented here. Rather
//! than hooking every place a window can change, the list of windows is
//! compared with what clients were last told once per loop turn.

use smithay::{
    desktop::Window,
    output::Output,
    reexports::{
        wayland_protocols_wlr::foreign_toplevel::v1::server::{
            zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
            zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
};

use crate::{state::State, window};

const VERSION: u32 = 3;

/// What clients are told about one window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub title: String,
    pub app_id: String,
    pub maximized: bool,
    pub minimized: bool,
    pub activated: bool,
}

/// What has to be sent to bring a client from `old` to `new`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    pub title: bool,
    pub app_id: bool,
    pub state: bool,
}

impl Changes {
    pub fn between(old: &Info, new: &Info) -> Self {
        Self {
            title: old.title != new.title,
            app_id: old.app_id != new.app_id,
            state: (old.maximized, old.minimized, old.activated)
                != (new.maximized, new.minimized, new.activated),
        }
    }

    pub fn any(self) -> bool {
        self.title || self.app_id || self.state
    }
}

impl Info {
    /// The `state` event's argument: the set states as native-endian u32s.
    pub fn state_array(&self) -> Vec<u8> {
        use zwlr_foreign_toplevel_handle_v1::State;
        [
            (self.maximized, State::Maximized),
            (self.minimized, State::Minimized),
            (self.activated, State::Activated),
        ]
        .into_iter()
        .filter(|(set, _)| *set)
        .flat_map(|(_, state)| (state as u32).to_ne_bytes())
        .collect()
    }
}

struct Entry {
    id: u64,
    window: Window,
    info: Info,
    handles: Vec<ZwlrForeignToplevelHandleV1>,
}

pub struct ForeignToplevelState {
    _global: GlobalId,
    managers: Vec<ZwlrForeignToplevelManagerV1>,
    entries: Vec<Entry>,
    next_id: u64,
}

impl ForeignToplevelState {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<State, ZwlrForeignToplevelManagerV1, ()>(VERSION, ()),
            managers: Vec::new(),
            entries: Vec::new(),
            next_id: 0,
        }
    }

    fn window(&self, id: u64) -> Option<Window> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.window.clone())
    }
}

/// Creates a handle for `entry` on `manager` and sends everything known.
fn announce(
    display: &DisplayHandle,
    manager: &ZwlrForeignToplevelManagerV1,
    entry: &mut Entry,
    output: Option<&Output>,
) {
    let Some(client) = manager.client() else {
        return;
    };
    let Ok(handle) = client.create_resource::<ZwlrForeignToplevelHandleV1, u64, State>(
        display,
        manager.version(),
        entry.id,
    ) else {
        return;
    };
    manager.toplevel(&handle);
    handle.title(entry.info.title.clone());
    handle.app_id(entry.info.app_id.clone());
    // Docks filter by output, so say where the window is.
    if let Some(output) = output {
        for wl_output in output.client_outputs(&client) {
            handle.output_enter(&wl_output);
        }
    }
    handle.state(entry.info.state_array());
    handle.done();
    entry.handles.push(handle);
}

impl State {
    fn foreign_info(&self, window: &Window) -> Info {
        let data = window::data(window);
        Info {
            title: window::title(window),
            app_id: window::app_id(window),
            maximized: data.restore.get().is_some(),
            minimized: data.minimised.get().is_some(),
            activated: self.focus_stack.current() == Some(window),
        }
    }

    fn foreign_unchanged(&self, window: &Window, info: &Info) -> bool {
        let data = window::data(window);
        info.maximized == data.restore.get().is_some()
            && info.minimized == data.minimised.get().is_some()
            && info.activated == (self.focus_stack.current() == Some(window))
            && window::with_title(window, |title| title == info.title)
            && window::with_app_id(window, |app_id| app_id == info.app_id)
    }

    /// Brings every listening client up to date with the windows as they
    /// are now. Cheap when nothing changed.
    pub fn refresh_foreign_toplevels(&mut self) {
        let windows: Vec<Window> = self.managed_windows();
        let output = self.output();

        // Gone windows first.
        self.foreign_toplevels.entries.retain(|entry| {
            let alive = windows.contains(&entry.window);
            if !alive {
                for handle in &entry.handles {
                    handle.closed();
                }
            }
            alive
        });

        for window in windows {
            // This runs every turn of the loop; most of the time nothing
            // changed and no strings need to be made to find that out.
            let known = self
                .foreign_toplevels
                .entries
                .iter()
                .find(|entry| entry.window == window);
            if known.is_some_and(|entry| self.foreign_unchanged(&window, &entry.info)) {
                continue;
            }
            let info = self.foreign_info(&window);
            let foreign = &mut self.foreign_toplevels;
            match foreign.entries.iter_mut().find(|e| e.window == window) {
                Some(entry) => {
                    let changes = Changes::between(&entry.info, &info);
                    if !changes.any() {
                        continue;
                    }
                    for handle in &entry.handles {
                        if changes.title {
                            handle.title(info.title.clone());
                        }
                        if changes.app_id {
                            handle.app_id(info.app_id.clone());
                        }
                        if changes.state {
                            handle.state(info.state_array());
                        }
                        handle.done();
                    }
                    entry.info = info;
                }
                None => {
                    let mut entry = Entry {
                        id: foreign.next_id,
                        window,
                        info,
                        handles: Vec::new(),
                    };
                    foreign.next_id += 1;
                    for manager in &foreign.managers {
                        announce(&self.display_handle, manager, &mut entry, output.as_ref());
                    }
                    foreign.entries.push(entry);
                }
            }
        }
    }
}

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn bind(
        state: &mut Self,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        let output = state.output();
        for entry in &mut state.foreign_toplevels.entries {
            announce(display, &manager, entry, output.as_ref());
        }
        state.foreign_toplevels.managers.push(manager);
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ZwlrForeignToplevelManagerV1,
        request: zwlr_foreign_toplevel_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Request::Stop = request {
            state.foreign_toplevels.managers.retain(|m| m != manager);
            manager.finished();
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        manager: &ZwlrForeignToplevelManagerV1,
        _data: &(),
    ) {
        state.foreign_toplevels.managers.retain(|m| m != manager);
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, u64> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _handle: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        id: &u64,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Request;

        let Some(window) = state.foreign_toplevels.window(*id) else {
            return;
        };
        // The dock is not reachable on the lock screen; anything else
        // asking is not to be trusted with the windows behind it.
        if state.is_locked() {
            return;
        }
        match request {
            Request::Activate { .. } => {
                state.set_minimised(&window, false);
                state.focus_window(&window);
            }
            Request::Close => {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_close();
                }
            }
            Request::SetMaximized => state.set_maximised(&window, true),
            Request::UnsetMaximized => state.set_maximised(&window, false),
            Request::SetMinimized => state.set_minimised(&window, true),
            Request::UnsetMinimized => state.set_minimised(&window, false),
            // Fullscreen is not implemented; the rectangle is only a hint
            // for minimise animations.
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        handle: &ZwlrForeignToplevelHandleV1,
        id: &u64,
    ) {
        if let Some(entry) = state
            .foreign_toplevels
            .entries
            .iter_mut()
            .find(|e| e.id == *id)
        {
            entry.handles.retain(|h| h != handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(info: &Info) -> Vec<u32> {
        info.state_array()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_ne_bytes(*bytes))
            .collect()
    }

    #[test]
    fn state_array_lists_the_set_states() {
        let mut info = Info::default();
        assert!(info.state_array().is_empty());

        info.activated = true;
        assert_eq!(states(&info), vec![2]);

        info.maximized = true;
        info.minimized = true;
        assert_eq!(states(&info), vec![0, 1, 2]);
    }

    #[test]
    fn only_what_differs_is_sent() {
        let old = Info {
            title: "foot".into(),
            app_id: "foot".into(),
            ..Info::default()
        };
        assert!(!Changes::between(&old, &old).any());

        let mut retitled = old.clone();
        retitled.title = "vim".into();
        let changes = Changes::between(&old, &retitled);
        assert_eq!(
            (changes.title, changes.app_id, changes.state),
            (true, false, false)
        );

        let mut focused = old.clone();
        focused.activated = true;
        let changes = Changes::between(&old, &focused);
        assert_eq!(
            (changes.title, changes.app_id, changes.state),
            (false, false, true)
        );
    }
}
