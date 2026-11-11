//! wlr-layer-shell handling: the bar, dock, launcher and notifications.

use smithay::{
    delegate_layer_shell,
    desktop::{layer_map_for_output, LayerSurface, WindowSurfaceType},
    output::Output,
    reexports::wayland_server::protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{
            Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
            WlrLayerShellState,
        },
    },
};

use crate::state::State;

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.output());
        let Some(output) = output else {
            surface.send_close();
            return;
        };
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            eprintln!("sengi-compositor: cannot map layer surface: {err}");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        for output in self.space.outputs() {
            let mut map = layer_map_for_output(output);
            let layer = map
                .layers()
                .find(|layer| layer.layer_surface() == &surface)
                .cloned();
            if let Some(layer) = layer {
                map.unmap_layer(&layer);
            }
        }
        // The launcher closing hands the keyboard back to the window, the
        // bar closing gives its space back.
        self.update_keyboard_focus();
        self.usable_area_changed();
        self.request_redraw();
    }
}

impl State {
    /// Re-arranges the layers of the output `surface` sits on and answers the
    /// surface's first commit with a configure.
    pub fn layer_commit(&mut self, surface: &WlSurface) {
        let Some(output) = self.output_for_layer(surface) else {
            return;
        };
        let usable_before = self.usable_area();
        {
            let mut map = layer_map_for_output(&output);
            // Anchors, size and exclusive zone are double-buffered, so the
            // usable area can change on any commit.
            map.arrange();

            let configured = with_states(surface, |states| {
                states
                    .data_map
                    .get::<LayerSurfaceData>()
                    .is_some_and(|data| data.lock().unwrap().initial_configure_sent)
            });
            if !configured {
                if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                    layer.layer_surface().send_configure();
                }
            }
        }
        // Keyboard interactivity is double-buffered too.
        self.update_keyboard_focus();
        if self.usable_area() != usable_before {
            self.usable_area_changed();
        }
    }

    fn output_for_layer(&self, surface: &WlSurface) -> Option<Output> {
        self.space
            .outputs()
            .find(|output| {
                layer_map_for_output(output)
                    .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                    .is_some()
            })
            .cloned()
    }
}

delegate_layer_shell!(State);
