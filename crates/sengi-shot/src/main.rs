//! sengi-shot, run by the compositor for Print and Alt+Print. It asks the
//! compositor for a picture through wlr-screencopy, saves it in
//! ~/Pictures and says so with a notification. It runs for a moment and
//! is not running otherwise.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use sengi_shot::{Invocation, Region, Stamp};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_output::{self, WlOutput},
    wl_registry::WlRegistry,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

/// Long enough for a slow machine to draw a frame twice, short enough that
/// a compositor that never answers does not leave this running.
const GIVE_UP_SECONDS: u32 = 15;

#[derive(Default)]
struct State {
    names: Vec<Option<String>>,
    buffer: Option<(wl_shm::Format, u32, u32, u32)>,
    ready: bool,
    failed: bool,
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

impl Dispatch<WlOutput, usize> for State {
    fn event(
        state: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some(slot) = state.names.get_mut(*index) {
                *slot = Some(name);
            }
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format: WEnum::Value(format),
                width,
                height,
                stride,
            } => state.buffer = Some((format, width, height, stride)),
            zwlr_screencopy_frame_v1::Event::Ready { .. } => state.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => state.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: <WlShm as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: <WlShmPool as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlBuffer,
        _: <WlBuffer as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrScreencopyManagerV1,
        _: <ZwlrScreencopyManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

struct Picture {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let (wanted, region) = match sengi_shot::parse(&arguments) {
        Ok(Invocation::Take { output, region }) => (output, region),
        Ok(Invocation::Help) => {
            print!("{}", sengi_shot::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Invocation::Version) => {
            println!("sengi-shot {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("sengi-shot: {message}\n\n{}", sengi_shot::USAGE);
            return ExitCode::from(2);
        }
    };
    // SAFETY: alarm only sets a timer; SIGALRM ends the process.
    unsafe { libc::alarm(GIVE_UP_SECONDS) };
    match take(wanted.as_deref(), region) {
        Ok(saved) => {
            for path in &saved {
                println!("{}", path.display());
            }
            notify(&saved);
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("sengi-shot: {message}");
            tell("Screenshot failed", &message);
            ExitCode::FAILURE
        }
    }
}

fn take(wanted: Option<&str>, region: Option<Region>) -> Result<Vec<PathBuf>, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dir = sengi_shot::pictures_dir(home.as_deref()).ok_or("no home folder to save in")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;

    let connection = Connection::connect_to_env().map_err(|e| format!("no compositor: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&connection).map_err(|e| format!("no registry: {e}"))?;
    let qh = queue.handle();
    let mut state = State::default();
    let shm: WlShm = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| "the compositor has no shared memory")?;
    let manager: ZwlrScreencopyManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|_| "the compositor cannot copy the screen")?;

    let mut outputs = Vec::new();
    for global in globals.contents().clone_list() {
        if global.interface == "wl_output" {
            let index = outputs.len();
            state.names.push(None);
            outputs.push(globals.registry().bind::<WlOutput, _, _>(
                global.name,
                global.version.min(4),
                &qh,
                index,
            ));
        }
    }
    queue
        .roundtrip(&mut state)
        .map_err(|e| format!("the compositor went away: {e}"))?;

    let chosen: Vec<usize> = (0..outputs.len())
        .filter(|&i| wanted.is_none_or(|name| state.names[i].as_deref() == Some(name)))
        .collect();
    if chosen.is_empty() {
        return Err("there is no such screen".into());
    }
    // A region belongs to one output; if its name is not known, the first.
    let chosen = if region.is_some() && wanted.is_none() {
        chosen[..1].to_vec()
    } else {
        chosen
    };

    let stamp = Stamp::now();
    let several = chosen.len() > 1;
    let mut saved = Vec::new();
    for index in chosen {
        let picture = capture(
            &mut queue,
            &mut state,
            &manager,
            &shm,
            &outputs[index],
            region,
        )?;
        let name = state.names[index].as_deref().filter(|_| several);
        let stem = sengi_shot::file_stem(&stamp, name);
        let path = sengi_shot::free_path(&dir, &stem, Path::exists);
        save(&path, &picture)?;
        saved.push(path);
    }
    Ok(saved)
}

fn capture(
    queue: &mut EventQueue<State>,
    state: &mut State,
    manager: &ZwlrScreencopyManagerV1,
    shm: &WlShm,
    output: &WlOutput,
    region: Option<Region>,
) -> Result<Picture, String> {
    let qh = queue.handle();
    *state = State {
        names: std::mem::take(&mut state.names),
        ..State::default()
    };
    let frame = match region {
        Some(r) => manager.capture_output_region(0, output, r.x, r.y, r.width, r.height, &qh, ()),
        None => manager.capture_output(0, output, &qh, ()),
    };
    while state.buffer.is_none() && !state.failed {
        queue
            .blocking_dispatch(state)
            .map_err(|e| format!("the compositor went away: {e}"))?;
    }
    let Some((format, width, height, stride)) = state.buffer else {
        return Err("the compositor would not copy the screen (is it locked?)".into());
    };
    if !matches!(format, wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888) {
        return Err(format!("the compositor sent a format we cannot read: {format:?}"));
    }
    let bytes = stride as usize * height as usize;
    let file = memory_file(bytes)?;
    let pool = shm.create_pool(file.as_fd(), bytes as i32, &qh, ());
    let buffer = pool.create_buffer(0, width as i32, height as i32, stride as i32, format, &qh, ());
    pool.destroy();
    frame.copy(&buffer);
    while !state.ready && !state.failed {
        queue
            .blocking_dispatch(state)
            .map_err(|e| format!("the compositor went away: {e}"))?;
    }
    if !state.ready {
        return Err("the compositor failed to copy the screen".into());
    }
    let mut xrgb = vec![0u8; bytes];
    file.read_exact_at(&mut xrgb, 0)
        .map_err(|e| format!("cannot read the copy: {e}"))?;
    frame.destroy();
    buffer.destroy();
    let rgb = sengi_shot::xrgb_to_rgb(&xrgb, width as usize, height as usize, stride as usize)
        .ok_or("the picture is not the size it said")?;
    Ok(Picture { width, height, rgb })
}

/// Memory to share with the compositor, that goes with the process.
fn memory_file(bytes: usize) -> Result<File, String> {
    use std::os::fd::FromRawFd;
    // SAFETY: the name is a C string; the descriptor is ours.
    let fd = unsafe { libc::memfd_create(c"sengi-shot".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!("no memory to copy into: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: fd is a fresh descriptor nobody else owns.
    let file = unsafe { File::from_raw_fd(fd) };
    file.set_len(bytes as u64)
        .map_err(|e| format!("no memory to copy into: {e}"))?;
    Ok(file)
}

/// Writes the file whole or not at all: a half-written picture would be
/// picked up by whatever watches the folder.
fn save(path: &Path, picture: &Picture) -> Result<(), String> {
    let bytes = sengi_shot::encode_png(&picture.rgb, picture.width, picture.height)
        .map_err(|e| format!("cannot make the PNG: {e}"))?;
    let partial = path.with_extension("png.part");
    let written = File::create(&partial).and_then(|mut file| {
        file.write_all(&bytes)?;
        file.sync_all()
    });
    written
        .and_then(|()| std::fs::rename(&partial, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&partial);
        })
        .map_err(|e| format!("cannot save {}: {e}", path.display()))
}

fn notify(saved: &[PathBuf]) {
    let body = saved
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    tell("Screenshot saved", &body);
}

/// Through the notification service; a session without it just goes quiet.
fn tell(summary: &str, body: &str) {
    let _ = Command::new("sengi-notify")
        .args(["--post", summary, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}
