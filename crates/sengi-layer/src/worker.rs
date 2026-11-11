//! Running a second event queue on the app's Wayland connection.

use std::io::ErrorKind;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use wayland_client::backend::WaylandError;
use wayland_client::{DispatchError, EventQueue};

/// Wakes a parked thread when its queue has something to dispatch.
struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// Dispatches an event queue of your own on the calling thread, for as long
/// as you want, without ever reading the Wayland socket.
///
/// This is how to use the connection from [`Handle::connection`] for a
/// protocol sengi-layer does not speak. Make a queue with
/// `connection.new_event_queue()`, create your objects on it, then call
/// this on a thread of your own:
///
/// ```no_run
/// # use std::ops::ControlFlow;
/// # use sengi_layer::wayland_client::{protocol::wl_registry, Connection, Dispatch, QueueHandle};
/// struct Windows { changed: bool }
/// # impl Dispatch<wl_registry::WlRegistry, ()> for Windows {
/// #     fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &(),
/// #         _: &Connection, _: &QueueHandle<Self>) {}
/// # }
///
/// fn track(connection: Connection, updates: sengi_layer::Sender<()>) {
///     let mut queue = connection.new_event_queue::<Windows>();
///     connection.display().get_registry(&queue.handle(), ());
///     let mut windows = Windows { changed: false };
///     let result = sengi_layer::dispatch_queue(&mut queue, &mut windows, |windows| {
///         // Everything that has arrived so far has been handled.
///         if std::mem::take(&mut windows.changed) && updates.send(()).is_err() {
///             return ControlFlow::Break(()); // the UI loop has ended
///         }
///         ControlFlow::Continue(())
///     });
///     if let Err(error) = result {
///         eprintln!("lost the compositor: {error}");
///     }
/// }
/// ```
///
/// Each round it handles every event waiting on `queue`, calls `drained`,
/// sends the requests made so far, and sleeps until the UI loop has put more
/// events on the queue. It returns `Ok` when `drained` breaks and an error
/// when the connection fails.
///
/// `drained` is the place to pass what was learned on to the UI, through
/// [`Handle::channel`], and to decide whether to go on. It can also be
/// called with nothing new, so make it cheap. To stop the worker from
/// another thread, set a flag that `drained` checks and `unpark` the
/// worker's thread.
///
/// # Why not `blocking_dispatch`
///
/// The UI loop reads the socket for the whole connection and files each
/// event on the queue it belongs to. `EventQueue::blocking_dispatch` reads
/// the socket too: it handles what is queued, then waits for the socket to
/// become readable. If the UI loop reads in between, the worker's event is
/// already on its queue and the worker waits on a socket with nothing left
/// on it. It stays asleep until something unrelated arrives, which for an
/// idle dock can be until the pointer next moves. The stall is real and
/// easy to produce; `examples/soak.rs` shows it with `--blocking-dispatch`.
///
/// This function never touches the socket. It waits for the queue itself,
/// so an event the UI loop has filed wakes it however the two threads
/// interleave. It follows that the worker only makes progress while the UI
/// loop runs: before [`App::run`] and after it returns nothing arrives.
///
/// [`Handle::connection`]: crate::Handle::connection
/// [`Handle::channel`]: crate::Handle::channel
/// [`App::run`]: crate::App::run
pub fn dispatch_queue<State>(
    queue: &mut EventQueue<State>,
    state: &mut State,
    mut drained: impl FnMut(&mut State) -> ControlFlow<()>,
) -> Result<(), DispatchError> {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        // Ready only ever carries an error. Pending means the queue is
        // empty and the waker is registered for the next event filed on it.
        if let Poll::Ready(Err(error)) = queue.poll_dispatch_pending(&mut context, state) {
            return Err(error);
        }
        if drained(state).is_break() {
            return Ok(());
        }
        match queue.flush() {
            Ok(()) => {}
            // A full socket is flushed again by the UI loop.
            Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => return Err(DispatchError::Backend(error)),
        }
        // An event filed since the poll has already unparked this thread,
        // and then this returns at once.
        thread::park();
    }
}
