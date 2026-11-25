//! Tells Files when a drive is plugged in, taken out, mounted or unmounted.
//! One thread sleeps in `poll` on two descriptors, so nothing runs between
//! events: the kernel's uevent socket (block devices appearing and going),
//! and `/proc/self/mountinfo`, which the kernel flags whenever a mount
//! changes. After an event it waits until things are quiet, because udev
//! needs a moment to learn what is on a new drive, and tells once more a
//! little later in case it was slow.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::thread;
use std::time::{Duration, Instant};

/// Quiet time after a block event before the first telling, and the second
/// telling after the last event.
const SETTLE: Duration = Duration::from_millis(600);
const AGAIN: Duration = Duration::from_millis(2500);
/// Mounts are known at once; a short wait lets a burst count as one.
const MOUNT_SETTLE: Duration = Duration::from_millis(150);

/// Disks that come and go all the time and are never a drive to open.
const NOISE: &[&str] = &["loop", "ram", "zram", "dm-", "md", "nbd"];

/// The name of the block device a kernel uevent is about, if it is one that
/// could be a drive. A message is "action@path", a NUL, then KEY=VALUE
/// pairs each ended by a NUL.
pub fn block_event(message: &[u8]) -> Option<String> {
    let mut parts = message.split(|&byte| byte == 0);
    let head = parts.next()?;
    if !head.contains(&b'@') {
        return None;
    }
    let mut block = false;
    let mut name = None;
    for pair in parts {
        if pair == b"SUBSYSTEM=block" {
            block = true;
        } else if let Some(value) = pair.strip_prefix(b"DEVNAME=") {
            name = Some(String::from_utf8_lossy(value).into_owned());
        }
    }
    let name = name.filter(|_| block)?;
    let short = name.rsplit('/').next().unwrap_or(&name).to_string();
    if NOISE.iter().any(|kind| short.starts_with(kind)) {
        return None;
    }
    Some(short)
}

fn netlink() -> io::Result<OwnedFd> {
    // SAFETY: plain system calls; the descriptor is owned right away.
    unsafe {
        let raw = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_KOBJECT_UEVENT,
        );
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = OwnedFd::from_raw_fd(raw);
        let mut address: libc::sockaddr_nl = std::mem::zeroed();
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // Group 1: what the kernel says, to anyone.
        address.nl_groups = 1;
        let bound = libc::bind(
            fd.as_raw_fd(),
            (&raw const address).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        );
        if bound < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd)
    }
}

/// Reads what is waiting. True if any of it was a block event from the
/// kernel (and not from another program that can write to the socket).
fn drain(fd: &OwnedFd) -> bool {
    let mut seen = false;
    let mut buffer = vec![0u8; 8192];
    loop {
        // SAFETY: reads into a buffer of the length given; the address is a
        // struct the call fills in.
        let (count, from) = unsafe {
            let mut address: libc::sockaddr_nl = std::mem::zeroed();
            let mut length = std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
            let count = libc::recvfrom(
                fd.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
                (&raw mut address).cast::<libc::sockaddr>(),
                &mut length,
            );
            (count, address.nl_pid)
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return seen;
        }
        // Only the kernel is process 0.
        if from == 0 && block_event(&buffer[..count as usize]).is_some() {
            seen = true;
        }
    }
}

/// Reads the whole of mountinfo again, which is what lets the kernel flag
/// the next change.
fn rearm(file: &mut File) {
    let mut sink = Vec::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.read_to_end(&mut sink);
}

/// Starts the thread. `changed` is called from it, after each burst of
/// events and once more a moment later.
pub fn spawn(changed: impl Fn() + Send + 'static) -> io::Result<()> {
    let socket = netlink();
    let mounts = File::open("/proc/self/mountinfo");
    if let (Err(error), Err(_)) = (&socket, &mounts) {
        return Err(io::Error::new(error.kind(), error.to_string()));
    }
    let socket = socket.ok();
    let mut mounts = mounts.ok();
    if let Some(file) = mounts.as_mut() {
        // The first read arms it.
        rearm(file);
    }
    thread::Builder::new()
        .name("hotplug".into())
        .stack_size(128 * 1024)
        .spawn(move || run(socket.as_ref(), mounts, &changed))?;
    Ok(())
}

fn run(socket: Option<&OwnedFd>, mut mounts: Option<File>, changed: &dyn Fn()) {
    let mut first: Option<Instant> = None;
    let mut second: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let wake = [first, second].into_iter().flatten().min();
        let wait = wake.map_or(-1, |at| {
            at.saturating_duration_since(now)
                .as_millis()
                .min(i32::MAX as u128) as i32
        });
        let mut polls = Vec::with_capacity(2);
        if let Some(fd) = socket {
            polls.push(libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let mounts_at = polls.len();
        if let Some(file) = mounts.as_ref() {
            polls.push(libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLPRI | libc::POLLERR,
                revents: 0,
            });
        }
        // SAFETY: a valid array of pollfd of the length given.
        let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, wait) };
        if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
        let now = Instant::now();
        if let Some(fd) = socket {
            if n > 0 && polls[0].revents != 0 && drain(fd) {
                first = Some(now + SETTLE);
                second = Some(now + AGAIN);
            }
        }
        if let Some(file) = mounts.as_mut() {
            if n > 0 && polls[mounts_at].revents & (libc::POLLPRI | libc::POLLERR) != 0 {
                rearm(file);
                first = Some(first.map_or(now + MOUNT_SETTLE, |at| at.min(now + MOUNT_SETTLE)));
            }
        }
        let now = Instant::now();
        let mut tell = false;
        if first.is_some_and(|at| now >= at) {
            first = None;
            tell = true;
        }
        if second.is_some_and(|at| now >= at) {
            second = None;
            tell = true;
        }
        if tell {
            changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(parts: &[&str]) -> Vec<u8> {
        parts.join("\0").into_bytes()
    }

    #[test]
    fn a_plugged_in_stick_is_a_block_event() {
        let add = message(&[
            "add@/devices/pci0000:00/usb1/1-2/6:0:0:0/block/sdb/sdb1",
            "ACTION=add",
            "SUBSYSTEM=block",
            "DEVNAME=sdb1",
            "DEVTYPE=partition",
            "",
        ]);
        assert_eq!(block_event(&add).as_deref(), Some("sdb1"));
        let remove = message(&[
            "remove@/devices/x/block/sdb",
            "SUBSYSTEM=block",
            "DEVNAME=sdb",
        ]);
        assert_eq!(block_event(&remove).as_deref(), Some("sdb"));
    }

    #[test]
    fn other_events_and_noise_are_not() {
        let usb = message(&[
            "add@/devices/x/usb1/1-2",
            "SUBSYSTEM=usb",
            "DEVNAME=bus/usb/001/005",
        ]);
        assert_eq!(block_event(&usb), None);
        let lo = message(&[
            "add@/devices/virtual/block/loop3",
            "SUBSYSTEM=block",
            "DEVNAME=loop3",
        ]);
        assert_eq!(block_event(&lo), None);
        // The udev database's own messages begin with "libudev", no "@".
        assert_eq!(block_event(b"libudev\0\xfe\xed\xca\xfe"), None);
        assert_eq!(block_event(b""), None);
    }

    #[test]
    fn the_uevent_socket_can_be_opened_or_refused_but_not_crash() {
        // A sandbox may not allow it; either answer is fine.
        let _ = netlink();
    }
}
