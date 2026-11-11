//! Drawing an output, shared by every backend.

use std::{error::Error, time::Duration};

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker,
        element::{
            memory::MemoryRenderBufferRenderElement, render_elements,
            solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement, AsRenderElements,
            Kind,
        },
        ImportAll, ImportMem, Renderer, Texture,
    },
    desktop::{layer_map_for_output, Window},
    output::Output,
    utils::{Logical, Point, Scale},
    wayland::shell::wlr_layer::Layer,
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
) -> Result<(), Box<dyn Error>>
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

    // Front to back: overlay and top layers, windows, bottom and background.
    let mut elements: Vec<OutputElement<R>> = Vec::new();
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

    damage
        .render_output(renderer, target, age, &elements, state.desktop_color())
        .map_err(|err| format!("{err:?}"))?;
    Ok(())
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

    let sides = [
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

/// Tells every visible surface that a frame was shown, so clients that wait
/// for frame callbacks draw their next one.
pub fn send_frames(state: &State, output: &Output) {
    let time = state.start_time.elapsed();
    let throttle = Some(Duration::ZERO);

    for window in state.space.elements() {
        window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
    }
    for layer in layer_map_for_output(output).layers() {
        layer.send_frame(output, time, throttle, |_, _| Some(output.clone()));
    }
}
