//! Drawing an output, shared by every backend.

use std::{error::Error, time::Duration};

use smithay::{
    backend::renderer::{
        damage::{OutputDamageTracker, RenderOutputResult},
        element::{
            default_primary_scanout_output_compare,
            memory::MemoryRenderBufferRenderElement,
            render_elements,
            solid::SolidColorRenderElement,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            AsRenderElements, Kind, RenderElementStates,
        },
        Color32F, ImportAll, ImportMem, Renderer, Texture,
    },
    desktop::{
        layer_map_for_output,
        utils::{
            send_frames_surface_tree, surface_primary_scanout_output,
            update_surface_primary_scanout_output, with_surfaces_surface_tree,
        },
        Window,
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::{
        compositor::{with_states, SurfaceData},
        shell::wlr_layer::Layer,
    },
};

use crate::{state::State, window};

render_elements! {
    pub OutputElement<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Memory=MemoryRenderBufferRenderElement<R>,
    Solid=SolidColorRenderElement,
}

/// Renders everything visible on `output` into `target`.
///
/// `age` is how many frames old the contents of `target` are, 0 if unknown.
pub fn output<R>(
    state: &State,
    output: &Output,
    renderer: &mut R,
    target: &mut R::Framebuffer<'_>,
    age: usize,
    damage: &mut OutputDamageTracker,
) -> Result<Rendered, Box<dyn Error>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + Texture + 'static,
{
    let (elements, background) = elements(state, output, renderer);
    finish(damage.render_output(renderer, target, age, &elements, background))
}

/// Everything visible on `output`, front to back, and the colour behind it.
pub fn elements<R>(
    state: &State,
    output: &Output,
    renderer: &mut R,
) -> (Vec<OutputElement<R>>, Color32F)
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + Texture + 'static,
{
    let scale = Scale::from(output.current_scale().fractional_scale());
    let origin = state
        .space
        .output_geometry(output)
        .map(|geometry| geometry.loc)
        .unwrap_or_default();

    let mut elements: Vec<OutputElement<R>> = Vec::new();
    cursor(state, origin, renderer, scale, &mut elements);

    if state.is_locked() {
        // Nothing of the session may show: the lock surface if there is
        // one, on a plain background that also covers a locker that has
        // not drawn yet or has died.
        if let Some(surface) = state.lock_surface() {
            elements.extend(render_elements_from_surface_tree(
                renderer,
                &surface,
                (0, 0),
                scale,
                1.0,
                Kind::Unspecified,
            ));
        }
        return (elements, state.color(state.theme.inactive));
    }

    // Overlay and top layers, windows, bottom and background layers.
    layers(
        output,
        [Layer::Overlay, Layer::Top],
        renderer,
        scale,
        &mut elements,
    );
    for window in state.space.elements().rev() {
        let Some(location) = state.space.element_location(window) else {
            continue;
        };
        let location = location - origin;
        // Surface coordinates start at the buffer, not at the window
        // geometry the location refers to.
        let surface_origin = location - window.geometry().loc;
        elements.extend(window.render_elements::<OutputElement<R>>(
            renderer,
            surface_origin.to_physical_precise_round(scale),
            scale,
            1.0,
        ));
        frame(state, window, location, renderer, scale, &mut elements);
    }
    layers(
        output,
        [Layer::Bottom, Layer::Background],
        renderer,
        scale,
        &mut elements,
    );
    (elements, state.desktop_color())
}

fn layers<R>(
    output: &Output,
    kinds: [Layer; 2],
    renderer: &mut R,
    scale: Scale<f64>,
    elements: &mut Vec<OutputElement<R>>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + Texture + 'static,
{
    let map = layer_map_for_output(output);
    for kind in kinds {
        for layer in map.layers_on(kind).rev() {
            let Some(geometry) = map.layer_geometry(layer) else {
                continue;
            };
            elements.extend(layer.render_elements::<OutputElement<R>>(
                renderer,
                geometry.loc.to_physical_precise_round(scale),
                scale,
                1.0,
            ));
        }
    }
}

/// Adds the pointer cursor, which goes above everything else.
fn cursor<R>(
    state: &State,
    origin: Point<i32, Logical>,
    renderer: &mut R,
    scale: Scale<f64>,
    elements: &mut Vec<OutputElement<R>>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + Texture + 'static,
{
    let pointer = state.seat.get_pointer().expect("seat has a pointer");
    let location = pointer.current_location() - origin.to_f64();
    match state.cursor() {
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Named(icon) => {
            let sprite = state.cursors.get(icon);
            let hotspot =
                Point::<f64, Logical>::from((sprite.hotspot.0 as f64, sprite.hotspot.1 as f64));
            let element = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (location - hotspot).to_physical_precise_round(scale),
                &sprite.buffer,
                None,
                None,
                None,
                Kind::Cursor,
            );
            match element {
                Ok(element) => elements.push(OutputElement::Memory(element)),
                Err(err) => eprintln!("sengi-compositor: cannot draw the cursor: {err:?}"),
            }
        }
        CursorImageStatus::Surface(surface) => {
            let hotspot = with_states(&surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|data| data.lock().unwrap().hotspot)
                    .unwrap_or_default()
            });
            let position = location.to_i32_round() - hotspot;
            elements.extend(render_elements_from_surface_tree(
                renderer,
                &surface,
                position.to_physical_precise_round(scale),
                scale,
                1.0,
                Kind::Cursor,
            ));
        }
    }
}

/// Adds the title bar and border of `window`, whose client area is at
/// `location` on the output.
fn frame<R>(
    state: &State,
    window: &Window,
    location: Point<i32, Logical>,
    renderer: &mut R,
    scale: Scale<f64>,
    elements: &mut Vec<OutputElement<R>>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + Texture + 'static,
{
    let data = window::data(window);
    if !data.decorated.get() {
        return;
    }
    let size = window.geometry().size;
    state.refresh_frame(window, size.w, size.h);
    let slot = data.frame.borrow();
    let Some(buffers) = slot.as_ref() else {
        return;
    };

    let metrics = &state.theme.metrics;
    let at = |x: i32, y: i32| Point::<i32, Logical>::from((location.x + x, location.y + y));
    let titlebar = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        at(-metrics.border, -metrics.titlebar)
            .to_f64()
            .to_physical_precise_round(scale),
        &buffers.titlebar,
        None,
        None,
        None,
        Kind::Unspecified,
    );
    match titlebar {
        Ok(element) => elements.push(OutputElement::Memory(element)),
        Err(err) => eprintln!("sengi-compositor: cannot draw a title bar: {err:?}"),
    }

    let painted = buffers.look.0.width;
    let sides = [
        (
            &buffers.rest,
            at(painted - metrics.border, -metrics.titlebar),
        ),
        (&buffers.left, at(-metrics.border, 0)),
        (&buffers.right, at(size.w, 0)),
        (&buffers.bottom, at(-metrics.border, size.h)),
    ];
    for (buffer, position) in sides {
        elements.push(OutputElement::Solid(SolidColorRenderElement::from_buffer(
            buffer,
            position.to_physical_precise_round(scale),
            scale,
            1.0,
            Kind::Unspecified,
        )));
    }
}

/// How often a surface that is not visible still gets a frame callback, so
/// a hidden client keeps ticking slowly instead of stalling or spinning.
pub const HIDDEN_FRAME_INTERVAL: Duration = Duration::from_secs(1);

/// What `output` produced: the regions that were repainted, `None` if
/// nothing changed, and which elements ended up visible.
pub struct Rendered {
    // Only a backend that hands frames to someone else needs the regions.
    #[cfg_attr(not(feature = "winit"), allow(dead_code))]
    pub damage: Option<Vec<Rectangle<i32, Physical>>>,
    pub states: RenderElementStates,
}

fn finish<E: std::fmt::Debug>(
    result: Result<RenderOutputResult<'_>, E>,
) -> Result<Rendered, Box<dyn Error>> {
    let result = result.map_err(|err| format!("{err:?}"))?;
    Ok(Rendered {
        damage: result.damage.cloned(),
        states: result.states,
    })
}

/// To be called once a frame is on its way to the screen: records which
/// surfaces were visible in it and lets their clients draw the next one.
pub fn presented(state: &mut State, output: &Output, states: &RenderElementStates) {
    let seen = |surface: &WlSurface, data: &SurfaceData| {
        update_surface_primary_scanout_output(
            surface,
            output,
            data,
            states,
            default_primary_scanout_output_compare,
        );
    };
    if state.is_locked() {
        if let Some(surface) = state.lock_surface() {
            with_surfaces_surface_tree(&surface, seen);
        }
    } else {
        for window in state.space.elements() {
            window.with_surfaces(seen);
        }
        for layer in layer_map_for_output(output).layers() {
            layer.with_surfaces(seen);
        }
    }
    if let CursorImageStatus::Surface(surface) = state.cursor() {
        with_surfaces_surface_tree(&surface, seen);
    }

    send_frames(state, output);
    state.frame_presented();
}

/// Sends frame callbacks: at once to surfaces that were visible on `output`
/// in the last frame, and at most every `HIDDEN_FRAME_INTERVAL` to the rest.
pub fn send_frames(state: &State, output: &Output) {
    let time = state.start_time.elapsed();
    let throttle = Some(HIDDEN_FRAME_INTERVAL);

    if state.is_locked() {
        if let Some(surface) = state.lock_surface() {
            send_frames_surface_tree(
                &surface,
                output,
                time,
                throttle,
                surface_primary_scanout_output,
            );
        }
        return;
    }

    for window in state.space.elements() {
        window.send_frame(output, time, throttle, surface_primary_scanout_output);
    }
    for layer in layer_map_for_output(output).layers() {
        layer.send_frame(output, time, throttle, surface_primary_scanout_output);
    }
    // Animated client cursors wait for frame callbacks like any surface.
    if let CursorImageStatus::Surface(surface) = state.cursor() {
        send_frames_surface_tree(
            &surface,
            output,
            time,
            throttle,
            surface_primary_scanout_output,
        );
    }
}
