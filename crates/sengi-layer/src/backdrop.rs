//! An invisible surface over a whole output, there only to be clicked. A
//! menu uses it to hear of a click anywhere else; see
//! [`Handle::catch_clicks`](crate::Handle::catch_clicks).

use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerSurface,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::Shm;
use wayland_client::protocol::{wl_output, wl_shm, wl_surface};
use wayland_client::QueueHandle;

use crate::backend::State;

pub(crate) struct Backdrop {
    pub output: wl_output::WlOutput,
    /// A button went down on it and has not come up.
    pub pressed: bool,
    /// None once the compositor has closed it.
    layer: Option<LayerSurface>,
    /// The empty frame and the memory it is in, freed with the surface.
    frame: Option<(SlotPool, Buffer)>,
}

impl Backdrop {
    /// Asks for a surface over all of `output`. It is on screen once the
    /// compositor has said how large it is.
    pub(crate) fn new(
        shell: &LayerShell,
        queue: &QueueHandle<State>,
        surface: wl_surface::WlSurface,
        namespace: String,
        output: wl_output::WlOutput,
    ) -> Backdrop {
        let layer =
            shell.create_layer_surface(queue, surface, Layer::Top, Some(namespace), Some(&output));
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        // Over what the bar and the dock keep for themselves too.
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        Backdrop {
            output,
            pressed: false,
            layer: Some(layer),
            frame: None,
        }
    }

    pub(crate) fn is(&self, surface: &wl_surface::WlSurface) -> bool {
        self.layer
            .as_ref()
            .is_some_and(|layer| layer.wl_surface() == surface)
    }

    pub(crate) fn close(&mut self) {
        self.layer = None;
        self.frame = None;
    }

    /// Shows a transparent frame of the size the compositor chose.
    pub(crate) fn configure(&mut self, shm: &Shm, (width, height): (u32, u32)) {
        let Some(layer) = &self.layer else {
            return;
        };
        let (Ok(width), Ok(height)) = (i32::try_from(width), i32::try_from(height)) else {
            return;
        };
        if width == 0 || height == 0 {
            return;
        }
        let bytes = width as usize * height as usize * 4;
        let frame = SlotPool::new(bytes, shm).ok().and_then(|mut pool| {
            let (buffer, canvas) = pool
                .create_buffer(width, height, width * 4, wl_shm::Format::Argb8888)
                .ok()?;
            canvas.fill(0);
            Some((pool, buffer))
        });
        let Some((pool, buffer)) = frame else {
            eprintln!("sengi-layer: could not allocate the click catcher");
            return;
        };
        let surface = layer.wl_surface();
        if buffer.attach_to(surface).is_ok() {
            surface.damage_buffer(0, 0, width, height);
            surface.commit();
            self.frame = Some((pool, buffer));
        }
    }
}
