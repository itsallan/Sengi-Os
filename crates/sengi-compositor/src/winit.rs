//! A backend that runs the compositor in a window on another desktop.
//!
//! This is the one to use while developing: it has keyboard and pointer input
//! and needs no privileges. It renders with GLES through the host's EGL.

use std::{cell::RefCell, error::Error, rc::Rc};

use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::Output,
    reexports::{
        calloop::{
            ping::make_ping,
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        wayland_server::Display,
        winit::{dpi::LogicalSize, window::Window},
    },
    utils::Transform,
};

use crate::{
    render,
    state::{State, REFRESH_MHZ},
    Options,
};

pub fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display = Display::new()?;
    let mut state = State::new(&mut event_loop, display)?;

    let (width, height) = options.size;
    let attributes = Window::default_attributes()
        .with_inner_size(LogicalSize::new(f64::from(width), f64::from(height)))
        .with_title("Sengi OS");
    let (backend, events) = winit::init_from_attributes::<GlesRenderer>(attributes)
        .map_err(|err| format!("cannot open a window: {err}"))?;
    // We draw our own.
    backend.window().set_cursor_visible(false);

    // EGL surfaces have their origin at the bottom left.
    let output = state.add_output(
        "WINIT-1",
        "Window",
        backend.window_size(),
        REFRESH_MHZ,
        Transform::Flipped180,
    );
    let mut damage = OutputDamageTracker::from_output(&output);
    let backend = Rc::new(RefCell::new(backend));

    // A frame is drawn when the state asks for one: the request becomes a
    // redraw request to the host, which paces it to its own refresh rate.
    let handle = event_loop.handle();
    let (ping, wakeups) = make_ping()?;
    let window = backend.clone();
    handle.insert_source(wakeups, move |_, _, _| {
        window.borrow().window().request_redraw();
    })?;
    state.set_redraw_ping(ping);

    handle.insert_source(events, move |event, _, state| match event {
        WinitEvent::Resized { size, .. } => state.resize_output(&output, size),
        WinitEvent::Input(event) => state.process_input_event(event),
        // Also sent by the host when the window is exposed, so draw even
        // if nothing of ours changed.
        WinitEvent::Redraw => {
            let mut backend = backend.borrow_mut();
            if let Err(err) = redraw(&mut backend, &mut damage, &output, state) {
                eprintln!("sengi-compositor: frame failed: {err}");
                state.failed = true;
                state.loop_signal.stop();
            }
        }
        WinitEvent::CloseRequested => state.loop_signal.stop(),
        WinitEvent::Focus(_) => {}
    })?;

    if let Some(after) = options.exit_after {
        handle.insert_source(Timer::from_duration(after), |_, _, state| {
            state.loop_signal.stop();
            TimeoutAction::Drop
        })?;
    }

    crate::start_clients(&mut state, &options);
    state.request_redraw();
    state.run(event_loop)
}

fn redraw(
    backend: &mut WinitGraphicsBackend<GlesRenderer>,
    damage: &mut OutputDamageTracker,
    output: &Output,
    state: &mut State,
) -> Result<(), Box<dyn Error>> {
    state.needs_redraw = false;
    let epoch = state.lock_epoch();
    // How many frames old the buffer we are about to draw into is; the
    // damage tracker repaints only what changed since then.
    let age = backend.buffer_age().unwrap_or(0);
    let rendered = {
        let (renderer, mut target) = backend.bind()?;
        render::output(state, output, renderer, &mut target, age, damage)?
    };
    // Nothing changed: keep the frame that is on screen.
    if let Some(damage) = &rendered.damage {
        backend.submit(Some(damage))?;
    }
    render::presented(state, output, &rendered.states, epoch);
    Ok(())
}
