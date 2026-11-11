//! Exercises the window tracker against a real compositor.
//!
//!     track [--shared] [activate|minimize|restore|close]
//!
//! Prints the window list whenever it changes. With a request, makes it on
//! the oldest window one second after starting. `--shared` runs the tracker
//! on a connection that this thread reads, the way it will sit beside the
//! UI's connection in the dock.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use sengi_dock::windows::{self, Source, Window};
use wayland_client::Connection;

struct Idle;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let shared = args.iter().any(|a| a == "--shared");
    let request = args.iter().find(|a| !a.starts_with("--")).cloned();

    let (sender, receiver) = mpsc::channel::<Vec<Window>>();
    let on_change = move |list: &[Window]| {
        let _ = sender.send(list.to_vec());
    };

    let controller = if shared {
        let connection = Connection::connect_to_env().expect("a compositor");
        let controller =
            windows::spawn(Source::Shared(connection.clone()), on_change).expect("the tracker");
        // Stand in for the UI's event loop: read the socket, own no objects.
        thread::spawn(move || {
            let mut queue = connection.new_event_queue::<Idle>();
            loop {
                queue.blocking_dispatch(&mut Idle).expect("the connection");
            }
        });
        controller
    } else {
        windows::spawn(Source::Own, on_change).expect("the tracker")
    };

    let mut oldest = None;
    let mut asked = request.is_none();
    loop {
        match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(list) => {
                println!("{} window(s)", list.len());
                for w in &list {
                    println!(
                        "  #{} {:?} {:?} activated={} minimized={} maximized={} focus_seq={}",
                        w.id, w.app_id, w.title, w.activated, w.minimized, w.maximized, w.focus_seq
                    );
                }
                oldest = list.first().map(|w| w.id);
            }
            Err(mpsc::RecvTimeoutError::Timeout) if !asked => {
                asked = true;
                let Some(id) = oldest else { continue };
                println!("requesting {} on #{id}", request.as_deref().unwrap_or(""));
                match request.as_deref() {
                    Some("activate") => controller.activate(id),
                    Some("minimize") => controller.set_minimized(id, true),
                    Some("restore") => controller.set_minimized(id, false),
                    Some("close") => controller.close(id),
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}
