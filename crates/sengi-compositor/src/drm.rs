//! The backend for a real screen: a libseat session on a virtual terminal,
//! the display through DRM/KMS, input through libinput.
//!
//! Frames are composed with pixman into dumb buffers, which every KMS
//! driver has, and shown with page flips. A frame is drawn when the state
//! asks for one and the previous flip has completed, so an idle screen
//! flips nothing and wakes nothing. There is no GPU rendering yet.

use std::{cell::RefCell, error::Error, path::Path, rc::Rc};

use smithay::{
    backend::{
        allocator::{dmabuf::Dmabuf, dumb::DumbAllocator, gbm::GbmDevice, Fourcc},
        drm::{
            compositor::{DrmCompositor, FrameFlags},
            DrmDevice, DrmDeviceFd, DrmEvent,
        },
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{damage::OutputDamageTracker, pixman::PixmanRenderer, Bind, Offscreen},
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
    utils::{DeviceFd, Size, Transform},
};

use crate::{headless, render, state::State, Options};

type Compositor = DrmCompositor<DumbAllocator, DrmDeviceFd, (), DrmDeviceFd>;

struct Drm {
    /// Held so the seat stays ours for as long as the backend lives.
    _session: LibSeatSession,
    device: DrmDevice,
    compositor: Compositor,
    renderer: PixmanRenderer,
    output: Output,
    libinput: Libinput,
    /// A frame is on its way to the screen; the next one waits for it.
    flip_pending: bool,
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

    let surface = device.create_surface(crtc, mode, &[connector.handle()])?;
    let renderer = PixmanRenderer::new()?;
    let formats = Bind::<Dmabuf>::supported_formats(&renderer).unwrap_or_default();
    let compositor: Compositor = DrmCompositor::new(
        &output,
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

    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| "libinput cannot use this seat")?;

    eprintln!(
        "sengi-compositor: {name} at {width}x{height}, {} Hz, on {} (pixman, dumb buffers)",
        mode.vrefresh(),
        path.display()
    );

    let mut vt_session = session.clone();
    state.switch_vt = Some(Box::new(move |vt| {
        if let Err(err) = vt_session.change_vt(vt) {
            eprintln!("sengi-compositor: cannot switch to terminal {vt}: {err}");
        }
    }));

    let backend: Shared = Rc::new(RefCell::new(Drm {
        _session: session,
        device,
        compositor,
        renderer,
        output,
        libinput: libinput.clone(),
        flip_pending: false,
        active: true,
    }));

    // A frame is drawn when the state asks for one...
    let (ping, wakeups) = make_ping()?;
    let on_wake = backend.clone();
    handle.insert_source(wakeups, move |_, _, state| draw_if_ready(&on_wake, state))?;
    state.set_redraw_ping(ping);

    // ...and the next one when that one has reached the screen.
    let on_flip = backend.clone();
    handle.insert_source(device_events, move |event, _, state| match event {
        DrmEvent::VBlank(_) => {
            {
                let mut backend = on_flip.borrow_mut();
                backend.flip_pending = false;
                if let Err(err) = backend.compositor.frame_submitted() {
                    eprintln!("sengi-compositor: page flip: {err}");
                }
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
                backend.flip_pending = false;
            }
            SessionEvent::ActivateSession => {
                if backend.libinput.resume().is_err() {
                    eprintln!("sengi-compositor: cannot get the input devices back");
                }
                if let Err(err) = backend.device.activate(false) {
                    eprintln!("sengi-compositor: cannot get the display back: {err}");
                }
                if let Err(err) = backend.compositor.reset_state() {
                    eprintln!("sengi-compositor: display state: {err}");
                }
                backend.compositor.reset_buffers();
                backend.active = true;
                drop(backend);
                state.request_redraw();
            }
        }
    })?;

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
    let mut backend = backend.borrow_mut();
    if !state.needs_redraw || backend.flip_pending || !backend.active {
        return;
    }
    if let Err(err) = backend.frame(state) {
        eprintln!("sengi-compositor: frame failed: {err}");
    }
}

impl Drm {
    /// Composes what changed and queues it for the next vertical blank.
    fn frame(&mut self, state: &mut State) -> Result<(), Box<dyn Error>> {
        state.needs_redraw = false;
        let (elements, background) = render::elements(state, &self.output, &mut self.renderer);
        // Always composed by us: no client buffer is put on a plane
        // directly, which pixman-rendered frames gain nothing from.
        let result = self
            .compositor
            .render_frame(
                &mut self.renderer,
                &elements,
                background,
                FrameFlags::empty(),
            )
            .map_err(|err| format!("{err:?}"))?;
        let changed = !result.is_empty;
        let states = result.states;

        if changed {
            self.compositor
                .queue_frame(())
                .map_err(|err| format!("{err:?}"))?;
            self.flip_pending = true;
        }
        render::presented(state, &self.output, &states);
        Ok(())
    }

    /// Renders the current picture once more, into memory, and saves it.
    /// The screen's own buffers cannot be read back portably.
    fn screenshot(&mut self, state: &State, path: &Path) -> Result<(), Box<dyn Error>> {
        let mode = self.output.current_mode().ok_or("the output has no mode")?;
        let size = Size::from((mode.size.w, mode.size.h));
        let mut image: Image<'static, 'static> =
            self.renderer.create_buffer(Fourcc::Xrgb8888, size)?;
        let mut damage = OutputDamageTracker::from_output(&self.output);
        let mut target = self.renderer.bind(&mut image)?;
        render::output(
            state,
            &self.output,
            &mut self.renderer,
            &mut target,
            0,
            &mut damage,
        )?;
        headless::save(&mut self.renderer, &target, size, path)
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
