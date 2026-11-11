//! The system tray: icons of apps that keep running in the background.
//!
//! On Wayland this is the StatusNotifierItem protocol on the session bus.
//! An app registers its item with a watcher, a host shows the registered
//! items, and the item's menu is exported over `com.canonical.dbusmenu`.
//! The bar is both watcher and host.
//!
//! * [`watcher`] is the registry apps sign up with.
//! * [`host`] decides what is shown and what a click does. It is a state
//!   machine with no bus in it; [`bus`] carries out what it asks for, on
//!   threads of its own.
//! * [`item`], [`pixmap`] and [`icons`] turn an item's properties into an
//!   icon; [`menu`] reads its menu, and [`menu_view`] draws it in a helper
//!   process.
//!
//! Everything an item says is untrusted. Limits are named where they are
//! applied: [`watcher::MAX_ITEMS`], [`text::MAX_TEXT`],
//! [`pixmap::MAX_SIDE`], [`menu::MAX_ENTRIES`] and the reply sizes in
//! [`bus`].

// Nothing starts the tray yet; the bar's own part comes next.
#![allow(dead_code, unused_imports)]

mod bus;
mod host;
pub mod icons;
mod item;
mod menu;
pub mod menu_view;
mod pace;
mod pixmap;
mod text;
mod watcher;

use std::sync::mpsc::Sender;

pub use host::{Button, View};
pub use pixmap::{Pixels, MAX_ICON};

use host::{Event, Place};

/// The UI's handle on the tray. Nothing here waits for the bus.
pub struct Tray {
    events: Sender<Event>,
}

impl Tray {
    /// Starts the tray's threads. `show` is called on the UI thread with
    /// the icons to draw whenever they change.
    pub fn start(
        handle: &sengi_layer::Handle,
        pixels: u32,
        show: impl FnMut(Vec<View>) + 'static,
    ) -> Result<Tray, sengi_layer::Error> {
        let views = handle.channel(show)?;
        Ok(Tray {
            events: bus::start(views, pixels.clamp(1, MAX_ICON)),
        })
    }

    /// A tray icon was clicked on `output`, with its left edge at `x`.
    pub fn pressed(&self, id: u32, button: Button, output: &str, x: i32) {
        let place = Place {
            output: output.to_string(),
            x,
        };
        let _ = self.events.send(Event::Pressed { id, button, place });
    }

    pub fn scrolled(&self, id: u32, delta: i32, horizontal: bool) {
        let _ = self.events.send(Event::Scrolled {
            id,
            delta,
            horizontal,
        });
    }

    /// Icons are now drawn at this many pixels per side.
    pub fn set_pixels(&self, pixels: u32) {
        let _ = self.events.send(Event::Pixels(pixels.clamp(1, MAX_ICON)));
    }
}
