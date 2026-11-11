//! What the compositor remembers about each window, and its frame.

use std::{
    cell::{Cell, RefCell},
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::element::{memory::MemoryRenderBuffer, solid::SolidColorBuffer},
    },
    desktop::Window,
    reexports::wayland_protocols::xdg::{
        decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode,
        shell::server::xdg_toplevel::State as ToplevelState,
    },
    utils::Transform,
    wayland::{
        compositor::with_states,
        shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData},
    },
};

use crate::{
    decoration::{self, Theme, Titlebar},
    frame::{self, Edges},
    layout::Rect,
    state::{set_activated, State},
};

#[derive(Default)]
pub struct WindowData {
    /// The compositor draws the title bar and border.
    pub decorated: Cell<bool>,
    /// Client area to go back to when the window is unmaximised.
    pub restore: Cell<Option<Rect>>,
    /// Set by an edge drag: the edges that moved, the client area the drag
    /// started from, and once the drag is over, when to stop caring. Until
    /// then the window is kept aligned to the opposite edges as the client
    /// catches up with new sizes.
    pub resize_anchor: Cell<Option<(Edges, Rect, Option<Instant>)>>,
    /// Where the client area was when the window was minimised. Set for as
    /// long as it is hidden.
    pub minimised: Cell<Option<(i32, i32)>>,
    pub frame: RefCell<Option<FrameBuffers>>,
}

/// The textures of a frame, kept until the frame looks different.
pub struct FrameBuffers {
    pub look: (Titlebar, String),
    pub titlebar: MemoryRenderBuffer,
    /// Plain fill for the part of a very wide title bar that is not painted.
    pub rest: SolidColorBuffer,
    pub left: SolidColorBuffer,
    pub right: SolidColorBuffer,
    pub bottom: SolidColorBuffer,
}

pub fn data(window: &Window) -> &WindowData {
    let map = window.user_data();
    map.insert_if_missing(WindowData::default);
    map.get::<WindowData>().expect("inserted above")
}

/// Cap on the painted title bar width when there is no output to go by.
const FALLBACK_PAINTED_WIDTH: i32 = 4096;

/// How long after an edge drag late commits are still aligned to it.
const RESIZE_SETTLE: Duration = Duration::from_millis(500);

/// Asks the client for a new client-area size.
pub fn request_size(window: &Window, size: (i32, i32), resizing: bool) {
    let Some(toplevel) = window.toplevel() else {
        return;
    };
    toplevel.with_pending_state(|state| {
        state.size = Some(size.into());
        if resizing {
            state.states.set(ToplevelState::Resizing);
        } else {
            state.states.unset(ToplevelState::Resizing);
        }
    });
    toplevel.send_pending_configure();
}

/// Ends an edge drag: the client may stop drawing in resize mode.
pub fn finish_resize(window: &Window) {
    let data = data(window);
    if let Some((edges, initial, _)) = data.resize_anchor.get() {
        let until = Instant::now() + RESIZE_SETTLE;
        data.resize_anchor.set(Some((edges, initial, Some(until))));
    }
    if let Some(toplevel) = window.toplevel() {
        toplevel.with_pending_state(|state| state.states.unset(ToplevelState::Resizing));
        toplevel.send_pending_configure();
    }
}

/// Whether the client agreed to let the compositor draw its frame.
pub fn wants_server_decoration(window: &Window) -> bool {
    window
        .toplevel()
        .is_some_and(|t| t.current_state().decoration_mode == Some(Mode::ServerSide))
}

pub fn title(window: &Window) -> String {
    let Some(toplevel) = window.toplevel() else {
        return String::new();
    };
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().unwrap().title.clone())
            .unwrap_or_default()
    })
}

pub fn app_id(window: &Window) -> String {
    let Some(toplevel) = window.toplevel() else {
        return String::new();
    };
    with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().unwrap().app_id.clone())
            .unwrap_or_default()
    })
}

impl State {
    /// Every window the user can get at: on screen or minimised.
    pub fn managed_windows(&self) -> Vec<Window> {
        self.space
            .elements()
            .chain(self.minimised.iter())
            .cloned()
            .collect()
    }

    /// Hides a window without closing it, or brings it back where it was.
    /// The dock is how it comes back, through foreign-toplevel activate.
    pub fn set_minimised(&mut self, window: &Window, minimised: bool) {
        self.request_redraw();
        let data = data(window);
        match (minimised, data.minimised.get()) {
            (true, None) => {
                let Some(location) = self.space.element_location(window) else {
                    return;
                };
                data.minimised.set(Some((location.x, location.y)));
                self.forget_pointer_state(window);
                self.space.unmap_elem(window);
                self.minimised.push(window.clone());
                set_activated(window, false);
                if self.focus_stack.remove(window) {
                    match self.focus_stack.current().cloned() {
                        Some(next) => self.focus_window(&next),
                        None => self.update_keyboard_focus(),
                    }
                }
            }
            (false, Some(location)) => {
                data.minimised.set(None);
                self.minimised.retain(|w| w != window);
                self.space.map_element(window.clone(), location, true);
            }
            _ => {}
        }
    }

    /// The client area of a mapped window in desktop coordinates.
    pub fn client_rect(&self, window: &Window) -> Option<Rect> {
        let location = self.space.element_location(window)?;
        let size = window.geometry().size;
        Some(Rect::new(location.x, location.y, size.w, size.h))
    }

    /// The outline of a mapped window: its frame if it has one, else the
    /// client area.
    pub fn frame_rect(&self, window: &Window) -> Option<Rect> {
        let client = self.client_rect(window)?;
        if data(window).decorated.get() {
            Some(self.theme.metrics.frame(client))
        } else {
            Some(client)
        }
    }

    /// Smallest and largest client size for `window`; zero means no limit.
    pub fn size_limits(&self, window: &Window) -> ((i32, i32), (i32, i32)) {
        let Some(toplevel) = window.toplevel() else {
            return ((0, 0), (0, 0));
        };
        let (mut min, max) = with_states(toplevel.wl_surface(), |states| {
            let mut guard = states.cached_state.get::<SurfaceCachedState>();
            let cached = guard.current();
            (cached.min_size, cached.max_size)
        });
        if data(window).decorated.get() {
            // Never narrower than the title bar buttons.
            let metrics = &self.theme.metrics;
            min.w = min.w.max(metrics.min_frame_width() - 2 * metrics.border);
        }
        ((min.w, min.h), (max.w, max.h))
    }

    /// Moves a window's client area to `(x, y)` without changing which
    /// window is on top.
    pub fn move_window(&mut self, window: &Window, x: i32, y: i32) {
        // Only windows on screen have a place; minimised ones keep theirs.
        match self.space.element_location(window) {
            Some(current) if current != (x, y).into() => {}
            _ => return,
        }
        // Mapping at a new place also raises, so put the order back after.
        self.request_redraw();
        let order: Vec<Window> = self.space.elements().cloned().collect();
        self.space.map_element(window.clone(), (x, y), false);
        for other in &order {
            self.space.raise_element(other, false);
        }
    }

    /// Moves a window so its frame's top-left corner is as close to
    /// `(x, y)` as it may be.
    pub fn move_frame_to(&mut self, window: &Window, x: i32, y: i32) {
        let Some(frame) = self.frame_rect(window) else {
            return;
        };
        let Some(client) = self.client_rect(window) else {
            return;
        };
        // Wherever a finished resize wanted the window, it goes here now.
        data(window).resize_anchor.set(None);
        let wanted = Rect::new(x, y, frame.w, frame.h);
        let (x, y) = self.reachable(wanted);
        self.move_window(window, x + client.x - frame.x, y + client.y - frame.y);
    }

    fn reachable(&self, frame: Rect) -> (i32, i32) {
        let metrics = &self.theme.metrics;
        frame::keep_reachable(
            self.usable_area(),
            frame,
            metrics.titlebar,
            metrics.min_frame_width(),
        )
    }

    /// Keeps a window that was resized by its left or top edge aligned to
    /// the edges that did not move. Called when the client commits.
    pub fn apply_resize_anchor(&mut self, window: &Window) {
        let data = data(window);
        let Some((edges, initial, until)) = data.resize_anchor.get() else {
            return;
        };
        let size = window.geometry().size;
        let expired = until.is_some_and(|until| Instant::now() >= until);
        match frame::anchored(initial, edges, (size.w, size.h), expired) {
            Some((x, y)) => self.move_window(window, x, y),
            None => data.resize_anchor.set(None),
        }
    }

    /// The client area a maximised `window` gets.
    fn maximised_rect(&self, window: &Window) -> Rect {
        let usable = self.usable_area();
        if data(window).decorated.get() {
            self.theme.metrics.client(usable)
        } else {
            usable
        }
    }

    pub fn set_maximised(&mut self, window: &Window, maximised: bool) {
        let data = data(window);
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let target = match (maximised, data.restore.get()) {
            (true, None) => {
                let Some(current) = self.client_rect(window) else {
                    return;
                };
                data.restore.set(Some(current));
                self.maximised_rect(window)
            }
            (false, Some(restore)) => {
                data.restore.set(None);
                restore
            }
            // Already in the state asked for; still owed a configure.
            _ => {
                toplevel.send_configure();
                return;
            }
        };

        data.resize_anchor.set(None);
        // A drag of this window cannot go on from a different geometry.
        self.forget_pointer_state(window);
        toplevel.with_pending_state(|state| {
            state.size = Some((target.w, target.h).into());
            if maximised {
                state.states.set(ToplevelState::Maximized);
            } else {
                state.states.unset(ToplevelState::Maximized);
            }
        });
        toplevel.send_configure();
        self.move_window(window, target.x, target.y);
    }

    pub fn toggle_maximise(&mut self, window: &Window) {
        let maximised = data(window).restore.get().is_some();
        self.set_maximised(window, !maximised);
    }

    /// The bar or dock appeared, went away or changed size: maximised
    /// windows follow, the others are pulled out from under it.
    pub fn usable_area_changed(&mut self) {
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            if data(&window).restore.get().is_some() {
                let target = self.maximised_rect(&window);
                data(&window).resize_anchor.set(None);
                request_size(&window, (target.w, target.h), false);
                self.move_window(&window, target.x, target.y);
            } else if let Some(frame) = self.frame_rect(&window) {
                self.move_frame_to(&window, frame.x, frame.y);
            }
        }
    }

    /// The widest title bar that is painted as a texture.
    fn max_painted_width(&self) -> i32 {
        self.output()
            .and_then(|output| output.current_mode())
            .map(|mode| mode.size.w)
            .unwrap_or(FALLBACK_PAINTED_WIDTH)
            .max(self.theme.metrics.min_frame_width())
    }

    /// Makes sure the frame textures of `window` match how it should look
    /// now. `width` and `height` are the client size.
    pub fn refresh_frame(&self, window: &Window, width: i32, height: i32) {
        let theme: &Theme = &self.theme;
        let border = theme.metrics.border;
        let hovered = match &self.hovered_button {
            Some((hovered, button)) if hovered == window => Some(*button),
            _ => None,
        };
        // The client picks the window's width, so it must not pick how much
        // memory the title bar takes: nothing wider than the screen is
        // painted, the remainder is a flat fill.
        let full = width + 2 * border;
        let painted = full.min(self.max_painted_width());
        let active = self.focus_stack.current() == Some(window);
        let bar = Titlebar {
            width: painted,
            active,
            hovered,
            square: data(window).restore.get().is_some(),
            cut: painted < full,
        };
        let look = (bar, title(window));
        let fill = self.color(if active { theme.active } else { theme.inactive });
        let rest_size = (full - painted, theme.metrics.titlebar);

        let mut slot = data(window).frame.borrow_mut();
        match slot.as_mut() {
            Some(buffers) if buffers.look == look => {}
            _ => {
                let text = self.font.as_ref().map(|font| font.render(&look.1));
                let canvas = decoration::draw_titlebar(theme, &look.0, text.as_ref());
                let size = (look.0.width.max(1), theme.metrics.titlebar.max(1));
                let titlebar = MemoryRenderBuffer::from_slice(
                    &canvas.into_pixels(),
                    Fourcc::Argb8888,
                    size,
                    1,
                    Transform::Normal,
                    None,
                );
                let color = self.color(theme.border);
                let side = || SolidColorBuffer::new((border, height), color);
                *slot = Some(FrameBuffers {
                    look,
                    titlebar,
                    rest: SolidColorBuffer::new(rest_size, fill),
                    left: side(),
                    right: side(),
                    bottom: SolidColorBuffer::new((width + 2 * border, border), color),
                });
            }
        }
        if let Some(buffers) = slot.as_mut() {
            let color = self.color(theme.border);
            buffers.rest.update(rest_size, fill);
            buffers.left.update((border, height), color);
            buffers.right.update((border, height), color);
            buffers.bottom.update((width + 2 * border, border), color);
        }
    }
}
