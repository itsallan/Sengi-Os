//! Running a program for the report. Unlike the services, the report wants
//! to know why a command gave nothing, so that the file can say so.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::programs::find_program;

/// What became of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// It ran to its end; `code` is `None` if a signal ended it.
    Done {
        code: Option<i32>,
        out: String,
        err: String,
    },
    /// The program is not installed.
    Missing,
    /// It had not finished in the time it was given, and was ended.
    TimedOut { out: String },
    /// It could not be started.
    NotStarted(String),
}

/// What a stream printed, its start and its end if it was too much: the
/// first messages after a start say what went wrong and the last ones what
/// is going wrong now.
struct Kept {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    half: usize,
    left_out: usize,
}

impl Kept {
    fn new(cap: usize) -> Kept {
        Kept {
            head: Vec::new(),
            tail: VecDeque::new(),
            half: cap / 2,
            left_out: 0,
        }
    }

    fn add(&mut self, bytes: &[u8]) {
        let room = self.half.saturating_sub(self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..room]);
        self.tail.extend(&bytes[room..]);
        while self.tail.len() > self.half {
            self.tail.pop_front();
            self.left_out += 1;
        }
    }

    fn text(self) -> String {
        let mut bytes = self.head;
        if self.left_out > 0 {
            let note = format!("\n[... {} bytes left out ...]\n", self.left_out);
            bytes.extend_from_slice(note.as_bytes());
        }
        bytes.extend(self.tail);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// End the program and whatever it started, and reap it.
fn end(child: &mut Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill only sends a signal. The group is the child's own,
        // made for it, and the child has not been reaped yet.
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Run `program`, looked for on `path`, for at most `limit`, keeping at
/// most about `cap` bytes of each of its two outputs.
pub fn run(path: &str, program: &str, args: &[&str], limit: Duration, cap: usize) -> Ran {
    let Some(found) = find_program(program, path) else {
        return Ran::Missing;
    };
    let spawned = Command::new(found)
        .args(args)
        // Messages in one language, without colour or a pager.
        .env("LC_ALL", "C.UTF-8")
        .env("SYSTEMD_COLORS", "0")
        .env("SYSTEMD_PAGER", "")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => return Ran::NotStarted(error.to_string()),
    };
    let deadline = Instant::now() + limit;
    let mut streams = [
        Stream::of(child.stdout.take(), cap),
        Stream::of(child.stderr.take(), cap),
    ];
    let mut finished = read_both(&mut streams, deadline);
    let mut code = None;
    // Having closed its outputs it has nearly always exited.
    while finished {
        match child.try_wait() {
            Ok(Some(status)) => {
                code = status.code();
                break;
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            _ => finished = false,
        }
    }
    if !finished {
        end(&mut child);
    }
    let [out, err] = streams.map(|stream| stream.kept.text());
    if finished {
        Ran::Done { code, out, err }
    } else {
        Ran::TimedOut { out }
    }
}

/// One of the child's two outputs while it is being read.
struct Stream {
    /// `None` once it has ended.
    pipe: Option<Box<dyn Read>>,
    descriptor: RawFd,
    kept: Kept,
}

impl Stream {
    fn of(pipe: Option<impl Read + AsRawFd + 'static>, cap: usize) -> Stream {
        Stream {
            descriptor: pipe.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            pipe: pipe.map(|pipe| Box::new(pipe) as Box<dyn Read>),
            kept: Kept::new(cap),
        }
    }
}

/// Read both streams to their ends. False if `deadline` came first.
fn read_both(streams: &mut [Stream; 2], deadline: Instant) -> bool {
    let mut buffer = [0u8; 8192];
    loop {
        if streams.iter().all(|stream| stream.pipe.is_none()) {
            return true;
        }
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        // poll passes over a negative descriptor.
        let mut waiting = [0, 1].map(|i| libc::pollfd {
            fd: if streams[i].pipe.is_some() {
                streams[i].descriptor
            } else {
                -1
            },
            events: libc::POLLIN,
            revents: 0,
        });
        let timeout = i32::try_from(left.as_millis()).unwrap_or(i32::MAX).max(1);
        // SAFETY: poll is given two descriptors that the streams keep open
        // for as long as this runs, and writes only to `waiting`.
        let ready = unsafe { libc::poll(waiting.as_mut_ptr(), 2, timeout) };
        if ready < 0 && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted {
            return false;
        }
        if ready <= 0 {
            continue;
        }
        for (waited, stream) in waiting.iter().zip(streams.iter_mut()) {
            if waited.revents == 0 {
                continue;
            }
            let Some(pipe) = stream.pipe.as_mut() else {
                continue;
            };
            match pipe.read(&mut buffer) {
                Ok(0) => stream.pipe = None,
                Ok(count) => stream.kept.add(&buffer[..count]),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => stream.pipe = None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, limit: Duration, cap: usize) -> Ran {
        run("/bin:/usr/bin", "sh", &["-c", script], limit, cap)
    }

    const LONG: Duration = Duration::from_secs(20);

    #[test]
    fn both_outputs_and_the_status_come_back() {
        assert_eq!(
            sh("echo out; echo err >&2; exit 3", LONG, 1024),
            Ran::Done {
                code: Some(3),
                out: "out\n".to_string(),
                err: "err\n".to_string()
            }
        );
    }

    #[test]
    fn a_missing_program_is_said_to_be_missing() {
        assert_eq!(
            run("/bin", "no-such-program-anywhere", &[], LONG, 64),
            Ran::Missing
        );
    }

    #[test]
    fn a_command_that_hangs_is_ended_with_what_it_started() {
        let short = Duration::from_millis(300);
        for script in [
            "echo started; sleep 30",
            // A grandchild holds the pipes after the shell has gone.
            "echo started; sleep 30 & exit 0",
            "echo started; exec >&- 2>&-; sleep 30",
        ] {
            let started = Instant::now();
            assert_eq!(
                sh(script, short, 1024),
                Ran::TimedOut {
                    out: "started\n".to_string()
                },
                "{script}"
            );
            assert!(started.elapsed() < Duration::from_secs(10), "{script}");
        }
    }

    #[test]
    fn too_much_output_keeps_its_start_and_end() {
        let Ran::Done { out, .. } = sh(
            "echo first; i=0; while [ $i -lt 2000 ]; do echo xxxxxxxxx; i=$((i+1)); done; echo last",
            LONG,
            400,
        ) else {
            panic!("it ends");
        };
        assert!(out.starts_with("first\n"), "{out}");
        assert!(out.ends_with("last\n"), "{out}");
        assert!(out.contains("left out"), "{out}");
        assert!(out.len() < 500, "{}", out.len());
    }
}
