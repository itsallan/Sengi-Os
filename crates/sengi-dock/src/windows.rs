//! The list of open windows, kept current through the
//! wlr-foreign-toplevel-management protocol, and the requests the dock makes
//! on them.
//!
//! The tracker runs on a thread and an event queue of its own, so it can sit
//! beside whatever drives the UI. It sleeps until the compositor has news.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use sengi_layer::wayland_client::backend::ObjectId;
use sengi_layer::wayland_client::protocol::wl_callback::{self, WlCallback};
use sengi_layer::wayland_client::protocol::wl_registry::{self, WlRegistry};
use sengi_layer::wayland_client::protocol::wl_seat::WlSeat;
use sengi_layer::wayland_client::{
    event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self as handle, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self as manager, ZwlrForeignToplevelManagerV1,
};

/// One open window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    /// Ours, not the compositor's. Later windows get larger numbers.
    pub id: u32,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub minimized: bool,
    pub maximized: bool,
    /// Grows each time a window gains focus, so the largest value marks the
    /// window used most recently. Zero until the window is first focused.
    pub focus_seq: u64,
    /// The window this one belongs to, for a dialog or a file chooser.
    pub parent: Option<u32>,
}

/// True when two window lists differ in something the dock shows. Titles are
/// left out: terminals retitle themselves constantly and the dock does not
/// display them.
pub fn differs_for_dock(a: &[Window], b: &[Window]) -> bool {
    a.len() != b.len()
        || a.iter().zip(b).any(|(a, b)| {
            a.id != b.id
                || a.app_id != b.app_id
                || a.activated != b.activated
                || a.minimized != b.minimized
                || a.focus_seq != b.focus_seq
                || a.parent != b.parent
        })
}

#[derive(Debug)]
pub enum Error {
    Connect(String),
    /// The compositor does not offer the protocol.
    Unsupported,
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "cannot reach the compositor: {e}"),
            Self::Unsupported => write!(
                f,
                "the compositor does not offer wlr-foreign-toplevel-management"
            ),
            Self::Protocol(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Where the tracker gets its events from.
pub enum Source {
    /// Open a connection of our own and read it here.
    Own,
    /// Use a connection that somebody else reads, such as the one the UI
    /// runs on. The tracker then only handles the events routed to its
    /// queue. It must not read the socket itself: `blocking_dispatch` on a
    /// second thread can go to sleep on events the first thread has already
    /// read and queued for it.
    Shared(Connection),
}

/// The window state flags the protocol packs into an array of 32-bit values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Flags {
    activated: bool,
    minimized: bool,
    maximized: bool,
}

fn parse_state(bytes: &[u8]) -> Flags {
    let mut flags = Flags::default();
    for chunk in bytes.as_chunks::<4>().0 {
        let value = u32::from_ne_bytes(*chunk);
        if value == handle::State::Activated as u32 {
            flags.activated = true;
        } else if value == handle::State::Minimized as u32 {
            flags.minimized = true;
        } else if value == handle::State::Maximized as u32 {
            flags.maximized = true;
        }
    }
    flags
}

/// What the compositor has said about a window since its last `done`.
#[derive(Default)]
struct Pending {
    app_id: Option<String>,
    title: Option<String>,
    flags: Option<Flags>,
    /// `Some(None)` says the window has stopped having a parent.
    parent: Option<Option<u32>>,
}

/// The bookkeeping behind the tracker, apart from the Wayland plumbing.
///
/// The protocol sends a window's properties one by one and then `done`.
/// Changes are held back until then, and a window is only listed after its
/// first `done`, so nobody sees a window that has no app_id yet.
#[derive(Default)]
struct Book {
    /// The windows as published, oldest first.
    windows: Vec<Window>,
    pending: HashMap<u32, Pending>,
    next_id: u32,
    focus_counter: u64,
}

impl Book {
    fn open(&mut self) -> u32 {
        self.next_id += 1;
        self.pending.insert(self.next_id, Pending::default());
        self.next_id
    }

    /// The changes waiting for `done`, if the window is still open.
    fn pending(&mut self, id: u32) -> Option<&mut Pending> {
        self.pending.get_mut(&id)
    }

    /// Applies what has arrived since the last `done`.
    fn commit(&mut self, id: u32) {
        let Some(pending) = self.pending.get_mut(&id).map(std::mem::take) else {
            return;
        };
        let at = match self.windows.iter().position(|w| w.id == id) {
            Some(at) => at,
            None => {
                let at = self.windows.partition_point(|w| w.id < id);
                self.windows.insert(
                    at,
                    Window {
                        id,
                        ..Window::default()
                    },
                );
                at
            }
        };
        let window = &mut self.windows[at];
        if let Some(app_id) = pending.app_id {
            window.app_id = app_id;
        }
        if let Some(title) = pending.title {
            window.title = title;
        }
        if let Some(flags) = pending.flags {
            if flags.activated && !window.activated {
                self.focus_counter += 1;
                window.focus_seq = self.focus_counter;
            }
            window.activated = flags.activated;
            window.minimized = flags.minimized;
            window.maximized = flags.maximized;
        }
        if let Some(parent) = pending.parent {
            window.parent = parent.filter(|&parent| parent != id);
        }
    }

    fn close(&mut self, id: u32) {
        self.pending.remove(&id);
        self.windows.retain(|w| w.id != id);
        // A dialog that outlives its window stands on its own.
        for window in &mut self.windows {
            if window.parent == Some(id) {
                window.parent = None;
            }
        }
    }
}

#[derive(Default)]
struct Shared {
    handles: HashMap<u32, ZwlrForeignToplevelHandleV1>,
    seat: Option<WlSeat>,
}

/// Makes requests on tracked windows. Cheap to clone, usable from any thread.
#[derive(Clone)]
pub struct Controller {
    connection: Connection,
    shared: Arc<Mutex<Shared>>,
}

impl Controller {
    fn with(&self, id: u32, request: impl FnOnce(&ZwlrForeignToplevelHandleV1, &Shared)) {
        {
            let shared = self.shared.lock().unwrap();
            // The window may have closed since the dock was last drawn.
            let Some(handle) = shared.handles.get(&id) else {
                return;
            };
            request(handle, &shared);
        }
        let _ = self.connection.flush();
    }

    /// Focuses the window, bringing it back first if it was minimised.
    pub fn activate(&self, id: u32) {
        self.with(id, |handle, shared| match &shared.seat {
            Some(seat) => {
                handle.unset_minimized();
                handle.activate(seat);
            }
            None => {
                // Focusing a window needs a seat to focus it for.
                static WARNED: AtomicBool = AtomicBool::new(false);
                if !WARNED.swap(true, Ordering::Relaxed) {
                    eprintln!("sengi-dock: cannot focus windows: the compositor offers no seat");
                }
            }
        });
    }

    pub fn set_minimized(&self, id: u32, minimized: bool) {
        self.with(id, |handle, _| {
            if minimized {
                handle.set_minimized();
            } else {
                handle.unset_minimized();
            }
        });
    }

    pub fn close(&self, id: u32) {
        self.with(id, |handle, _| handle.close());
    }
}

struct Tracker<F> {
    book: Book,
    ids: HashMap<ObjectId, u32>,
    shared: Arc<Mutex<Shared>>,
    on_change: F,
    manager_found: bool,
    finished: bool,
    /// Set by events, cleared when the listener has been told.
    dirty: bool,
}

impl<F: FnMut(&[Window])> Tracker<F> {
    fn id_of(&self, toplevel: &ZwlrForeignToplevelHandleV1) -> Option<u32> {
        self.ids.get(&toplevel.id()).copied()
    }

    fn notify(&mut self) {
        if self.dirty {
            self.dirty = false;
            (self.on_change)(&self.book.windows);
        }
    }

    /// Forgets every window and tells the listener once.
    fn stop(&mut self) {
        self.book = Book::default();
        self.ids.clear();
        self.shared.lock().unwrap().handles.clear();
        self.dirty = true;
        self.notify();
    }
}

impl<F: FnMut(&[Window]) + 'static> Dispatch<WlRegistry, ()> for Tracker<F> {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        if interface == ZwlrForeignToplevelManagerV1::interface().name && !state.manager_found {
            registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(name, version.min(3), queue, ());
            state.manager_found = true;
        } else if interface == WlSeat::interface().name {
            let mut shared = state.shared.lock().unwrap();
            if shared.seat.is_none() {
                shared.seat = Some(registry.bind::<WlSeat, _, _>(name, 1, queue, ()));
            }
        }
    }
}

/// Answers the sync sent after asking for the globals: by now every global
/// has been announced, so a missing manager is missing for good.
impl<F: FnMut(&[Window]) + 'static> Dispatch<WlCallback, ()> for Tracker<F> {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) && !state.manager_found {
            eprintln!(
                "sengi-dock: windows are not tracked: {}",
                Error::Unsupported
            );
        }
    }
}

impl<F: FnMut(&[Window]) + 'static> Dispatch<WlSeat, ()> for Tracker<F> {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl<F: FnMut(&[Window]) + 'static> Dispatch<ZwlrForeignToplevelManagerV1, ()> for Tracker<F> {
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
                let id = state.book.open();
                state.ids.insert(toplevel.id(), id);
                state.shared.lock().unwrap().handles.insert(id, toplevel);
            }
            manager::Event::Finished => state.finished = true,
            _ => {}
        }
    }

    event_created_child!(Tracker<F>, ZwlrForeignToplevelManagerV1, [
        manager::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl<F: FnMut(&[Window]) + 'static> Dispatch<ZwlrForeignToplevelHandleV1, ()> for Tracker<F> {
    fn event(
        state: &mut Self,
        toplevel: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(id) = state.id_of(toplevel) else {
            return;
        };
        match event {
            handle::Event::AppId { app_id } => {
                if let Some(pending) = state.book.pending(id) {
                    pending.app_id = Some(app_id);
                }
            }
            handle::Event::Title { title } => {
                if let Some(pending) = state.book.pending(id) {
                    pending.title = Some(title);
                }
            }
            handle::Event::State { state: bytes } => {
                if let Some(pending) = state.book.pending(id) {
                    pending.flags = Some(parse_state(&bytes));
                }
            }
            handle::Event::Parent { parent } => {
                let parent = parent.and_then(|parent| state.id_of(&parent));
                if let Some(pending) = state.book.pending(id) {
                    pending.parent = Some(parent);
                }
            }
            // The compositor sends this after each batch of changes to one window.
            handle::Event::Done => {
                state.book.commit(id);
                state.dirty = true;
            }
            handle::Event::Closed => {
                state.book.close(id);
                state.ids.remove(&toplevel.id());
                state.shared.lock().unwrap().handles.remove(&id);
                toplevel.destroy();
                state.dirty = true;
            }
            _ => {}
        }
    }
}

struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Handles events until the compositor stops sending them, and says why
/// it stopped. With a shared connection another thread does the reading, so
/// this one parks until that thread puts something on our queue.
fn dispatch<F: FnMut(&[Window]) + 'static>(
    queue: &mut EventQueue<Tracker<F>>,
    tracker: &mut Tracker<F>,
    shared_connection: bool,
) -> String {
    const FINISHED: &str = "the compositor stopped reporting windows";
    if !shared_connection {
        while !tracker.finished {
            if let Err(error) = queue.blocking_dispatch(tracker) {
                return error.to_string();
            }
            tracker.notify();
        }
        return FINISHED.to_string();
    }

    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    while !tracker.finished {
        match queue.poll_dispatch_pending(&mut context, tracker) {
            Poll::Ready(Err(error)) => return error.to_string(),
            Poll::Pending => {
                tracker.notify();
                let _ = queue.flush();
                thread::park();
            }
        }
    }
    FINISHED.to_string()
}

/// The tracker's thread. When tracking ends, for whatever reason, the
/// listener is told there are no windows, so nothing stays marked as
/// running on the strength of a list that will never be updated again.
fn run<F: FnMut(&[Window]) + 'static>(
    mut queue: EventQueue<Tracker<F>>,
    mut tracker: Tracker<F>,
    shared_connection: bool,
) {
    let reason = dispatch(&mut queue, &mut tracker, shared_connection);
    eprintln!("sengi-dock: windows are no longer tracked: {reason}");
    tracker.stop();
}

/// Starts tracking windows. `on_change` is called on the tracker's thread
/// with the full list, oldest window first, once at the start and then after
/// every change; hand the list on to the UI thread from there.
pub fn spawn<F>(source: Source, on_change: F) -> Result<Controller, Error>
where
    F: FnMut(&[Window]) + Send + 'static,
{
    let (connection, shared_connection) = match source {
        Source::Own => (
            Connection::connect_to_env().map_err(|e| Error::Connect(e.to_string()))?,
            false,
        ),
        Source::Shared(connection) => (connection, true),
    };
    let shared = Arc::new(Mutex::new(Shared::default()));
    let mut queue = connection.new_event_queue();
    connection.display().get_registry(&queue.handle(), ());

    let mut tracker = Tracker {
        book: Book::default(),
        ids: HashMap::new(),
        shared: shared.clone(),
        on_change,
        manager_found: false,
        finished: false,
        dirty: true,
    };

    if shared_connection {
        // Somebody else reads the socket, so a blocking roundtrip here could
        // wait forever. The thread binds the manager when the globals arrive,
        // and the sync's answer tells it when they all have.
        connection.display().sync(&queue.handle(), ());
        connection
            .flush()
            .map_err(|e| Error::Protocol(e.to_string()))?;
    } else {
        // One roundtrip for the globals and one for the windows already open.
        for _ in 0..2 {
            queue
                .roundtrip(&mut tracker)
                .map_err(|e| Error::Protocol(e.to_string()))?;
        }
        if !tracker.manager_found {
            return Err(Error::Unsupported);
        }
        tracker.notify();
    }

    thread::Builder::new()
        .name("windows".into())
        // Event handling is shallow, so a small stack is plenty.
        .stack_size(128 * 1024)
        .spawn(move || run(queue, tracker, shared_connection))
        .map_err(|e| Error::Protocol(e.to_string()))?;

    Ok(Controller { connection, shared })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_bytes(states: &[handle::State]) -> Vec<u8> {
        states
            .iter()
            .flat_map(|s| (*s as u32).to_ne_bytes())
            .collect()
    }

    #[test]
    fn state_arrays_are_decoded() {
        assert_eq!(parse_state(&[]), Flags::default());
        assert_eq!(
            parse_state(&state_bytes(&[handle::State::Activated])),
            Flags {
                activated: true,
                ..Flags::default()
            }
        );
        assert_eq!(
            parse_state(&state_bytes(&[
                handle::State::Maximized,
                handle::State::Minimized,
                handle::State::Fullscreen,
            ])),
            Flags {
                activated: false,
                minimized: true,
                maximized: true,
            }
        );
        // A truncated value at the end is ignored.
        let mut bytes = state_bytes(&[handle::State::Activated]);
        bytes.extend([2, 0]);
        assert!(parse_state(&bytes).activated);
        assert!(!parse_state(&bytes).minimized);
    }

    fn active() -> Flags {
        Flags {
            activated: true,
            ..Flags::default()
        }
    }

    fn set_state(book: &mut Book, id: u32, flags: Flags) {
        if let Some(pending) = book.pending(id) {
            pending.flags = Some(flags);
        }
        book.commit(id);
    }

    fn get(book: &Book, id: u32) -> &Window {
        book.windows.iter().find(|w| w.id == id).unwrap()
    }

    #[test]
    fn windows_get_increasing_ids_and_stay_in_that_order() {
        let mut book = Book::default();
        let a = book.open();
        let b = book.open();
        let c = book.open();
        assert!(a < b && b < c);
        // The compositor may finish describing them in any order.
        book.commit(c);
        book.commit(a);
        book.commit(b);
        book.close(a);
        assert_eq!(
            book.windows.iter().map(|w| w.id).collect::<Vec<_>>(),
            [b, c]
        );
    }

    #[test]
    fn a_window_is_not_listed_until_its_first_done() {
        let mut book = Book::default();
        let a = book.open();
        book.pending(a).unwrap().title = Some("~".into());
        assert!(book.windows.is_empty());
        book.pending(a).unwrap().app_id = Some("foot".into());
        book.pending(a).unwrap().flags = Some(active());
        assert!(book.windows.is_empty());

        book.commit(a);
        assert_eq!(book.windows.len(), 1);
        assert_eq!(get(&book, a).app_id, "foot");
        assert_eq!(get(&book, a).title, "~");
        assert!(get(&book, a).activated);
    }

    #[test]
    fn changes_wait_for_done_and_leave_other_fields_alone() {
        let mut book = Book::default();
        let a = book.open();
        book.pending(a).unwrap().app_id = Some("foot".into());
        book.pending(a).unwrap().title = Some("~".into());
        book.commit(a);

        book.pending(a).unwrap().title = Some("vim".into());
        assert_eq!(get(&book, a).title, "~");
        book.commit(a);
        assert_eq!(get(&book, a).title, "vim");
        assert_eq!(get(&book, a).app_id, "foot");

        // A done with nothing before it changes nothing.
        let before = book.windows.clone();
        book.commit(a);
        assert_eq!(book.windows, before);
    }

    #[test]
    fn when_tracking_stops_the_listener_is_told_there_are_no_windows() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let mut tracker = Tracker {
            book: Book::default(),
            ids: HashMap::new(),
            shared: Arc::new(Mutex::new(Shared::default())),
            on_change: move |list: &[Window]| log.lock().unwrap().push(list.len()),
            manager_found: true,
            finished: false,
            dirty: false,
        };
        let a = tracker.book.open();
        tracker.book.commit(a);
        tracker.dirty = true;
        tracker.notify();
        // Nothing new, nothing said.
        tracker.notify();

        tracker.stop();
        assert_eq!(*seen.lock().unwrap(), [1, 0]);
        assert!(tracker.book.windows.is_empty());
    }

    #[test]
    fn a_dialog_knows_its_parent_until_the_parent_closes() {
        let mut book = Book::default();
        let main = book.open();
        let dialog = book.open();
        book.commit(main);
        book.pending(dialog).unwrap().parent = Some(Some(main));
        book.commit(dialog);
        assert_eq!(get(&book, dialog).parent, Some(main));
        assert_eq!(get(&book, main).parent, None);

        // Unrelated changes leave the parent alone.
        book.pending(dialog).unwrap().title = Some("Open".into());
        book.commit(dialog);
        assert_eq!(get(&book, dialog).parent, Some(main));

        book.close(main);
        assert_eq!(get(&book, dialog).parent, None);

        // The compositor can also take the parent away.
        let other = book.open();
        book.commit(other);
        book.pending(dialog).unwrap().parent = Some(Some(other));
        book.commit(dialog);
        book.pending(dialog).unwrap().parent = Some(None);
        book.commit(dialog);
        assert_eq!(get(&book, dialog).parent, None);
    }

    #[test]
    fn focus_order_is_remembered() {
        let mut book = Book::default();
        let a = book.open();
        let b = book.open();
        set_state(&mut book, a, active());
        set_state(&mut book, a, Flags::default());
        set_state(&mut book, b, active());
        let seq = |book: &Book, id| get(book, id).focus_seq;
        assert!(seq(&book, b) > seq(&book, a));
        assert!(seq(&book, a) > 0);

        // Staying focused through another state change is not a new focus.
        let before = seq(&book, b);
        set_state(
            &mut book,
            b,
            Flags {
                activated: true,
                maximized: true,
                minimized: false,
            },
        );
        assert_eq!(seq(&book, b), before);
        assert!(get(&book, b).maximized);

        set_state(&mut book, b, Flags::default());
        set_state(&mut book, a, active());
        assert!(seq(&book, a) > seq(&book, b));
    }

    #[test]
    fn events_for_a_closed_window_are_ignored() {
        let mut book = Book::default();
        let a = book.open();
        book.commit(a);
        book.close(a);
        set_state(&mut book, a, active());
        assert!(book.windows.is_empty());
        assert_eq!(book.focus_counter, 0);
    }

    #[test]
    fn title_changes_do_not_count_for_the_dock() {
        let a = Window {
            id: 1,
            app_id: "foot".into(),
            title: "~".into(),
            ..Window::default()
        };
        let mut b = a.clone();
        b.title = "vim".into();
        let differ = |a: &Window, b: &Window| {
            differs_for_dock(std::slice::from_ref(a), std::slice::from_ref(b))
        };
        assert!(!differ(&a, &b));
        b.maximized = true;
        assert!(!differ(&a, &b));
        b.activated = true;
        assert!(differ(&a, &b));
        assert!(differs_for_dock(std::slice::from_ref(&a), &[]));
        b.activated = false;
        b.app_id = "other".into();
        assert!(differ(&a, &b));
        b.app_id = a.app_id.clone();
        b.parent = Some(7);
        assert!(differ(&a, &b));
    }
}
