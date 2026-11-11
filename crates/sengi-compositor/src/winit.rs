//! A backend that runs the compositor in a window on another desktop.
//!
//! This is the one to use while developing: it has keyboard and pointer input
//! and needs no privileges. It renders with GLES through the host's EGL.

use std::error::Error;

use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::Output,
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        wayland_server::Display,
        winit::{dpi::LogicalSize, window::Window},
    },
    utils::{Rectangle, Transform},
};

use crate::{render, state::State, Options};

pub fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display = Display::new()?;
    let mut state = State::new(&mut event_loop, display)?;

    let (width, height) = options.size;
    let attributes = Window::default_attributes()
        .with_inner_size(LogicalSize::new(f64::from(width), f64::from(height)))
        .with_title("Sengi OS");
    let (mut backend, events) = winit::init_from_attributes::<GlesRenderer>(attributes)
        .map_err(|err| format!("cannot open a window: {err}"))?;
    // We draw our own.
    backend.window().set_cursor_visible(false);

    // EGL surfaces have their origin at the bottom left.
    let output = state.add_output(
        "WINIT-1",
        "Window",
        backend.window_size(),
        Transform::Flipped180,
    );
    let mut damage = OutputDamageTracker::from_output(&output);

    let handle = event_loop.handle();
    handle.insert_source(events, move |event, _, state| match event {
        WinitEvent::Resized { size, .. } => state.resize_output(&output, size),
        WinitEvent::Input(event) => state.process_input_event(event),
        WinitEvent::Redraw => {
            if let Err(err) = redraw(&mut backend, &mut damage, &output, state) {
                eprintln!("sengi-compositor: frame failed: {err}");
                state.failed = true;
                state.loop_signal.stop();
            }
            // The host throttles this to its own refresh rate.
            backend.window().request_redraw();
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
    state.run(event_loop)
}

fn redraw(
    backend: &mut WinitGraphicsBackend<GlesRenderer>,
    damage: &mut OutputDamageTracker,
    output: &Output,
    state: &mut State,
) -> Result<(), Box<dyn Error>> {
    let size = backend.window_size();
    {
        let (renderer, mut target) = backend.bind()?;
        // Buffer age is not tracked yet, so every frame is drawn in full.
        render::output(state, output, renderer, &mut target, 0, damage)?;
    }
    backend.submit(Some(&[Rectangle::from_size(size)]))?;
    render::send_frames(state, output);
    Ok(())
}
