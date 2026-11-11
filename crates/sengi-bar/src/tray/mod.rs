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
//! * [`pace`] and [`flow`] are what keep a busy or hostile item from
//!   costing more than a quiet one.
//!
//! Everything an item says is untrusted. Limits are named where they are
//! applied: [`watcher::MAX_ITEMS`], [`text::MAX_TEXT`],
//! [`pixmap::MAX_SIDE`], [`menu::MAX_ENTRIES`] and the reply sizes in
//! [`bus`].

mod bus;
mod flow;
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

pub use flow::{icon_for, ClickGuard};
pub use host::{Button, View};
pub use pixmap::{Pixels, MAX_ICON};

use flow::Latest;
use host::{Event, Place};

/// Icon sizes decoded at once. One per output scale in use; three covers
/// a laptop panel and two kinds of monitor.
pub const MAX_SIZES: usize = 3;

/// The UI's handle on the tray. Nothing here waits for the bus.
pub struct Tray {
    events: Sender<Event>,
}

fn sizes(pixels: &[u32]) -> Vec<u32> {
    let mut sizes: Vec<u32> = pixels
        .iter()
        .map(|size| (*size).clamp(1, MAX_ICON))
        .collect();
    sizes.sort_unstable();
    sizes.dedup();
    // The largest are kept: an icon can be drawn smaller, not sharper.
    let extra = sizes.len().saturating_sub(MAX_SIZES);
    sizes.split_off(extra)
}

impl Tray {
    /// Starts the tray's threads. `show` is called on the UI thread with
    /// the icons to draw whenever they change. However often that is, the
    /// UI is handed only the newest set, once.
    pub fn start(
        handle: &sengi_layer::Handle,
        pixels: &[u32],
        mut show: impl FnMut(Vec<View>) + 'static,
    ) -> Result<Tray, sengi_layer::Error> {
        let views = Latest::default();
        let wake = handle.channel({
            let views = views.clone();
            move |()| {
                if let Some(views) = views.take() {
                    show(views);
                }
            }
        })?;
        Ok(Tray {
            events: bus::start(bus::Outbox { views, wake }, sizes(pixels)),
        })
    }

    fn place(output: &str, x: i32) -> Place {
        Place {
            output: output.to_string(),
            x,
        }
    }

    /// A tray icon was clicked on `output`, with its left edge at `x`.
    pub fn pressed(&self, id: u32, button: Button, output: &str, x: i32) {
        let place = Self::place(output, x);
        let _ = self.events.send(Event::Pressed { id, button, place });
    }

    /// The wheel moved over a tray icon.
    pub fn scrolled(&self, id: u32, delta: f32, horizontal: bool) {
        let _ = self.events.send(Event::Scrolled {
            id,
            delta,
            horizontal,
        });
    }

    /// The "more" item was clicked on a bar that has no room for `hidden`.
    pub fn more(&self, hidden: Vec<u32>, output: &str, x: i32) {
        let place = Self::place(output, x);
        let _ = self.events.send(Event::More { place, hidden });
    }

    /// Icons are now drawn at these sizes, in pixels per side.
    pub fn set_pixels(&self, pixels: &[u32]) {
        let _ = self.events.send(Event::Pixels(sizes(pixels)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_sizes_are_sorted_bounded_and_few() {
        assert_eq!(sizes(&[32, 16, 32]), [16, 32]);
        assert_eq!(sizes(&[0, 100_000]), [1, MAX_ICON]);
        assert_eq!(sizes(&[16, 32, 48, 64, 80]), [48, 64, 80]);
        assert!(sizes(&[]).is_empty());
    }
}
