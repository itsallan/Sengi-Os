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
        let named = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .filter(|output| self.head(output).is_some_and(|head| head.enabled));
        // The bar and the dock that name no output go to the primary one.
        let follows_primary = named.is_none() && FOLLOWS_PRIMARY.contains(&namespace.as_str());
        let output = named
            .or_else(|| follows_primary.then(|| self.primary_output()).flatten())
            .or_else(|| self.active_output());
        let Some(output) = output else {
            surface.send_close();
            return;
        };
        if follows_primary {
            self.primary_layers.push(surface.wl_surface().clone());
        }
        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            crate::say!("cannot map layer surface: {err}");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        self.primary_layers
            .retain(|kept| kept != surface.wl_surface());
        // Also on outputs that are off: a bar can be destroyed after its
        // output was switched off.
        let outputs: Vec<Output> = self
            .heads()
            .iter()
            .map(|head| head.output.clone())
            .collect();
        for output in &outputs {
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

/// The layer surfaces that go to the primary output when they ask for none.
const FOLLOWS_PRIMARY: [&str; 2] = ["sengi-bar", "sengi-dock"];

impl State {
    /// The output `display.primary` names, if it is there and on.
    pub fn primary_output(&self) -> Option<Output> {
        let name = &self.config.display.primary;
        self.heads()
            .iter()
            .find(|head| head.enabled && !name.is_empty() && head.output.name() == *name)
            .map(|head| head.output.clone())
    }

    /// The primary output changed, or was plugged or pulled: the bar and
    /// the dock that took the default go where it now points. Without a
    /// primary they stay where they are.
    pub fn follow_primary(&mut self) {
        let Some(target) = self.primary_output() else {
            return;
        };
        let moving: Vec<WlSurface> = self.primary_layers.clone();
        let mut moved = false;
        for surface in moving {
            let Some(from) = self.output_for_layer(&surface) else {
                continue;
            };
            if from == target {
                continue;
            }
            let layer = layer_map_for_output(&from)
                .layer_for_surface(&surface, WindowSurfaceType::TOPLEVEL)
                .cloned();
            let Some(layer) = layer else { continue };
            layer_map_for_output(&from).unmap_layer(&layer);
            layer_map_for_output(&from).arrange();
            if layer_map_for_output(&target).map_layer(&layer).is_ok() {
                layer_map_for_output(&target).arrange();
                // Told its new size at once: a different screen, a
                // different width.
                layer.layer_surface().send_configure();
                moved = true;
            }
        }
        if moved {
            self.usable_area_changed();
            self.update_keyboard_focus();
            self.request_redraw();
        }
    }

    /// Re-arranges the layers of the output `surface` sits on and answers the
    /// surface's first commit with a configure.
    pub fn layer_commit(&mut self, surface: &WlSurface) {
        let Some(output) = self.output_for_layer(surface) else {
            return;
        };
        let usable_before = self.usable_areas();
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
        if self.usable_areas() != usable_before {
            self.usable_area_changed();
        }
    }

    pub fn output_for_layer(&self, surface: &WlSurface) -> Option<Output> {
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
