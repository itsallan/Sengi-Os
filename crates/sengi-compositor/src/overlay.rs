//! The window switcher and the menus the compositor draws over the desktop:
//! what is open, the keys and the pointer that move through it, what each
//! item does, and the element that shows it. Only one of them is open at a
//! time, and none opens on the lock screen or the login screen.

use smithay::{
    backend::allocator::Fourcc,
    backend::renderer::element::memory::MemoryRenderBuffer,
    backend::renderer::{
        element::{memory::MemoryRenderBufferRenderElement, Kind},
        ImportMem, Renderer, Texture,
    },
    desktop::Window,
    output::Output,
    utils::{Logical, Physical, Point, Size, Transform},
};

use crate::{
    card::{self, Layout, Row, Tile},
    keys::{self, Action},
    render::OutputElement,
    state::State,
    window,
};

/// What is open.
pub enum Overlay {
    /// Alt+Tab: the windows in focus order, and the one that Alt releases to.
    Switcher {
        windows: Vec<Window>,
        selected: usize,
    },
    /// A menu, at the pointer where it was opened (logical, on the desktop).
    Menu {
        kind: MenuKind,
        selected: Option<usize>,
        at: (f64, f64),
    },
}

#[derive(Clone)]
pub enum MenuKind {
    /// Right-click on the desktop: what labwc's root menu holds.
    Desktop,
    /// Right-click on a title bar: what labwc's default client menu holds,
    /// less what the compositor cannot do.
    Window(Window),
}

/// One item of a menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    Search,
    Terminal,
    Settings,
    Lock,
    Session,
    Minimise,
    Maximise,
    Fullscreen,
    Close,
}

/// The settings and session programs, as menu.xml runs them.
const SETTINGS: &str = "sengi-run sengi-settings";
const SESSION: &str = "sengi-power";

fn rows(kind: &MenuKind) -> Vec<Option<Item>> {
    match kind {
        MenuKind::Desktop => vec![
            Some(Item::Search),
            Some(Item::Terminal),
            Some(Item::Settings),
            None,
            Some(Item::Lock),
            Some(Item::Session),
        ],
        MenuKind::Window(_) => vec![
            Some(Item::Minimise),
            Some(Item::Maximise),
            Some(Item::Fullscreen),
            Some(Item::Close),
        ],
    }
}

fn label(item: Item) -> &'static str {
    match item {
        Item::Search => "Search",
        Item::Terminal => "Terminal",
        Item::Settings => "Settings",
        Item::Lock => "Lock",
        Item::Session => "Session...",
        Item::Minimise => "Minimize",
        Item::Maximise => "Maximize",
        Item::Fullscreen => "Fullscreen",
        Item::Close => "Close",
    }
}

impl State {
    /// Whether the switcher or a menu is open.
    pub fn overlay_open(&self) -> bool {
        self.overlay.is_some()
    }

    /// Whether the keys and the pointer may open a card: not on the lock
    /// screen, where nothing of the session shows, nor on the login screen.
    fn may_open_overlay(&self) -> bool {
        !self.is_locked() && !self.login_screen
    }

    /// Alt+Tab: opens the switcher with the next window selected, or the
    /// previous one going backwards. Focus does not move until Alt is let go.
    pub fn open_switcher(&mut self, forward: bool) {
        if !self.may_open_overlay() {
            return;
        }
        let mut windows: Vec<Window> = self
            .focus_stack
            .recent_first()
            .into_iter()
            .filter(|window| self.space.element_location(window).is_some())
            .collect();
        let capacity = self.switcher_capacity();
        windows.truncate(capacity);
        if windows.is_empty() {
            return;
        }
        let count = windows.len();
        let selected = if count == 1 {
            0
        } else if forward {
            1
        } else {
            count - 1
        };
        self.overlay = Some(Overlay::Switcher { windows, selected });
        self.request_redraw();
    }

    /// How many tiles the switcher can show on the first output, the most
    /// recent windows first. The rest are not offered.
    fn switcher_capacity(&self) -> usize {
        let Some(output) = self.output() else {
            return 1;
        };
        let Some(geometry) = self.space.output_geometry(&output) else {
            return 1;
        };
        let screen_w = (f64::from(geometry.size.w) * self.output_scale).round() as i32;
        card::switcher_capacity(&self.paint_theme, screen_w)
    }

    /// A window went away: the switcher stops offering it, and a menu for its
    /// title bar closes with it.
    pub fn forget_overlay_window(&mut self, window: &Window) {
        let closes = match &self.overlay {
            Some(Overlay::Switcher { windows, .. }) => windows.contains(window),
            Some(Overlay::Menu {
                kind: MenuKind::Window(menu),
                ..
            }) => menu == window,
            _ => false,
        };
        if closes {
            self.overlay = None;
            self.request_redraw();
        }
    }

    /// Moves the switcher's selection on, or back.
    pub fn step_switcher(&mut self, forward: bool) {
        if let Some(Overlay::Switcher { windows, selected }) = self.overlay.as_mut() {
            *selected = keys::cycle(windows.len(), *selected, forward);
            self.request_redraw();
        }
    }

    /// Opens a menu at the logical point `at` for `kind`.
    pub fn open_menu(&mut self, kind: MenuKind, at: (f64, f64)) {
        if !self.may_open_overlay() {
            return;
        }
        self.overlay = Some(Overlay::Menu {
            kind,
            selected: None,
            at,
        });
        self.request_redraw();
    }

    /// The arrow keys in a menu: moves the selection, skipping lines.
    pub fn move_menu(&mut self, step: i32) {
        let Some(Overlay::Menu { kind, selected, .. }) = self.overlay.as_mut() else {
            return;
        };
        let rows = rows(kind);
        let parts = menu_rows(&rows);
        let layout = card::menu(&self.paint_theme, self.font.as_ref(), &parts);
        *selected = layout.step(*selected, step);
        self.request_redraw();
    }

    /// Closes the switcher or the menu without doing anything.
    pub fn close_overlay(&mut self) {
        if self.overlay.take().is_some() {
            self.request_redraw();
        }
    }

    /// Enter, or Alt let go: what the switcher or the menu has selected is
    /// done, and the card closes.
    pub fn activate_overlay(&mut self) {
        match self.overlay.take() {
            Some(Overlay::Switcher { windows, selected }) => {
                if let Some(window) = windows.get(selected).cloned() {
                    if self.space.element_location(&window).is_some() {
                        self.focus_window(&window);
                    }
                }
            }
            Some(Overlay::Menu {
                kind,
                selected: Some(index),
                ..
            }) => {
                if let Some(Some(item)) = rows(&kind).get(index).copied() {
                    self.run_item(item, &kind);
                }
            }
            _ => {}
        }
        self.request_redraw();
    }

    /// What an item of a menu does. The menu is already closed.
    fn run_item(&mut self, item: Item, kind: &MenuKind) {
        match (item, kind) {
            (Item::Search, _) => {
                if let Some(command) = keys::command(Action::Launcher) {
                    self.spawn(command);
                }
            }
            (Item::Terminal, _) => {
                if let Some(command) = keys::command(Action::Terminal) {
                    self.spawn(command);
                }
            }
            (Item::Settings, _) => {
                self.spawn(SETTINGS);
            }
            (Item::Lock, _) => self.spawn_locker(),
            (Item::Session, _) => {
                self.spawn(SESSION);
            }
            (Item::Minimise, MenuKind::Window(window)) => self.set_minimised(window, true),
            (Item::Maximise, MenuKind::Window(window)) => self.toggle_maximise(window),
            (Item::Fullscreen, MenuKind::Window(window)) => {
                let on = window::data(window).fullscreen.get().is_some();
                self.set_fullscreen(window, !on, None);
            }
            (Item::Close, MenuKind::Window(window)) => {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.send_close();
                }
            }
            _ => {}
        }
    }

    /// A pointer press while a card is open: an item of a menu is done, a
    /// press anywhere else closes the card, and nothing reaches the windows.
    pub fn overlay_press(&mut self, location: (f64, f64)) {
        let Some(output) = self.output() else {
            self.close_overlay();
            return;
        };
        let hit = self.overlay_part_at(&output, location);
        match self.overlay.take() {
            Some(Overlay::Switcher { windows, .. }) => {
                if let Some(index) = hit {
                    if let Some(window) = windows.get(index).cloned() {
                        if self.space.element_location(&window).is_some() {
                            self.focus_window(&window);
                        }
                    }
                }
            }
            Some(Overlay::Menu { kind, .. }) => {
                if let Some(index) = hit {
                    if let Some(Some(item)) = rows(&kind).get(index).copied() {
                        self.run_item(item, &kind);
                    }
                }
            }
            None => {}
        }
        self.request_redraw();
    }

    /// The pointer moved: a menu selects the item under it.
    pub fn overlay_motion(&mut self, location: (f64, f64)) {
        if !matches!(self.overlay, Some(Overlay::Menu { .. })) {
            return;
        }
        let Some(output) = self.output() else {
            return;
        };
        let hit = self.overlay_part_at(&output, location);
        if let Some(Overlay::Menu { selected, .. }) = self.overlay.as_mut() {
            if *selected != hit {
                *selected = hit;
                self.request_redraw();
            }
        }
    }

    /// The part of the open card under a logical point, by index into the
    /// rows or tiles it has.
    fn overlay_part_at(&self, output: &Output, location: (f64, f64)) -> Option<usize> {
        let (layout, (left, top)) = self.card_on(output)?;
        let geometry = self.space.output_geometry(output)?;
        let scale = self.output_scale;
        let x = ((location.0 - f64::from(geometry.loc.x)) * scale).round() as i32 - left;
        let y = ((location.1 - f64::from(geometry.loc.y)) * scale).round() as i32 - top;
        layout.part_at(x, y)
    }

    /// The open card as it is drawn on `output`, with its top left corner in
    /// the output's pixels. Only the first output shows it.
    fn card_on(&self, output: &Output) -> Option<(Layout, (i32, i32))> {
        if self.is_locked() || self.login_screen {
            return None;
        }
        if self.output().as_ref() != Some(output) {
            return None;
        }
        let geometry = self.space.output_geometry(output)?;
        let scale = self.output_scale;
        let screen_w = (f64::from(geometry.size.w) * scale).round() as i32;
        let screen_h = (f64::from(geometry.size.h) * scale).round() as i32;
        let theme = &self.paint_theme;
        let font = self.font.as_ref();
        match self.overlay.as_ref()? {
            Overlay::Switcher { windows, .. } => {
                let titles: Vec<(String, char)> = windows
                    .iter()
                    .map(|window| {
                        let title = window::with_title(window, str::to_string);
                        (title, badge_letter(window))
                    })
                    .collect();
                let tiles: Vec<Tile> = titles
                    .iter()
                    .map(|(title, badge)| Tile {
                        title,
                        badge: *badge,
                    })
                    .collect();
                let layout = card::switcher(theme, font, &tiles, screen_w);
                let left = (screen_w - layout.width) / 2;
                let top = (screen_h - layout.height) / 2;
                Some((layout, (left, top)))
            }
            Overlay::Menu { kind, at, .. } => {
                let rows = rows(kind);
                let parts = menu_rows(&rows);
                let layout = card::menu(theme, font, &parts);
                let x = ((at.0 - f64::from(geometry.loc.x)) * scale).round() as i32;
                let y = ((at.1 - f64::from(geometry.loc.y)) * scale).round() as i32;
                let left = x.clamp(0, (screen_w - layout.width).max(0));
                let top = y.clamp(0, (screen_h - layout.height).max(0));
                Some((layout, (left, top)))
            }
        }
    }

    /// The open card as a render element on `output`, drawn in the screen's
    /// pixels: nothing when no card is open there.
    pub fn overlay_element<R>(&self, output: &Output, renderer: &mut R) -> Option<OutputElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Clone + Send + Texture + 'static,
    {
        let (layout, (left, top)) = self.card_on(output)?;
        let selected = match self.overlay.as_ref()? {
            Overlay::Switcher { selected, .. } => Some(*selected),
            Overlay::Menu { selected, .. } => *selected,
        };
        let theme = &self.paint_theme;
        let canvas = layout.draw(theme, selected);
        let scale = self.output_scale;
        let buffer_scale = scale.round().max(1.0) as i32;
        let (w, h) = (canvas.width() as i32, canvas.height() as i32);
        let buffer = MemoryRenderBuffer::from_slice(
            &canvas.into_pixels(),
            Fourcc::Argb8888,
            (w, h),
            buffer_scale,
            Transform::Normal,
            None,
        );
        let logical = Size::<i32, Logical>::from((
            (f64::from(w) / scale).round() as i32,
            (f64::from(h) / scale).round() as i32,
        ));
        let at = Point::<f64, Physical>::from((f64::from(left), f64::from(top)));
        match MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            at,
            &buffer,
            None,
            None,
            Some(logical),
            Kind::Unspecified,
        ) {
            Ok(element) => Some(OutputElement::Memory(element)),
            Err(err) => {
                crate::say!("cannot draw a card: {err:?}");
                None
            }
        }
    }
}

fn menu_rows(rows: &[Option<Item>]) -> Vec<Row<'static>> {
    rows.iter()
        .map(|row| match row {
            Some(item) => Row::Item(label(*item)),
            None => Row::Separator,
        })
        .collect()
}

/// The letter a window's badge shows: the first letter of its app id, or of
/// its title when the app has no id.
fn badge_letter(window: &Window) -> char {
    let app_id = window::app_id(window);
    let source = if app_id.is_empty() {
        window::with_title(window, str::to_string)
    } else {
        app_id
    };
    source
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().next().unwrap_or(c))
        .unwrap_or('?')
}
