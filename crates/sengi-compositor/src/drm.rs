//! The backend for a real screen: a libseat session on a virtual terminal,
//! the display through DRM/KMS, input through libinput.
//!
//! There are two ways to compose a frame. With a GPU, GLES renders into GBM
//! buffers, clients can hand over dmabufs, and the cursor gets its own
//! plane. Without one, pixman renders on the CPU into dumb buffers, which
//! every KMS driver has. `gpu::choose` picks, and never lets Mesa's
//! software GL into this process.
//!
//! Either way frames are shown with page flips: one is drawn when the state
//! asks for it and the previous flip has completed, so an idle screen flips
//! nothing and wakes nothing.
//!
//! Every connected display is lit, side by side in the order they were
//! found, and udev says when one is plugged in or pulled.

use std::{
    cell::RefCell,
    error::Error,
    ffi::{c_char, CStr},
    fs::File,
    io::{BufWriter, Read as _},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{
            dmabuf::Dmabuf,
            dumb::DumbAllocator,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            exporter::gbm::GbmFramebufferExporter,
            DrmDevice, DrmDeviceFd, DrmEvent,
        },
        egl::{EGLContext, EGLDevice, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            damage::OutputDamageTracker,
            element::RenderElementStates,
            gles::{ffi, GlesRenderbuffer, GlesRenderer},
            pixman::PixmanRenderer,
            Bind, ExportMem, ImportDma, Offscreen,
        },
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{self, UdevBackend, UdevEvent},
    },
    output::Output,
    reexports::{
        calloop::{
            ping::make_ping,
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        drm::{
            control::{connector, crtc, Device as _, Mode, ModeTypeFlags},
            ClientCapability, Device as _,
        },
        input::{Device as InputDevice, Libinput},
        pixman::Image,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{Buffer as BufferCoords, DeviceFd, Rectangle, Size, Transform},
    wayland::dmabuf::DmabufFeedbackBuilder,
};

use crate::{
    cli::{self, Wanted},
    gpu, headless, png, render,
    state::State,
    Options,
};

type PixmanCompositor = DrmCompositor<DumbAllocator, DrmDeviceFd, (), DrmDeviceFd>;
type GlesCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// Set to claim support for the cursor hotspot property, which makes the
/// kernel show virtual GPUs' cursor planes. For exercising the cursor plane
/// in a VM; the host's own pointer may then sit a few pixels off.
const VIRTUAL_CURSOR_VARIABLE: &str = "SENGI_COMPOSITOR_VIRTUAL_CURSOR_PLANE";

/// What composes the frames of every screen.
enum Renderer {
    Pixman(Box<PixmanRenderer>),
    Gles {
        renderer: Box<GlesRenderer>,
        gbm: GbmDevice<DrmDeviceFd>,
    },
}

/// The KMS side of one screen, with buffers of the renderer's kind.
enum Compositor {
    Pixman(Box<PixmanCompositor>),
    Gles(Box<GlesCompositor>),
}

/// Runs the same code for whichever renderer is in use. The two sides have
/// different types, so this is a macro and not a function.
macro_rules! on_screen {
    ($renderer:expr, $compositor:expr, |$r:ident, $c:ident| $body:expr) => {
        match ($renderer, $compositor) {
            (Renderer::Pixman($r), Compositor::Pixman($c)) => {
                let $r = &mut **$r;
                let $c = &mut **$c;
                $body
            }
            (Renderer::Gles { renderer: $r, .. }, Compositor::Gles($c)) => {
                let $r = &mut **$r;
                let $c = &mut **$c;
                $body
            }
            _ => unreachable!("a screen is made for the renderer in use"),
        }
    };
}

macro_rules! on_compositor {
    ($compositor:expr, |$c:ident| $body:expr) => {
        match $compositor {
            Compositor::Pixman($c) => $body,
            Compositor::Gles($c) => $body,
        }
    };
}

/// One lit display.
struct Screen {
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: Mode,
    output: Output,
    compositor: Compositor,
    /// The frame that is on its way to the screen, if any: what was visible
    /// in it, for the frame callbacks sent when it gets there, and which
    /// lock it was drawn under.
    in_flight: Option<(RenderElementStates, Option<u64>)>,
    /// Something changed since this screen was last drawn.
    stale: bool,
    /// The lock under which the frame now on the glass was drawn, known
    /// from the flip that put it there.
    on_glass: Option<u64>,
    /// A timer will draw this screen; until then it is left alone.
    waiting: bool,
    /// How many frames in a row could not be drawn.
    failures: u32,
    /// Counts the frames queued, for the watchdog to tell them apart.
    queued: u64,
    /// The watchdog timer is running.
    watched: bool,
}

/// How long a queued frame may take to reach the screen before it is
/// given up as failed. A flip that never completes would otherwise block
/// its screen for good, and under a lock keep the locker waiting.
const FLIP_TIME: Duration = Duration::from_secs(1);

/// After this many failed frames in a row a screen is switched off if the
/// session is locked: what it still shows must not stay on the glass.
const FAILURES_UNDER_LOCK: u32 = 3;

/// How long one refresh of `output` takes.
fn refresh_interval(output: &Output) -> Duration {
    let millihertz = output
        .current_mode()
        .map_or(60_000, |mode| mode.refresh.max(1_000));
    Duration::from_micros(1_000_000_000 / millihertz as u64)
}

struct Drm {
    /// Held so the seat stays ours for as long as the backend lives.
    _session: LibSeatSession,
    device: DrmDevice,
    fd: DrmDeviceFd,
    renderer: Renderer,
    screens: Vec<Screen>,
    libinput: Libinput,
    /// False while another virtual terminal has the display.
    active: bool,
}

type Shared = Rc<RefCell<Drm>>;

/// A screen's size and its pixels as red, green and blue bytes.
type Picture = (Size<i32, BufferCoords>, Vec<u8>);

/// What is known about a display mode when choosing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeInfo {
    pub width: u16,
    pub height: u16,
    pub refresh: u32,
    pub preferred: bool,
}

/// Which of a connector's modes to use: the one the display prefers, else
/// the largest, and of equally large ones the fastest.
pub fn pick_mode(modes: &[ModeInfo]) -> Option<usize> {
    modes
        .iter()
        .enumerate()
        .max_by_key(|(_, mode)| {
            (
                mode.preferred,
                u32::from(mode.width) * u32::from(mode.height),
                mode.refresh,
            )
        })
        .map(|(index, _)| index)
}

/// Every connected connector that has a usable mode, with that mode.
fn connected(device: &DrmDevice) -> Result<Vec<(connector::Info, Mode)>, Box<dyn Error>> {
    let resources = device.resource_handles()?;
    let mut found = Vec::new();
    for handle in resources.connectors() {
        let info = device.get_connector(*handle, true)?;
        if info.state() != connector::State::Connected {
            continue;
        }
        let modes: Vec<ModeInfo> = info
            .modes()
            .iter()
            .map(|mode| ModeInfo {
                width: mode.size().0,
                height: mode.size().1,
                refresh: mode.vrefresh(),
                preferred: mode.mode_type().contains(ModeTypeFlags::PREFERRED),
            })
            .collect();
        if let Some(mode) = pick_mode(&modes).map(|index| info.modes()[index]) {
            found.push((info, mode));
        }
    }
    Ok(found)
}

/// A CRTC that can drive `connector` and is not in `taken`.
fn free_crtc(
    device: &DrmDevice,
    connector: &connector::Info,
    taken: &[crtc::Handle],
) -> Option<crtc::Handle> {
    let resources = device.resource_handles().ok()?;
    connector
        .encoders()
        .iter()
        .filter_map(|encoder| device.get_encoder(*encoder).ok())
        .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
        .find(|crtc| !taken.contains(crtc))
}

/// Touchpad defaults: tap to click, traditional scrolling, and no stray
/// pointer jumps from the palm while typing.
fn configure(device: &mut InputDevice) {
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(true);
    }
    if device.config_scroll_has_natural_scroll() {
        let _ = device.config_scroll_set_natural_scroll_enabled(false);
    }
    if device.config_dwt_is_available() {
        let _ = device.config_dwt_set_enabled(true);
    }
}

/// The render node belonging to a display device, e.g. `/dev/dri/renderD128`
/// for `/dev/dri/card0`.
fn render_node(card: &Path) -> Option<PathBuf> {
    let name = card.file_name()?;
    let siblings = Path::new("/sys/class/drm").join(name).join("device/drm");
    std::fs::read_dir(siblings)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name())
        .find(|name| name.to_string_lossy().starts_with("renderD"))
        .map(|name| Path::new("/dev/dri").join(name))
}

/// `--probe-gl NODE`: says what GL on that render node runs on. This loads
/// Mesa, which is why it is done in a process of its own.
pub fn probe(node: &str) -> Result<String, Box<dyn Error>> {
    let file = File::options().read(true).write(true).open(node)?;
    let gbm = GbmDevice::new(file)?;
    // Safety: the display is only used with the device it was made from,
    // which it keeps alive.
    let display = unsafe { EGLDisplay::new(gbm)? };
    let software = EGLDevice::device_for_display(&display)
        .map(|device| device.is_software())
        .unwrap_or(false);
    let context = EGLContext::new(&display)?;
    // Safety: the context is fresh and current nowhere else.
    let mut renderer = unsafe { GlesRenderer::new(context)? };
    let name = renderer.with_context(|gl| {
        // Safety: GetString returns a static NUL-terminated string or null.
        let name = unsafe { gl.GetString(ffi::RENDERER) } as *const c_char;
        if name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        }
    })?;
    Ok(if software {
        format!("{name} (software)")
    } else {
        name
    })
}

/// How long the probe may take. It prints one short line.
const PROBE_TIME: Duration = Duration::from_secs(5);

/// Runs the probe in a child and returns what it printed.
fn probe_in_child(node: &Path) -> Option<String> {
    let mut child = Command::new(std::env::current_exe().ok()?)
        .arg("--probe-gl")
        .arg(node)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    // A driver that hangs must not hang the session with it.
    let deadline = Instant::now() + PROBE_TIME;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                crate::say!("the GL probe did not answer in time");
                return None;
            }
        }
    };
    let mut name = String::new();
    child.stdout.take()?.read_to_string(&mut name).ok()?;
    let name = name.trim().to_string();
    (status.success() && !name.is_empty()).then_some(name)
}

fn gles_renderer(fd: &DrmDeviceFd) -> Result<Renderer, Box<dyn Error>> {
    let gbm = GbmDevice::new(fd.clone())?;
    // Safety: the display is only used with the device it was made from,
    // which it keeps alive.
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = EGLContext::new(&display)?;
    // Safety: the context is fresh and current nowhere else.
    let renderer = unsafe { GlesRenderer::new(context)? };
    Ok(Renderer::Gles {
        renderer: Box::new(renderer),
        gbm,
    })
}

pub fn run(options: Options) -> Result<(), Box<dyn Error>> {
    // The seat comes first. libseat's builtin backend forks a helper that
    // waits for the kernel's SIGUSR1 and SIGUSR2 to switch terminals, and a
    // helper forked after the state has blocked those for its own use never
    // sees them: the switch then silently does not happen.
    let (mut session, session_events) =
        LibSeatSession::new().map_err(|err| format!("cannot open a seat session: {err}"))?;
    let seat = session.seat();

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display = Display::new()?;
    let mut state = State::new(&mut event_loop, display)?;
    let handle = event_loop.handle();

    let path = udev::primary_gpu(&seat)?
        .or_else(|| udev::all_gpus(&seat).ok()?.into_iter().next())
        .ok_or("no display device on this seat")?;
    let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
    let fd = session
        .open(&path, flags)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let virtual_cursor = cfg!(feature = "test-hooks")
        && std::env::var_os(VIRTUAL_CURSOR_VARIABLE).is_some_and(|value| !value.is_empty());
    if virtual_cursor {
        // The kernel wants atomic mode setting switched on first, and the
        // device must see the plane from the start, so this goes ahead of it.
        let claimed = [
            ClientCapability::UniversalPlanes,
            ClientCapability::Atomic,
            ClientCapability::CursorPlaneHotspot,
        ]
        .into_iter()
        .try_for_each(|capability| fd.set_client_capability(capability, true));
        if let Err(err) = claimed {
            crate::say!("no virtual cursor plane: {err}");
        }
    }
    let (device, device_events) = DrmDevice::new(fd.clone(), true)?;

    // GPU or CPU: the command line, then the environment, then whatever
    // the hardware turns out to be.
    let wanted = match options.renderer {
        Wanted::Auto => std::env::var("SENGI_COMPOSITOR_RENDERER")
            .ok()
            .and_then(|name| cli::parse_renderer(&name))
            .unwrap_or(Wanted::Auto),
        chosen => chosen,
    };
    let driver = std::fs::metadata(&path)
        .ok()
        .and_then(|meta| udev::driver(meta.rdev()).ok().flatten())
        .map(|driver| driver.to_string_lossy().into_owned());
    let node = render_node(&path);
    let (choice, mut why) = gpu::choose(wanted, driver.as_deref(), node.is_some(), || {
        probe_in_child(node.as_deref()?)
    });
    let renderer = match choice {
        gpu::Renderer::Pixman => None,
        gpu::Renderer::Gles => match gles_renderer(&fd) {
            Ok(renderer) => Some(renderer),
            Err(err) => {
                why = format!("GLES failed: {err}");
                None
            }
        },
    };
    let renderer = match renderer {
        Some(renderer) => renderer,
        None => Renderer::Pixman(Box::new(PixmanRenderer::new()?)),
    };

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| "libinput cannot use this seat")?;

    let mut vt_session = session.clone();
    state.switch_vt = Some(Box::new(move |vt| {
        if let Err(err) = vt_session.change_vt(vt) {
            crate::say!("cannot switch to terminal {vt}: {err}");
        }
    }));

    // Lighting a display already asks for a frame, so the way to ask has
    // to be there first.
    let (ping, wakeups) = make_ping()?;
    state.set_redraw_ping(ping);

    let mut backend = Drm {
        _session: session,
        device,
        fd,
        renderer,
        screens: Vec::new(),
        libinput: libinput.clone(),
        active: true,
    };
    backend.rescan(&mut state);
    if backend.screens.is_empty() && matches!(backend.renderer, Renderer::Gles { .. }) {
        // GL came up but cannot drive the display: do without it.
        why = "GLES could not light a display".into();
        backend.renderer = Renderer::Pixman(Box::new(PixmanRenderer::new()?));
        backend.rescan(&mut state);
    }
    if backend.screens.is_empty() {
        return Err("no connected display".into());
    }
    crate::say!(
        "{} ({}); renderer: {} ({why})",
        path.display(),
        driver.as_deref().unwrap_or("unknown driver"),
        match backend.renderer {
            Renderer::Pixman(_) => "pixman on dumb buffers",
            Renderer::Gles { .. } => "GLES on GBM",
        },
    );

    let dmabuf_formats = match &backend.renderer {
        Renderer::Gles { renderer, .. } => Some(renderer.dmabuf_formats()),
        Renderer::Pixman(_) => None,
    };
    let device_id = backend.device.device_id();
    let backend: Shared = Rc::new(RefCell::new(backend));

    // GPU clients may hand over their buffers as they are. Mesa finds the
    // device to render on through the feedback, so that is always sent.
    if let (Some(formats), Some(node)) = (dmabuf_formats, &node) {
        let feedback = std::fs::metadata(node)
            .map_err(Box::<dyn Error>::from)
            .and_then(|meta| Ok(DmabufFeedbackBuilder::new(meta.rdev(), formats).build()?));
        match feedback {
            Ok(feedback) => {
                state
                    .dmabuf_state
                    .create_global_with_default_feedback::<State>(&state.display_handle, &feedback);
                let importer = backend.clone();
                state.dmabuf_import = Some(Box::new(move |dmabuf| {
                    match &mut importer.borrow_mut().renderer {
                        Renderer::Gles { renderer, .. } => {
                            renderer.import_dmabuf(dmabuf, None).is_ok()
                        }
                        Renderer::Pixman(_) => false,
                    }
                }));
            }
            Err(err) => crate::say!("no dmabuf for clients: {err}"),
        }
    }

    // A frame is drawn when the state asks for one...
    let on_wake = backend.clone();
    handle.insert_source(wakeups, move |_, _, state| draw_if_ready(&on_wake, state))?;

    // ...and the next one when that one has reached the screen, which is
    // also when clients are told to draw again.
    let on_flip = backend.clone();
    handle.insert_source(device_events, move |event, _, state| match event {
        DrmEvent::VBlank(crtc) => {
            state.page_flips += 1;
            let shown = {
                let mut backend = on_flip.borrow_mut();
                let screen = backend
                    .screens
                    .iter_mut()
                    .find(|screen| screen.crtc == crtc);
                screen.and_then(|screen| {
                    let submitted = on_compositor!(&mut screen.compositor, |compositor| {
                        compositor
                            .frame_submitted()
                            .map(|_| ())
                            .map_err(|err| format!("{err:?}"))
                    });
                    if let Err(err) = submitted {
                        crate::say!("page flip: {err}");
                    }
                    let output = screen.output.clone();
                    let frame = screen.in_flight.take();
                    if let Some((_, epoch)) = &frame {
                        screen.on_glass = *epoch;
                        screen.failures = 0;
                    }
                    frame.map(|frame| (output, frame))
                })
            };
            if let Some((output, (states, epoch))) = shown {
                render::presented(state, &output, &states, epoch);
            }
            draw_if_ready(&on_flip, state);
        }
        DrmEvent::Error(err) => crate::say!("display: {err}"),
    })?;

    // A display was plugged in or pulled.
    let on_hotplug = backend.clone();
    handle.insert_source(UdevBackend::new(&seat)?, move |event, _, state| {
        if matches!(event, UdevEvent::Changed { device_id: changed } if changed == device_id) {
            let mut backend = on_hotplug.borrow_mut();
            if backend.active {
                backend.rescan(state);
            }
        }
    })?;

    handle.insert_source(
        LibinputInputBackend::new(libinput),
        |mut event, _, state| {
            if let InputEvent::DeviceAdded { device } = &mut event {
                configure(device);
            }
            state.process_input_event(event);
        },
    )?;

    // Switching away hands the display and the input devices back; coming
    // back takes them again and repaints everything.
    let on_session = backend.clone();
    handle.insert_source(session_events, move |event, _, state| {
        let mut backend = on_session.borrow_mut();
        match event {
            SessionEvent::PauseSession => {
                backend.libinput.suspend();
                backend.device.pause();
                backend.active = false;
                for screen in &mut backend.screens {
                    screen.in_flight = None;
                    screen.on_glass = None;
                }
            }
            SessionEvent::ActivateSession => {
                if backend.libinput.resume().is_err() {
                    crate::say!("cannot get the input devices back");
                }
                if let Err(err) = backend.device.activate(false) {
                    crate::say!("cannot get the display back: {err}");
                }
                backend.active = true;
                // Displays may have come and gone meanwhile.
                backend.rescan(state);
                for screen in &mut backend.screens {
                    let reset = on_compositor!(&mut screen.compositor, |compositor| {
                        compositor.reset_buffers();
                        compositor.reset_state().map_err(|err| err.to_string())
                    });
                    if let Err(err) = reset {
                        crate::say!("display state: {err}");
                    }
                    screen.stale = true;
                }
                drop(backend);
                state.request_redraw();
            }
        }
    })?;

    if let Some(path) = options.screenshot.clone() {
        let on_signal = backend.clone();
        state.take_screenshot = Some(Box::new(move |state| {
            if let Err(err) = on_signal.borrow_mut().screenshot(state, &path) {
                crate::say!("screenshot failed: {err}");
            }
        }));
    }

    if let Some(after) = options.exit_after {
        let at_exit = backend.clone();
        let path = options.screenshot.clone();
        handle.insert_source(Timer::from_duration(after), move |_, _, state| {
            if let Some(path) = &path {
                if let Err(err) = at_exit.borrow_mut().screenshot(state, path) {
                    crate::say!("screenshot failed: {err}");
                }
            }
            state.loop_signal.stop();
            TimeoutAction::Drop
        })?;
    }

    crate::start_clients(&mut state, &options);
    state.request_redraw();
    // When the loop ends everything is dropped, which gives the display
    // and the terminal back.
    state.run(event_loop)
}

fn draw_if_ready(backend: &Shared, state: &mut State) {
    let mut shown = Vec::new();
    let mut retry = Vec::new();
    let mut hopeless = Vec::new();
    let mut watch = Vec::new();
    {
        let mut backend = backend.borrow_mut();
        if !backend.active {
            return;
        }
        let Drm {
            renderer, screens, ..
        } = &mut *backend;
        if state.needs_redraw {
            state.needs_redraw = false;
            for screen in screens.iter_mut() {
                screen.stale = true;
            }
        }
        for screen in screens.iter_mut() {
            // A screen with a frame under way is drawn when that lands.
            if !screen.stale || screen.in_flight.is_some() || screen.waiting {
                continue;
            }
            if state.is_locked() && screen.failures >= FAILURES_UNDER_LOCK {
                hopeless.push(screen.crtc);
                continue;
            }
            screen.stale = false;
            let epoch = state.lock_epoch();
            match screen.frame(renderer, state, epoch) {
                Ok(Some(states)) => {
                    screen.failures = 0;
                    screen.waiting = true;
                    // "Nothing changed" is measured against the last frame
                    // drawn, not the last one shown. Unless a flip proved
                    // that a frame of this lock is on the glass, this one
                    // does not count as covering the screen, and the next
                    // is painted in full so that it gets flipped.
                    let proven = epoch.is_none() || screen.on_glass == epoch;
                    if !proven {
                        screen.repaint();
                        screen.stale = true;
                    }
                    let epoch = epoch.filter(|_| proven);
                    shown.push((screen.crtc, screen.output.clone(), states, epoch));
                }
                Ok(None) => {
                    // Queued; whether it worked is known at the flip.
                    screen.queued += 1;
                    if !screen.watched {
                        screen.watched = true;
                        watch.push((screen.crtc, screen.queued));
                    }
                }
                Err(err) => {
                    // The screen still shows what it showed. Try again
                    // shortly, at growing intervals.
                    screen.failures += 1;
                    screen.stale = true;
                    // Whatever was half queued is dropped, and the retry
                    // paints everything.
                    screen.repaint();
                    if screen.failures.is_power_of_two() {
                        crate::say!(
                            "{}: frame failed ({} in a row): {err}",
                            screen.output.name(),
                            screen.failures
                        );
                    }
                    screen.waiting = true;
                    let pause = refresh_interval(&screen.output) * screen.failures.min(60);
                    retry.push((screen.crtc, pause));
                }
            }
        }
        for crtc in hopeless {
            if let Some(at) = screens.iter().position(|screen| screen.crtc == crtc) {
                // Dropping the screen turns its CRTC off.
                let screen = screens.remove(at);
                crate::say!(
                    "{} cannot be drawn while locked, switched off",
                    screen.output.name()
                );
                state.remove_output(&screen.output);
            }
        }
    }
    for (crtc, mut frame) in watch {
        let watching = backend.clone();
        let timer = Timer::from_duration(FLIP_TIME);
        let armed = state.loop_handle.insert_source(timer, move |_, _, state| {
            let mut backend = watching.borrow_mut();
            let Some(screen) = backend
                .screens
                .iter_mut()
                .find(|screen| screen.crtc == crtc)
            else {
                return TimeoutAction::Drop;
            };
            match &screen.in_flight {
                // Still the frame this was set for: it is not coming.
                Some(_) if screen.queued == frame => {
                    crate::say!(
                        "{}: a frame did not reach the screen in {FLIP_TIME:?}",
                        screen.output.name()
                    );
                    screen.in_flight = None;
                    screen.failures += 1;
                    screen.stale = true;
                    screen.watched = false;
                    screen.repaint();
                    drop(backend);
                    draw_if_ready(&watching, state);
                    TimeoutAction::Drop
                }
                // A later frame is under way: watch that one.
                Some(_) => {
                    frame = screen.queued;
                    TimeoutAction::ToDuration(FLIP_TIME)
                }
                None => {
                    screen.watched = false;
                    TimeoutAction::Drop
                }
            }
        });
        if armed.is_err() {
            if let Some(screen) = backend
                .borrow_mut()
                .screens
                .iter_mut()
                .find(|screen| screen.crtc == crtc)
            {
                screen.watched = false;
            }
        }
    }
    for (crtc, pause) in retry {
        let again = backend.clone();
        let armed =
            state
                .loop_handle
                .insert_source(Timer::from_duration(pause), move |_, _, state| {
                    let waited = again
                        .borrow_mut()
                        .screens
                        .iter_mut()
                        .find(|screen| screen.crtc == crtc)
                        .map(|screen| screen.waiting = false);
                    if waited.is_some() {
                        draw_if_ready(&again, state);
                    }
                    TimeoutAction::Drop
                });
        if armed.is_err() {
            if let Some(screen) = backend
                .borrow_mut()
                .screens
                .iter_mut()
                .find(|screen| screen.crtc == crtc)
            {
                screen.waiting = false;
            }
        }
    }
    // Nothing changed on these, so there is no flip to wait for. Their
    // clients are still only told to draw again one refresh later: a
    // client that commits without damage would otherwise get its frame
    // callback at once, commit again, and the two would spin.
    for (crtc, output, states, epoch) in shown {
        let again = backend.clone();
        let mut frame = Some((output.clone(), states));
        let timer = Timer::from_duration(refresh_interval(&output));
        let armed = state.loop_handle.insert_source(timer, move |_, _, state| {
            if let Some(screen) = again
                .borrow_mut()
                .screens
                .iter_mut()
                .find(|screen| screen.crtc == crtc)
            {
                screen.waiting = false;
            }
            if let Some((output, states)) = frame.take() {
                render::presented(state, &output, &states, epoch);
            }
            draw_if_ready(&again, state);
            TimeoutAction::Drop
        });
        if armed.is_err() {
            if let Some(screen) = backend
                .borrow_mut()
                .screens
                .iter_mut()
                .find(|screen| screen.crtc == crtc)
            {
                screen.waiting = false;
            }
        }
    }
}

impl Screen {
    /// Forgets what the buffers hold and what was queued, so that the next
    /// frame is drawn in full and committed afresh.
    fn repaint(&mut self) {
        on_compositor!(&mut self.compositor, |compositor| {
            compositor.reset_buffers();
            let _ = compositor.reset_state();
        });
    }

    /// Composes what changed and queues it for the next vertical blank.
    ///
    /// Returns the element states when nothing had to be shown, for frame
    /// callbacks to go out at once; otherwise they are kept until the flip.
    fn frame(
        &mut self,
        renderer: &mut Renderer,
        state: &mut State,
        epoch: Option<u64>,
    ) -> Result<Option<RenderElementStates>, Box<dyn Error>> {
        // With a GPU, a client buffer or the cursor may go straight onto a
        // plane; pixman-composed frames gain nothing from that.
        let flags = match renderer {
            Renderer::Pixman(_) => FrameFlags::empty(),
            Renderer::Gles { .. } => FrameFlags::DEFAULT,
        };
        let output = &self.output;
        let (changed, states) =
            on_screen!(renderer, &mut self.compositor, |renderer, compositor| {
                let (elements, background) = render::elements(state, output, renderer);
                let result = compositor
                    .render_frame(renderer, &elements, background, flags)
                    .map_err(|err| format!("{err:?}"))?;
                let changed = !result.is_empty;
                if changed && result.cursor_element.is_some() {
                    state.cursor_plane_frames += 1;
                }
                let states = result.states;
                if changed {
                    compositor
                        .queue_frame(())
                        .map_err(|err| format!("{err:?}"))?;
                }
                (changed, states)
            });

        if changed {
            self.in_flight = Some((states, epoch));
            Ok(None)
        } else {
            Ok(Some(states))
        }
    }

    /// Renders the current picture once more, into memory. The screen's
    /// own buffers cannot be read back portably.
    fn picture(&self, renderer: &mut Renderer, state: &State) -> Result<Picture, Box<dyn Error>> {
        let mode = self.output.current_mode().ok_or("the output has no mode")?;
        let size = Size::from((mode.size.w, mode.size.h));
        let mut damage = OutputDamageTracker::from_output(&self.output);
        let rgb = match renderer {
            Renderer::Pixman(renderer) => {
                let renderer = &mut **renderer;
                let mut image: Image<'static, 'static> =
                    renderer.create_buffer(Fourcc::Xrgb8888, size)?;
                let mut target = renderer.bind(&mut image)?;
                render::output(state, &self.output, renderer, &mut target, 0, &mut damage)?;
                headless::read_rgb(renderer, &target, size)?
            }
            Renderer::Gles { renderer, .. } => {
                let renderer = &mut **renderer;
                let mut buffer: GlesRenderbuffer =
                    renderer.create_buffer(Fourcc::Abgr8888, size)?;
                let mut target = renderer.bind(&mut buffer)?;
                render::output(state, &self.output, renderer, &mut target, 0, &mut damage)?;
                let mapping = renderer.copy_framebuffer(
                    &target,
                    Rectangle::from_size(size),
                    Fourcc::Abgr8888,
                )?;
                let pixels = renderer.map_texture(&mapping)?;
                // ABGR8888 is stored as red, green, blue, alpha bytes, and
                // an offscreen target comes back top row first.
                pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|px| [px[0], px[1], px[2]])
                    .collect()
            }
        };
        Ok((size, rgb))
    }
}

impl Drm {
    /// Lights displays that were plugged in and lets go of ones that were
    /// pulled.
    fn rescan(&mut self, state: &mut State) {
        let found = match connected(&self.device) {
            Ok(found) => found,
            Err(err) => {
                crate::say!("cannot list the displays: {err}");
                return;
            }
        };

        let (kept, gone): (Vec<Screen>, Vec<Screen>) = std::mem::take(&mut self.screens)
            .into_iter()
            .partition(|screen| {
                // A different monitor on the same connector wants another
                // mode: that is the old one gone and a new one found.
                found
                    .iter()
                    .any(|(info, mode)| info.handle() == screen.connector && *mode == screen.mode)
            });
        self.screens = kept;
        for screen in gone {
            crate::say!("{} is gone", screen.output.name());
            state.remove_output(&screen.output);
        }

        for (info, mode) in &found {
            if self
                .screens
                .iter()
                .any(|screen| screen.connector == info.handle())
            {
                continue;
            }
            if let Err(err) = self.light(state, info, *mode) {
                crate::say!(
                    "cannot use {}-{}: {err}",
                    info.interface().as_str(),
                    info.interface_id()
                );
            }
        }
        state.request_redraw();
    }

    /// Sets up a newly found display and adds it to the desktop.
    fn light(
        &mut self,
        state: &mut State,
        info: &connector::Info,
        mode: Mode,
    ) -> Result<(), Box<dyn Error>> {
        let taken: Vec<crtc::Handle> = self.screens.iter().map(|screen| screen.crtc).collect();
        let crtc = free_crtc(&self.device, info, &taken).ok_or("no CRTC is free for it")?;
        let surface = self.device.create_surface(crtc, mode, &[info.handle()])?;
        let planes = surface.planes().clone();

        let (width, height) = mode.size();
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let output = state.add_output(
            &name,
            "Display",
            (i32::from(width), i32::from(height)).into(),
            mode.vrefresh() as i32 * 1000,
            Transform::Normal,
        );

        let cursor_size = self.device.cursor_size();
        let compositor = match &self.renderer {
            Renderer::Pixman(renderer) => {
                let formats = Bind::<Dmabuf>::supported_formats(&**renderer).unwrap_or_default();
                DrmCompositor::new(
                    &output,
                    surface,
                    None,
                    DumbAllocator::new(self.fd.clone()),
                    self.fd.clone(),
                    [Fourcc::Xrgb8888, Fourcc::Argb8888],
                    formats,
                    cursor_size,
                    None::<GbmDevice<DrmDeviceFd>>,
                )
                .map(|compositor| Compositor::Pixman(Box::new(compositor)))
                .map_err(|err| err.to_string())
            }
            Renderer::Gles { renderer, gbm } => {
                let formats = renderer.egl_context().dmabuf_render_formats().clone();
                let allocator = GbmAllocator::new(
                    gbm.clone(),
                    GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
                );
                // Passing the GBM device also gives the cursor a plane of
                // its own where the driver has one.
                DrmCompositor::new(
                    &output,
                    surface,
                    None,
                    allocator,
                    GbmFramebufferExporter::new(gbm.clone(), None),
                    [Fourcc::Argb8888, Fourcc::Abgr8888],
                    formats,
                    cursor_size,
                    Some(gbm.clone()),
                )
                .map(|compositor| Compositor::Gles(Box::new(compositor)))
                .map_err(|err| err.to_string())
            }
        };
        let compositor = match compositor {
            Ok(compositor) => compositor,
            Err(err) => {
                state.remove_output(&output);
                return Err(format!("cannot set up the display: {err}").into());
            }
        };

        crate::say!(
            "{name} at {width}x{height}, {} Hz; planes: {} cursor, {} overlay",
            mode.vrefresh(),
            planes.cursor.len(),
            planes.overlay.len(),
        );
        self.screens.push(Screen {
            connector: info.handle(),
            crtc,
            mode,
            output,
            compositor,
            in_flight: None,
            stale: true,
            on_glass: None,
            waiting: false,
            failures: 0,
            queued: 0,
            watched: false,
        });
        Ok(())
    }

    /// Saves what every screen shows as one picture, laid out as the
    /// screens are on the desktop.
    fn screenshot(&mut self, state: &State, path: &Path) -> Result<(), Box<dyn Error>> {
        let rects = state.output_rects();
        let left = rects.iter().map(|rect| rect.x).min().unwrap_or(0);
        let top = rects.iter().map(|rect| rect.y).min().unwrap_or(0);
        let width = (rects.iter().map(|rect| rect.right()).max().unwrap_or(0) - left) as usize;
        let height = (rects.iter().map(|rect| rect.bottom()).max().unwrap_or(0) - top) as usize;
        let mut picture = vec![0u8; width * height * 3];

        let Drm {
            renderer, screens, ..
        } = self;
        for screen in screens.iter() {
            let Some(place) = state.space.output_geometry(&screen.output) else {
                continue;
            };
            let (size, rgb) = screen.picture(renderer, state)?;
            let row = size.w as usize * 3;
            for (y, pixels) in rgb.chunks_exact(row).enumerate() {
                let at = ((place.loc.y - top) as usize + y) * width * 3
                    + (place.loc.x - left) as usize * 3;
                picture[at..at + row].copy_from_slice(pixels);
            }
        }
        let mut file = BufWriter::new(png::create(path)?);
        png::write_rgb(&mut file, width as u32, height as u32, &picture)?;
        crate::say!("wrote {}", path.display());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u16, height: u16, refresh: u32, preferred: bool) -> ModeInfo {
        ModeInfo {
            width,
            height,
            refresh,
            preferred,
        }
    }

    #[test]
    fn the_preferred_mode_wins() {
        let modes = [
            mode(3840, 2160, 30, false),
            mode(1920, 1080, 60, true),
            mode(1280, 720, 60, false),
        ];
        assert_eq!(pick_mode(&modes), Some(1));
    }

    #[test]
    fn without_a_preference_the_largest_then_fastest_wins() {
        let modes = [
            mode(1280, 720, 60, false),
            mode(1920, 1080, 30, false),
            mode(1920, 1080, 60, false),
            mode(1024, 768, 75, false),
        ];
        assert_eq!(pick_mode(&modes), Some(2));
        assert_eq!(pick_mode(&[]), None);
    }
}
