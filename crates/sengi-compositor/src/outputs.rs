//! The outputs of the desktop: which there are, which are switched on, and
//! how a new arrangement of them is checked and put in place.
//!
//! Every output the backend finds is a head. A head that is on is mapped
//! into the one desktop space, has a `wl_output` for clients and is drawn;
//! one that is off is only remembered, so that it can come back as the same
//! head. The management protocol (`output_management.rs`) and the saved
//! layout (`output_store.rs`) both end up in `State::apply_plan`.

use std::time::Duration;

use smithay::{
    desktop::{layer_map_for_output, utils::with_surfaces_surface_tree, Window},
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale as OutputScale, Subpixel},
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::{backend::GlobalId, protocol::wl_surface::WlSurface},
    },
    utils::{Logical, Physical, Point, Size, Transform},
    wayland::fractional_scale::with_fractional_scale,
};

use crate::{
    decoration::Theme,
    layout::{self, Rect},
    output_store::{StoredHead, StoredLayout, SAVE_AFTER},
    state::{State, TestOutput},
    text::Font,
    window,
};

/// The smallest and largest scale an output takes.
pub const SCALE_RANGE: (f64, f64) = (0.5, 4.0);

/// The most heads there are; anything beyond is not announced.
pub const MAX_HEADS: usize = 16;

/// A screen the backend found.
pub struct Head {
    pub output: Output,
    pub enabled: bool,
    /// What announces the output to clients, while it is on.
    pub global: Option<GlobalId>,
    /// Takes any size, as a window or a virtual display does. Such a head
    /// lists no modes of its own.
    pub any_size: bool,
    pub serial: String,
}

/// What a backend says about a screen it found.
pub struct OutputSpec {
    pub name: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    /// Millimetres, zero when not known.
    pub physical: (i32, i32),
    pub modes: Vec<OutputMode>,
    pub preferred: Option<OutputMode>,
    pub current: OutputMode,
    pub any_size: bool,
    pub transform: Transform,
    pub enabled: bool,
}

impl OutputSpec {
    /// A display with no hardware behind it: it has the one size it was
    /// given and takes any other.
    pub fn virtual_screen(
        name: &str,
        model: &str,
        size: Size<i32, Physical>,
        refresh: i32,
        transform: Transform,
    ) -> Self {
        let mode = OutputMode { size, refresh };
        Self {
            name: name.into(),
            make: "Sengi OS".into(),
            model: model.into(),
            serial: String::new(),
            physical: (0, 0),
            modes: vec![mode],
            preferred: Some(mode),
            current: mode,
            any_size: true,
            transform,
            enabled: true,
        }
    }
}

/// What one head is to be, as a whole arrangement is checked and applied.
#[derive(Clone, Debug)]
pub struct HeadPlan {
    pub output: Output,
    pub enabled: bool,
    pub mode: OutputMode,
    pub position: (i32, i32),
    /// The client named the position; otherwise it follows its neighbours.
    pub position_set: bool,
    pub scale: f64,
    pub transform: Transform,
}

/// What the screen is painted with at one scale: the frame metrics, the
/// title typeface and the cursor sprites, all in that scale's pixels.
pub struct Painter {
    pub scale: f64,
    pub theme: Theme,
    pub font: Option<Font>,
    pub cursors: crate::cursor::Cursors,
}

/// A scale as a key: thousandths, which is also what is stored.
pub fn scale_key(scale: f64) -> u32 {
    (scale * 1000.0).round().max(0.0) as u32
}

/// The scale as `wl_output` announces it: whole numbers as they are,
/// anything else as a fraction clients may ask for.
pub fn announce_scale(scale: f64) -> OutputScale {
    if scale.fract() == 0.0 {
        OutputScale::Integer(scale as i32)
    } else {
        OutputScale::Fractional(scale)
    }
}

/// The size of an output on the desktop, in logical pixels.
pub fn logical_size(mode: OutputMode, transform: Transform, scale: f64) -> (i32, i32) {
    let size = transform.transform_size(mode.size);
    (
        (f64::from(size.w) / scale).round() as i32,
        (f64::from(size.h) / scale).round() as i32,
    )
}

/// Where each head ends up when some of them changed size and the rest
/// were not told where to be. What lay to the right of a head that
/// changed width moves by as much as the width changed, what lay below one
/// that changed height likewise, so that neighbours stay neighbours: a gap
/// between two screens would keep the pointer from crossing.
///
/// `old` and `new` are the rectangles before and after (`None` when the
/// head is off, `new` carrying the size and the position it asked for),
/// `fixed` whether a head's position was given and so is left as it is.
pub fn follow_changes(
    old: &[Option<Rect>],
    new: &[Option<Rect>],
    fixed: &[bool],
) -> Vec<Option<(i32, i32)>> {
    let mut at: Vec<Option<(i32, i32)>> = new.iter().map(|r| r.map(|r| (r.x, r.y))).collect();
    for (i, (before, after)) in old.iter().zip(new).enumerate() {
        let Some(before) = before else { continue };
        let (dw, dh) = match after {
            Some(after) => (after.w - before.w, after.h - before.h),
            None => (-before.w, -before.h),
        };
        if dw == 0 && dh == 0 {
            continue;
        }
        for (j, other) in old.iter().enumerate() {
            let (Some(other), Some(place)) = (other, at[j].as_mut()) else {
                continue;
            };
            if j == i || fixed[j] {
                continue;
            }
            if other.x >= before.right() {
                place.0 += dw;
            }
            if other.y >= before.bottom() {
                place.1 += dh;
            }
        }
    }
    at
}

/// Whether any two of `rects` overlap.
pub fn overlap(rects: &[Rect]) -> bool {
    rects.iter().enumerate().any(|(i, a)| {
        rects[i + 1..]
            .iter()
            .any(|b| a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom())
    })
}

impl State {
    /// The heads, switched on or not.
    pub fn heads(&self) -> &[Head] {
        &self.heads
    }

    pub fn head(&self, output: &Output) -> Option<&Head> {
        self.heads.iter().find(|head| &head.output == output)
    }

    /// The scale `output` is drawn at.
    pub fn scale_of(&self, output: &Output) -> f64 {
        output.current_scale().fractional_scale()
    }

    /// What to paint with at `scale`. A scale no output has falls back on
    /// the default one.
    pub fn painter(&self, scale: f64) -> &Painter {
        let key = scale_key(scale);
        self.painters
            .iter()
            .find(|painter| scale_key(painter.scale) == key)
            .unwrap_or_else(|| &self.painters[0])
    }

    /// Makes a painter for every scale in use, and drops the others. The
    /// default scale is always there and always first.
    pub fn refresh_painters(&mut self) {
        let mut scales = vec![self.output_scale];
        for output in self.space.outputs() {
            let scale = self.scale_of(output);
            if !scales.iter().any(|s| scale_key(*s) == scale_key(scale)) {
                scales.push(scale);
            }
        }
        let mut old = std::mem::take(&mut self.painters);
        for scale in scales {
            let key = scale_key(scale);
            let kept = old.iter().position(|p| scale_key(p.scale) == key);
            match kept {
                Some(at) => self.painters.push(old.remove(at)),
                None => {
                    let theme = self.theme.scaled(scale);
                    let font = Font::load(&theme.font_family, theme.font_weight, theme.font_px);
                    let cursors = crate::cursor::Cursors::new(&theme, scale);
                    self.painters.push(Painter {
                        scale,
                        theme,
                        font,
                        cursors,
                    });
                }
            }
        }
        // Frames painted at a scale nobody uses any more are not kept.
        let keys: Vec<u32> = self.painters.iter().map(|p| scale_key(p.scale)).collect();
        for window in self.space.elements().chain(self.minimised.iter()) {
            window::data(window)
                .frame
                .borrow_mut()
                .retain(|key, _| keys.contains(key));
        }
    }

    /// The output `location` is on, if it is on one.
    pub fn output_containing(&self, location: Point<f64, Logical>) -> Option<Output> {
        self.space
            .outputs()
            .find(|output| {
                self.space
                    .output_geometry(output)
                    .is_some_and(|geo| geo.to_f64().contains(location))
            })
            .cloned()
    }

    /// The output the pointer is on.
    pub fn pointer_output(&self) -> Option<Output> {
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        self.output_at(pointer.current_location())
    }

    /// The output things start on: where the pointer is, since that is
    /// where the user is looking, else where the window in front is, else
    /// the first.
    pub fn active_output(&self) -> Option<Output> {
        if self.pointer_seen {
            if let Some(output) = self.pointer_output() {
                return Some(output);
            }
        }
        self.focus_stack
            .current()
            .and_then(|window| self.output_of_window(window))
            .or_else(|| self.output())
    }

    /// The output most of `window`'s frame is on, going by its middle.
    pub fn output_of_window(&self, window: &Window) -> Option<Output> {
        let frame = self.frame_rect(window)?;
        let middle = (
            f64::from(frame.x + frame.w / 2),
            f64::from(frame.y + frame.h / 2),
        );
        self.output_containing(middle.into())
    }

    /// What is on now, as a plan that changes nothing.
    pub fn current_plan(&self) -> Vec<HeadPlan> {
        self.heads
            .iter()
            .map(|head| {
                let output = &head.output;
                let loc = output.current_location();
                HeadPlan {
                    output: output.clone(),
                    enabled: head.enabled,
                    mode: output.current_mode().unwrap_or(OutputMode {
                        size: (0, 0).into(),
                        refresh: 0,
                    }),
                    position: (loc.x, loc.y),
                    position_set: false,
                    scale: self.scale_of(output),
                    transform: output.current_transform(),
                }
            })
            .collect()
    }

    /// Announces a new screen to clients and puts it to the right of the
    /// ones already there, or where the saved layout says.
    pub fn add_head(&mut self, spec: OutputSpec) -> Output {
        let output = Output::new(
            spec.name.clone(),
            PhysicalProperties {
                size: spec.physical.into(),
                subpixel: Subpixel::Unknown,
                make: spec.make.clone(),
                model: spec.model.clone(),
            },
        );
        for mode in &spec.modes {
            output.add_mode(*mode);
        }
        if let Some(preferred) = spec.preferred {
            output.set_preferred(preferred);
        }
        let stored = self
            .stored_layout
            .find(&spec.name, &spec.make, &spec.model, &spec.serial)
            .cloned();
        let scale = stored
            .as_ref()
            .and_then(|s| s.scale)
            .map(|thousandths| f64::from(thousandths) / 1000.0)
            .filter(|scale| (SCALE_RANGE.0..=SCALE_RANGE.1).contains(scale))
            .unwrap_or(self.output_scale);
        let transform = stored
            .as_ref()
            .and_then(|s| s.transform)
            .filter(|_| spec.transform == Transform::Normal)
            .unwrap_or(spec.transform);
        let mode = self.initial_mode(&spec, stored.as_ref());
        output.change_current_state(
            Some(mode),
            Some(transform),
            Some(announce_scale(scale)),
            None,
        );

        let others: Vec<Rect> = self.output_rects();
        let mut position = (layout::next_output_x(&others), 0);
        if let Some(place) = stored.as_ref().and_then(|s| s.position) {
            let (w, h) = logical_size(mode, transform, scale);
            let wanted = Rect::new(place.0, place.1, w, h);
            let mut all = others.clone();
            all.push(wanted);
            if !overlap(&all) {
                position = place;
            }
        }
        output.change_current_state(None, None, None, Some(position.into()));

        let enabled = spec.enabled && stored.as_ref().is_none_or(|s| s.enabled);
        // A saved "off" is not honoured when it would leave nothing on.
        let enabled = enabled || !self.heads.iter().any(|head| head.enabled);
        let mut head = Head {
            output: output.clone(),
            enabled: false,
            global: None,
            any_size: spec.any_size,
            serial: spec.serial.clone(),
        };
        if enabled {
            head.global = Some(output.create_global::<Self>(&self.display_handle));
            head.enabled = true;
            self.space.map_output(&output, position);
        }
        self.heads.push(head);
        self.refresh_painters();
        self.scales_dirty = true;
        self.output_management_changed();
        output
    }

    /// The mode a new head starts in: the saved one if it is one of its
    /// own (or the head takes any size), else the one the backend chose.
    pub fn initial_mode(
        &self,
        spec: &OutputSpec,
        stored: Option<&crate::output_store::StoredHead>,
    ) -> OutputMode {
        let Some((w, h, refresh)) = stored.and_then(|s| s.mode) else {
            return spec.current;
        };
        let wanted = OutputMode {
            size: (w, h).into(),
            refresh,
        };
        if spec.modes.contains(&wanted) {
            wanted
        } else if spec.any_size && size_is_sane(w, h) {
            OutputMode {
                size: (w, h).into(),
                refresh: spec.current.refresh,
            }
        } else {
            spec.current
        }
    }

    /// Announces an output of `size` pixels to clients and puts it to the
    /// right of the ones already there.
    pub fn add_output(
        &mut self,
        name: &str,
        model: &str,
        size: Size<i32, Physical>,
        refresh_mhz: i32,
        transform: Transform,
    ) -> Output {
        self.add_head(OutputSpec::virtual_screen(
            name,
            model,
            size,
            refresh_mhz,
            transform,
        ))
    }

    /// What taking an output off the desktop means: its bars are closed,
    /// the lock and the power switch forget it, clients stop seeing it.
    fn take_off_desktop(&mut self, output: &Output) {
        for layer in layer_map_for_output(output).layers() {
            layer.layer_surface().send_close();
        }
        self.space.unmap_output(output);
        self.lock_output_removed(output);
        self.forget_output_power(output);
        if let Some(head) = self.heads.iter_mut().find(|head| &head.output == output) {
            head.enabled = false;
            if let Some(global) = head.global.take() {
                self.display_handle.remove_global::<Self>(global);
            }
        }
    }

    /// Takes an output that was unplugged off the desktop. Its bars are
    /// closed and its windows move to a screen that is still there.
    #[cfg_attr(not(feature = "drm"), allow(dead_code))]
    pub fn remove_output(&mut self, output: &Output) {
        self.take_off_desktop(output);
        self.heads.retain(|head| &head.output != output);
        self.configure_lock_surfaces();
        self.output_management_changed();

        let remaining = self.output_rects();
        let usable = self.active_output().map_or_else(
            || Rect::new(0, 0, 0, 0),
            |output| self.usable_area_of(&output),
        );
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(frame) = self.frame_rect(&window) else {
                continue;
            };
            if let Some((x, y)) = layout::rehome(frame, &remaining, usable) {
                self.move_frame_to(&window, x, y);
            }
        }
        self.refresh_painters();
        self.refit_fullscreen();
        self.usable_area_changed();
        self.settle_pointer_after_layout();
        self.update_keyboard_focus();
        self.request_redraw();
    }

    /// Keeps the pointer on the desktop after outputs went or moved.
    fn settle_pointer_after_layout(&mut self) {
        let remaining = self.output_rects();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let at = pointer.current_location();
        let (x, y) = layout::clamp_to_outputs(&remaining, at.x, at.y);
        if (x, y) != (at.x, at.y) && !pointer.is_grabbed() {
            let time = self.start_time.elapsed().as_millis() as u32;
            self.place_pointer((x, y).into(), time);
        }
    }

    /// Checks `plan` without changing anything.
    pub fn check_plan(&self, plan: &[HeadPlan]) -> Result<(), String> {
        if plan.iter().all(|head| !head.enabled) {
            return Err("that would switch every output off".into());
        }
        let mut rects = Vec::new();
        for head in plan {
            let known = self
                .head(&head.output)
                .ok_or("an output that is not there")?;
            if !head.enabled {
                continue;
            }
            let (w, h) = (head.mode.size.w, head.mode.size.h);
            if !size_is_sane(w, h) {
                return Err(format!("{w}x{h} is not a size of a screen"));
            }
            let listed = head.output.modes().contains(&head.mode);
            let current = head.output.current_mode() == Some(head.mode);
            let takes = known.any_size && self.output_backend.is_some();
            if !listed && !takes && !current {
                return Err(format!("{} has no {w}x{h} mode", head.output.name()));
            }
            if !(SCALE_RANGE.0..=SCALE_RANGE.1).contains(&head.scale) {
                return Err(format!("a scale of {} is not supported", head.scale));
            }
            let (lw, lh) = logical_size(head.mode, head.transform, head.scale);
            if lw < 1 || lh < 1 {
                return Err("that would leave the output with no size".into());
            }
            rects.push(Rect::new(head.position.0, head.position.1, lw, lh));
        }
        if overlap(&rects) {
            return Err("outputs would overlap".into());
        }
        Ok(())
    }

    /// Puts `plan` in place: either all of it, or an error and nothing.
    ///
    /// The plan names every head. A head whose position was not given
    /// keeps its place among its neighbours, see `follow_changes`.
    pub fn apply_plan(&mut self, mut plan: Vec<HeadPlan>) -> Result<(), String> {
        // Positions first: what was not named is derived from what moved.
        self.place_unnamed(&mut plan);
        self.check_plan(&plan)?;

        // Where each window and the pointer are, to move with their output.
        let before: Vec<(Window, Output, Point<i32, Logical>)> = self
            .space
            .elements()
            .filter_map(|window| {
                let output = self.output_of_window(window)?;
                let origin = self.space.output_geometry(&output)?.loc;
                Some((window.clone(), output, origin))
            })
            .collect();
        let pointer = self.seat.get_pointer().expect("seat has a pointer");
        let pointer_at = pointer.current_location();
        let pointer_on = self
            .output_containing(pointer_at)
            .and_then(|output| Some((output.clone(), self.space.output_geometry(&output)?.loc)));

        // The backend changes what it has to first; it may refuse.
        if let Some(mut backend) = self.output_backend.take() {
            let result = backend(self, &plan);
            self.output_backend = Some(backend);
            result?;
        }

        for head in &plan {
            self.commit_head(head);
        }
        self.refresh_painters();
        self.scales_dirty = true;
        for output in self.space.outputs().cloned().collect::<Vec<_>>() {
            layer_map_for_output(&output).arrange();
        }
        self.configure_lock_surfaces();

        // Windows follow the output they were on.
        for (window, output, was) in before {
            let Some(now) = self.space.output_geometry(&output).map(|geo| geo.loc) else {
                continue;
            };
            if now == was {
                continue;
            }
            if let Some(client) = self.client_rect(&window) {
                self.move_window(&window, client.x + now.x - was.x, client.y + now.y - was.y);
            }
        }
        if let Some((output, was)) = pointer_on {
            if let Some(now) = self.space.output_geometry(&output).map(|geo| geo.loc) {
                if now != was && !pointer.is_grabbed() {
                    let time = self.start_time.elapsed().as_millis() as u32;
                    let moved = pointer_at + (now - was).to_f64();
                    self.place_pointer(moved, time);
                }
            }
        }
        // What is left on no output is brought onto one.
        let remaining = self.output_rects();
        let usable = self.active_output().map_or_else(
            || Rect::new(0, 0, 0, 0),
            |output| self.usable_area_of(&output),
        );
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(frame) = self.frame_rect(&window) else {
                continue;
            };
            if let Some((x, y)) = layout::rehome(frame, &remaining, usable) {
                self.move_frame_to(&window, x, y);
            }
        }
        self.refit_fullscreen();
        self.usable_area_changed();
        self.settle_pointer_after_layout();
        self.update_keyboard_focus();
        self.output_management_changed();
        self.request_redraw();
        Ok(())
    }

    /// Gives the heads whose position was not named the one that keeps
    /// them beside their neighbours.
    fn place_unnamed(&self, plan: &mut [HeadPlan]) {
        let rect_of = |enabled: bool, position: (i32, i32), mode, transform, scale| {
            enabled.then(|| {
                let (w, h) = logical_size(mode, transform, scale);
                Rect::new(position.0, position.1, w.max(1), h.max(1))
            })
        };
        let now = self.current_plan();
        let old: Vec<Option<Rect>> = plan
            .iter()
            .map(|head| {
                let was = now.iter().find(|n| n.output == head.output)?;
                rect_of(
                    was.enabled,
                    was.position,
                    was.mode,
                    was.transform,
                    was.scale,
                )
            })
            .collect();
        let new: Vec<Option<Rect>> = plan
            .iter()
            .zip(&old)
            .map(|(head, was)| {
                // A head switched on without a place goes after the others.
                let position = match (head.position_set, was) {
                    (false, Some(was)) => (was.x, was.y),
                    (false, None) => (i32::MAX, 0),
                    (true, _) => head.position,
                };
                rect_of(
                    head.enabled,
                    position,
                    head.mode,
                    head.transform,
                    head.scale,
                )
            })
            .collect();
        let fixed: Vec<bool> = plan.iter().map(|head| head.position_set).collect();
        let mut at = follow_changes(&old, &new, &fixed);
        // Heads that come on without a place: to the right of the rest.
        let mut right = at
            .iter()
            .zip(&new)
            .zip(&old)
            .filter(|((_, _), was)| was.is_some())
            .filter_map(|((place, rect), _)| Some(place.as_ref()?.0 + rect.as_ref()?.w))
            .max()
            .unwrap_or(0);
        for (i, head) in plan.iter().enumerate() {
            if head.position_set || old[i].is_some() || !head.enabled {
                continue;
            }
            let Some(rect) = &new[i] else { continue };
            at[i] = Some((right, 0));
            right += rect.w;
        }
        for (head, place) in plan.iter_mut().zip(at) {
            if let Some(place) = place {
                head.position = place;
            }
        }
    }

    /// Puts one head of an accepted plan in place.
    fn commit_head(&mut self, head: &HeadPlan) {
        let output = &head.output;
        let was_on = self.head(output).is_some_and(|known| known.enabled);
        match (was_on, head.enabled) {
            (true, false) => {
                self.take_off_desktop(output);
            }
            (false, false) => {}
            (was, true) => {
                let before = output.current_mode();
                // A virtual head forgets the sizes it has had.
                if let Some(old) = before.filter(|old| *old != head.mode) {
                    if self.head(output).is_some_and(|known| known.any_size) {
                        output.delete_mode(old);
                    }
                }
                output.change_current_state(
                    Some(head.mode),
                    Some(head.transform),
                    Some(announce_scale(head.scale)),
                    Some(head.position.into()),
                );
                if !was {
                    let global = output.create_global::<Self>(&self.display_handle);
                    if let Some(known) = self.heads.iter_mut().find(|k| &k.output == output) {
                        known.global = Some(global);
                        known.enabled = true;
                    }
                }
                self.space.map_output(output, head.position);
            }
        }
    }

    /// Windows that fill a screen are sized to it again, after screens
    /// changed.
    pub fn refit_fullscreen(&mut self) {
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            if window::data(&window).fullscreen.get().is_none() {
                continue;
            }
            let asked = window::data(&window)
                .fullscreen_on
                .borrow()
                .clone()
                .filter(|output| self.head(output).is_some_and(|head| head.enabled));
            let Some(screen) = self.fullscreen_rect(&window, asked) else {
                continue;
            };
            window::request_size(&window, (screen.w, screen.h), false);
            self.move_window(&window, screen.x, screen.y);
        }
    }
}

impl State {
    /// `plug WxH [NAME] [panel]` or `unplug NAME`, from the control socket,
    /// where the backend lets screens come and go. The answer is one line.
    pub fn test_output_request(&mut self, line: &str) -> String {
        let mut words = line.split_whitespace();
        let request = match words.next() {
            Some("plug") => {
                let size = words.next().and_then(|text| {
                    let (w, h) = text.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                });
                let Some(size) = size.filter(|(w, h)| size_is_sane(*w, *h)) else {
                    return "failed: plug WIDTHxHEIGHT\n".into();
                };
                let mut panel = false;
                let mut name = None;
                for word in words {
                    if word == "panel" {
                        panel = true;
                    } else {
                        name = Some(word.to_string());
                    }
                }
                TestOutput::Plug { size, panel, name }
            }
            Some("unplug") => match words.next() {
                Some(name) => TestOutput::Unplug { name: name.into() },
                None => return "failed: unplug NAME\n".into(),
            },
            _ => return "failed: unknown request\n".into(),
        };
        let Some(mut backend) = self.test_outputs.take() else {
            return "failed: this backend cannot\n".into();
        };
        let result = backend(self, request);
        self.test_outputs = Some(backend);
        match result {
            Ok(name) => format!("ok {name}\n"),
            Err(why) => format!("failed: {why}\n"),
        }
    }

    /// The scale a surface is best drawn at: that of the output its window
    /// is mostly on, or its layer surface, or its lock screen.
    pub fn preferred_scale_of(&self, surface: &WlSurface) -> f64 {
        let output = self
            .window_for_surface(surface)
            .and_then(|window| self.output_of_window(&window))
            .or_else(|| self.output_for_layer(surface))
            .or_else(|| self.lock_output_of(surface))
            .or_else(|| self.active_output());
        output.map_or(self.output_scale, |output| self.scale_of(&output))
    }

    /// Tells every surface at which scale to draw. Cheap to repeat: a
    /// surface hears of it only when it changes.
    pub fn update_preferred_scales(&mut self) {
        if self.painters.len() < 2 && !self.scales_dirty {
            return;
        }
        self.scales_dirty = false;
        let set = |scale: f64| {
            move |_: &WlSurface, states: &smithay::wayland::compositor::SurfaceData| {
                with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
            }
        };
        for window in self.space.elements().chain(self.minimised.iter()) {
            let scale = self
                .output_of_window(window)
                .map_or(self.output_scale, |output| self.scale_of(&output));
            window.with_surfaces(set(scale));
        }
        for output in self.space.outputs() {
            let scale = self.scale_of(output);
            for layer in layer_map_for_output(output).layers() {
                layer.with_surfaces(set(scale));
            }
        }
        for (output, surface) in self.lock_surfaces_by_output() {
            with_surfaces_surface_tree(&surface, set(self.scale_of(&output)));
        }
    }

    /// A program that manages outputs set something: it is saved once it
    /// has stayed that way for `SAVE_AFTER`. A later change starts the
    /// wait over, and one that is taken back (as Settings does with a
    /// change nobody keeps) writes what is there again, which is what was.
    pub fn output_layout_set_by_client(&mut self) {
        if let Some(token) = self.output_save.take() {
            self.loop_handle.remove(token);
        }
        let timer = Timer::from_duration(save_after());
        let armed = self.loop_handle.insert_source(timer, |_, _, state| {
            state.output_save = None;
            state.save_output_layout();
            TimeoutAction::Drop
        });
        self.output_save = armed.ok();
    }

    /// The layout as it is now, one entry per head.
    fn layout_now(&self) -> Vec<StoredHead> {
        self.heads
            .iter()
            .map(|head| {
                let output = &head.output;
                let physical = output.physical_properties();
                let loc = output.current_location();
                StoredHead {
                    name: output.name(),
                    make: physical.make,
                    model: physical.model,
                    serial: head.serial.clone(),
                    enabled: head.enabled,
                    mode: output
                        .current_mode()
                        .map(|mode| (mode.size.w, mode.size.h, mode.refresh)),
                    position: Some((loc.x, loc.y)),
                    scale: Some(scale_key(self.scale_of(output))),
                    transform: Some(output.current_transform()),
                }
            })
            .collect()
    }

    /// Writes the layout, with the entries of screens that are not here.
    pub fn save_output_layout(&mut self) {
        for stored in self.layout_now() {
            self.stored_layout.update(stored);
        }
        if let Err(err) = self.stored_layout.save() {
            crate::say!("cannot save the layout of the outputs: {err}");
        }
    }

    /// SIGHUP: reads the saved layout again and puts it in place, unless a
    /// change is waiting to be saved, which the file does not know of yet.
    pub fn reload_output_layout(&mut self) {
        if self.output_save.is_some() {
            return;
        }
        let fresh = StoredLayout::load();
        if fresh == self.stored_layout {
            return;
        }
        self.stored_layout = fresh;
        let mut plan = self.current_plan();
        for entry in &mut plan {
            let output = &entry.output;
            let physical = output.physical_properties();
            let serial = self.head(output).map(|head| head.serial.clone());
            let Some(stored) = self.stored_layout.find(
                &output.name(),
                &physical.make,
                &physical.model,
                serial.as_deref().unwrap_or(""),
            ) else {
                continue;
            };
            entry.enabled = stored.enabled;
            if let Some((w, h, refresh)) = stored.mode {
                entry.mode = OutputMode {
                    size: (w, h).into(),
                    refresh,
                };
            }
            if let Some(position) = stored.position {
                entry.position = position;
                entry.position_set = true;
            }
            if let Some(thousandths) = stored.scale {
                entry.scale = f64::from(thousandths) / 1000.0;
            }
            if let Some(transform) = stored.transform {
                entry.transform = transform;
            }
        }
        match self.apply_plan(plan) {
            Ok(()) => crate::say!("layout of the outputs read again"),
            Err(why) => crate::say!("the saved layout of the outputs does not fit: {why}"),
        }
    }
}

/// `SAVE_AFTER`, or what the tests set in its place.
fn save_after() -> Duration {
    if cfg!(feature = "test-hooks") {
        let set = std::env::var("SENGI_COMPOSITOR_OUTPUT_SAVE_MS").ok();
        if let Some(millis) = set.and_then(|text| text.parse().ok()) {
            return Duration::from_millis(millis);
        }
    }
    SAVE_AFTER
}

/// Whether `w` by `h` is a size a screen may have.
pub fn size_is_sane(w: i32, h: i32) -> bool {
    (1..=16384).contains(&w) && (1..=16384).contains(&h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Option<Rect> {
        Some(Rect::new(x, y, w, h))
    }

    #[test]
    fn neighbours_follow_a_screen_that_shrinks() {
        // Two screens side by side; the left one is drawn at twice the
        // scale, so it is half as wide on the desktop.
        let old = [r(0, 0, 1280, 720), r(1280, 0, 1280, 720)];
        let new = [r(0, 0, 640, 360), r(1280, 0, 1280, 720)];
        let at = follow_changes(&old, &new, &[false, false]);
        assert_eq!(at, [Some((0, 0)), Some((640, 0))]);
    }

    #[test]
    fn a_position_that_was_given_is_not_moved() {
        let old = [r(0, 0, 1280, 720), r(1280, 0, 1280, 720)];
        let new = [r(0, 0, 640, 360), r(1280, 0, 1280, 720)];
        let at = follow_changes(&old, &new, &[false, true]);
        assert_eq!(at[1], Some((1280, 0)));
    }

    #[test]
    fn a_screen_switched_off_closes_the_gap() {
        let old = [r(0, 0, 100, 100), r(100, 0, 100, 100), r(200, 0, 100, 100)];
        let new = [r(0, 0, 100, 100), None, r(200, 0, 100, 100)];
        let at = follow_changes(&old, &new, &[false; 3]);
        assert_eq!(at, [Some((0, 0)), None, Some((100, 0))]);
    }

    #[test]
    fn screens_one_above_the_other_keep_their_column() {
        let old = [r(0, 0, 100, 100), r(0, 100, 100, 100)];
        let new = [r(0, 0, 50, 50), r(0, 100, 100, 100)];
        let at = follow_changes(&old, &new, &[false; 2]);
        assert_eq!(at[1], Some((0, 50)));
    }

    #[test]
    fn overlap_is_found_and_touching_is_not_one() {
        let a = Rect::new(0, 0, 100, 100);
        assert!(!overlap(&[a, Rect::new(100, 0, 100, 100)]));
        assert!(overlap(&[a, Rect::new(99, 0, 100, 100)]));
        assert!(overlap(&[a, Rect::new(10, 10, 5, 5)]));
        assert!(!overlap(&[a]));
    }

    #[test]
    fn the_size_of_an_output_on_the_desktop() {
        let mode = OutputMode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        };
        assert_eq!(logical_size(mode, Transform::Normal, 1.0), (1920, 1080));
        assert_eq!(logical_size(mode, Transform::Normal, 2.0), (960, 540));
        assert_eq!(logical_size(mode, Transform::Normal, 1.25), (1536, 864));
        assert_eq!(logical_size(mode, Transform::_90, 1.0), (1080, 1920));
        assert_eq!(logical_size(mode, Transform::Flipped270, 2.0), (540, 960));
    }

    #[test]
    fn scales_make_keys_in_thousandths() {
        assert_eq!(scale_key(1.0), 1000);
        assert_eq!(scale_key(1.25), 1250);
        assert_eq!(scale_key(1.7499999), 1750);
    }
}
