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

/// What is known about the output a surface is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// The connector name, such as `eDP-1`, if the compositor gives one.
    pub name: Option<String>,
    /// The size in logical pixels, which is what surface sizes are measured
    /// in: a 3840 x 2160 panel at scale 2 is 1920 x 1080. `None` if the
    /// compositor does not say.
    pub size: Option<(u32, u32)>,
    /// The integer scale of the output.
    pub scale: u32,
}

/// A rectangle on a surface, in logical pixels from its top left corner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }
}

/// A callback the app has set on a surface, if any.
pub(crate) type Hook<A> = Option<Box<dyn FnMut(A)>>;

/// Calls a hook that may want to set itself again, or set another, without
/// holding the cell it lives in.
pub(crate) fn call_hook<A>(
    own: &Rc<RefCell<SurfaceShared>>,
    slot: fn(&mut SurfaceShared) -> &mut Hook<A>,
    argument: A,
) {
    let Some(mut hook) = slot(&mut own.borrow_mut()).take() else {
        return;
    };
    hook(argument);
    let mut own = own.borrow_mut();
    let slot = slot(&mut own);
    if slot.is_none() {
        *slot = Some(hook);
    }
}

/// What one instance has asked for and what is known about its surface.
pub(crate) struct SurfaceShared {
    pub size: Option<Stamped<(u32, u32)>>,
    pub input_region: Option<Stamped<Option<Vec<Rect>>>>,
    pub visible: bool,
    /// Counts this surface's own changes, like `Shared::revision`.
    pub revision: u64,
    /// The size the compositor last settled on; zero before that.
    pub configured: (u32, u32),
    pub scale: u32,
    pub output: Option<Output>,
    pub on_output: Hook<Output>,
    pub focused: bool,
    pub on_focus: Hook<bool>,
}

impl SurfaceShared {
    pub(crate) fn new() -> SurfaceShared {
        SurfaceShared {
            size: None,
            input_region: None,
            visible: true,
            revision: 0,
            configured: (0, 0),
            scale: 1,
            output: None,
            on_output: None,
            focused: false,
            on_focus: None,
        }
    }

    /// The part of the surface that takes pointer input, by the same rule
    /// as the size. `None` is all of it.
    pub(crate) fn wanted_input_region(&self, app: &Shared) -> Option<Vec<Rect>> {
        match &self.input_region {
            Some(own) if own.stamp == app.input_stamp => own.value.clone(),
            _ => app.input_region.clone(),
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

    /// Limits where this surface takes pointer input; see
    /// [`Handle::set_input_region`](crate::Handle::set_input_region), which
    /// does the same for every surface and replaces what is set here.
    pub fn set_input_region(&self, region: Option<&[Rect]>) {
        let app = self.app.borrow();
        let mut own = self.own.borrow_mut();
        own.input_region = Some(Stamped {
            value: region.map(<[Rect]>::to_vec),
            stamp: app.input_stamp,
        });
        own.revision += 1;
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

    /// The output this surface is on, once that is known. With
    /// [`Outputs::All`](crate::Outputs::All) and
    /// [`Outputs::Named`](crate::Outputs::Named) it is known from the
    /// start. With [`Outputs::Auto`](crate::Outputs::Auto) the compositor
    /// picks, and it is `None` until the surface is on screen.
    pub fn output(&self) -> Option<Output> {
        self.own.borrow().output.clone()
    }

    /// Calls `changed` whenever the surface's output becomes known or
    /// changes: another resolution or scale, or, with
    /// [`Outputs::Auto`](crate::Outputs::Auto), another output altogether.
    /// It is not called for what [`SurfaceHandle::output`] already returns
    /// when the hook is set. A second call replaces the hook.
    ///
    /// This is where a dock clamps its width to the output and a launcher
    /// sizes itself to the screen.
    pub fn on_output_changed(&self, changed: impl FnMut(Output) + 'static) {
        self.own.borrow_mut().on_output = Some(Box::new(changed));
    }

    /// True while this surface has the keyboard.
    pub fn has_focus(&self) -> bool {
        self.own.borrow().focused
    }

    /// Calls `changed` with `true` when the surface gets the keyboard and
    /// `false` when it loses it, including when it is hidden. A second
    /// call replaces the hook.
    ///
    /// A launcher uses this to dismiss itself when the user turns to
    /// something else. Only surfaces with a keyboard mode other than
    /// `None` ever get the keyboard.
    pub fn on_focus_changed(&self, changed: impl FnMut(bool) + 'static) {
        self.own.borrow_mut().on_focus = Some(Box::new(changed));
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
