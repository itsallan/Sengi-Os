//! Drawing an output, shared by every backend.

use std::{error::Error, time::Duration};

use smithay::{
    backend::renderer::{
        damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement, ImportAll,
        ImportMem, Renderer, Texture,
    },
    desktop::{layer_map_for_output, space::render_output},
    output::Output,
};

use crate::state::State;

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
    R::TextureId: Clone + Texture + 'static,
{
    render_output::<_, WaylandSurfaceRenderElement<R>, _, _>(
        output,
        renderer,
        target,
        1.0,
        age,
        [&state.space],
        &[],
        damage,
        state.desktop_color(),
    )
    .map_err(|err| format!("{err:?}"))?;
    Ok(())
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
