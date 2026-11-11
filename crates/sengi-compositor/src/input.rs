//! Turning backend input events into keyboard and pointer events for
//! clients, and into window management when they land on a frame.

use smithay::{
    backend::input::{ButtonState, KeyState, Keycode},
    desktop::{layer_map_for_output, Window, WindowSurfaceType},
    input::{
        keyboard::FilterResult,
        pointer::{
            AxisFrame, ButtonEvent, CursorImageStatus, Focus, GestureHoldBeginEvent,
            GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
            GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
            GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
            RelativeMotionEvent,
        },
    },
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::{Logical, Point, Serial, SERIAL_COUNTER},
    wayland::shell::wlr_layer::Layer,
};

use crate::{
    cursor::resize_cursor,
    frame::{self, Button, Edges, Hit},
    keys::{self, Action, Mods},
    layout::Rect,
    state::State,
    window,
};

/// Evdev code of the left mouse button.
const BTN_LEFT: u32 = 0x110;

/// A pointer drag the compositor handles itself.
pub enum Drag {
    Move {
        window: Window,
        /// From the pointer to the frame's top-left corner.
        offset: (i32, i32),
    },
    Resize {
        window: Window,
        edges: Edges,
        /// Client area when the drag started.
        initial: Rect,
        /// Pointer position when the drag started.
        start: (i32, i32),
    },
    /// A title bar button is held down; it fires if released over it.
    Press { window: Window, button: Button },
}

/// Holds the pointer for the drag in `State::drag`, so that a move, resize
/// or button press shares Smithay's grab bookkeeping with popup grabs and
/// client clicks: one of them at a time, started by a known press, ended by
/// that button or by whoever takes the pointer next.
struct FrameGrab {
    start: GrabStartData<State>,
}

impl PointerGrab<State> for FrameGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // No client has the pointer while a frame is being dragged.
        handle.motion(data, None, event);
        data.drag_motion(event.location);
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if event.state == ButtonState::Released && event.button == self.start.button {
            data.finish_drag(handle.current_location());
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &GrabStartData<State> {
        &self.start
    }

    fn unset(&mut self, data: &mut State) {
        data.drag_ended();
    }
}

/// What the pointer is over.
enum Target {
    /// A client surface and where its origin is on the desktop.
    Surface {
        window: Option<Window>,
        surface: WlSurface,
        origin: Point<f64, Logical>,
    },
    /// Part of a frame the compositor draws.
    Frame { window: Window, hit: Hit },
}

pub fn cell(location: Point<f64, Logical>) -> (i32, i32) {
    (location.x.floor() as i32, location.y.floor() as i32)
}

impl State {
    pub fn key(&mut self, keycode: Keycode, state: KeyState, time: u32) {
        self.note_activity();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let code = keycode.raw();
        let action = keyboard.input(
            self,
            keycode,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |data, modifiers, key| {
                if state == KeyState::Released {
                    // The client never saw the press of a bound key.
                    return if data.swallowed_keys.release(code) {
                        FilterResult::Intercept(None)
                    } else {
                        FilterResult::Forward
                    };
                }
                // No shortcuts on the lock screen: every key is the locker's.
                if data.is_locked() {
                    return FilterResult::Forward;
                }
                let mods = Mods {
                    logo: modifiers.logo,
                    alt: modifiers.alt,
                    ctrl: modifiers.ctrl,
                    shift: modifiers.shift,
                };
                match keys::binding(mods, key.modified_sym().raw()) {
                    Some(action) => {
                        data.swallowed_keys.press(code);
                        FilterResult::Intercept(Some(action))
                    }
                    None => FilterResult::Forward,
                }
            },
        );
        if let Some(action) = action.flatten() {
            self.run_action(action);
        }
        // Letting go of Alt settles on the window Alt+Tab stopped at.
        if !keyboard.modifier_state().alt {
            self.cycle = None;
        }
    }

    fn run_action(&mut self, action: Action) {
        if let Some(command) = keys::command(action) {
            self.spawn(command);
            return;
        }
        match action {
            Action::CloseWindow => {
                // Only a window that has the keyboard: with the launcher
                // open, Super+Q is not aimed at the window behind it.
                let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
                let focused = keyboard.current_focus();
                let toplevel = self.focus_stack.current().and_then(Window::toplevel);
                if let Some(toplevel) = toplevel {
                    if focused.as_ref() == Some(toplevel.wl_surface()) {
                        toplevel.send_close();
                    }
                }
            }
            Action::NextWindow | Action::PreviousWindow => {
                let forward = action == Action::NextWindow;
                let (order, index) = self
                    .cycle
                    .take()
                    .unwrap_or_else(|| (self.focus_stack.recent_first(), 0));
                // Windows may have closed since the cycle began.
                let order: Vec<Window> = order
                    .into_iter()
                    .filter(|w| self.space.element_location(w).is_some())
                    .collect();
                let index = keys::cycle(
                    order.len(),
                    index.min(order.len().saturating_sub(1)),
                    forward,
                );
                if let Some(window) = order.get(index).cloned() {
                    self.focus_window(&window);
                }
                self.cycle = Some((order, index));
            }
            Action::Launcher | Action::Terminal | Action::Lock => {}
        }
    }

    /// Gives the pointer to whatever is under it now, if that changed
    /// without the pointer moving: a window closed, opened, moved or was
    /// maximised, or the session locked.
    pub fn sync_pointer_focus(&mut self) {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        if !self.pointer_seen || pointer.is_grabbed() {
            return;
        }
        let location = pointer.current_location();
        let wanted = match self.target_at(location) {
            Some(Target::Surface { surface, .. }) => Some(surface),
            _ => None,
        };
        if self.pointer_stale || pointer.current_focus() != wanted {
            let time = self.start_time.elapsed().as_millis() as u32;
            self.pointer_motion(location, time);
        }
    }

    pub fn pointer_motion(&mut self, location: Point<f64, Logical>, time: u32) {
        self.note_activity();
        self.pointer_seen = true;
        self.pointer_stale = false;

        // During a drag the grab does the work, see `FrameGrab`.
        let mut focus = None;
        if self.drag.is_none() {
            let mut hovered = None;
            let mut frame_cursor = None;
            match self.target_at(location) {
                Some(Target::Surface {
                    surface, origin, ..
                }) => focus = Some((surface, origin)),
                Some(Target::Frame {
                    window,
                    hit: Hit::Button(button),
                }) => hovered = Some((window, button)),
                Some(Target::Frame {
                    hit: Hit::Resize(edges),
                    ..
                }) => frame_cursor = Some(resize_cursor(edges)),
                _ => {}
            }
            self.hovered_button = hovered;
            self.frame_cursor = frame_cursor;
        }

        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        // The cursor a client set is its own; the next client under the
        // pointer starts from the default until it sets one.
        let same_client = match (pointer.current_focus(), &focus) {
            (Some(old), Some((new, _))) => old.id().same_client_as(&new.id()),
            (None, None) => true,
            _ => false,
        };
        if !same_client {
            self.client_cursor = CursorImageStatus::default_named();
        }
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    pub fn pointer_button(&mut self, button: u32, state: ButtonState, time: u32) {
        self.note_activity();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let serial = SERIAL_COUNTER.next_serial();

        if state == ButtonState::Pressed && !pointer.is_grabbed() {
            self.press(button, pointer.current_location(), serial);
        }

        // Presses on a frame reach no client, the pointer has no focus
        // there; a release that ends a drag is seen by the grab.
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// A button went down with nothing holding the pointer: focus what was
    /// clicked and start a drag if it was part of a frame.
    fn press(&mut self, button: u32, location: Point<f64, Logical>, serial: Serial) {
        // A click on the lock screen changes nothing behind it.
        if self.is_locked() {
            return;
        }
        let (x, y) = cell(location);
        match self.target_at(location) {
            Some(Target::Surface {
                window: Some(window),
                ..
            }) => self.focus_window(&window),
            Some(Target::Surface {
                window: None,
                surface,
                ..
            }) => self.click_layer(surface),
            Some(Target::Frame { window, hit }) => {
                self.focus_window(&window);
                if button != BTN_LEFT {
                    return;
                }
                match hit {
                    Hit::Titlebar => self.start_move(&window, (x, y)),
                    Hit::Resize(edges) => self.start_resize(&window, edges, (x, y)),
                    Hit::Button(button) => {
                        self.hovered_button = Some((window.clone(), button));
                        self.drag = Some(Drag::Press { window, button });
                    }
                    Hit::Client => {}
                }
                if self.drag.is_some() {
                    let start = GrabStartData {
                        focus: None,
                        button,
                        location,
                    };
                    self.grab_for_drag(start, serial);
                }
            }
            None => {
                self.layer_focus = None;
                self.update_keyboard_focus();
            }
        }
    }

    /// Hands the pointer to the drag in `self.drag` until the button that
    /// started it is released. `start` is the press it began with: ours for
    /// a press on a frame, the client's when the client asked for the drag.
    pub fn grab_for_drag(&mut self, start: GrabStartData<Self>, serial: Serial) {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let grab = FrameGrab { start };
        pointer.set_grab(self, grab, serial, Focus::Clear);
    }

    /// Prepares dragging `window` by its title bar from `pointer`. The
    /// caller installs the grab.
    pub fn start_move(&mut self, window: &Window, pointer: (i32, i32)) {
        // A maximised window stays where it is.
        if window::data(window).restore.get().is_some() {
            return;
        }
        let Some(frame) = self.frame_rect(window) else {
            return;
        };
        window::data(window).resize_anchor.set(None);
        self.drag = Some(Drag::Move {
            window: window.clone(),
            offset: (frame.x - pointer.0, frame.y - pointer.1),
        });
    }

    /// Prepares dragging the given edges of `window` from `pointer`. The
    /// caller installs the grab.
    pub fn start_resize(&mut self, window: &Window, edges: Edges, pointer: (i32, i32)) {
        if !edges.any() || window::data(window).restore.get().is_some() {
            return;
        }
        let Some(initial) = self.client_rect(window) else {
            return;
        };
        window::data(window)
            .resize_anchor
            .set(Some((edges, initial, None)));
        self.drag = Some(Drag::Resize {
            window: window.clone(),
            edges,
            initial,
            start: pointer,
        });
    }

    /// The pointer moved during a drag. Called by the grab, so nothing in
    /// here may use the pointer handle.
    fn drag_motion(&mut self, location: Point<f64, Logical>) {
        let (x, y) = cell(location);
        match &self.drag {
            Some(Drag::Move { window, offset }) => {
                let (window, offset) = (window.clone(), *offset);
                self.move_frame_to(&window, x + offset.0, y + offset.1);
            }
            Some(Drag::Resize {
                window,
                edges,
                initial,
                start,
            }) => {
                let (min, max) = self.size_limits(window);
                let outline = if window::data(window).decorated.get() {
                    self.theme.metrics.frame(*initial)
                } else {
                    *initial
                };
                let delta = (x - start.0, y - start.1);
                let delta = frame::limit_resize(outline, *edges, delta, self.usable_area());
                let wanted = frame::resize(*initial, *edges, delta, min, max);
                window::request_size(window, (wanted.w, wanted.h), true);
            }
            Some(Drag::Press { window, button }) => {
                // Shown as pressed only while the pointer is still on it.
                let on_it = self.button_at(window, x, y) == Some(*button);
                self.hovered_button = on_it.then(|| (window.clone(), *button));
            }
            None => {}
        }
    }

    /// The button that started the drag came up. Called by the grab, so
    /// nothing in here may use the pointer handle.
    fn finish_drag(&mut self, location: Point<f64, Logical>) {
        let (x, y) = cell(location);
        match self.drag.take() {
            Some(Drag::Resize { window, .. }) => window::finish_resize(&window),
            Some(Drag::Press { window, button }) => {
                if self.button_at(&window, x, y) == Some(button) {
                    self.press_button(&window, button);
                }
            }
            Some(Drag::Move { .. }) | None => {}
        }
    }

    /// The grab is gone, by release or because something took the pointer
    /// away (the lock, a popup). Whatever was in progress stops here.
    fn drag_ended(&mut self) {
        if let Some(Drag::Resize { window, .. }) = self.drag.take() {
            window::finish_resize(&window);
        }
        // What is under the pointer may be something else by now.
        self.pointer_stale = true;
        self.request_redraw();
    }

    fn press_button(&mut self, window: &Window, button: Button) {
        match button {
            Button::Close => {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_close();
                }
            }
            Button::Maximise => self.toggle_maximise(window),
            Button::Minimise => self.set_minimised(window, true),
        }
    }

    fn button_at(&self, window: &Window, x: i32, y: i32) -> Option<Button> {
        if !window::data(window).decorated.get() {
            return None;
        }
        let frame = self.frame_rect(window)?;
        match self.theme.metrics.hit(frame, x, y, false) {
            Some(Hit::Button(button)) => Some(button),
            _ => None,
        }
    }

    fn click_layer(&mut self, surface: WlSurface) {
        let Some(output) = self.output() else { return };
        let wants_keyboard = layer_map_for_output(&output)
            .layer_for_surface(&surface, WindowSurfaceType::ALL)
            .is_some_and(|layer| layer.can_receive_keyboard_focus());
        self.layer_focus = wants_keyboard.then_some(surface);
        self.update_keyboard_focus();
    }

    /// What is at `location`, front to back: overlay and top layers,
    /// windows with their frames, then bottom and background layers.
    fn target_at(&self, location: Point<f64, Logical>) -> Option<Target> {
        if self.is_locked() {
            // The lock surface covers the output; nothing else exists.
            let origin = self
                .output()
                .and_then(|output| self.space.output_geometry(&output))
                .map(|geometry| geometry.loc.to_f64())
                .unwrap_or_default();
            return self.lock_surface().map(|surface| Target::Surface {
                window: None,
                surface,
                origin,
            });
        }
        let layer = |layers: &[Layer]| {
            let (surface, origin) = self.layer_surface_under(location, layers)?;
            Some(Target::Surface {
                window: None,
                surface,
                origin,
            })
        };
        layer(&[Layer::Overlay, Layer::Top])
            .or_else(|| self.window_target_at(location))
            .or_else(|| layer(&[Layer::Bottom, Layer::Background]))
    }

    fn window_target_at(&self, location: Point<f64, Logical>) -> Option<Target> {
        let (x, y) = cell(location);
        for window in self.space.elements().rev() {
            let Some(client) = self.space.element_location(window) else {
                continue;
            };
            // Surfaces first: popups and subsurfaces may reach past the frame.
            let surface_origin = client - window.geometry().loc;
            if let Some((surface, offset)) =
                window.surface_under(location - surface_origin.to_f64(), WindowSurfaceType::ALL)
            {
                return Some(Target::Surface {
                    window: Some(window.clone()),
                    surface,
                    origin: (offset + surface_origin).to_f64(),
                });
            }

            let data = window::data(window);
            if !data.decorated.get() {
                continue;
            }
            let Some(frame) = self.frame_rect(window) else {
                continue;
            };
            let resizable = data.restore.get().is_none();
            if let Some(hit) = self.theme.metrics.hit(frame, x, y, resizable) {
                return Some(Target::Frame {
                    window: window.clone(),
                    hit,
                });
            }
        }
        None
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
            // A surface whose input region misses lets the one below it
            // on the same layer have the pointer.
            map.layers_on(kind).rev().find_map(|layer| {
                let origin = map.layer_geometry(layer)?.loc;
                layer
                    .surface_under(local - origin.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, offset)| (surface, (offset + origin + output_origin).to_f64()))
            })
        })
    }
}
