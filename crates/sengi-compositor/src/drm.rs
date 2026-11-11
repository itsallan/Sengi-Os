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

use std::{
    cell::RefCell,
    error::Error,
    ffi::{c_char, CStr},
    fs::File,
    io::BufWriter,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
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
        udev,
    },
    output::Output,
    reexports::{
        calloop::{
            ping::make_ping,
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        drm::control::{connector, crtc, Device as _, Mode, ModeTypeFlags},
        input::{Device as InputDevice, Libinput},
        pixman::Image,
        rustix::fs::OFlags,
        wayland_server::Display,
    },
    utils::{DeviceFd, Rectangle, Size, Transform},
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

/// The renderer and the KMS compositor that goes with its buffers.
enum Screen {
    Pixman {
        renderer: Box<PixmanRenderer>,
        compositor: Box<PixmanCompositor>,
    },
    Gles {
        renderer: Box<GlesRenderer>,
        compositor: Box<GlesCompositor>,
    },
}

/// Runs the same code for whichever renderer is in use. The two sides have
/// different types, so this is a macro and not a function.
macro_rules! on_screen {
    ($screen:expr, |$renderer:ident, $compositor:ident| $body:expr) => {
        match $screen {
            Screen::Pixman {
                renderer: $renderer,
                compositor: $compositor,
            } => {
                let $renderer = &mut **$renderer;
                let $compositor = &mut **$compositor;
                $body
            }
            Screen::Gles {
                renderer: $renderer,
                compositor: $compositor,
            } => {
                let $renderer = &mut **$renderer;
                let $compositor = &mut **$compositor;
                $body
            }
        }
    };
}

struct Drm {
    /// Held so the seat stays ours for as long as the backend lives.
    _session: LibSeatSession,
    device: DrmDevice,
    screen: Screen,
    output: Output,
    libinput: Libinput,
    /// The frame that is on its way to the screen, if any: what was visible
    /// in it, for the frame callbacks sent when it gets there.
    in_flight: Option<RenderElementStates>,
    /// False while another virtual terminal has the display.
    active: bool,
}

type Shared = Rc<RefCell<Drm>>;

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

/// The first connected connector with a usable mode and a CRTC to drive it.
fn pick_output(
    device: &DrmDevice,
) -> Result<(connector::Info, Mode, crtc::Handle), Box<dyn Error>> {
    let resources = device.resource_handles()?;
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
        let Some(mode) = pick_mode(&modes).map(|index| info.modes()[index]) else {
            continue;
        };
        let crtc = info
            .encoders()
            .iter()
            .filter_map(|encoder| device.get_encoder(*encoder).ok())
            .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
            .next();
        if let Some(crtc) = crtc {
            return Ok((info, mode, crtc));
        }
    }
    Err("no connected display".into())
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

/// Runs the probe in a child and returns what it printed.
fn probe_in_child(node: &Path) -> Option<String> {
    let output = Command::new(std::env::current_exe().ok()?)
        .arg("--probe-gl")
        .arg(node)
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !name.is_empty()).then_some(name)
}

fn pixman_screen(
    device: &mut DrmDevice,
    fd: &DrmDeviceFd,
    output: &Output,
    crtc: crtc::Handle,
    mode: Mode,
    connector: connector::Handle,
) -> Result<Screen, Box<dyn Error>> {
    let surface = device.create_surface(crtc, mode, &[connector])?;
    let renderer = PixmanRenderer::new()?;
    let formats = Bind::<Dmabuf>::supported_formats(&renderer).unwrap_or_default();
    let compositor: PixmanCompositor = DrmCompositor::new(
        output,
        surface,
        None,
        DumbAllocator::new(fd.clone()),
        fd.clone(),
        [Fourcc::Xrgb8888, Fourcc::Argb8888],
        formats,
        device.cursor_size(),
        None::<GbmDevice<DrmDeviceFd>>,
    )
    .map_err(|err| format!("cannot set up the display: {err}"))?;
    Ok(Screen::Pixman {
        renderer: Box::new(renderer),
        compositor: Box::new(compositor),
    })
}

fn gles_screen(
    device: &mut DrmDevice,
    fd: &DrmDeviceFd,
    output: &Output,
    crtc: crtc::Handle,
    mode: Mode,
    connector: connector::Handle,
) -> Result<Screen, Box<dyn Error>> {
    let gbm = GbmDevice::new(fd.clone())?;
    // Safety: the display is only used with the device it was made from,
    // which it keeps alive.
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let context = EGLContext::new(&display)?;
    // Safety: the context is fresh and current nowhere else.
    let renderer = unsafe { GlesRenderer::new(context)? };
    let formats = renderer.egl_context().dmabuf_render_formats().clone();

    let surface = device.create_surface(crtc, mode, &[connector])?;
    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    // Passing the GBM device also gives the cursor a plane of its own where
    // the driver has one.
    let compositor: GlesCompositor = DrmCompositor::new(
        output,
        surface,
        None,
        allocator,
        GbmFramebufferExporter::new(gbm.clone(), None),
        [Fourcc::Argb8888, Fourcc::Abgr8888],
        formats,
        device.cursor_size(),
        Some(gbm),
    )
    .map_err(|err| format!("cannot set up the display: {err}"))?;
    Ok(Screen::Gles {
        renderer: Box::new(renderer),
        compositor: Box::new(compositor),
    })
}

pub fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display = Display::new()?;
    let mut state = State::new(&mut event_loop, display)?;
    let handle = event_loop.handle();

    let (mut session, session_events) =
        LibSeatSession::new().map_err(|err| format!("cannot open a seat session: {err}"))?;
    let seat = session.seat();

    let path = udev::primary_gpu(&seat)?
        .or_else(|| udev::all_gpus(&seat).ok()?.into_iter().next())
        .ok_or("no display device on this seat")?;
    let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
    let fd = session
        .open(&path, flags)
        .map_err(|err| format!("cannot open {}: {err}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (mut device, device_events) = DrmDevice::new(fd.clone(), true)?;

    let (connector, mode, crtc) = pick_output(&device)?;
    let (width, height) = mode.size();
    let name = format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    );
    let output = state.add_output(
        &name,
        "Display",
        (i32::from(width), i32::from(height)).into(),
        mode.vrefresh() as i32 * 1000,
        Transform::Normal,
    );

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
    let (renderer, mut why) = gpu::choose(wanted, driver.as_deref(), node.is_some(), || {
        probe_in_child(node.as_deref()?)
    });
    let screen = match renderer {
        gpu::Renderer::Pixman => None,
        gpu::Renderer::Gles => {
            match gles_screen(&mut device, &fd, &output, crtc, mode, connector.handle()) {
                Ok(screen) => Some(screen),
                Err(err) => {
                    why = format!("GLES failed: {err}");
                    None
                }
            }
        }
    };
    let screen = match screen {
        Some(screen) => screen,
        None => pixman_screen(&mut device, &fd, &output, crtc, mode, connector.handle())?,
    };
    eprintln!(
        "sengi-compositor: {name} at {width}x{height}, {} Hz, on {} ({}); renderer: {} ({why})",
        mode.vrefresh(),
        path.display(),
        driver.as_deref().unwrap_or("unknown driver"),
        match screen {
            Screen::Pixman { .. } => "pixman on dumb buffers",
            Screen::Gles { .. } => "GLES on GBM",
        },
    );

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| "libinput cannot use this seat")?;

    let mut vt_session = session.clone();
    state.switch_vt = Some(Box::new(move |vt| {
        if let Err(err) = vt_session.change_vt(vt) {
            eprintln!("sengi-compositor: cannot switch to terminal {vt}: {err}");
        }
    }));

    let dmabuf_formats = match &screen {
        Screen::Gles { renderer, .. } => Some(renderer.dmabuf_formats()),
        Screen::Pixman { .. } => None,
    };

    let backend: Shared = Rc::new(RefCell::new(Drm {
        _session: session,
        device,
        screen,
        output,
        libinput: libinput.clone(),
        in_flight: None,
        active: true,
    }));

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
                    match &mut importer.borrow_mut().screen {
                        Screen::Gles { renderer, .. } => {
                            renderer.import_dmabuf(dmabuf, None).is_ok()
                        }
                        Screen::Pixman { .. } => false,
                    }
                }));
            }
            Err(err) => eprintln!("sengi-compositor: no dmabuf for clients: {err}"),
        }
    }

    // A frame is drawn when the state asks for one...
    let (ping, wakeups) = make_ping()?;
    let on_wake = backend.clone();
    handle.insert_source(wakeups, move |_, _, state| draw_if_ready(&on_wake, state))?;
    state.set_redraw_ping(ping);

    // ...and the next one when that one has reached the screen, which is
    // also when clients are told to draw again.
    let on_flip = backend.clone();
    handle.insert_source(device_events, move |event, _, state| match event {
        DrmEvent::VBlank(_) => {
            state.page_flips += 1;
            let shown = {
                let mut backend = on_flip.borrow_mut();
                let submitted = on_screen!(&mut backend.screen, |_renderer, compositor| {
                    compositor
                        .frame_submitted()
                        .map(|_| ())
                        .map_err(|err| format!("{err:?}"))
                });
                if let Err(err) = submitted {
                    eprintln!("sengi-compositor: page flip: {err}");
                }
                let output = backend.output.clone();
                backend.in_flight.take().map(|states| (output, states))
            };
            if let Some((output, states)) = shown {
                render::presented(state, &output, &states);
            }
            draw_if_ready(&on_flip, state);
        }
        DrmEvent::Error(err) => eprintln!("sengi-compositor: display: {err}"),
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
                backend.in_flight = None;
            }
            SessionEvent::ActivateSession => {
                if backend.libinput.resume().is_err() {
                    eprintln!("sengi-compositor: cannot get the input devices back");
                }
                if let Err(err) = backend.device.activate(false) {
                    eprintln!("sengi-compositor: cannot get the display back: {err}");
                }
                let reset = on_screen!(&mut backend.screen, |_renderer, compositor| {
                    compositor.reset_buffers();
                    compositor.reset_state().map_err(|err| err.to_string())
                });
                if let Err(err) = reset {
                    eprintln!("sengi-compositor: display state: {err}");
                }
                backend.active = true;
                drop(backend);
                state.request_redraw();
            }
        }
    })?;

    if let Some(path) = options.screenshot.clone() {
        let on_signal = backend.clone();
        state.take_screenshot = Some(Box::new(move |state| {
            if let Err(err) = on_signal.borrow_mut().screenshot(state, &path) {
                eprintln!("sengi-compositor: screenshot failed: {err}");
            }
        }));
    }

    if let Some(after) = options.exit_after {
        let at_exit = backend.clone();
        let path = options.screenshot.clone();
        handle.insert_source(Timer::from_duration(after), move |_, _, state| {
            if let Some(path) = &path {
                if let Err(err) = at_exit.borrow_mut().screenshot(state, path) {
                    eprintln!("sengi-compositor: screenshot failed: {err}");
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
    let shown = {
        let mut backend = backend.borrow_mut();
        if !state.needs_redraw || backend.in_flight.is_some() || !backend.active {
            return;
        }
        match backend.frame(state) {
            Ok(shown) => shown.map(|states| (backend.output.clone(), states)),
            Err(err) => {
                eprintln!("sengi-compositor: frame failed: {err}");
                None
            }
        }
    };
    // Nothing changed on screen, so there is no flip to wait for.
    if let Some((output, states)) = shown {
        render::presented(state, &output, &states);
    }
}

impl Drm {
    /// Composes what changed and queues it for the next vertical blank.
    ///
    /// Returns the element states when nothing had to be shown, for frame
    /// callbacks to go out at once; otherwise they are kept until the flip.
    fn frame(&mut self, state: &mut State) -> Result<Option<RenderElementStates>, Box<dyn Error>> {
        state.needs_redraw = false;
        // With a GPU, a client buffer or the cursor may go straight onto a
        // plane; pixman-composed frames gain nothing from that.
        let flags = match self.screen {
            Screen::Pixman { .. } => FrameFlags::empty(),
            Screen::Gles { .. } => FrameFlags::DEFAULT,
        };
        let output = &self.output;
        let (changed, states) = on_screen!(&mut self.screen, |renderer, compositor| {
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
            self.in_flight = Some(states);
            Ok(None)
        } else {
            Ok(Some(states))
        }
    }

    /// Renders the current picture once more, into memory, and saves it.
    /// The screen's own buffers cannot be read back portably.
    fn screenshot(&mut self, state: &State, path: &Path) -> Result<(), Box<dyn Error>> {
        let mode = self.output.current_mode().ok_or("the output has no mode")?;
        let size = Size::from((mode.size.w, mode.size.h));
        let mut damage = OutputDamageTracker::from_output(&self.output);
        match &mut self.screen {
            Screen::Pixman { renderer, .. } => {
                let renderer = &mut **renderer;
                let mut image: Image<'static, 'static> =
                    renderer.create_buffer(Fourcc::Xrgb8888, size)?;
                let mut target = renderer.bind(&mut image)?;
                render::output(state, &self.output, renderer, &mut target, 0, &mut damage)?;
                headless::save(renderer, &target, size, path)
            }
            Screen::Gles { renderer, .. } => {
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
                let rgb: Vec<u8> = pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|px| [px[0], px[1], px[2]])
                    .collect();
                let mut file = BufWriter::new(File::create(path)?);
                png::write_rgb(&mut file, size.w as u32, size.h as u32, &rgb)?;
                eprintln!("sengi-compositor: wrote {}", path.display());
                Ok(())
            }
        }
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
