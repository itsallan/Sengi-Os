//! What the compositor remembers about each window, and its frame.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::element::{memory::MemoryRenderBuffer, solid::SolidColorBuffer},
    },
    desktop::Window,
    output::Output,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as ToplevelState,
    utils::{Logical, Rectangle, Size, Transform},
    wayland::{
        compositor::with_states,
        shell::xdg::{SurfaceCachedState, XdgToplevelSurfaceData},
    },
    xwayland::X11Surface,
};

use crate::{
    decoration::{self, TextBitmap, Theme, Titlebar},
    frame::{self, Edges},
    input::Drag,
    keys::Edge,
    layout::Rect,
    outputs::scale_key,
    snap::Snap,
    state::{set_activated, State},
    text::Font,
};

/// The width of a frame around a client `width` wide. The client picks that
/// width, so the sum is saturated rather than trusted not to overflow.
fn frame_span(width: i32, border: i32) -> i32 {
    width.saturating_add(border.saturating_mul(2))
}

/// The widest title bar, in logical pixels, that is painted on a screen
/// `screen_w` pixels wide at `scale`. A scale that is not a positive number
/// counts as one.
fn painted_limit(screen_w: i32, scale: f64, floor: i32) -> i32 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    ((f64::from(screen_w) / scale).ceil() as i32).max(floor)
}

#[cfg(test)]
mod painted_tests {
    use super::{frame_span, painted_limit};

    #[test]
    fn a_huge_client_width_does_not_overflow_the_frame() {
        assert_eq!(frame_span(i32::MAX, 2), i32::MAX);
        assert_eq!(frame_span(i32::MAX - 1, 1), i32::MAX);
        assert_eq!(frame_span(300, 1), 302);
    }

    #[test]
    fn the_title_cap_is_the_screen_in_logical_pixels_at_any_scale() {
        assert_eq!(painted_limit(1280, 1.0, 100), 1280);
        assert_eq!(painted_limit(1280, 2.0, 100), 640);
        // Below one the screen is larger in logical pixels, not smaller.
        assert_eq!(painted_limit(1280, 0.5, 100), 2560);
        // A scale of 60 is far past anything the compositor takes; the floor
        // still holds, and nothing divides by zero.
        assert_eq!(painted_limit(1280, 60.0, 100), 100);
        assert_eq!(painted_limit(1280, 0.0, 100), 1280);
        assert_eq!(painted_limit(1280, f64::NAN, 100), 1280);
    }
}

/// The title last rasterised: its text, the room it had, and its pixels.
pub type TitleCache = Option<(String, usize, Option<Rc<TextBitmap>>)>;

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
    /// The client asked to be maximised before its first buffer.
    pub start_maximised: Cell<bool>,
    /// Set once a window too large for the screen has been asked to be
    /// smaller, so that it is mapped whatever it answers the second time.
    pub fitted: Cell<bool>,
    /// The screen a window asked to fill before it was first shown.
    pub fullscreen_on: RefCell<Option<Output>>,
    /// Set while the window fills a screen: the client area to go back
    /// to, once it has had one.
    pub fullscreen: Cell<Option<Option<Rect>>>,
    /// The frame as painted for each scale an output has, by `scale_key`.
    pub frame: RefCell<HashMap<u32, FrameBuffers>>,
    /// The shadow pictures of this window, by `scale_key` and whether it is
    /// focused.
    pub shadows: RefCell<HashMap<(u32, bool), crate::shadow::Buffers>>,
    /// The names the cut pieces of the client are drawn under.
    pub corner_ids: RefCell<crate::corners::Ids>,
    /// The title last rasterised, the room it had and its pixels, if the
    /// typeface is there.
    pub title: RefCell<TitleCache>,
}

/// The textures of a frame, kept until the frame looks different.
pub struct FrameBuffers {
    pub look: (Titlebar, String),
    /// Width, height and whether square: what the bottom is shaped for.
    pub shape: (i32, i32, bool),
    /// The title bar, in the screen's pixels.
    pub titlebar: MemoryRenderBuffer,
    /// Its size in logical pixels.
    pub titlebar_size: (i32, i32),
    /// Plain fill for the part of a very wide title bar that is not painted.
    pub rest: SolidColorBuffer,
    pub left: SolidColorBuffer,
    pub right: SolidColorBuffer,
    pub bottom: SolidColorBuffer,
    /// The border around the left and right bottom corners, when they are
    /// rounded: drawn in front of the client, whose own square corners are
    /// cut away. Each is `bottom_corner` pixels square.
    pub bottom_corners: Option<[MemoryRenderBuffer; 2]>,
    /// The border, in logical pixels.
    pub border: i32,
    /// What the client loses of each of its bottom rows, in the screen's
    /// pixels, see `decoration::bottom_insets`.
    pub insets: Vec<i32>,
    /// The border in the screen's pixels, and the side of a corner there.
    pub border_pixels: i32,
    pub corner_pixels: i32,
    /// Where they are, from the top of the frame's client area.
    pub bottom_top: i32,
    /// Their size, and how far the right one is from the left one.
    pub bottom_corner: i32,
    pub bottom_right: i32,
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

/// The X11 side of a window, if it is one.
pub fn x11(window: &Window) -> Option<&X11Surface> {
    window.x11_surface()
}

pub fn is_x11(window: &Window) -> bool {
    x11(window).is_some()
}

/// A menu of an X11 toolkit: override-redirect, so the compositor does not
/// manage it.
pub fn is_override(window: &Window) -> bool {
    x11(window).is_some_and(X11Surface::is_override_redirect)
}

/// Whether the window's client is there to be told things: an xdg toplevel
/// or an X11 window.
fn is_client(window: &Window) -> bool {
    window.toplevel().is_some() || is_x11(window)
}

/// Sends the xdg configure that carries the pending state of `window`. An X11
/// window has no such thing: its state is set as it changes.
fn send_configure(window: &Window) {
    if let Some(toplevel) = window.toplevel() {
        toplevel.send_configure();
    }
}

/// Tells an X11 window where its client area is on the desktop and how large
/// it is. The compositor's placement is what X11 sees as the window's place.
pub fn place_x11(window: &Window, x: i32, y: i32, size: (i32, i32)) {
    if let Some(x11) = x11(window) {
        let _ = x11.configure(Rectangle::new((x, y).into(), Size::from(size)));
    }
}

/// Asks the client for a new client-area size.
pub fn request_size(window: &Window, size: (i32, i32), resizing: bool) {
    if let Some(x11) = x11(window) {
        let at = x11.geometry().loc;
        place_x11(window, at.x, at.y, size);
        return;
    }
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

/// Whether the compositor draws the window's frame. An X11
/// window gets one unless its Motif hints ask for none; Smithay's
/// `is_decorated` is true exactly when they do.
pub fn wants_server_decoration(window: &Window) -> bool {
    if let Some(x11) = x11(window) {
        return !x11.is_decorated();
    }
    // An xdg toplevel always gets our frame, whatever it said to
    // xdg-decoration or whether it said anything. Chromium, Firefox and GTK4
    // draw their own bar; it stays inside our frame so the user always has
    // close, minimise and maximise.
    true
}

/// Whether a window that is on screen gets a frame now: it does unless it
/// is fullscreen, and unless the client asked for none.
pub fn wants_frame(window: &Window) -> bool {
    wants_server_decoration(window) && data(window).fullscreen.get().is_none()
}

/// Asks the program in `window` to close it, as its title bar does.
pub fn close(window: &Window) {
    if let Some(toplevel) = window.toplevel() {
        toplevel.send_close();
    } else if let Some(x11) = x11(window) {
        let _ = x11.close();
    }
}

/// Looks at the window's title without copying it.
pub fn with_title<T>(window: &Window, f: impl FnOnce(&str) -> T) -> T {
    if let Some(x11) = x11(window) {
        return f(&x11.title());
    }
    let Some(toplevel) = window.toplevel() else {
        return f("");
    };
    with_states(toplevel.wl_surface(), |states| {
        let data = states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .map(|data| data.lock().unwrap());
        f(data
            .as_ref()
            .and_then(|data| data.title.as_deref())
            .unwrap_or(""))
    })
}

pub fn title(window: &Window) -> String {
    with_title(window, str::to_string)
}

pub fn app_id(window: &Window) -> String {
    with_app_id(window, str::to_string)
}

pub fn with_app_id<T>(window: &Window, f: impl FnOnce(&str) -> T) -> T {
    // An X11 window's app id is its WM_CLASS class, the name of its program.
    if let Some(x11) = x11(window) {
        return f(&x11.class());
    }
    let Some(toplevel) = window.toplevel() else {
        return f("");
    };
    with_states(toplevel.wl_surface(), |states| {
        let data = states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .map(|data| data.lock().unwrap());
        f(data
            .as_ref()
            .and_then(|data| data.app_id.as_deref())
            .unwrap_or(""))
    })
}

impl State {
    /// Every window the user can get at: on screen or minimised. Menus of
    /// X11 toolkits are not windows of the user's.
    pub fn managed_windows(&self) -> Vec<Window> {
        self.space
            .elements()
            .chain(self.minimised.iter())
            .filter(|window| !is_override(window))
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
                // Asked to fill the screen while it was hidden: it comes
                // back filling it, on the screen it was asked for.
                let location = if data.fullscreen.get().is_some() {
                    let asked = data.fullscreen_on.borrow().clone();
                    self.fullscreen_rect(window, asked)
                        .map_or(location, |screen| (screen.x, screen.y))
                } else {
                    location
                };
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
        let (mut min, max): (Size<i32, Logical>, Size<i32, Logical>) =
            if let Some(x11) = x11(window) {
                let zero = Size::from((0, 0));
                (
                    x11.min_size().unwrap_or(zero),
                    x11.max_size().unwrap_or(zero),
                )
            } else {
                let Some(toplevel) = window.toplevel() else {
                    return ((0, 0), (0, 0));
                };
                with_states(toplevel.wl_surface(), |states| {
                    let mut guard = states.cached_state.get::<SurfaceCachedState>();
                    let cached = guard.current();
                    (cached.min_size, cached.max_size)
                })
            };
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
        if !is_override(window) {
            let size = window.geometry().size;
            place_x11(window, x, y, (size.w, size.h));
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
            self.usable_area_near(frame),
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
    /// The usable area of the screen the middle of `frame` is on.
    pub fn usable_area_near(&self, frame: Rect) -> Rect {
        let middle = (
            f64::from(frame.x + frame.w / 2),
            f64::from(frame.y + frame.h / 2),
        );
        self.output_at(middle.into())
            .map(|output| self.usable_area_of(&output))
            .unwrap_or_else(|| self.usable_area())
    }

    pub fn maximised_rect(&self, window: &Window) -> Rect {
        let usable = match self.frame_rect(window) {
            Some(frame) => self.usable_area_near(frame),
            None => self.usable_area(),
        };
        let mut rect = if data(window).decorated.get() {
            self.theme.metrics.client(usable)
        } else {
            usable
        };
        // A client that cannot grow that far stays in the top-left corner.
        let (_, max) = self.size_limits(window);
        if max.0 > 0 {
            rect.w = rect.w.min(max.0);
        }
        if max.1 > 0 {
            rect.h = rect.h.min(max.1);
        }
        rect
    }

    pub fn set_maximised(&mut self, window: &Window, maximised: bool) {
        let data = data(window);
        if !is_client(window) {
            return;
        }
        // A window that fills the screen is left as it is.
        if data.fullscreen.get().is_some() {
            send_configure(window);
            return;
        }
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
                send_configure(window);
                return;
            }
        };

        data.resize_anchor.set(None);
        // A drag of this window cannot go on from a different geometry.
        self.forget_pointer_state(window);
        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|state| {
                state.size = Some((target.w, target.h).into());
                if maximised {
                    state.states.set(ToplevelState::Maximized);
                } else {
                    state.states.unset(ToplevelState::Maximized);
                }
            });
            toplevel.send_configure();
        } else if let Some(x11) = x11(window) {
            let _ = x11.set_maximized(maximised);
            request_size(window, (target.w, target.h), false);
        }
        self.move_window(window, target.x, target.y);
        // A minimised window has no place to move; it comes back there.
        if data.minimised.get().is_some() {
            data.minimised.set(Some((target.x, target.y)));
        }
        if !maximised {
            // The bar may have moved while the window was maximised.
            if let Some(frame) = self.frame_rect(window) {
                self.move_frame_to(window, frame.x, frame.y);
            }
        }
    }

    /// The screen a window fills when it goes fullscreen: the one named,
    /// else the one most of it is on, else the first.
    pub fn fullscreen_rect(&self, window: &Window, output: Option<Output>) -> Option<Rect> {
        let output = output
            .or_else(|| {
                let frame = self.frame_rect(window)?;
                let middle = (
                    f64::from(frame.x + frame.w / 2),
                    f64::from(frame.y + frame.h / 2),
                );
                self.output_at(middle.into())
            })
            .or_else(|| self.active_output())?;
        let screen = self.space.output_geometry(&output)?;
        Some(Rect::new(
            screen.loc.x,
            screen.loc.y,
            screen.size.w,
            screen.size.h,
        ))
    }

    /// Makes `window` fill a screen, without a frame and over the bar and
    /// the dock for as long as it is the window in front, or puts it back
    /// where it was. Before the window is on screen this only tells the
    /// client. Its first map places it on its screen and focuses it, in
    /// `shell.rs`: the one time a fullscreen window is brought to the front
    /// without being asked to be.
    pub fn set_fullscreen(&mut self, window: &Window, fullscreen: bool, output: Option<Output>) {
        let data = data(window);
        if !is_client(window) {
            return;
        }
        let mapped = self.space.element_location(window).is_some();
        match (fullscreen, data.fullscreen.get()) {
            (true, None) => {
                *data.fullscreen_on.borrow_mut() = output.clone();
                let Some(screen) = self.fullscreen_rect(window, output) else {
                    return;
                };
                data.fullscreen.set(Some(self.client_rect(window)));
                data.decorated.set(false);
                data.resize_anchor.set(None);
                self.forget_pointer_state(window);
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.states.set(ToplevelState::Fullscreen);
                        state.size = Some((screen.w, screen.h).into());
                    });
                    if mapped || toplevel.is_initial_configure_sent() {
                        toplevel.send_configure();
                    }
                } else if let Some(x11) = x11(window) {
                    let _ = x11.set_fullscreen(true);
                    request_size(window, (screen.w, screen.h), false);
                }
                // It is not brought in front for asking: a window behind
                // stays behind, and under the bar, until the user goes to
                // it. Otherwise any program could put itself over
                // everything by asking.
                if mapped {
                    self.move_window(window, screen.x, screen.y);
                }
            }
            (false, Some(restore)) => {
                data.fullscreen.set(None);
                data.decorated.set(wants_server_decoration(window));
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.states.unset(ToplevelState::Fullscreen);
                        state.size = restore.map(|rect| (rect.w, rect.h).into());
                    });
                    toplevel.send_configure();
                } else if let Some(x11) = x11(window) {
                    let _ = x11.set_fullscreen(false);
                    if let Some(rect) = restore {
                        request_size(window, (rect.w, rect.h), false);
                    }
                }
                if let (true, Some(rect)) = (mapped, restore) {
                    self.move_window(window, rect.x, rect.y);
                }
            }
            // Already as asked; still owed an answer once it can have one.
            _ if mapped => {
                send_configure(window);
            }
            _ => {}
        }
        self.request_redraw();
    }

    /// The window in front, if it fills its screen. That one alone is
    /// drawn over the bar and the dock; any other window brought in front
    /// of it brings them back.
    pub fn fullscreen_in_front(&self) -> Option<Window> {
        self.space
            .elements()
            .rev()
            .find(|window| !is_override(window))
            .filter(|window| data(window).fullscreen.get().is_some())
            .cloned()
    }

    /// The usable part of the screen `window` is on, inset by the gap
    /// labwc keeps from the edges for these actions.
    fn snap_area(&self, window: &Window) -> Option<Rect> {
        let frame = self.frame_rect(window)?;
        let usable = self.usable_area_near(frame);
        let gap = self.edge_gap();
        Some(Rect::new(
            usable.x + gap,
            usable.y + gap,
            (usable.w - 2 * gap).max(1),
            (usable.h - 2 * gap).max(1),
        ))
    }

    /// The gap labwc keeps between windows and the screen edges, from the
    /// same token its rc.xml takes.
    fn edge_gap(&self) -> i32 {
        i32::try_from(self.tokens.px("space.xs").unwrap_or(0)).unwrap_or(0)
    }

    /// Gives `window` the frame `frame`, without changing whether it is
    /// maximised. Remembers the size before the first snap.
    fn fill_frame(&mut self, window: &Window, frame: Rect) {
        let frame = Rect::new(frame.x, frame.y, frame.w.max(1), frame.h.max(1));
        let data = data(window);
        if !is_client(window) {
            return;
        }
        let client = if data.decorated.get() {
            self.theme.metrics.client(frame)
        } else {
            frame
        };
        data.resize_anchor.set(None);
        self.forget_pointer_state(window);
        request_size(window, (client.w, client.h), false);
        self.move_window(window, client.x, client.y);
        self.request_redraw();
    }

    /// The frame `window` takes when it fills half of the screen on
    /// `edge`, as labwc's SnapToEdge does.
    pub fn half_frame(&self, window: &Window, edge: Edge) -> Option<Rect> {
        let area = self.snap_area(window)?;
        let gap = self.edge_gap();
        Some(match edge {
            Edge::Left => Rect::new(area.x, area.y, area.w / 2 - gap / 2, area.h),
            Edge::Right => {
                let at = area.w / 2 + gap / 2;
                Rect::new(area.x + at, area.y, area.w - at, area.h)
            }
            Edge::Up => Rect::new(area.x, area.y, area.w, area.h / 2 - gap / 2),
            Edge::Down => {
                let at = area.h / 2 + gap / 2;
                Rect::new(area.x, area.y + at, area.w, area.h - at)
            }
        })
    }

    /// Whether letting go of `window` at a screen edge may snap it: not a
    /// window that fills the screen, or a menu.
    pub fn can_snap(&self, window: &Window) -> bool {
        is_client(window)
            && !is_override(window)
            && data(window).fullscreen.get().is_none()
            && self.frame_rect(window).is_some()
    }

    /// Fills half of the screen on `edge`, as labwc's SnapToEdge does. A
    /// maximised window is first given back its size.
    pub fn snap_to_edge(&mut self, window: &Window, edge: Edge) {
        if data(window).fullscreen.get().is_some() {
            return;
        }
        if data(window).restore.get().is_some() {
            self.set_maximised(window, false);
        }
        let Some(frame) = self.half_frame(window, edge) else {
            return;
        };
        self.fill_frame(window, frame);
    }

    /// Moves `window` to the edge of the screen on `edge`, keeping its
    /// size, as labwc's MoveToEdge does with no other window in the way.
    pub fn move_to_edge(&mut self, window: &Window, edge: Edge) {
        if data(window).restore.get().is_some() {
            return;
        }
        if data(window).fullscreen.get().is_some() {
            return;
        }
        let (Some(frame), Some(area)) = (self.frame_rect(window), self.snap_area(window)) else {
            return;
        };
        let (x, y) = match edge {
            Edge::Left => (area.x, frame.y),
            Edge::Right => (area.right() - frame.w, frame.y),
            Edge::Up => (frame.x, area.y),
            Edge::Down => (frame.x, area.bottom() - frame.h),
        };
        self.move_frame_to(window, x, y);
    }

    /// The window was let go of after a move, at `location`: at the top
    /// edge of its screen it is maximised, at a side it is snapped to that
    /// side, as labwc does with its default range of one pixel and
    /// topMaximize.
    pub fn snap_after_move(&mut self, window: &Window, location: (f64, f64)) {
        match self.snap_for(location) {
            Some((_, Snap::Maximise)) => self.set_maximised(window, true),
            Some((_, Snap::Half(edge))) => self.snap_to_edge(window, edge),
            None => {}
        }
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

    /// The widest title bar that is painted as a texture at `scale`: no
    /// wider than the widest screen drawn at that scale.
    fn max_painted_width(&self, scale: f64) -> i32 {
        let floor = self.theme.metrics.min_frame_width();
        self.space
            .outputs()
            .filter(|output| scale_key(self.scale_of(output)) == scale_key(scale))
            .filter_map(Output::current_mode)
            .map(|mode| painted_limit(mode.size.w, scale, floor))
            .max()
            .unwrap_or(FALLBACK_PAINTED_WIDTH)
            .max(floor)
    }

    /// The title of `window` as pixels, cut to `room` pixels with an
    /// ellipsis if it does not fit. Rasterised once per title and room.
    fn title_bitmap(
        &self,
        window: &Window,
        room: usize,
        font: Option<&Font>,
    ) -> (String, Option<Rc<TextBitmap>>) {
        let mut cache = data(window).title.borrow_mut();
        let fresh = cache.as_ref().is_some_and(|(text, cached_room, _)| {
            *cached_room == room && with_title(window, |title| title == text)
        });
        if !fresh {
            let text = with_title(window, str::to_string);
            let bitmap = font.map(|font| Rc::new(font.fit(&text, room)));
            *cache = Some((text, room, bitmap));
        }
        cache
            .as_ref()
            .map(|(text, _, bitmap)| (text.clone(), bitmap.clone()))
            .unwrap_or_default()
    }

    /// Makes sure the frame textures of `window` match how it should look
    /// now. `width` and `height` are the client size, in logical pixels.
    /// The frame is painted at the screen's scale, with the theme scaled to
    /// match, so it is as sharp there as the screen allows. Nothing is
    /// painted or allocated while the look stays the same.
    pub fn refresh_frame(&self, window: &Window, width: i32, height: i32, scale: f64) {
        let logical: &Theme = &self.theme;
        let painter = self.painter(scale);
        let paint: &Theme = &painter.theme;
        let key = scale_key(scale);
        let to_pixels = |v: i32| (f64::from(v) * scale).round() as i32;
        let border = logical.metrics.border;
        let group_hovered = self.hovered_group.as_ref() == Some(window);
        // The client picks the window's width, so it must not pick how much
        // memory the title bar takes: nothing wider than the screen is
        // painted, the remainder is a flat fill.
        let full = frame_span(width, border);
        let painted = full.min(self.max_painted_width(scale));
        let active = self.focus_stack.current() == Some(window);
        let square = data(window).restore.get().is_some();
        // Held down, and the pointer still on it: moving between the buttons
        // does not change what the bar looks like, only this does.
        let pressed = match (&self.drag, &self.hovered_button) {
            (
                Some(Drag::Press {
                    window: held,
                    button,
                }),
                Some((over, on)),
            ) if held == window && over == window && on == button => Some(*button),
            _ => None,
        };
        let bar = Titlebar {
            width: to_pixels(painted),
            active,
            group_hovered,
            pressed,
            square,
            cut: painted < full,
        };
        let fill = self.color(if active {
            logical.active
        } else {
            logical.inactive
        });
        let line = self.color(logical.border);
        let rest_size = (full - painted, logical.metrics.titlebar);
        // Square corners at the bottom when the window fills the screen.
        let radius = if square { 0 } else { logical.radius };
        // The size of each bottom corner: the radius and the border.
        let corner = logical.radius + border;
        let side_height = (height - radius).max(0);
        // The straight part of the bottom edge: the corners have their own.
        let bottom_width = if square {
            full
        } else {
            (full - 2 * corner).max(0)
        };
        let shape = (full, height, square);
        // Where the title may go: between the buttons and the frame's edge.
        let room = to_pixels(painted)
            - (paint.metrics.buttons_end() + paint.metrics.padding)
            - (paint.metrics.padding + paint.metrics.border);

        let mut slot = data(window).frame.borrow_mut();
        let unchanged = slot.get(&key).is_some_and(|buffers| {
            buffers.look.0 == bar
                && buffers.shape == shape
                && with_title(window, |title| title == buffers.look.1)
        });
        if !unchanged {
            let (title, text) =
                self.title_bitmap(window, room.max(0) as usize, painter.title_font.as_ref());
            let canvas = decoration::draw_titlebar(paint, &bar, text.as_deref());
            let size = (bar.width.max(1), paint.metrics.titlebar.max(1));
            // The buffer carries the output's scale, rounded. That is what keeps
            // the edges sharp at a scale of two. Below one the buffer is drawn
            // at half size and the bar is partly unpainted: a known limit, see
            // the ignored half-scale test in tests/windows.rs.
            let titlebar = MemoryRenderBuffer::from_slice(
                &canvas.into_pixels(),
                Fourcc::Argb8888,
                size,
                scale.round().max(1.0) as i32,
                Transform::Normal,
                None,
            );
            // Two small corners, whatever the window's width: the client
            // sets that, so nothing here may grow with it.
            let corner_pixels = to_pixels(corner).max(1);
            let border_pixels = to_pixels(border);
            let radius_pixels = to_pixels(radius) as f32;
            let insets = if square {
                Vec::new()
            } else {
                decoration::bottom_insets(corner_pixels, radius_pixels, border_pixels)
            };
            let bottom_corners = (!square).then(|| {
                [false, true].map(|right| {
                    let picture = decoration::draw_bottom_corner(
                        corner_pixels,
                        radius_pixels,
                        border_pixels,
                        logical.border,
                        right,
                    );
                    MemoryRenderBuffer::from_slice(
                        &picture.into_pixels(),
                        Fourcc::Argb8888,
                        (corner_pixels, corner_pixels),
                        scale.round().max(1.0) as i32,
                        Transform::Normal,
                        None,
                    )
                })
            });
            slot.insert(
                key,
                FrameBuffers {
                    look: (bar, title),
                    shape,
                    titlebar,
                    titlebar_size: (painted, logical.metrics.titlebar),
                    rest: SolidColorBuffer::new(rest_size, fill),
                    left: SolidColorBuffer::new((border, side_height), line),
                    right: SolidColorBuffer::new((border, side_height), line),
                    bottom: SolidColorBuffer::new((bottom_width, border), line),
                    bottom_corners,
                    border,
                    insets,
                    border_pixels,
                    corner_pixels,
                    bottom_top: height - radius,
                    bottom_corner: corner,
                    bottom_right: (full - corner).max(0),
                },
            );
        }
        if let Some(buffers) = slot.get_mut(&key) {
            buffers.rest.update(rest_size, fill);
            buffers.left.update((border, side_height), line);
            buffers.right.update((border, side_height), line);
            buffers.titlebar_size = (painted, logical.metrics.titlebar);
            buffers.bottom.update((bottom_width, border), line);
        }
    }
}
