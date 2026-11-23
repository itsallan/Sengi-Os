//! A backend with no display and no input devices.
//!
//! It renders with pixman into memory whenever something changed, at most
//! once per frame interval, which is enough to run clients in CI and to save
//! a screenshot of what a real output would show. Input comes from a script.

use std::{
    cell::RefCell,
    collections::VecDeque,
    error::Error,
    fs,
    io::BufWriter,
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            pixman::{PixmanRenderer, PixmanTarget},
            Bind, ExportMem, Offscreen,
        },
    },
    output::{Mode as OutputMode, Output},
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
    outputs::{HeadPlan, OutputSpec},
    png, render,
    script::{self, Action, Step},
    state::{State, TestOutput, REFRESH_MHZ},
    Options,
};

/// The shortest time between two frames, as on a 60 Hz panel.
const FRAME: Duration = Duration::from_micros(16_667);

/// One screen that does not exist: what it is drawn into.
struct Screen {
    image: Image<'static, 'static>,
    size: Size<i32, Buffer>,
    damage: OutputDamageTracker,
    output: Output,
}

struct Headless {
    renderer: PixmanRenderer,
    screens: Vec<Screen>,
    /// What is left of the script, soonest step first.
    steps: VecDeque<Step>,
    started: Instant,
    last_frame: Option<Instant>,
    /// A timer is already waiting for the frame interval to pass.
    frame_pending: bool,
    /// How many screens have been made, for their names.
    made: u32,
}

type Shared = Rc<RefCell<Headless>>;

impl Headless {
    fn screen_for(&mut self, output: &Output) -> Result<Screen, Box<dyn Error>> {
        let mode = output.current_mode().ok_or("the output has no mode")?;
        let size = Size::from((mode.size.w, mode.size.h));
        let image = self.renderer.create_buffer(Fourcc::Xrgb8888, size)?;
        Ok(Screen {
            image,
            size,
            damage: OutputDamageTracker::from_output(output),
            output: output.clone(),
        })
    }
}

/// The sizes `--outputs` takes: `1920x1080`, or `1920x1080:panel` for one
/// that has a list of modes of its own like a real display.
pub fn parse_outputs(text: &str) -> Result<Vec<(i32, i32, bool)>, String> {
    let err = |item: &str| {
        format!("invalid output: {item} (expected WIDTHxHEIGHT or WIDTHxHEIGHT:panel)")
    };
    let mut found = Vec::new();
    for item in text.split(',') {
        let (size, panel) = match item.split_once(':') {
            Some((size, "panel")) => (size, true),
            Some(_) => return Err(err(item)),
            None => (item, false),
        };
        let (w, h) = size.split_once('x').ok_or_else(|| err(item))?;
        let (w, h): (i32, i32) = (
            w.parse().map_err(|_| err(item))?,
            h.parse().map_err(|_| err(item))?,
        );
        if !crate::outputs::size_is_sane(w, h) {
            return Err(err(item));
        }
        found.push((w, h, panel));
    }
    if found.len() > crate::outputs::MAX_HEADS {
        return Err("too many outputs".into());
    }
    Ok(found)
}

/// What a screen of `size` offers: a virtual one takes any size; a panel
/// has a few modes.
fn spec_for(name: &str, size: (i32, i32), panel: bool, transform: Transform) -> OutputSpec {
    let mut spec = OutputSpec::virtual_screen(
        name,
        if panel { "Panel" } else { "Headless" },
        size.into(),
        REFRESH_MHZ,
        transform,
    );
    if panel {
        let mode = |w: i32, h: i32, refresh: i32| OutputMode {
            size: (w, h).into(),
            refresh,
        };
        spec.modes = vec![
            mode(size.0, size.1, REFRESH_MHZ),
            mode(size.0, size.1, REFRESH_MHZ / 2),
            mode(
                (size.0 * 2 / 3).max(1),
                (size.1 * 2 / 3).max(1),
                REFRESH_MHZ,
            ),
        ];
        spec.preferred = Some(spec.modes[0]);
        spec.current = spec.modes[0];
        spec.any_size = false;
        spec.physical = (size.0 * 25 / 100, size.1 * 25 / 100);
        spec.make = "Sengi OS".into();
        spec.serial = format!("{}-{}", size.0, size.1);
    }
    spec
}

pub fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let steps = steps(&options)?;

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display = Display::new()?;
    let mut state = State::new(&mut event_loop, display)?;
    state.set_output_scale(f64::from(options.scale_120) / 120.0);

    let sizes = if options.outputs.is_empty() {
        vec![(options.size.0, options.size.1, false)]
    } else {
        options.outputs.clone()
    };
    let renderer = PixmanRenderer::new()?;
    let first_step = steps.front().map(|step| step.at);
    let backend: Shared = Rc::new(RefCell::new(Headless {
        renderer,
        screens: Vec::new(),
        steps,
        started: Instant::now(),
        last_frame: None,
        frame_pending: false,
        made: 0,
    }));
    for (width, height, panel) in sizes {
        let name = {
            let mut backend = backend.borrow_mut();
            backend.made += 1;
            format!("HEADLESS-{}", backend.made)
        };
        let output = state.add_head(spec_for(&name, (width, height), panel, Transform::Normal));
        if state.head(&output).is_some_and(|head| head.enabled) {
            let mut headless = backend.borrow_mut();
            let screen = headless.screen_for(&output)?;
            headless.screens.push(screen);
        }
    }

    // The sizes, modes and switching on and off the protocol asks for.
    let changing = backend.clone();
    state.output_backend = Some(Box::new(move |state, plan| {
        change_screens(&changing, state, plan)
    }));

    // Screens plugged in and pulled out by the tests.
    let plugging = backend.clone();
    state.test_outputs = Some(Box::new(move |state, request| {
        plug(&plugging, state, request)
    }));

    // A picture for whoever asks: the frame drawn once more, apart from
    // the buffer the ordinary frames go into.
    let pictured = backend.clone();
    state.capture = Some(Box::new(move |state, output| {
        let mut backend = pictured.borrow_mut();
        let mode = output.current_mode()?;
        let size = Size::from((mode.size.w, mode.size.h));
        let renderer = &mut backend.renderer;
        let mut damage = OutputDamageTracker::from_output(output);
        let mut image: Image<'static, 'static> =
            renderer.create_buffer(Fourcc::Xrgb8888, size).ok()?;
        let mut target = renderer.bind(&mut image).ok()?;
        render::output(state, output, renderer, &mut target, 0, &mut damage).ok()?;
        let rgb = read_rgb(renderer, &target, size).ok()?;
        Some((size.w, size.h, rgb))
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
        crate::say!("frame failed: {err}");
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

        match step.action {
            // The picture has to show the steps before it, so draw now
            // whether or not a frame was due.
            Action::Screenshot(path) => draw(backend, state, Some(&path)),
            Action::Exit => {
                state.loop_signal.stop();
                return TimeoutAction::Drop;
            }
            input => state.perform(&input, time),
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

/// Puts the screens as `plan` wants them: made, resized or dropped. All
/// or nothing: the new set is made first and only then replaces the old.
fn change_screens(backend: &Shared, _state: &mut State, plan: &[HeadPlan]) -> Result<(), String> {
    let mut backend = backend.borrow_mut();
    let mut screens = Vec::new();
    for head in plan.iter().filter(|head| head.enabled) {
        let size = Size::from((head.mode.size.w, head.mode.size.h));
        let kept = backend
            .screens
            .iter()
            .position(|screen| screen.output == head.output && screen.size == size);
        match kept {
            Some(at) => {
                let screen = backend.screens.remove(at);
                screens.push(screen);
            }
            None => {
                let image = backend
                    .renderer
                    .create_buffer(Fourcc::Xrgb8888, size)
                    .map_err(|err| {
                        format!("cannot make a buffer of {}x{}: {err}", size.w, size.h)
                    })?;
                screens.push(Screen {
                    image,
                    size,
                    damage: OutputDamageTracker::from_output(&head.output),
                    output: head.output.clone(),
                });
            }
        }
    }
    backend.screens = screens;
    Ok(())
}

/// A test plugs in a screen or pulls one out.
fn plug(backend: &Shared, state: &mut State, request: TestOutput) -> Result<String, String> {
    match request {
        TestOutput::Plug { size, panel, name } => {
            let name = {
                let mut backend = backend.borrow_mut();
                name.unwrap_or_else(|| {
                    backend.made += 1;
                    format!("HEADLESS-{}", backend.made)
                })
            };
            if state.heads().iter().any(|head| head.output.name() == name) {
                return Err(format!("{name} is there already"));
            }
            let output = state.add_head(spec_for(&name, size, panel, Transform::Normal));
            if state.head(&output).is_some_and(|head| head.enabled) {
                let mut headless = backend.borrow_mut();
                let screen = headless
                    .screen_for(&output)
                    .map_err(|err| err.to_string())?;
                headless.screens.push(screen);
            }
            state.request_redraw();
            Ok(name)
        }
        TestOutput::Unplug { name } => {
            let output = state
                .heads()
                .iter()
                .map(|head| head.output.clone())
                .find(|output| output.name() == name)
                .ok_or_else(|| format!("no output {name}"))?;
            backend
                .borrow_mut()
                .screens
                .retain(|screen| screen.output != output);
            state.remove_output(&output);
            Ok(name)
        }
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
        let epoch = state.lock_epoch();
        let mut presented = Vec::new();
        let mut pictures: Vec<(i32, Size<i32, Buffer>, Vec<u8>)> = Vec::new();
        for screen in &mut self.screens {
            let mut target = self.renderer.bind(&mut screen.image)?;
            // The image is reused, so it always holds the previous frame
            // and only damaged regions are painted.
            let rendered = render::output(
                state,
                &screen.output,
                &mut self.renderer,
                &mut target,
                1,
                &mut screen.damage,
            )?;
            if screenshot.is_some() {
                let at = state
                    .space
                    .output_geometry(&screen.output)
                    .map_or(0, |geo| geo.loc.x);
                pictures.push((
                    at,
                    screen.size,
                    read_rgb(&mut self.renderer, &target, screen.size)?,
                ));
            }
            drop(target);
            presented.push((screen.output.clone(), rendered.states));
        }
        if let Some(path) = screenshot {
            save_all(&pictures, path)?;
        }

        self.last_frame = Some(Instant::now());
        for (output, states) in presented {
            render::presented(state, &output, &states, epoch);
        }
        Ok(())
    }
}

/// Writes the pictures of the screens as one, side by side in the order of
/// their places on the desktop.
fn save_all(
    pictures: &[(i32, Size<i32, Buffer>, Vec<u8>)],
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let mut order: Vec<&(i32, Size<i32, Buffer>, Vec<u8>)> = pictures.iter().collect();
    order.sort_by_key(|(at, _, _)| *at);
    let width: i32 = order.iter().map(|(_, size, _)| size.w).sum();
    let height = order.iter().map(|(_, size, _)| size.h).max().unwrap_or(0);
    let mut all = vec![0u8; (width * height * 3) as usize];
    let mut left = 0;
    for (_, size, rgb) in order {
        let row = (size.w * 3) as usize;
        for (y, pixels) in rgb.chunks_exact(row).enumerate() {
            let at = (y * width as usize + left as usize) * 3;
            all[at..at + row].copy_from_slice(pixels);
        }
        left += size.w;
    }
    let mut file = BufWriter::new(png::create(path)?);
    png::write_rgb(&mut file, width as u32, height as u32, &all)?;
    crate::say!("wrote {}", path.display());
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
