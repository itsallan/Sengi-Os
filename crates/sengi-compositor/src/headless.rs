//! A backend with no display and no input devices.
//!
//! It renders with pixman into memory at a fixed rate, which is enough to run
//! clients in CI and to save a screenshot of what a real output would show.

use std::{
    error::Error,
    fs::File,
    io::BufWriter,
    path::Path,
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
    output::{Mode, Output, PhysicalProperties, Subpixel},
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

use crate::{png, render, state::State, Options};

const REFRESH_MHZ: i32 = 60_000;
const FRAME: Duration = Duration::from_micros(16_667);

struct Headless {
    renderer: PixmanRenderer,
    image: Image<'static, 'static>,
    size: Size<i32, Buffer>,
    damage: OutputDamageTracker,
    output: Output,
    options: Options,
    started: Instant,
}

pub fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display = Display::new()?;
    let mut state = State::new(&mut event_loop, display)?;

    let (width, height) = options.size;
    let mode = Mode {
        size: (width, height).into(),
        refresh: REFRESH_MHZ,
    };
    let output = Output::new(
        "HEADLESS-1".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Sengi OS".into(),
            model: "Headless".into(),
        },
    );
    let _global = output.create_global::<State>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));

    let mut renderer = PixmanRenderer::new()?;
    let size = Size::from((width, height));
    let image = renderer.create_buffer(Fourcc::Xrgb8888, size)?;
    let mut backend = Headless {
        renderer,
        image,
        size,
        damage: OutputDamageTracker::from_output(&output),
        output,
        options: options.clone(),
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

    crate::start_clients(&state, &options)?;
    event_loop.run(None, &mut state, |state| {
        state.space.refresh();
        state.popups.cleanup();
        let _ = state.display_handle.flush_clients();
    })?;

    if state.failed {
        return Err("rendering failed".into());
    }
    Ok(())
}

impl Headless {
    /// Draws one frame. Returns false once `--exit-after` has passed.
    fn frame(&mut self, state: &mut State) -> Result<bool, Box<dyn Error>> {
        let last = self
            .options
            .exit_after
            .is_some_and(|after| self.started.elapsed() >= after);

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
        if last {
            if let Some(path) = &self.options.screenshot {
                save(&mut self.renderer, &target, self.size, path)?;
            }
        }
        drop(target);

        render::send_frames(state, &self.output);
        Ok(!last)
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
