//! A backend with no display and no input devices.
//!
//! It renders with pixman into memory whenever something changed, at most
//! once per frame interval, which is enough to run clients in CI and to save
//! a screenshot of what a real output would show. Input comes from a script.

use std::{
    cell::RefCell,
    collections::VecDeque,
    error::Error,
    fs::{self, File},
    io::BufWriter,
    path::Path,
    rc::Rc,
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
            ping::make_ping,
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
    state::{State, REFRESH_MHZ},
    Options,
};

/// The shortest time between two frames, as on a 60 Hz panel.
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
    last_frame: Option<Instant>,
    /// A timer is already waiting for the frame interval to pass.
    frame_pending: bool,
}

type Shared = Rc<RefCell<Headless>>;

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
        REFRESH_MHZ,
        Transform::Normal,
    );

    let mut renderer = PixmanRenderer::new()?;
    let size = Size::from((width, height));
    let image = renderer.create_buffer(Fourcc::Xrgb8888, size)?;
    let first_step = steps.front().map(|step| step.at);
    let backend: Shared = Rc::new(RefCell::new(Headless {
        renderer,
        image,
        size,
        damage: OutputDamageTracker::from_output(&output),
        output,
        steps,
        started: Instant::now(),
        last_frame: None,
        frame_pending: false,
    }));

    // Nothing runs on a clock: a frame is drawn when the state asks for one.
    let handle = event_loop.handle();
    let (ping, wakeups) = make_ping()?;
    let on_wake = backend.clone();
    handle.insert_source(wakeups, move |_, _, state| schedule(&on_wake, state))?;
    state.set_redraw_ping(ping);

    if let Some(at) = first_step {
        let scripted = backend.clone();
        handle.insert_source(Timer::from_duration(at), move |_, _, state| {
            play(&scripted, state)
        })?;
    }

    crate::start_clients(&mut state, &options);
    state.request_redraw();
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

/// A frame was asked for: draw it now, or once the frame interval is over.
fn schedule(backend: &Shared, state: &mut State) {
    if !state.needs_redraw {
        return;
    }
    let wait = {
        let mut backend = backend.borrow_mut();
        if backend.frame_pending {
            return;
        }
        let since = backend.last_frame.map(|at| at.elapsed());
        match since {
            Some(since) if since < FRAME => {
                backend.frame_pending = true;
                Some(FRAME - since)
            }
            _ => None,
        }
    };

    match wait {
        None => draw(backend, state, None),
        Some(wait) => {
            let later = backend.clone();
            let timer = Timer::from_duration(wait);
            let armed = state.loop_handle.insert_source(timer, move |_, _, state| {
                later.borrow_mut().frame_pending = false;
                if state.needs_redraw {
                    draw(&later, state, None);
                }
                TimeoutAction::Drop
            });
            if armed.is_err() {
                backend.borrow_mut().frame_pending = false;
            }
        }
    }
}

fn draw(backend: &Shared, state: &mut State, screenshot: Option<&Path>) {
    if let Err(err) = backend.borrow_mut().frame(state, screenshot) {
        eprintln!("sengi-compositor: frame failed: {err}");
        state.failed = true;
        state.loop_signal.stop();
    }
}

/// Plays the script steps that are due and says when the next one is.
fn play(backend: &Shared, state: &mut State) -> TimeoutAction {
    loop {
        let (step, time) = {
            let mut backend = backend.borrow_mut();
            let now = backend.started.elapsed();
            if backend.steps.front().is_none_or(|step| step.at > now) {
                break;
            }
            (backend.steps.pop_front(), now.as_millis() as u32)
        };
        let Some(step) = step else { break };

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
            // The picture has to show the steps before it, so draw now
            // whether or not a frame was due.
            Action::Screenshot(path) => draw(backend, state, Some(&path)),
            Action::Exit => {
                state.loop_signal.stop();
                return TimeoutAction::Drop;
            }
        }
    }

    let backend = backend.borrow();
    match backend.steps.front() {
        Some(step) => {
            let wait = step.at.saturating_sub(backend.started.elapsed());
            TimeoutAction::ToDuration(wait.max(Duration::from_millis(1)))
        }
        None => TimeoutAction::Drop,
    }
}

impl Headless {
    /// Draws what changed since the last frame and tells clients about it.
    fn frame(
        &mut self,
        state: &mut State,
        screenshot: Option<&Path>,
    ) -> Result<(), Box<dyn Error>> {
        state.needs_redraw = false;
        let mut target = self.renderer.bind(&mut self.image)?;
        // The image is reused, so it always holds the previous frame and
        // only damaged regions are painted.
        let rendered = render::output(
            state,
            &self.output,
            &mut self.renderer,
            &mut target,
            1,
            &mut self.damage,
        )?;
        if let Some(path) = screenshot {
            save(&mut self.renderer, &target, self.size, path)?;
        }
        drop(target);

        self.last_frame = Some(Instant::now());
        render::presented(state, &self.output, &rendered.states);
        Ok(())
    }
}

pub fn save(
    renderer: &mut PixmanRenderer,
    target: &PixmanTarget<'_>,
    size: Size<i32, Buffer>,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let rgb = read_rgb(renderer, target, size)?;
    let mut file = BufWriter::new(File::create(path)?);
    png::write_rgb(&mut file, size.w as u32, size.h as u32, &rgb)?;
    eprintln!("sengi-compositor: wrote {}", path.display());
    Ok(())
}

/// What was drawn into `target`, as red, green and blue bytes.
pub fn read_rgb(
    renderer: &mut PixmanRenderer,
    target: &PixmanTarget<'_>,
    size: Size<i32, Buffer>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mapping =
        renderer.copy_framebuffer(target, Rectangle::from_size(size), Fourcc::Xrgb8888)?;
    let pixels = renderer.map_texture(&mapping)?;

    // XRGB8888 is stored little-endian: blue, green, red, padding.
    Ok(pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|px| [px[2], px[1], px[0]])
        .collect())
}
