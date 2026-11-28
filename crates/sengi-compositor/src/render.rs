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
            utils::CropRenderElement,
            AsRenderElements, Element, Kind, RenderElementStates,
        },
        Color32F, ImportAll, ImportMem, Renderer, Texture,
    },
    desktop::{
        layer_map_for_output,
        utils::{
            send_frames_surface_tree, surface_primary_scanout_output,
            update_surface_primary_scanout_output, with_surfaces_surface_tree,
        },
        PopupManager, Window,
    },
    input::pointer::{CursorImageStatus, CursorImageSurfaceData},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Logical, Physical, Point, Rectangle, Scale, Size},
    wayland::{
        compositor::{with_states, SurfaceData},
        seat::WaylandFocus,
        shell::wlr_layer::Layer,
    },
};

use crate::{
    autohide::Shifts,
    corners::{Notch, Piece},
    outputs::scale_key,
    state::State,
    window,
};

render_elements! {
    pub OutputElement<R> where R: ImportAll + ImportMem;
    Surface=WaylandSurfaceRenderElement<R>,
    Memory=MemoryRenderBufferRenderElement<R>,
    Solid=SolidColorRenderElement,
    // A client with its bottom corners cut off: the part above them, and
    // the pieces beside them.
    Cropped=CropRenderElement<WaylandSurfaceRenderElement<R>>,
    Piece=Piece<WaylandSurfaceRenderElement<R>>,
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
        if let Some(surface) = state.lock_surface_on(output) {
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

    // What a drag carries along, right under the cursor.
    if let Some(icon) = state.drag_icon.as_ref().filter(|icon| icon.alive()) {
        let pointer = state.seat.get_pointer().expect("seat has a pointer");
        let at = (pointer.current_location() - origin.to_f64()).to_i32_round::<i32>();
        elements.extend(render_elements_from_surface_tree(
            renderer,
            icon,
            at.to_physical_precise_round(scale),
            scale,
            1.0,
            Kind::Unspecified,
        ));
    }

    // The overlay layer; a window in front that fills its screen; the
    // top layer, where the bar and the dock are; the other windows; the
    // bottom and background layers.
    let filling = state.fullscreen_in_front();
    // The bar and the dock may be sliding, or out of sight, over a
    // maximised window.
    let shifts = state.panel_shifts(output, false);
    layers(
        output,
        &[Layer::Overlay],
        shifts,
        renderer,
        scale,
        &mut elements,
    );
    // The window switcher or a menu, above the windows and the bar.
    elements.extend(state.overlay_element(output, renderer));
    // Where a window dragged to a screen edge will land.
    elements.extend(state.snap_hint_element(output, renderer));
    // `None` stands for the top layer.
    let mut order: Vec<Option<&Window>> = Vec::new();
    order.extend(filling.as_ref().map(Some));
    order.push(None);
    let behind = |window: &&Window| Some(*window) != filling.as_ref();
    order.extend(state.space.elements().rev().filter(behind).map(Some));
    for item in order {
        let Some(window) = item else {
            layers(
                output,
                &[Layer::Top],
                shifts,
                renderer,
                scale,
                &mut elements,
            );
            continue;
        };
        let Some(location) = state.space.element_location(window) else {
            continue;
        };
        let location = location - origin;
        // A title bar that moved below the bar lies over the client;
        // otherwise it is part of the frame behind it.
        let drop = state.titlebar_drop(window);
        let titled = location + Point::from((0, drop));
        if drop > 0 {
            titlebar(state, window, titled, renderer, scale, &mut elements);
        }
        // The rounded bottom corners go in front of the client's own area.
        frame_corners(state, window, location, renderer, scale, &mut elements);
        client(window, location, renderer, scale, &mut elements);
        if drop == 0 {
            titlebar(state, window, location, renderer, scale, &mut elements);
        }
        frame(state, window, location, scale, &mut elements);
        // Last, so it is behind the window and in front of those below.
        shadow(state, window, location, renderer, scale, &mut elements);
    }
    layers(
        output,
        &[Layer::Bottom, Layer::Background],
        shifts,
        renderer,
        scale,
        &mut elements,
    );
    (elements, state.desktop_color())
}

fn layers<R>(
    output: &Output,
    kinds: &[Layer],
    shifts: Shifts,
    renderer: &mut R,
    scale: Scale<f64>,
    elements: &mut Vec<OutputElement<R>>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + Texture + 'static,
{
    let map = layer_map_for_output(output);
    for kind in kinds {
        for layer in map.layers_on(*kind).rev() {
            let (Some(geometry), Some(shift)) =
                (map.layer_geometry(layer), shifts.of(layer.namespace()))
            else {
                continue;
            };
            elements.extend(layer.render_elements::<OutputElement<R>>(
                renderer,
                (geometry.loc + Point::from((0, shift))).to_physical_precise_round(scale),
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
    if !state.draw_cursor {
        return;
    }
    let pointer = state.seat.get_pointer().expect("seat has a pointer");
    let location = pointer.current_location() - origin.to_f64();
    match state.cursor() {
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Named(icon) => {
            // The sprite is in the screen's pixels already, so its hotspot
            // is taken off in those too.
            let sprite = state.painter(scale.x).cursors.get(icon);
            let hotspot = Point::<i32, Physical>::from((sprite.hotspot.0, sprite.hotspot.1));
            let element = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (location.to_physical_precise_round(scale) - hotspot).to_f64(),
                &sprite.buffer,
                None,
                None,
                None,
                Kind::Cursor,
            );
            match element {
                Ok(element) => elements.push(OutputElement::Memory(element)),
                Err(err) => crate::say!("cannot draw the cursor: {err:?}"),
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

/// Adds what the client of `window` draws, its popups in front. Under a
/// frame with rounded bottom corners the client's own corners are left out.
fn client<R>(
    window: &Window,
    location: Point<i32, Logical>,
    renderer: &mut R,
    scale: Scale<f64>,
    elements: &mut Vec<OutputElement<R>>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Clone + Send + Texture + 'static,
{
    // Surface coordinates start at the buffer, not at the window geometry
    // the location refers to.
    let origin: Point<i32, Physical> =
        (location - window.geometry().loc).to_physical_precise_round(scale);
    let data = window::data(window);
    let slot = data.frame.borrow();
    let cut = slot
        .get(&scale_key(scale.x))
        .filter(|buffers| data.decorated.get() && buffers.bottom_corners.is_some())
        .zip(window.wl_surface());
    let Some((buffers, surface)) = cut else {
        elements.extend(window.render_elements::<OutputElement<R>>(renderer, origin, scale, 1.0));
        return;
    };

    // Popups reach where they like, corners included.
    if window.toplevel().is_some() {
        for (popup, offset) in PopupManager::popups_for_surface(&surface) {
            let offset = (window.geometry().loc + offset - popup.geometry().loc)
                .to_physical_precise_round(scale);
            elements.extend(render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                origin + offset,
                scale,
                1.0,
                Kind::Unspecified,
            ));
        }
    }

    // In the screen's pixels, from where the corner pictures are put.
    let right_corner: Point<i32, Physical> = Point::<i32, Logical>::from((
        location.x - buffers.border + buffers.bottom_right,
        location.y + buffers.bottom_top,
    ))
    .to_f64()
    .to_physical_precise_round(scale);
    let left_edge: Point<i32, Physical> = location.to_f64().to_physical_precise_round(scale);
    let notch = Notch {
        left: left_edge.x,
        top: right_corner.y,
        right: right_corner.x + buffers.corner_pixels - buffers.border_pixels,
        insets: &buffers.insets,
        border: buffers.border_pixels,
    };
    let mut ids = data.corner_ids.borrow_mut();
    for (index, piece) in notch.pieces().into_iter().enumerate() {
        let tree: Vec<WaylandSurfaceRenderElement<R>> = render_elements_from_surface_tree(
            renderer,
            &surface,
            origin,
            scale,
            1.0,
            Kind::Unspecified,
        );
        for element in tree {
            let Some(element) = CropRenderElement::from_element(element, scale, piece) else {
                continue;
            };
            elements.push(if index == 0 {
                OutputElement::Cropped(element)
            } else {
                OutputElement::Piece(Piece {
                    id: ids.get(element.id(), index),
                    element,
                })
            });
        }
    }
}

/// Adds the title bar of `window`, whose client area is at `location` on
/// the output, or would be if the bar had not moved down.
fn titlebar<R>(
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
    state.refresh_frame(window, size.w, size.h, scale.x);
    let slot = data.frame.borrow();
    let Some(buffers) = slot.get(&scale_key(scale.x)) else {
        return;
    };

    let metrics = &state.theme.metrics;
    let at = |x: i32, y: i32| Point::<i32, Logical>::from((location.x + x, location.y + y));
    let bar = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        at(-metrics.border, -metrics.titlebar)
            .to_f64()
            .to_physical_precise_round(scale),
        &buffers.titlebar,
        None,
        None,
        Some(Size::from(buffers.titlebar_size)),
        Kind::Unspecified,
    );
    match bar {
        Ok(element) => elements.push(OutputElement::Memory(element)),
        Err(err) => crate::say!("cannot draw a title bar: {err:?}"),
    }
    // The logical width of the bar, as the frame is laid out in.
    let painted = buffers.titlebar_size.0;
    elements.push(OutputElement::Solid(SolidColorRenderElement::from_buffer(
        &buffers.rest,
        at(painted - metrics.border, -metrics.titlebar).to_physical_precise_round(scale),
        scale,
        1.0,
        Kind::Unspecified,
    )));
}

/// Adds the border of `window`, whose client area is at `location` on the
/// output.
fn frame<R>(
    state: &State,
    window: &Window,
    location: Point<i32, Logical>,
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
    state.refresh_frame(window, size.w, size.h, scale.x);
    let slot = data.frame.borrow();
    let Some(buffers) = slot.get(&scale_key(scale.x)) else {
        return;
    };

    let metrics = &state.theme.metrics;
    let at = |x: i32, y: i32| Point::<i32, Logical>::from((location.x + x, location.y + y));
    let bottom_x = if buffers.bottom_corners.is_some() {
        buffers.bottom_corner
    } else {
        0
    };
    let sides = [
        (&buffers.left, at(-metrics.border, 0)),
        (&buffers.right, at(size.w, 0)),
        // Drawn after the client, so it is behind it. Rounded corners draw
        // their own part of it.
        (&buffers.bottom, at(bottom_x - metrics.border, size.h)),
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

/// Adds the shadow around the frame of `window`: eight pieces from the
/// pictures shared at this scale. None for a window that fills the screen.
fn shadow<R>(
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
    if !data.decorated.get() || data.restore.get().is_some() {
        return;
    }
    let Some(set) = state.shadow_set(scale.x) else {
        return;
    };
    let style = &state.theme.shadow;
    let metrics = &state.theme.metrics;
    let size = window.geometry().size;
    let active = state.focus_stack.current() == Some(window);
    let mut cache = data.shadows.borrow_mut();
    let buffers = cache
        .entry((scale_key(scale.x), active))
        .or_insert_with(|| set.buffers(active));
    let (frame_w, frame_h) = (
        size.w.saturating_add(2 * metrics.border),
        metrics.titlebar + size.h + metrics.border,
    );
    let (side, reach) = (set.side, set.reach);
    let (spread, drop) = (style.spread, style.offset);
    let strip_w = frame_w - 2 * reach;
    let strip_h = frame_h - 2 * reach;
    let origin = location - Point::from((metrics.border, metrics.titlebar));
    let (right, below) = (frame_w - reach, drop + frame_h - reach);
    let parts = [
        (-spread, drop - spread, side, side),
        (right, drop - spread, side, side),
        (-spread, below, side, side),
        (right, below, side, side),
        (reach, drop - spread, strip_w, side),
        (reach, below, strip_w, side),
        (-spread, drop + reach, side, strip_h),
        (right, drop + reach, side, strip_h),
    ];
    for (index, (buffer, (x, y, w, h))) in buffers.iter().zip(parts).enumerate() {
        if w <= 0 || h <= 0 {
            continue;
        }
        let at = origin + Point::from((x, y));
        let element = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            at.to_f64().to_physical_precise_round(scale),
            buffer,
            None,
            Some(set.source(index)),
            Some(Size::from((w, h))),
            Kind::Unspecified,
        );
        match element {
            Ok(element) => elements.push(OutputElement::Memory(element)),
            Err(err) => crate::say!("cannot draw a shadow: {err:?}"),
        }
    }
}

/// The rounded bottom corners of `window`'s frame, which go in front of its
/// client. Nothing when the corners are square.
fn frame_corners<R>(
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
    state.refresh_frame(window, size.w, size.h, scale.x);
    let slot = data.frame.borrow();
    let Some(buffers) = slot.get(&scale_key(scale.x)) else {
        return;
    };
    let Some(corners) = buffers.bottom_corners.as_ref() else {
        return;
    };
    let border = state.theme.metrics.border;
    let left = Point::<i32, Logical>::from((location.x - border, location.y + buffers.bottom_top));
    for (corner, offset) in corners.iter().zip([0, buffers.bottom_right]) {
        let at = left + Point::from((offset, 0));
        let element = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            at.to_f64().to_physical_precise_round(scale),
            corner,
            None,
            None,
            Some(Size::from((buffers.bottom_corner, buffers.bottom_corner))),
            Kind::Unspecified,
        );
        match element {
            Ok(element) => elements.push(OutputElement::Memory(element)),
            Err(err) => crate::say!("cannot draw the window corners: {err:?}"),
        }
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
///
/// `epoch` is `State::lock_epoch` as it was when the frame was drawn.
pub fn presented(
    state: &mut State,
    output: &Output,
    states: &RenderElementStates,
    epoch: Option<u64>,
) {
    let seen = |surface: &WlSurface, data: &SurfaceData| {
        update_surface_primary_scanout_output(
            surface,
            output,
            data,
            states,
            default_primary_scanout_output_compare,
        );
    };
    // Everything is asked, also what cannot be in this frame: a window
    // that was visible and is now minimised or behind the lock must stop
    // counting as visible, or it would keep getting callbacks at full rate.
    for surface in state.lock_surfaces() {
        with_surfaces_surface_tree(&surface, seen);
    }
    for window in state.space.elements().chain(state.minimised.iter()) {
        window.with_surfaces(seen);
    }
    for layer in layer_map_for_output(output).layers() {
        layer.with_surfaces(seen);
    }
    if let CursorImageStatus::Surface(surface) = state.cursor() {
        with_surfaces_surface_tree(&surface, seen);
    }

    send_frames(state, output);
    state.frame_presented(output, epoch);
}

/// Sends frame callbacks: at once to surfaces that were visible on `output`
/// in the last frame, and at most every `HIDDEN_FRAME_INTERVAL` to the rest.
pub fn send_frames(state: &State, output: &Output) {
    let time = state.start_time.elapsed();
    let throttle = Some(HIDDEN_FRAME_INTERVAL);

    // Under the lock nothing else is on screen, and a minimised window
    // never is. Their clients still get the slow tick: one that waits for
    // a frame callback before it reads its socket must not hang.
    for surface in state.lock_surfaces() {
        send_frames_surface_tree(
            &surface,
            output,
            time,
            throttle,
            surface_primary_scanout_output,
        );
    }
    for window in state.pending_windows.iter() {
        window.send_frame(output, time, throttle, surface_primary_scanout_output);
    }
    for window in state.space.elements().chain(state.minimised.iter()) {
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
