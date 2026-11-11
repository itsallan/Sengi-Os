//! Turning backend input events into keyboard and pointer events for clients.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent,
    },
    desktop::{layer_map_for_output, WindowSurfaceType},
    input::{
        keyboard::FilterResult,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::shell::wlr_layer::Layer,
};

use crate::state::State;

impl State {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => {
                let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
                keyboard.input::<(), _>(
                    self,
                    event.key_code(),
                    event.state(),
                    SERIAL_COUNTER.next_serial(),
                    event.time_msec(),
                    |_, _, _| FilterResult::Forward,
                );
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output) = self.output() else { return };
                let Some(geometry) = self.space.output_geometry(&output) else {
                    return;
                };
                let location = event.position_transformed(geometry.size) + geometry.loc.to_f64();
                let under = self.surface_under(location);
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location,
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerButton { event } => {
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                if event.state() == ButtonState::Pressed && !pointer.is_grabbed() {
                    self.click_to_focus(pointer.current_location());
                }
                pointer.button(
                    self,
                    &ButtonEvent {
                        button: event.button_code(),
                        state: event.state(),
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
                pointer.frame(self);
            }
            InputEvent::PointerAxis { event } => {
                let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
                for axis in [Axis::Horizontal, Axis::Vertical] {
                    let v120 = event.amount_v120(axis);
                    // Wheels report steps only; 15 px per notch is the usual
                    // conversion.
                    let amount = event
                        .amount(axis)
                        .or_else(|| v120.map(|steps| steps * 15.0 / 120.0))
                        .unwrap_or(0.0);
                    if amount != 0.0 {
                        frame = frame.value(axis, amount);
                        if let Some(steps) = v120 {
                            frame = frame.v120(axis, steps as i32);
                        }
                    } else if event.source() == AxisSource::Finger {
                        frame = frame.stop(axis);
                    }
                }
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            _ => {}
        }
    }

    fn click_to_focus(&mut self, location: Point<f64, Logical>) {
        // A click on the bar or dock must not steal focus from the window.
        if self
            .layer_surface_under(location, &[Layer::Overlay, Layer::Top])
            .is_some()
        {
            return;
        }
        let window = self
            .space
            .element_under(location)
            .map(|(window, _)| window.clone());
        if let Some(window) = window {
            self.focus_window(&window);
        }
    }

    /// The surface at `location` and where its origin is, front to back:
    /// overlay and top layers, windows, then bottom and background layers.
    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.layer_surface_under(location, &[Layer::Overlay, Layer::Top])
            .or_else(|| {
                let (window, origin) = self.space.element_under(location)?;
                window
                    .surface_under(location - origin.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, offset)| (surface, (offset + origin).to_f64()))
            })
            .or_else(|| self.layer_surface_under(location, &[Layer::Bottom, Layer::Background]))
    }

    fn layer_surface_under(
        &self,
        location: Point<f64, Logical>,
        layers: &[Layer],
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.output()?;
        let output_origin = self.space.output_geometry(&output)?.loc;
        let map = layer_map_for_output(&output);
        let local = location - output_origin.to_f64();

        layers.iter().find_map(|&kind| {
            let layer = map.layer_under(kind, local)?;
            let origin = map.layer_geometry(layer)?.loc;
            layer
                .surface_under(local - origin.to_f64(), WindowSurfaceType::ALL)
                .map(|(surface, offset)| (surface, (offset + origin + output_origin).to_f64()))
        })
    }
}
