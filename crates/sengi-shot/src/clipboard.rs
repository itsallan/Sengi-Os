//! Puts a picture on the clipboard without a window of its own, through the
//! data-control protocol: ext-data-control where the compositor has it,
//! wlr-data-control where it has that, nothing else to install.
//!
//! The clipboard is only as good as the program that owns it, so after the
//! selection is taken the program goes on in the background, detached from
//! the shortcut that started it, and hands the picture to whoever pastes
//! until another program takes the clipboard over, or the time is up.

use std::any::Any;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
};
use wayland_client::{Connection, Dispatch, QueueHandle};

/// The kind of picture offered.
pub const MIME: &str = "image/png";

/// How long the picture is kept for pasting if nothing else is copied.
pub const KEEP_SECONDS: u32 = 3600;

/// How long one paste may take before it is given up on, so a program that
/// never reads cannot hold the clipboard hostage.
const PASTE_TIME: Duration = Duration::from_secs(10);

/// How long the shortcut waits to hear that the clipboard was taken.
const REPORT_TIME: Duration = Duration::from_secs(5);

/// The protocol to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Ext,
    Wlr,
}

pub const EXT_INTERFACE: &str = "ext_data_control_manager_v1";
pub const WLR_INTERFACE: &str = "zwlr_data_control_manager_v1";

/// The protocol to use, given the interfaces the compositor announces: the
/// ext one first.
pub fn pick<S: AsRef<str>>(interfaces: &[S]) -> Option<Protocol> {
    let has = |name: &str| interfaces.iter().any(|i| i.as_ref() == name);
    if has(EXT_INTERFACE) {
        Some(Protocol::Ext)
    } else if has(WLR_INTERFACE) {
        Some(Protocol::Wlr)
    } else {
        None
    }
}

/// The objects that make the clipboard ours, whichever protocol they are
/// of. They only have to stay alive.
pub struct Held(#[allow(dead_code)] Box<dyn Any>);

impl Held {
    fn new<T: Any>(objects: T) -> Held {
        Held(Box::new(objects))
    }
}

#[derive(Default)]
pub struct State {
    /// Descriptors of the programs that want the picture.
    requests: Vec<OwnedFd>,
    /// Another program has the clipboard now.
    cancelled: bool,
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

mod ext {
    use wayland_protocols::ext::data_control::v1::client::{
        ext_data_control_device_v1::{
            ExtDataControlDeviceV1 as Device, EVT_DATA_OFFER_OPCODE as DATA_OFFER_OPCODE,
        },
        ext_data_control_manager_v1::ExtDataControlManagerV1 as Manager,
        ext_data_control_offer_v1::ExtDataControlOfferV1 as Offer,
        ext_data_control_source_v1::{Event as SourceEvent, ExtDataControlSourceV1 as Source},
    };
    include!("flavour.rs");
}

mod wlr {
    use wayland_protocols_wlr::data_control::v1::client::{
        zwlr_data_control_device_v1::{
            ZwlrDataControlDeviceV1 as Device, EVT_DATA_OFFER_OPCODE as DATA_OFFER_OPCODE,
        },
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1 as Manager,
        zwlr_data_control_offer_v1::ZwlrDataControlOfferV1 as Offer,
        zwlr_data_control_source_v1::{Event as SourceEvent, ZwlrDataControlSourceV1 as Source},
    };
    include!("flavour.rs");
}

/// Writes all of `data` to `fd`, giving up once `timeout` has passed
/// without the other end taking all of it.
pub fn write_within(fd: OwnedFd, data: &[u8], timeout: Duration) -> std::io::Result<()> {
    let raw = fd.as_raw_fd();
    // SAFETY: fcntl on a descriptor this function owns.
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        if flags < 0 || libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let mut file = std::fs::File::from(fd);
    let deadline = Instant::now() + timeout;
    let mut done = 0;
    while done < data.len() {
        match file.write(&data[done..]) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => done += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(ErrorKind::TimedOut.into());
                }
                let _ = wait_for(raw, libc::POLLOUT, left);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Waits for `events` on `fd`, for at most a second and at most `limit`;
/// whether something happened.
fn wait_for(fd: RawFd, events: libc::c_short, limit: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let millis = limit.as_millis().clamp(1, 1000) as libc::c_int;
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(&mut poll, 1, millis) > 0 }
}

/// Takes the clipboard and serves `picture` from it. `ready` is told once
/// the clipboard is ours. Returns when the clipboard is someone else's or
/// the compositor is gone.
fn serve(picture: &[u8], ready: &mut dyn FnMut()) -> Result<(), String> {
    let connection = Connection::connect_to_env().map_err(|e| format!("no compositor: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&connection).map_err(|e| format!("no registry: {e}"))?;
    let qh = queue.handle();
    let interfaces: Vec<String> = globals
        .contents()
        .clone_list()
        .into_iter()
        .map(|global| global.interface)
        .collect();
    let protocol = pick(&interfaces)
        .ok_or("the compositor has no clipboard protocol for programs without a window")?;
    let seat: WlSeat = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| "the compositor has no seat")?;
    let mut state = State::default();
    let held = match protocol {
        Protocol::Ext => ext::offer(&globals, &mut queue, &mut state, &seat)?,
        Protocol::Wlr => wlr::offer(&globals, &mut queue, &mut state, &seat)?,
    };
    ready();
    loop {
        for fd in std::mem::take(&mut state.requests) {
            // A paste that fails is that program's loss, not ours.
            let _ = write_within(fd, picture, PASTE_TIME);
        }
        if state.cancelled {
            break;
        }
        queue
            .blocking_dispatch(&mut state)
            .map_err(|e| format!("the compositor went away: {e}"))?;
    }
    drop(held);
    Ok(())
}

/// Puts `picture` (a PNG) on the clipboard. The work is done by a child
/// process that stays behind in a session of its own after this returns;
/// an error says why the clipboard was not taken.
pub fn copy(picture: Vec<u8>) -> Result<(), String> {
    let mut ends = [0 as libc::c_int; 2];
    // SAFETY: pipe2 fills two descriptors.
    if unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(format!(
            "cannot make a pipe: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: fork in a program that has started no threads of its own.
    match unsafe { libc::fork() } {
        -1 => Err(format!("cannot fork: {}", std::io::Error::last_os_error())),
        0 => {
            // SAFETY: the child owns its copy of both ends.
            let (read, write) =
                unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
            drop(read);
            detach();
            let mut report = std::fs::File::from(write);
            let mut said = false;
            let result = serve(&picture, &mut || {
                said = true;
                let _ = report.write_all(b"ok");
                // Closing the pipe is what lets the shortcut go on.
                let _ = report.flush();
            });
            if let (false, Err(why)) = (said, &result) {
                let line = format!("error {}", why.replace('\n', " "));
                let _ = report.write_all(line.as_bytes());
            }
            drop(report);
            // SAFETY: leave without running the parent's exit handlers.
            unsafe { libc::_exit(i32::from(result.is_err())) }
        }
        _child => {
            // SAFETY: the parent owns its copy of both ends.
            let (read, write) =
                unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
            drop(write);
            hear(read)
        }
    }
}

/// The child's part of leaving: a session of its own, no terminal or
/// pipes shared with the shortcut, and a limit on how long it lives.
fn detach() {
    // SAFETY: plain system calls on descriptors this process owns.
    unsafe {
        libc::setsid();
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if null >= 0 {
            for fd in 0..=2 {
                libc::dup2(null, fd);
            }
            if null > 2 {
                libc::close(null);
            }
        }
        // Replaces the time limit the shortcut's own copy had.
        libc::alarm(KEEP_SECONDS);
    }
}

/// What the child says: "ok" once the clipboard is ours, "error WHY"
/// otherwise, or nothing before the pipe closes.
fn hear(report: OwnedFd) -> Result<(), String> {
    let raw = report.as_raw_fd();
    let mut file = std::fs::File::from(report);
    let deadline = Instant::now() + REPORT_TIME;
    let mut text = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the clipboard was not taken in time".to_string());
        }
        if !wait_for(raw, libc::POLLIN, left) {
            continue;
        }
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => text.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("lost the clipboard process: {e}")),
        }
    }
    parse_report(&String::from_utf8_lossy(&text))
}

/// The child's report as a result.
pub fn parse_report(text: &str) -> Result<(), String> {
    match text.trim() {
        "ok" => Ok(()),
        "" => Err("the clipboard process ended without a word".to_string()),
        other => Err(other.strip_prefix("error ").unwrap_or(other).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe() -> (OwnedFd, OwnedFd) {
        let mut ends = [0 as libc::c_int; 2];
        // SAFETY: pipe2 fills two descriptors, which are then owned.
        unsafe {
            assert_eq!(libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC), 0);
            (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1]))
        }
    }

    #[test]
    fn ext_is_preferred_to_wlr() {
        assert_eq!(
            pick(&["wl_seat", WLR_INTERFACE, EXT_INTERFACE]),
            Some(Protocol::Ext)
        );
        assert_eq!(pick(&["wl_seat", WLR_INTERFACE]), Some(Protocol::Wlr));
        assert_eq!(pick(&["wl_seat", EXT_INTERFACE]), Some(Protocol::Ext));
        assert_eq!(pick(&["wl_seat", "wl_shm"]), None);
        assert_eq!(pick::<&str>(&[]), None);
    }

    #[test]
    fn a_big_picture_goes_through_a_reader() {
        let (read, write) = pipe();
        let data: Vec<u8> = (0..1_000_000u32).map(|n| (n % 251) as u8).collect();
        let expected = data.clone();
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            std::fs::File::from(read).read_to_end(&mut out).unwrap();
            out
        });
        write_within(write, &data, Duration::from_secs(10)).unwrap();
        assert_eq!(reader.join().unwrap(), expected);
    }

    #[test]
    fn a_reader_that_never_reads_is_given_up_on() {
        let (read, write) = pipe();
        let started = Instant::now();
        let outcome = write_within(write, &vec![7u8; 1 << 20], Duration::from_millis(300));
        assert_eq!(outcome.unwrap_err().kind(), ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(read);
    }

    #[test]
    fn a_reader_that_left_is_an_error_not_a_crash() {
        let (read, write) = pipe();
        drop(read);
        // SAFETY: ignoring a signal, as the real program's runtime does.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
        assert!(write_within(write, b"png", Duration::from_secs(1)).is_err());
    }

    #[test]
    fn what_the_child_says_is_understood() {
        assert_eq!(parse_report("ok"), Ok(()));
        assert_eq!(
            parse_report("error no compositor"),
            Err("no compositor".into())
        );
        assert!(parse_report("").is_err());
    }
}
