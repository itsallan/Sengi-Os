//! The list of open windows, kept current through the
//! wlr-foreign-toplevel-management protocol, and the requests the dock makes
//! on them.
//!
//! The tracker runs on a thread and an event queue of its own, so it can sit
//! beside whatever drives the UI. It sleeps until the compositor has news.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle};
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
    /// runs on. The tracker then only handles the events routed to its queue.
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

/// The bookkeeping behind the tracker, apart from the Wayland plumbing.
#[derive(Default)]
struct Book {
    windows: Vec<Window>,
    next_id: u32,
    focus_counter: u64,
}

impl Book {
    fn open(&mut self) -> u32 {
        self.next_id += 1;
        self.windows.push(Window {
            id: self.next_id,
            ..Window::default()
        });
        self.next_id
    }

    fn get(&mut self, id: u32) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn set_state(&mut self, id: u32, flags: Flags) {
        let next = self.focus_counter + 1;
        let Some(window) = self.windows.iter_mut().find(|w| w.id == id) else {
            return;
        };
        if flags.activated && !window.activated {
            window.focus_seq = next;
            self.focus_counter = next;
        }
        window.activated = flags.activated;
        window.minimized = flags.minimized;
        window.maximized = flags.maximized;
    }

    fn close(&mut self, id: u32) {
        self.windows.retain(|w| w.id != id);
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
        self.with(id, |handle, shared| {
            if let Some(seat) = &shared.seat {
                handle.unset_minimized();
                handle.activate(seat);
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
    ids: HashMap<wayland_client::backend::ObjectId, u32>,
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
                if let Some(window) = state.book.get(id) {
                    window.app_id = app_id;
                }
            }
            handle::Event::Title { title } => {
                if let Some(window) = state.book.get(id) {
                    window.title = title;
                }
            }
            handle::Event::State { state: bytes } => {
                state.book.set_state(id, parse_state(&bytes));
            }
            // The compositor sends this after each batch of changes to one window.
            handle::Event::Done => state.dirty = true,
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

/// Handles events until the compositor goes away. With a shared connection
/// another thread does the reading, so this one parks until that thread
/// puts something on our queue.
fn run<F: FnMut(&[Window]) + 'static>(
    mut queue: EventQueue<Tracker<F>>,
    mut tracker: Tracker<F>,
    shared_connection: bool,
) {
    if !shared_connection {
        while !tracker.finished {
            if queue.blocking_dispatch(&mut tracker).is_err() {
                break;
            }
            tracker.notify();
        }
        return;
    }

    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    while !tracker.finished {
        match queue.poll_dispatch_pending(&mut context, &mut tracker) {
            Poll::Ready(_) => break,
            Poll::Pending => {
                tracker.notify();
                let _ = queue.flush();
                thread::park();
            }
        }
    }
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
        // wait forever. The thread binds the manager when the globals arrive.
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

    #[test]
    fn windows_get_increasing_ids() {
        let mut book = Book::default();
        let a = book.open();
        let b = book.open();
        book.close(a);
        let c = book.open();
        assert!(a < b && b < c);
        assert_eq!(
            book.windows.iter().map(|w| w.id).collect::<Vec<_>>(),
            [b, c]
        );
    }

    #[test]
    fn focus_order_is_remembered() {
        let mut book = Book::default();
        let a = book.open();
        let b = book.open();
        book.set_state(a, active());
        book.set_state(a, Flags::default());
        book.set_state(b, active());
        let seq = |book: &mut Book, id| book.get(id).unwrap().focus_seq;
        assert!(seq(&mut book, b) > seq(&mut book, a));
        assert!(seq(&mut book, a) > 0);

        // Staying focused through another state change is not a new focus.
        let before = seq(&mut book, b);
        book.set_state(
            b,
            Flags {
                activated: true,
                maximized: true,
                minimized: false,
            },
        );
        assert_eq!(seq(&mut book, b), before);
        assert!(book.get(b).unwrap().maximized);

        book.set_state(b, Flags::default());
        book.set_state(a, active());
        assert!(seq(&mut book, a) > seq(&mut book, b));
    }

    #[test]
    fn state_for_a_closed_window_is_ignored() {
        let mut book = Book::default();
        let a = book.open();
        book.close(a);
        book.set_state(a, active());
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
    }
}
