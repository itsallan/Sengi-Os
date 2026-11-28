//! What the tests ask about the windows: where everything is, and whether
//! what the compositor remembers still agrees with itself. Answers the
//! `inspect` request of the control socket; not part of the image's build.

use std::fmt::Write;

use smithay::{
    desktop::{layer_map_for_output, PopupManager, Window, WindowSurfaceType},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::IsAlive,
    wayland::seat::WaylandFocus,
};

use crate::{
    frame::BUTTONS,
    input::Drag,
    layout::Rect,
    overlay::{MenuKind, Overlay},
    state::State,
    window,
};

impl State {
    /// One line for each fact, the first of them `ok`. A line starting with
    /// `broken` names something that must never be so.
    pub fn inspect(&self) -> String {
        let mut report = String::from("ok\n");
        self.inspect_counts(&mut report);
        self.inspect_windows(&mut report);
        let (screens, unsettled) = self.autohide_report();
        for screen in screens {
            let _ = writeln!(report, "{screen}");
        }
        for broken in self.broken().into_iter().chain(unsettled) {
            let _ = writeln!(report, "broken {broken}");
        }
        report
    }

    fn inspect_counts(&self, report: &mut String) {
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let drag = match &self.drag {
            None => "none",
            Some(Drag::Move { .. }) => "move",
            Some(Drag::Resize { .. }) => "resize",
            Some(Drag::Press { .. }) => "press",
        };
        let surfaces: Vec<WlSurface> = self
            .space
            .elements()
            .chain(self.minimised.iter())
            .chain(self.pending_windows.iter())
            .filter_map(|window| window.wl_surface().map(|surface| surface.into_owned()))
            .collect();
        let popups: usize = surfaces
            .iter()
            .map(|surface| PopupManager::popups_for_surface(surface).count())
            .sum();
        let layers: usize = self
            .space
            .outputs()
            .map(|output| layer_map_for_output(output).layers().count())
            .sum();
        let at = pointer.current_location();
        let _ = writeln!(
            report,
            "state pointer_grabbed={} keyboard_grabbed={} drag={drag} locked={} {} \
             pointer={:.0},{:.0}",
            u8::from(pointer.is_grabbed()),
            u8::from(keyboard.is_grabbed()),
            u8::from(self.is_locked()),
            self.overlay_status().replace(": ", "="),
            at.x,
            at.y,
        );
        let _ = writeln!(
            report,
            "count outputs={} mapped={} minimised={} pending={} listed={} popups={popups} \
             layers={layers} primary_layers={} inhibitors={} children={} painters={} \
             contexts={} held_buttons={}",
            self.space.outputs().count(),
            self.space.elements().count(),
            self.minimised.len(),
            self.pending_windows.len(),
            self.foreign_toplevels.len(),
            self.primary_layers.len(),
            self.idle_inhibitors.len(),
            self.children.len(),
            self.painters.len(),
            self.security_contexts.len(),
            self.wake_buttons.len() + self.overlay_buttons.len(),
        );
    }

    /// The outputs, then the windows, the back-most first, then the hidden
    /// ones. Places are where a click would have to go.
    fn inspect_windows(&self, report: &mut String) {
        for (output, rect) in self.space.outputs().zip(self.output_rects()) {
            let _ = writeln!(
                report,
                "output name={} at={},{},{},{}",
                output.name(),
                rect.x,
                rect.y,
                rect.w,
                rect.h
            );
        }
        let on_screen = self.space.elements().map(|window| (window, "mapped"));
        let hidden = self.minimised.iter().map(|window| (window, "minimised"));
        for (window, place) in on_screen.chain(hidden) {
            if window::is_override(window) {
                continue;
            }
            let data = window::data(window);
            let title = window::title(window).replace(' ', "_");
            let _ = write!(
                report,
                "window title={title} place={place} active={} maximised={} fullscreen={}",
                u8::from(self.focus_stack.current() == Some(window)),
                u8::from(data.restore.get().is_some()),
                u8::from(data.fullscreen.get().is_some()),
            );
            if let Some(client) = self.client_rect(window) {
                let _ = write!(
                    report,
                    " client={},{},{},{}",
                    client.x, client.y, client.w, client.h
                );
            }
            if let Some(frame) = self.frame_rect(window).filter(|_| data.decorated.get()) {
                let _ = write!(
                    report,
                    " frame={},{},{},{}",
                    frame.x, frame.y, frame.w, frame.h
                );
                for button in BUTTONS {
                    let rect = self.theme.metrics.button_rect(button);
                    let _ = write!(
                        report,
                        " {}={},{}",
                        format!("{button:?}").to_ascii_lowercase(),
                        frame.x + rect.x + rect.w / 2,
                        frame.y + rect.y + rect.h / 2,
                    );
                }
            }
            report.push('\n');
        }
    }

    /// Everything that does not agree.
    fn broken(&self) -> Vec<String> {
        let mut broken = Vec::new();
        let keyboard = self.seat.get_keyboard().expect("seat has a keyboard");
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let name = |window: &Window| window::title(window).replace(' ', "_");
        let mapped: Vec<&Window> = self.space.elements().collect();
        let on_screen = |window: &Window| mapped.contains(&window);

        for window in &mapped {
            if self.minimised.contains(window) || self.pending_windows.contains(window) {
                broken.push(format!("{} is mapped and hidden too", name(window)));
            }
            if window::data(window).minimised.get().is_some() {
                broken.push(format!("{} is mapped but marked minimised", name(window)));
            }
            if !window.alive() {
                broken.push(format!("{} is mapped but gone", name(window)));
            }
        }
        for window in &self.minimised {
            if window::data(window).minimised.get().is_none() {
                broken.push(format!("{} is hidden but not marked so", name(window)));
            }
            if !window.alive() {
                broken.push(format!("{} is minimised but gone", name(window)));
            }
            if self.pending_windows.contains(window) {
                broken.push(format!("{} is minimised and pending", name(window)));
            }
        }
        if self.pending_windows.iter().any(|window| !window.alive()) {
            broken.push("a window that never showed is kept after it went".into());
        }

        let order = self.focus_stack.recent_first();
        for (index, window) in order.iter().enumerate() {
            if !on_screen(window) {
                broken.push(format!("{} may be focused but is not mapped", name(window)));
            }
            if order[index + 1..].contains(window) {
                broken.push(format!("{} is in the focus order twice", name(window)));
            }
        }
        for window in &mapped {
            if !window::is_override(window) && !order.contains(window) {
                broken.push(format!("{} is not in the focus order", name(window)));
            }
        }

        match &self.drag {
            Some(Drag::Move { window, .. })
            | Some(Drag::Resize { window, .. })
            | Some(Drag::Press { window, .. }) => {
                if !pointer.is_grabbed() {
                    broken.push("a drag without the pointer".into());
                }
                if !on_screen(window) {
                    broken.push(format!("{} is dragged but not mapped", name(window)));
                }
            }
            None => {}
        }
        if self.released_drag.is_some() {
            broken.push("a released drag was never carried out".into());
        }
        let remembered = [
            self.hovered_button.as_ref().map(|(window, _)| window),
            self.hovered_group.as_ref(),
            self.titlebar_press.as_ref().map(|(window, _, _)| window),
        ];
        for window in remembered.into_iter().flatten() {
            if !on_screen(window) {
                broken.push(format!("{} is remembered but not mapped", name(window)));
            }
        }
        match &self.overlay {
            Some(Overlay::Menu {
                kind: MenuKind::Window(window),
                ..
            }) if !on_screen(window) => {
                broken.push(format!(
                    "a menu is open for {}, which is not mapped",
                    name(window)
                ));
            }
            Some(Overlay::Switcher { windows, .. })
                if windows.iter().any(|window| !window.alive()) =>
            {
                broken.push("the switcher offers a window that is gone".into());
            }
            _ => {}
        }
        if self.is_locked() && (self.overlay.is_some() || self.snap_hint.is_some()) {
            broken.push("something of the session is drawn over the lock".into());
        }
        if self.snap_hint.is_some() && !matches!(self.drag, Some(Drag::Move { .. })) {
            broken.push("a snap outline without a move".into());
        }

        if !keyboard.is_grabbed() {
            let wanted = self.keyboard_wanted();
            let has = keyboard.current_focus();
            let fine = if self.is_locked() {
                // Whichever lock surface the pointer was on last.
                match &has {
                    Some(surface) => self.lock_surfaces().contains(surface),
                    None => self.lock_surfaces().is_empty(),
                }
            } else {
                has == wanted
            };
            if !fine {
                broken.push(format!(
                    "the keyboard is on {}, not on {}",
                    self.describe_surface(has.as_ref()),
                    self.describe_surface(wanted.as_ref())
                ));
            }
        }

        let outputs = self.output_rects();
        if !outputs.is_empty() {
            for window in &mapped {
                // While it is resized the window goes where the edge does;
                // it is looked at again once let go of.
                let resized = matches!(
                    &self.drag,
                    Some(Drag::Resize { window: of, .. }) if of == *window
                );
                if window::is_override(window) || resized {
                    continue;
                }
                let Some(frame) = self.frame_rect(window) else {
                    continue;
                };
                // The strip a window is taken hold of by.
                let bar = Rect::new(frame.x, frame.y, frame.w, self.theme.metrics.titlebar);
                let reachable = outputs.iter().any(|output| {
                    bar.x < output.right()
                        && output.x < bar.right()
                        && bar.y < output.bottom()
                        && output.y < bar.bottom()
                });
                if !reachable {
                    broken.push(format!(
                        "{} is on no screen, at {},{}",
                        name(window),
                        frame.x,
                        frame.y
                    ));
                }
            }
        }
        broken
    }

    /// Who should have the keyboard when no menu holds it, as
    /// `update_keyboard_focus` has it.
    fn keyboard_wanted(&self) -> Option<WlSurface> {
        let clicked = self.layer_focus.clone().filter(|surface| {
            surface.alive()
                && self.output_for_layer(surface).is_some_and(|output| {
                    layer_map_for_output(&output)
                        .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                        .is_some_and(|layer| layer.can_receive_keyboard_focus())
                })
        });
        self.exclusive_layer().or(clicked).or_else(|| {
            let window = self.focus_stack.current()?;
            window.wl_surface().map(|surface| surface.into_owned())
        })
    }

    fn describe_surface(&self, surface: Option<&WlSurface>) -> String {
        let Some(surface) = surface else {
            return "nobody".into();
        };
        if let Some(window) = self.window_for_surface(surface) {
            return window::title(&window).replace(' ', "_");
        }
        if self.output_for_layer(surface).is_some() {
            return "a layer surface".into();
        }
        if !surface.alive() {
            return "a surface that is gone".into();
        }
        "another surface".into()
    }
}
