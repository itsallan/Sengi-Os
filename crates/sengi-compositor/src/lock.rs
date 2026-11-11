//! ext-session-lock: the lock screen.
//!
//! While `State::lock` is set nothing but the lock surfaces is drawn and
//! nothing else receives input. That is decided in three places, all of
//! which ask `is_locked`: `render::elements`, the pointer hit test in
//! `input.rs`, and `State::update_keyboard_focus`. If the locker dies
//! without unlocking, the session stays locked and blank, as the protocol
//! demands, and the compositor starts its own locker for it.
//!
//! Who may do what:
//!
//! - Only the configured locker program may ask for the lock at all; the
//!   global is hidden from everything else, see `Lockers`.
//! - While a locker lives, a second one is turned away.
//! - Only the lock object that was granted the lock can unlock or supply
//!   lock surfaces. Smithay's own handling does not make that difference,
//!   which is why requests on lock objects pass through `request` below
//!   before they reach it.
//! - The locker is told `locked` once every screen has shown a frame that
//!   was drawn after the lock began, see `Covering`.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use smithay::{
    output::Output,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_protocols::ext::session_lock::v1::server::{
            ext_session_lock_manager_v1::ExtSessionLockManagerV1,
            ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
            ext_session_lock_v1::{self, ExtSessionLockV1},
        },
        wayland_server::{
            delegate_dispatch, delegate_global_dispatch,
            protocol::{wl_output::WlOutput, wl_surface::WlSurface},
            Client, DataInit, Dispatch, DisplayHandle, Resource,
        },
    },
    utils::SERIAL_COUNTER,
    wayland::session_lock::{
        ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
        SessionLockManagerState, SessionLockState, SessionLocker,
    },
};

use crate::{
    state::{ClientState, State},
    window,
};

pub struct Lock {
    /// The lock screen of each output that has one.
    surfaces: Vec<(Output, LockSurface)>,
    /// The lock object of the client holding the lock. When that client
    /// dies this goes dead, and another locker may take over.
    locker: ExtSessionLockV1,
    /// Counts up with every lock, so a frame can say which one it shows.
    epoch: u64,
    /// The locker's confirmation and the screens it still waits for.
    pending: Option<(SessionLocker, Covering<Output>)>,
}

/// Who holds the session lock when a client asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    Nobody,
    /// A locker that is still connected.
    Alive,
    /// The session is locked but its locker is gone.
    Dead,
}

/// Whether a lock request is granted. `recognised` says the client is the
/// configured locker program.
///
/// Nobody may lock over a live locker, and nothing but the locker may lock
/// at all: a program that can lock can also show its own password prompt,
/// and one that can take over a dead locker's session can unlock it.
pub fn may_lock(holder: Holder, recognised: bool) -> bool {
    recognised && holder != Holder::Alive
}

/// The screens that have not yet shown a frame drawn under the lock.
#[derive(Debug)]
pub struct Covering<O> {
    waiting: Vec<O>,
}

impl<O: PartialEq> Covering<O> {
    pub fn new(outputs: Vec<O>) -> Self {
        Self { waiting: outputs }
    }

    /// `output` showed a frame; `locked_frame` says it was drawn after
    /// this lock began. Returns whether every screen is covered now.
    pub fn presented(&mut self, output: &O, locked_frame: bool) -> bool {
        if locked_frame {
            self.waiting.retain(|waiting| waiting != output);
        }
        self.waiting.is_empty()
    }

    /// A screen that is gone shows nothing any more.
    pub fn removed(&mut self, output: &O) -> bool {
        self.waiting.retain(|waiting| waiting != output);
        self.waiting.is_empty()
    }
}

/// Which program is the screen locker, shared with the filter that decides
/// who sees the lock global.
#[derive(Debug, Clone, Default)]
pub struct Lockers {
    program: Arc<Mutex<Option<PathBuf>>>,
}

impl Lockers {
    pub fn set(&self, program: Option<PathBuf>) {
        *self.program.lock().unwrap() = program;
    }

    pub fn program(&self) -> Option<PathBuf> {
        self.program.lock().unwrap().clone()
    }

    /// Whether `client` runs the locker program and is not sandboxed.
    ///
    /// This goes by the executable of the process that connected. It keeps
    /// out everything that is confined and everything that merely asks; a
    /// program of the same user that is free to start the locker and take
    /// over its connection is not stopped by it, nor by anything else here.
    pub fn recognises(&self, client: &Client) -> bool {
        // Nothing here may ask the display: this also runs while it is
        // deciding which globals a client sees, and would wait on itself.
        let Some(data) = client.get_data::<ClientState>() else {
            return false;
        };
        let (Some(pid), Some(program), false) = (data.pid, self.program(), data.sandboxed) else {
            return false;
        };
        std::fs::read_link(format!("/proc/{pid}/exe")).is_ok_and(|exe| exe == program)
    }
}

/// The first word of a locker command line as an absolute path, looked up
/// in `path` (the value of `PATH`) the way a shell would.
pub fn locker_program(command: &str, path: &str) -> Option<PathBuf> {
    let name = command.split_whitespace().next()?;
    let found = if name.contains('/') {
        PathBuf::from(name)
    } else {
        path.split(':')
            .filter(|dir| dir.starts_with('/'))
            .map(|dir| Path::new(dir).join(name))
            .find(|candidate| candidate.is_file())?
    };
    // The kernel reports a running program by its real path.
    found.canonicalize().ok()
}

/// How long to wait before starting a locker for a session whose locker
/// died, and between attempts if that one fails too.
const RELOCK_DELAY: Duration = Duration::from_secs(1);

impl State {
    fn lock_holder(&self) -> Holder {
        match &self.lock {
            None => Holder::Nobody,
            Some(lock) if lock.locker.is_alive() => Holder::Alive,
            Some(_) => Holder::Dead,
        }
    }

    /// Makes `command` (a program and its arguments, no shell syntax) the
    /// screen locker.
    pub fn set_locker(&mut self, command: &str) {
        let path = std::env::var("PATH").unwrap_or_default();
        let program = locker_program(command, &path);
        if program.is_none() {
            crate::say!("no locker: cannot find the program of `{command}`");
        }
        self.locker_command = command.to_string();
        self.lockers.set(program);
    }

    /// Starts the configured locker.
    pub fn spawn_locker(&mut self) {
        let Some(program) = self.lockers.program() else {
            return;
        };
        let arguments: Vec<String> = self
            .locker_command
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect();
        if let Some(pid) = self.spawn_program(&program, &arguments) {
            self.locker_pids.push(pid);
        }
    }

    /// A locked session whose locker died must not stay a blank screen
    /// with no way in: start our locker for it, and keep trying.
    pub fn watch_locker(&mut self) {
        let running = self.child_pids();
        self.locker_pids.retain(|pid| running.contains(pid));
        let orphaned = self.lock_holder() == Holder::Dead && self.locker_pids.is_empty();
        if !orphaned || self.relock_armed {
            return;
        }
        let timer = Timer::from_duration(RELOCK_DELAY);
        let armed = self.loop_handle.insert_source(timer, |_, _, state| {
            state.relock_armed = false;
            if state.lock_holder() == Holder::Dead && state.locker_pids.is_empty() {
                state.spawn_locker();
            }
            TimeoutAction::Drop
        });
        self.relock_armed = armed.is_ok();
    }

    pub fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Which lock a frame drawn now shows, if the session is locked.
    pub fn lock_epoch(&self) -> Option<u64> {
        self.lock.as_ref().map(|lock| lock.epoch)
    }

    /// The lock screen of `output`, once the locker has made one.
    pub fn lock_surface_on(&self, output: &Output) -> Option<WlSurface> {
        self.lock
            .as_ref()?
            .surfaces
            .iter()
            .find(|(on, surface)| on == output && surface.alive())
            .map(|(_, surface)| surface.wl_surface().clone())
    }

    /// Every lock screen there is.
    pub fn lock_surfaces(&self) -> Vec<WlSurface> {
        self.lock.as_ref().map_or_else(Vec::new, |lock| {
            lock.surfaces
                .iter()
                .filter(|(_, surface)| surface.alive())
                .map(|(_, surface)| surface.wl_surface().clone())
                .collect()
        })
    }

    /// The lock screen that gets the keyboard: the one the pointer is on,
    /// or else any.
    pub fn lock_focus(&self) -> Option<WlSurface> {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        self.output_at(pointer.current_location())
            .and_then(|output| self.lock_surface_on(&output))
            .or_else(|| self.lock_surfaces().into_iter().next())
    }

    /// One line on the lock for the SIGUSR1 report.
    pub fn lock_status(&self) -> String {
        match &self.lock {
            None => "session unlocked".into(),
            Some(lock) => format!(
                "session locked, locker {}, {} lock surface(s), locker told {}",
                if lock.locker.is_alive() {
                    "alive"
                } else {
                    "gone"
                },
                self.lock_surfaces().len(),
                if lock.pending.is_none() { "yes" } else { "no" },
            ),
        }
    }

    /// `output` showed a frame that was drawn under lock `epoch`, or under
    /// none. Once every screen has shown one drawn under the current lock,
    /// the locker is told that the session is covered.
    pub fn frame_presented(&mut self, output: &Output, epoch: Option<u64>) {
        let Some(lock) = &mut self.lock else { return };
        let locked_frame = epoch == Some(lock.epoch);
        let covered = lock
            .pending
            .as_mut()
            .is_some_and(|(_, covering)| covering.presented(output, locked_frame));
        if covered {
            if let Some((locker, _)) = lock.pending.take() {
                locker.lock();
            }
        }
    }

    /// An output went away: its lock screen goes with it, and the locker
    /// does not wait for it any longer.
    pub fn lock_output_removed(&mut self, output: &Output) {
        let Some(lock) = &mut self.lock else { return };
        lock.surfaces.retain(|(on, _)| on != output);
        let covered = lock
            .pending
            .as_mut()
            .is_some_and(|(_, covering)| covering.removed(output));
        if covered {
            if let Some((locker, _)) = lock.pending.take() {
                locker.lock();
            }
        }
    }

    /// Tells each lock screen the size of its output again, after outputs
    /// changed.
    pub fn configure_lock_surfaces(&mut self) {
        let Some(lock) = &self.lock else { return };
        for (output, surface) in &lock.surfaces {
            let Some(size) = self.space.output_geometry(output).map(|geo| geo.size) else {
                continue;
            };
            let size = (size.w.max(0) as u32, size.h.max(0) as u32).into();
            let changed = surface.with_pending_state(|state| {
                let changed = state.size != Some(size);
                state.size = Some(size);
                changed
            });
            if changed {
                surface.send_configure();
            }
        }
    }
}

impl SessionLockHandler for State {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock_state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Dropping the confirmation tells a refused client that it did not
        // get the lock.
        let recognised = confirmation
            .ext_session_lock()
            .client()
            .is_some_and(|client| self.lockers.recognises(&client));
        if !may_lock(self.lock_holder(), recognised) {
            return;
        }
        // Whatever the pointer or keyboard was in the middle of ends here.
        // The grab ends a resize properly while it still knows of it.
        let time = self.start_time.elapsed().as_millis() as u32;
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        pointer.unset_grab(self, SERIAL_COUNTER.next_serial(), time);
        keyboard.unset_grab(self);
        if let Some(crate::input::Drag::Resize { window, .. }) = self.drag.take() {
            window::finish_resize(&window);
        }
        self.hovered_button = None;
        self.frame_cursor = None;
        self.cycle = None;
        self.layer_focus = None;

        self.lock_count += 1;
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        self.lock = Some(Lock {
            surfaces: Vec::new(),
            locker: confirmation.ext_session_lock().clone(),
            epoch: self.lock_count,
            // From here on every frame shows the lock or nothing. The
            // client is told so once each screen has shown such a frame.
            pending: Some((confirmation, Covering::new(outputs))),
        });
        self.update_keyboard_focus();
        self.refocus_pointer();
        self.request_redraw();
    }

    fn unlock(&mut self) {
        self.lock = None;
        self.request_redraw();
        self.update_keyboard_focus();
        self.refocus_pointer();
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        // Smithay hands over lock surfaces made on any lock object. Only
        // the holder's count; see `request` below.
        if !std::mem::take(&mut self.lock_request_from_holder) {
            return;
        }
        let Some(output) = Output::from_resource(&wl_output) else {
            return;
        };
        let size = self
            .space
            .output_geometry(&output)
            .map(|geometry| geometry.size)
            .unwrap_or_default();
        surface.with_pending_state(|state| {
            state.size = Some((size.w.max(0) as u32, size.h.max(0) as u32).into());
        });
        surface.send_configure();

        if let Some(lock) = &mut self.lock {
            lock.surfaces.retain(|(on, _)| on != &output);
            lock.surfaces.push((output, surface));
        }
        self.request_redraw();
        self.update_keyboard_focus();
        self.refocus_pointer();
    }
}

/// Requests on a lock object. Smithay would let any lock object unlock
/// the session and supply lock surfaces, also one that was refused, so
/// only the holder's requests are passed on as they are.
impl Dispatch<ExtSessionLockV1, SessionLockState> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        data: &SessionLockState,
        display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let holder = state
            .lock
            .as_ref()
            .is_some_and(|lock| &lock.locker == resource);
        match request {
            ext_session_lock_v1::Request::UnlockAndDestroy if !holder => {
                resource.post_error(
                    ext_session_lock_v1::Error::InvalidUnlock,
                    "This lock object does not hold the session lock.",
                );
            }
            request => {
                // A lock surface from someone else still becomes an
                // object, so a locker that lost the race is not killed for
                // it, but `new_surface` drops it.
                state.lock_request_from_holder = holder;
                <SessionLockManagerState as Dispatch<ExtSessionLockV1, SessionLockState, Self>>::request(
                    state, client, resource, request, data, display, data_init,
                );
                state.lock_request_from_holder = false;
            }
        }
    }
}

delegate_global_dispatch!(State: [ExtSessionLockManagerV1: SessionLockManagerGlobalData] => SessionLockManagerState);
delegate_dispatch!(State: [ExtSessionLockManagerV1: ()] => SessionLockManagerState);
delegate_dispatch!(State: [ExtSessionLockSurfaceV1: ExtLockSurfaceUserData] => SessionLockManagerState);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_locker_program_may_lock() {
        assert!(may_lock(Holder::Nobody, true));
        assert!(!may_lock(Holder::Nobody, false));
    }

    #[test]
    fn a_live_locker_is_never_replaced() {
        assert!(!may_lock(Holder::Alive, false));
        assert!(!may_lock(Holder::Alive, true));
    }

    #[test]
    fn only_the_locker_program_takes_over_a_dead_one() {
        assert!(!may_lock(Holder::Dead, false));
        assert!(may_lock(Holder::Dead, true));
    }

    #[test]
    fn the_session_is_covered_when_every_screen_showed_a_locked_frame() {
        let mut covering = Covering::new(vec!["left", "right"]);
        // A frame that was already on its way when the lock began.
        assert!(!covering.presented(&"left", false));
        assert!(!covering.presented(&"left", true));
        // The same screen again does not stand in for the other.
        assert!(!covering.presented(&"left", true));
        assert!(!covering.presented(&"right", false));
        assert!(covering.presented(&"right", true));
    }

    #[test]
    fn a_screen_that_is_unplugged_is_not_waited_for() {
        let mut covering = Covering::new(vec!["left", "right"]);
        assert!(!covering.presented(&"left", true));
        assert!(covering.removed(&"right"));
        assert!(Covering::<&str>::new(Vec::new()).presented(&"left", false));
    }

    #[test]
    fn the_locker_is_found_by_absolute_path() {
        assert_eq!(locker_program("", "/usr/bin:/bin"), None);
        assert_eq!(locker_program("no-such-locker -f", "/usr/bin:/bin"), None);
        let shell = locker_program("sh -c true", "/nonexistent:/usr/bin:/bin");
        assert!(shell.is_some_and(|path| path.is_absolute()));
        // A relative directory in PATH is never searched.
        assert_eq!(locker_program("sh", ".:bin"), None);
        let direct = locker_program("/bin/sh -c true", "");
        assert!(direct.is_some_and(|path| path.is_absolute()));
    }
}
