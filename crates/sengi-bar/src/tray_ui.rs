//! The tray as the bars draw it: what the tray's threads report, turned
//! into Slint models, and the bars' clicks sent back.
//!
//! Bars on outputs of the same scale share a model; each scale has its
//! own, with icons decoded for it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sengi_layer::{Context, Handle, Instances, Output};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use crate::tray::{self, Button, ClickGuard, Pixels, Tray, View};
use crate::{Bar, TrayEntry};

/// The icons at one size.
struct AtSize {
    pixels: u32,
    model: Rc<VecModel<TrayEntry>>,
}

#[derive(Default)]
struct Shown {
    /// What the tray last reported, for models made later.
    views: Vec<View>,
    sizes: Vec<AtSize>,
    /// The image made from each icon's pixels, so an update that leaves
    /// an icon alone does not make a new one.
    images: Vec<(Arc<Pixels>, Image)>,
    guard: ClickGuard,
}

fn image(pixels: &Pixels) -> Option<Image> {
    let side = pixels.size;
    if side == 0 || pixels.data.len() != (side * side * 4) as usize {
        return None;
    }
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pixels.data, side, side);
    Some(Image::from_rgba8_premultiplied(buffer))
}

impl Shown {
    fn image(&mut self, pixels: &Arc<Pixels>) -> Option<Image> {
        if let Some((_, image)) = self
            .images
            .iter()
            .find(|(held, _)| Arc::ptr_eq(held, pixels))
        {
            return Some(image.clone());
        }
        let image = image(pixels)?;
        self.images.push((pixels.clone(), image.clone()));
        Some(image)
    }

    fn entries(&mut self, pixels: u32) -> Vec<TrayEntry> {
        let views = std::mem::take(&mut self.views);
        let entries = views
            .iter()
            .map(|view| {
                let image = tray::icon_for(&view.icons, pixels).and_then(|icon| self.image(icon));
                TrayEntry {
                    id: view.id as i32,
                    has_icon: image.is_some(),
                    icon: image.unwrap_or_default(),
                    letter: view.letter.as_str().into(),
                    label: view.label.as_str().into(),
                    attention: view.attention,
                }
            })
            .collect();
        self.views = views;
        entries
    }

    /// Shows these items on every model. Returns whether the set of items
    /// changed, as opposed to how some of them look.
    fn show(&mut self, views: Vec<View>, now: Instant) -> bool {
        self.views = views;
        // Images of icons that are no longer shown anywhere are let go.
        let views = &self.views;
        self.images.retain(|(held, _)| {
            views
                .iter()
                .flat_map(|view| &view.icons)
                .any(|(_, icon)| Arc::ptr_eq(icon, held))
        });
        let ids: Vec<u32> = self.views.iter().map(|view| view.id).collect();
        self.guard.keep(&ids);

        let mut set_changed = false;
        for index in 0..self.sizes.len() {
            let entries = self.entries(self.sizes[index].pixels);
            let model = self.sizes[index].model.clone();
            let held: Vec<i32> = model.iter().map(|entry| entry.id).collect();
            if held != entries.iter().map(|entry| entry.id).collect::<Vec<_>>() {
                model.set_vec(entries);
                set_changed = true;
                continue;
            }
            // The same items in the same places: change rows in place, so
            // the icon under the pointer stays the icon under the pointer.
            for (row, entry) in entries.into_iter().enumerate() {
                let Some(old) = model.row_data(row).filter(|old| *old != entry) else {
                    continue;
                };
                if (&old.icon, old.has_icon, &old.letter)
                    != (&entry.icon, entry.has_icon, &entry.letter)
                {
                    self.guard.icon_changed(entry.id as u32, now);
                }
                model.set_row_data(row, entry);
            }
        }
        if set_changed {
            self.guard.set_changed(now);
        }
        set_changed
    }

    /// The model for icons of this size, made if need be. The second
    /// value says whether it is new, and the tray needs telling.
    fn model(&mut self, pixels: u32) -> (Rc<VecModel<TrayEntry>>, bool) {
        if let Some(sized) = self.sizes.iter().find(|sized| sized.pixels == pixels) {
            return (sized.model.clone(), false);
        }
        if self.sizes.len() >= tray::MAX_SIZES {
            // More scales than we decode for: the nearest is drawn scaled.
            let nearest = self
                .sizes
                .iter()
                .min_by_key(|sized| sized.pixels.abs_diff(pixels))
                .expect("there is at least one size");
            return (nearest.model.clone(), false);
        }
        let model = Rc::new(VecModel::from(self.entries(pixels)));
        self.sizes.push(AtSize {
            pixels,
            model: model.clone(),
        });
        (model, true)
    }

    fn pixels(&self) -> Vec<u32> {
        self.sizes.iter().map(|sized| sized.pixels).collect()
    }
}

/// The tray's place in the bars.
pub struct TrayUi {
    tray: Tray,
    shown: Rc<RefCell<Shown>>,
    /// The size an icon is drawn at on an output of scale 1.
    icon_size: u32,
    /// How long after a change a click is not acted on.
    guard: Duration,
}

impl TrayUi {
    /// Starts the tray. `bar` is any bar, to read the sizes from.
    pub fn start(
        handle: &Handle,
        bar: &Bar,
        scale: u32,
        bars: Instances<Bar>,
    ) -> Result<Rc<TrayUi>, sengi_layer::Error> {
        let icon_size = (bar.get_tray_icon_size().round() as u32).max(1);
        let shown: Rc<RefCell<Shown>> = Rc::default();
        let show = {
            let shown = shown.clone();
            move |views| {
                if shown.borrow_mut().show(views, Instant::now()) {
                    bars.for_each(|bar| bar.invoke_tray_changed());
                }
            }
        };
        let tray = Tray::start(handle, &[icon_size * scale.max(1)], show)?;
        Ok(Rc::new(TrayUi {
            tray,
            shown,
            icon_size,
            guard: Duration::from_millis(bar.get_click_guard().max(0) as u64),
        }))
    }

    /// Gives a bar its icons, at the size its output draws them, and
    /// tells it how wide that output is so it knows how many fit.
    fn show_on(&self, bar: &Bar, output: Option<&Output>) {
        let scale = output.map_or(1, |output| output.scale.max(1));
        if let Some((width, _)) = output.and_then(|output| output.size) {
            bar.set_output_width(width as f32);
        }
        let (model, new) = self.shown.borrow_mut().model(self.icon_size * scale);
        bar.set_tray(ModelRc::from(model));
        if new {
            self.tray.set_pixels(&self.shown.borrow().pixels());
        }
    }

    /// Connects a bar to the tray.
    pub fn attach(self: &Rc<Self>, bar: &Bar, context: &Context) {
        self.show_on(bar, context.output().as_ref());
        context.surface().on_output_changed({
            let (ui, bar) = (self.clone(), bar.as_weak());
            move |output| {
                if let Some(bar) = bar.upgrade() {
                    ui.show_on(&bar, Some(&output));
                }
            }
        });

        let output = context.output_name().unwrap_or_default().to_string();
        bar.on_tray_pressed({
            let (ui, output) = (self.clone(), output.clone());
            move |id, button, x| {
                let Ok(id) = u32::try_from(id) else {
                    return;
                };
                // The tray has just changed: the click may have been
                // aimed at what was here a moment ago.
                if ui.shown.borrow().guard.blocks(id, Instant::now(), ui.guard) {
                    return;
                }
                let button = match button {
                    0 => Button::Left,
                    1 => Button::Middle,
                    _ => Button::Right,
                };
                ui.tray.pressed(id, button, &output, x.round() as i32);
            }
        });
        bar.on_tray_scrolled({
            let ui = self.clone();
            move |id, dx, dy| {
                let (delta, horizontal) = if dx.abs() > dy.abs() {
                    (dx, true)
                } else {
                    (dy, false)
                };
                // A wheel notch is 60 to Slint and 120 to the apps.
                if let Ok(id) = u32::try_from(id) {
                    ui.tray.scrolled(id, delta * 2.0, horizontal);
                }
            }
        });
        bar.on_tray_more({
            let (ui, bar) = (self.clone(), bar.as_weak());
            move |x| {
                let Some(bar) = bar.upgrade() else {
                    return;
                };
                // The icons this bar has no room for are the leftmost.
                let hidden = usize::try_from(bar.get_tray_hidden()).unwrap_or(0);
                let ids: Vec<u32> = bar
                    .get_tray()
                    .iter()
                    .take(hidden)
                    .filter_map(|entry| u32::try_from(entry.id).ok())
                    .collect();
                ui.tray.more(ids, &output, x.round() as i32);
            }
        });
    }
}
