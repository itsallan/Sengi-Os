//! The handle to one instance's surface.

use std::cell::RefCell;
use std::rc::Rc;

use crate::backend::Shared;
use crate::config::{self, ConfigError};

/// A per-surface setting together with the app-wide stamp it was made at.
/// It holds until the same thing is next set for the whole app, so the
/// most recent call wins whichever handle it was made through.
pub(crate) struct Stamped<T> {
    pub value: T,
    pub stamp: u64,
}

/// What one instance has asked for and what is known about its surface.
pub(crate) struct SurfaceShared {
    pub size: Option<Stamped<(u32, u32)>>,
    pub visible: bool,
    /// Counts this surface's own changes, like `Shared::revision`.
    pub revision: u64,
    /// The size the compositor last settled on; zero before that.
    pub configured: (u32, u32),
    pub scale: u32,
}

impl SurfaceShared {
    pub(crate) fn new() -> SurfaceShared {
        SurfaceShared {
            size: None,
            visible: true,
            revision: 0,
            configured: (0, 0),
            scale: 1,
        }
    }

    /// The size to ask the compositor for: this surface's own if it is
    /// newer than the app's, otherwise the app's.
    pub(crate) fn wanted_size(&self, app: &Shared) -> (u32, u32) {
        match &self.size {
            Some(own) if own.stamp == app.size_stamp => own.value,
            _ => app.config.size,
        }
    }
}

/// Controls the surface of one instance of the component.
///
/// With [`Outputs::All`](crate::Outputs::All) every output has its own
/// instance, and this is how one of them is resized or hidden without the
/// others. With a single surface it does the same as [`Handle`](crate::Handle).
/// Get it from [`Context::surface`](crate::Context::surface) in the build
/// closure and clone it into callbacks. Like `Handle` it belongs to the UI
/// thread, and changes take effect before the loop next sleeps.
///
/// A handle that outlives its instance, because the output was unplugged,
/// does nothing.
#[derive(Clone)]
pub struct SurfaceHandle {
    pub(crate) app: Rc<RefCell<Shared>>,
    pub(crate) own: Rc<RefCell<SurfaceShared>>,
}

impl SurfaceHandle {
    /// Asks for a new size for this surface only, in logical pixels; 0
    /// stretches along that axis and needs both edges of the axis anchored.
    ///
    /// The most recent of this and [`Handle::set_size`](crate::Handle::set_size)
    /// wins: a later app-wide size replaces it.
    pub fn set_size(&self, width: u32, height: u32) -> Result<(), ConfigError> {
        let mut app = self.app.borrow_mut();
        config::validate_size(app.config.anchor, (width, height))?;
        app.size_overridden = true;
        let mut own = self.own.borrow_mut();
        if own.wanted_size(&app) != (width, height) {
            own.revision += 1;
        }
        own.size = Some(Stamped {
            value: (width, height),
            stamp: app.size_stamp,
        });
        Ok(())
    }

    /// The size this surface has on screen, in logical pixels, as last
    /// confirmed by the compositor. `(0, 0)` until it is first shown. For a
    /// stretched axis this is the real extent, not the 0 that was asked for.
    pub fn size(&self) -> (u32, u32) {
        self.own.borrow().configured
    }

    /// The integer scale the surface is drawn at; 2 on a HiDPI output.
    pub fn scale(&self) -> u32 {
        self.own.borrow().scale
    }

    /// Takes this surface off the screen and keeps its component alive.
    pub fn hide(&self) {
        self.own.borrow_mut().visible = false;
    }

    /// Undoes [`SurfaceHandle::hide`]. The surface is on screen only while
    /// the app is shown too, see [`Handle::hide`](crate::Handle::hide).
    pub fn show(&self) {
        self.own.borrow_mut().visible = true;
    }

    /// False between [`SurfaceHandle::hide`] and [`SurfaceHandle::show`].
    pub fn is_visible(&self) -> bool {
        self.own.borrow().visible
    }
}
