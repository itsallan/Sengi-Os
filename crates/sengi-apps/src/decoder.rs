//! The asking side of icon decoding: a decoded icon, and the client that has
//! a child process do the finding and decoding. It needs no image library,
//! so a program that only shows icons another program has decoded, such as
//! the launcher, links none. The decoding itself is in `icons`.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Largest icon that will be produced, in pixels per side.
pub const MAX_PIXELS: u32 = 256;

/// First argument of the child that decodes one icon.
pub const DECODE_FLAG: &str = "--decode-icon";

/// Longest theme path or icon path we will pass on.
pub(crate) const MAX_PATH: usize = 1024;

/// A decoded icon: `size` by `size` pixels of premultiplied RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    /// Pixels per side.
    pub size: u32,
    /// `size * size * 4` bytes, row by row.
    pub data: Vec<u8>,
}

impl Pixels {
    /// Whether every pixel is fully transparent.
    pub fn is_blank(&self) -> bool {
        self.data.iter().skip(3).step_by(4).all(|&alpha| alpha == 0)
    }
}

/// The running program, to start again as a helper. Not `current_exe`:
/// after a package upgrade that names a file that is gone, or worse, the
/// new version, which may not speak to this one. This is the binary we
/// are actually running, whatever has happened to its name.
pub const THIS_PROGRAM: &str = "/proc/self/exe";

/// Finds and decodes icons in a child process, so that no path and no
/// icon file can crash or stall the program that asked.
#[derive(Clone, Debug)]
pub struct Decoder {
    program: PathBuf,
}

impl Default for Decoder {
    /// A decoder that runs the program we are part of.
    fn default() -> Self {
        Self::new(PathBuf::from(THIS_PROGRAM))
    }
}

impl Decoder {
    /// A decoder that runs `program`, which must answer [`DECODE_FLAG`] by
    /// calling [`run_decoder`].
    pub fn new(program: PathBuf) -> Self {
        Self { program }
    }

    /// Finds the icon `name` and decodes it to `pixels` per side. The
    /// lookup happens in the child too, in the standard places of the
    /// environment and every context, after `theme_path` if that is not
    /// empty. Blocks for at most `timeout`, so call it away from the UI
    /// thread. None means there is no usable icon by that name, whatever
    /// the reason; a `pixels` of zero or above [`MAX_PIXELS`] is one such
    /// reason.
    pub fn decode(
        &self,
        name: &str,
        theme_path: &str,
        pixels: u32,
        timeout: Duration,
    ) -> Option<Pixels> {
        // An argument cannot hold a NUL, and nothing real is this long.
        let sane = |text: &str, limit: usize| text.len() <= limit && !text.contains('\0');
        if pixels == 0 || pixels > MAX_PIXELS || name.is_empty() {
            return None;
        }
        if !sane(name, MAX_PATH) || !sane(theme_path, MAX_PATH) {
            return None;
        }
        let expected = (pixels * pixels * 4) as usize;
        let mut child = Command::new(&self.program)
            .arg(DECODE_FLAG)
            .arg(pixels.to_string())
            .arg(theme_path)
            .arg(name)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut output = child.stdout.take()?;

        // Someone has to watch the clock while the pipe is read.
        let watchdog = std::thread::spawn(move || {
            let started = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return status.success(),
                    Ok(None) if started.elapsed() < timeout => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return false;
                    }
                }
            }
        });

        let mut data = Vec::with_capacity(expected);
        let read = (&mut output)
            .take(expected as u64 + 1)
            .read_to_end(&mut data);
        // Closing our end unblocks a child that has more to say.
        drop(output);
        let finished = watchdog.join().unwrap_or(false);
        (finished && read.is_ok() && data.len() == expected)
            .then_some(Pixels { size: pixels, data })
    }

    /// Decodes the file at `path`, which the caller found itself with
    /// [`find`], to `pixels` per side. Otherwise as [`Decoder::decode`].
    pub fn decode_file(&self, path: &Path, pixels: u32, timeout: Duration) -> Option<Pixels> {
        self.decode(path.to_str()?, "", pixels, timeout)
    }
}
