//! Translating a backend's input device events into the compositor's own
//! key and pointer entry points. Only built when a backend has devices.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, Event, InputBackend, InputEvent, KeyboardKeyEvent,
        PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::pointer::AxisFrame,
    utils::Point,
};

use crate::state::State;

impl State {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event } => {
                self.key(event.key_code(), event.state(), event.time_msec());
            }
            InputEvent::PointerMotionAbsolute { event } => {
                let Some(output) = self.output() else { return };
                let Some(geometry) = self.space.output_geometry(&output) else {
                    return;
                };
                let location = event.position_transformed(geometry.size) + geometry.loc.to_f64();
                self.pointer_motion(location, event.time_msec());
            }
            InputEvent::PointerMotion { event } => {
                let Some(output) = self.output() else { return };
                let Some(geometry) = self.space.output_geometry(&output) else {
                    return;
                };
                let pointer = self.seat.get_pointer().expect("seat has a pointer");
                let moved = pointer.current_location() + event.delta();
                // The pointer stays on the output.
                let area = geometry.to_f64();
                let location = Point::from((
                    moved.x.clamp(area.loc.x, area.loc.x + area.size.w - 1.0),
                    moved.y.clamp(area.loc.y, area.loc.y + area.size.h - 1.0),
                ));
                self.pointer_motion(location, event.time_msec());
            }
            InputEvent::PointerButton { event } => {
                self.pointer_button(event.button_code(), event.state(), event.time_msec());
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
                    } else if event.amount(axis) == Some(0.0)
                        && event.source() == AxisSource::Finger
                    {
                        // The finger lifted on this axis. An axis the event
                        // says nothing about is left alone.
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
}
