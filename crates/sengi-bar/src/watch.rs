//! The worker that keeps the bar's slow readings, the ones another program
//! has to be run for: when it reads and what it passes on.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::status::Volume;

/// Readings that take a subprocess to get.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slow {
    pub volume: Option<Volume>,
    pub wifi_name: Option<String>,
}

/// What the worker is woken for between its slow readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// The mute button.
    ToggleMute,
    /// The volume was changed somewhere else in the shell.
    Volume,
}

/// How the machine is read, so that tests can stand in for it.
pub struct Readers<V, W, T> {
    pub volume: V,
    pub wifi_name: W,
    pub toggle_mute: T,
}

/// While there is no volume to show at the start, how long to wait before
/// each further look. PipeWire and WirePlumber come up with the session,
/// often after the bar has first asked, and nothing announces them. These
/// are some fourteen seconds in all; after them a machine with no sound
/// device is asked only every full reading.
const SETTLE: [Duration; 9] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(2),
    Duration::from_secs(3),
    Duration::from_secs(4),
];

/// Reads everything every `interval`, and the volume whenever asked, and
/// gives `send` each reading that differs from the one before. Returns
/// when `send` refuses one or nobody is left to ask.
pub fn run<V, W, T>(
    asks: &Receiver<Ask>,
    interval: Duration,
    machine: &Readers<V, W, T>,
    send: impl FnMut(Slow) -> bool,
) where
    V: Fn() -> Option<Volume>,
    W: Fn() -> Option<String>,
    T: Fn(),
{
    run_settling(asks, interval, &SETTLE, machine, send);
}

/// [`run`] with the waits of [`SETTLE`] given.
fn run_settling<V, W, T>(
    asks: &Receiver<Ask>,
    interval: Duration,
    settle: &[Duration],
    machine: &Readers<V, W, T>,
    mut send: impl FnMut(Slow) -> bool,
) where
    V: Fn() -> Option<Volume>,
    W: Fn() -> Option<String>,
    T: Fn(),
{
    let mut last = Slow {
        volume: (machine.volume)(),
        wifi_name: (machine.wifi_name)(),
    };
    if !send(last.clone()) {
        return;
    }
    // Being asked for the volume must not put the full reading off.
    let mut due = Instant::now() + interval;
    let mut waits = settle.iter();
    let mut soon = match last.volume {
        None => waits.next().map(|&wait| Instant::now() + wait),
        Some(_) => None,
    };
    loop {
        let mut slow = last.clone();
        let wake = soon.map_or(due, |soon| soon.min(due));
        match asks.recv_timeout(wake.saturating_duration_since(Instant::now())) {
            Ok(first) => {
                // A held key asks many times over: one reading answers all
                // that is waiting.
                for ask in std::iter::once(first).chain(asks.try_iter()) {
                    if ask == Ask::ToggleMute {
                        (machine.toggle_mute)();
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() >= due {
            slow.wifi_name = (machine.wifi_name)();
            due = Instant::now() + interval;
        }
        slow.volume = (machine.volume)();
        // The first volume ends the settling for good.
        soon = match soon {
            Some(at) if slow.volume.is_none() => {
                if Instant::now() >= at {
                    waits.next().map(|&wait| Instant::now() + wait)
                } else {
                    Some(at)
                }
            }
            _ => None,
        };
        if slow != last {
            if !send(slow.clone()) {
                return;
            }
            last = slow;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;

    use sengi_system::volume::{announce_in, Notices};

    use super::*;

    const NEVER: Duration = Duration::from_secs(3600);
    const SOON: Duration = Duration::from_secs(2);

    #[derive(Default)]
    struct Machine {
        percent: AtomicU32,
        volume_reads: AtomicU32,
        wifi_reads: AtomicU32,
        toggles: AtomicU32,
    }

    fn start(machine: &Arc<Machine>, asks: Receiver<Ask>, interval: Duration) -> Receiver<Slow> {
        let (send, readings) = mpsc::channel();
        let machine = machine.clone();
        thread::spawn(move || {
            let readers = Readers {
                volume: || {
                    machine.volume_reads.fetch_add(1, Ordering::SeqCst);
                    Some(Volume {
                        percent: machine.percent.load(Ordering::SeqCst),
                        muted: false,
                    })
                },
                wifi_name: || {
                    machine.wifi_reads.fetch_add(1, Ordering::SeqCst);
                    None
                },
                toggle_mute: || {
                    machine.toggles.fetch_add(1, Ordering::SeqCst);
                },
            };
            run(&asks, interval, &readers, |slow| send.send(slow).is_ok());
        });
        readings
    }

    fn percent(slow: Slow) -> u32 {
        slow.volume.unwrap().percent
    }

    #[test]
    fn an_announced_change_is_read_at_once() {
        let dir = std::env::temp_dir().join(format!("sengi-bar-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let notices = Notices::listen_in(&dir).unwrap();
        let (ask, asks) = mpsc::channel();
        thread::spawn(move || while notices.wait().is_ok() && ask.send(Ask::Volume).is_ok() {});
        let machine = Arc::new(Machine::default());
        let readings = start(&machine, asks, NEVER);
        assert_eq!(percent(readings.recv_timeout(SOON).unwrap()), 0);

        // What a volume key does: change it, then say so.
        machine.percent.store(45, Ordering::SeqCst);
        announce_in(&dir);
        assert_eq!(percent(readings.recv_timeout(SOON).unwrap()), 45);
        // Only the volume: the Wi-Fi name keeps to its own interval.
        assert_eq!(machine.wifi_reads.load(Ordering::SeqCst), 1);
        assert_eq!(machine.toggles.load(Ordering::SeqCst), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn asks_that_queued_up_are_answered_together() {
        let (ask, asks) = mpsc::channel();
        for wanted in [Ask::Volume, Ask::Volume, Ask::ToggleMute, Ask::Volume] {
            ask.send(wanted).unwrap();
        }
        drop(ask);
        let machine = Arc::new(Machine::default());
        let readings = start(&machine, asks, NEVER);
        // The channel closes once the worker has returned.
        assert_eq!(readings.iter().count(), 1);
        assert_eq!(machine.toggles.load(Ordering::SeqCst), 1);
        assert_eq!(machine.volume_reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_held_key_does_not_put_the_full_reading_off() {
        let (ask, asks) = mpsc::channel();
        let machine = Arc::new(Machine::default());
        let _readings = start(&machine, asks, Duration::from_millis(100));
        let begun = Instant::now();
        while begun.elapsed() < Duration::from_millis(600) {
            ask.send(Ask::Volume).unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        assert!(machine.wifi_reads.load(Ordering::SeqCst) >= 3);
    }

    /// At login the bar can ask before PipeWire has a device: nothing
    /// announces its arrival, and the volume must not wait for the next
    /// full reading.
    #[test]
    fn sound_that_comes_up_late_is_shown_soon() {
        let (_ask, asks) = mpsc::channel();
        let (send, readings) = mpsc::channel();
        let reads = Arc::new(AtomicU32::new(0));
        let counted = reads.clone();
        thread::spawn(move || {
            let readers = Readers {
                volume: || {
                    (counted.fetch_add(1, Ordering::SeqCst) >= 3).then_some(Volume {
                        percent: 40,
                        muted: false,
                    })
                },
                wifi_name: || None,
                toggle_mute: || {},
            };
            run(&asks, NEVER, &readers, |slow| send.send(slow).is_ok());
        });
        let first: Slow = readings.recv_timeout(SOON).unwrap();
        assert_eq!(first.volume, None);
        assert_eq!(percent(readings.recv_timeout(SOON).unwrap()), 40);
        // Once there is a volume, the asking stops.
        thread::sleep(SOON);
        assert_eq!(reads.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_machine_with_no_sound_is_not_asked_for_ever() {
        let (_ask, asks) = mpsc::channel();
        let reads = Arc::new(AtomicU32::new(0));
        let counted = reads.clone();
        thread::spawn(move || {
            let readers = Readers {
                volume: || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    None
                },
                wifi_name: || None,
                toggle_mute: || {},
            };
            let waits = [Duration::from_millis(10); 3];
            run_settling(&asks, NEVER, &waits, &readers, |_| true);
        });
        thread::sleep(Duration::from_millis(500));
        assert_eq!(reads.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn nothing_is_read_while_nothing_happens() {
        let (_ask, asks) = mpsc::channel();
        let machine = Arc::new(Machine::default());
        let _readings = start(&machine, asks, NEVER);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(machine.volume_reads.load(Ordering::SeqCst), 1);
    }
}
