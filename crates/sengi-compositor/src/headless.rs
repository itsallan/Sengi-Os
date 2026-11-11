//! A backend with no display and no input devices.
//!
//! It renders with pixman into memory at a fixed rate, which is enough to run
//! clients in CI and to save a screenshot of what a real output would show.

use std::{
    collections::VecDeque,
    error::Error,
    fs::{self, File},
    io::BufWriter,
    path::Path,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        input::{ButtonState, KeyState, Keycode},
        renderer::{
            damage::OutputDamageTracker,
            pixman::{PixmanRenderer, PixmanTarget},
            Bind, ExportMem, Offscreen,
        },
    },
    output::Output,
    reexports::{
        calloop::{
            timer::{TimeoutAction, Timer},
            EventLoop,
        },
        pixman::Image,
        wayland_server::Display,
    },
    utils::{Buffer, Rectangle, Size, Transform},
};

use crate::{
    png, render,
    script::{self, Action, Step},
    state::State,
    Options,
};

const FRAME: Duration = Duration::from_micros(16_667);

struct Headless {
    renderer: PixmanRenderer,
    image: Image<'static, 'static>,
    size: Size<i32, Buffer>,
    damage: OutputDamageTracker,
    output: Output,
    /// What is left of the script, soonest step first.
    steps: VecDeque<Step>,
    started: Instant,
}

pub fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let steps = steps(&options)?;

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display = Display::new()?;
    let mut state = State::new(&mut event_loop, display)?;

    let (width, height) = options.size;
    let output = state.add_output(
        "HEADLESS-1",
        "Headless",
        (width, height).into(),
        Transform::Normal,
    );

    let mut renderer = PixmanRenderer::new()?;
    let size = Size::from((width, height));
    let image = renderer.create_buffer(Fourcc::Xrgb8888, size)?;
    let mut backend = Headless {
        renderer,
        image,
        size,
        damage: OutputDamageTracker::from_output(&output),
        output,
        steps,
        started: Instant::now(),
    };

    event_loop
        .handle()
        .insert_source(Timer::immediate(), move |_, _, state| {
            match backend.frame(state) {
                Ok(true) => TimeoutAction::ToDuration(FRAME),
                Ok(false) => {
                    state.loop_signal.stop();
                    TimeoutAction::Drop
                }
                Err(err) => {
                    eprintln!("sengi-compositor: frame failed: {err}");
                    state.failed = true;
                    state.loop_signal.stop();
                    TimeoutAction::Drop
                }
            }
        })?;

    crate::start_clients(&mut state, &options);
    state.run(event_loop)
}

/// The script, with `--screenshot` and `--exit-after` folded in as steps.
fn steps(options: &Options) -> Result<VecDeque<Step>, Box<dyn Error>> {
    let mut steps = match &options.script {
        Some(path) => {
            let text = fs::read_to_string(path)
                .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
            script::parse(&text)?
        }
        None => Vec::new(),
    };
    if let Some(at) = options.exit_after {
        if let Some(path) = &options.screenshot {
            let action = Action::Screenshot(path.clone());
            steps.push(Step { at, action });
        }
        let action = Action::Exit;
        steps.push(Step { at, action });
    }
    // Stable, so steps at the same time keep the order they were written in.
    steps.sort_by_key(|step| step.at);
    Ok(steps.into())
}

impl Headless {
    /// Plays the script steps that are due and draws one frame. Returns
    /// false when the script asked to exit.
    fn frame(&mut self, state: &mut State) -> Result<bool, Box<dyn Error>> {
        let now = self.started.elapsed();
        let time = now.as_millis() as u32;
        let mut screenshot = None;
        let mut exit = false;

        while self.steps.front().is_some_and(|step| step.at <= now) {
            let Some(step) = self.steps.pop_front() else {
                break;
            };
            let key_state = |pressed| match pressed {
                true => KeyState::Pressed,
                false => KeyState::Released,
            };
            let button_state = |pressed| match pressed {
                true => ButtonState::Pressed,
                false => ButtonState::Released,
            };
            match step.action {
                Action::Key { code, pressed } => {
                    // xkb keycodes are evdev codes plus eight.
                    state.key(Keycode::new(code + 8), key_state(pressed), time);
                }
                Action::Move { x, y } => state.pointer_motion((x, y).into(), time),
                Action::Button { code, pressed } => {
                    state.pointer_button(code, button_state(pressed), time);
                }
                // The picture has to show the steps before it, so draw first
                // and leave the rest for the next frame.
                Action::Screenshot(path) => {
                    screenshot = Some(path);
                    break;
                }
                Action::Exit => {
                    exit = true;
                    break;
                }
            }
        }

        let mut target = self.renderer.bind(&mut self.image)?;
        // The image is reused, so it always holds the previous frame.
        render::output(
            state,
            &self.output,
            &mut self.renderer,
            &mut target,
            1,
            &mut self.damage,
        )?;
        if let Some(path) = screenshot {
            save(&mut self.renderer, &target, self.size, &path)?;
        }
        drop(target);

        render::send_frames(state, &self.output);
        Ok(!exit)
    }
}

fn save(
    renderer: &mut PixmanRenderer,
    target: &PixmanTarget<'_>,
    size: Size<i32, Buffer>,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let mapping =
        renderer.copy_framebuffer(target, Rectangle::from_size(size), Fourcc::Xrgb8888)?;
    let pixels = renderer.map_texture(&mapping)?;

    // XRGB8888 is stored little-endian: blue, green, red, padding.
    let rgb: Vec<u8> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| [px[2], px[1], px[0]])
        .collect();
    let mut file = BufWriter::new(File::create(path)?);
    png::write_rgb(&mut file, size.w as u32, size.h as u32, &rgb)?;
    eprintln!("sengi-compositor: wrote {}", path.display());
    Ok(())
}
