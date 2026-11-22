//! More of the test client: what an ordinary program does with the
//! protocols in `src/apps.rs`, with fullscreen, with a bar on the top
//! layer, and with a picture of the screen, which the tests also use to
//! see what the compositor really drew.

use std::{
    fs::{self, File},
    os::{fd::AsFd, unix::fs::FileExt},
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use wayland_client::{
    delegate_noop, event_created_child,
    protocol::{
        wl_buffer::WlBuffer,
        wl_data_device::{self, WlDataDevice},
        wl_data_device_manager::WlDataDeviceManager,
        wl_data_offer::WlDataOffer,
        wl_data_source::WlDataSource,
        wl_output::{self, WlOutput},
        wl_pointer::{self, WlPointer},
        wl_region::WlRegion,
        wl_seat::WlSeat,
        wl_shm,
        wl_surface::WlSurface,
    },
    Connection, Dispatch, QueueHandle, WEnum,
};
use wayland_protocols::{
    wp::{
        fractional_scale::v1::client::{
            wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
            wp_fractional_scale_v1::{self, WpFractionalScaleV1},
        },
        pointer_constraints::zv1::client::{
            zwp_confined_pointer_v1::{self, ZwpConfinedPointerV1},
            zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
            zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
        },
        primary_selection::zv1::client::{
            zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
            zwp_primary_selection_device_v1::{self, ZwpPrimarySelectionDeviceV1},
            zwp_primary_selection_offer_v1::{self, ZwpPrimarySelectionOfferV1},
            zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
        },
        relative_pointer::zv1::client::{
            zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
            zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
        },
    },
    xdg::{
        activation::v1::client::{
            xdg_activation_token_v1::{self, XdgActivationTokenV1},
            xdg_activation_v1::XdgActivationV1,
        },
        shell::client::xdg_toplevel::{self, XdgToplevel},
    },
};
use wayland_protocols_wlr::{
    layer_shell::v1::client::{
        zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
        zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
    },
    screencopy::v1::client::{
        zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
        zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
    },
};

use super::{Client, Failure, Seen};

pub const SCREENCOPY: &str = "zwlr_screencopy_manager_v1";

const SOON: Duration = Duration::from_secs(5);

/// What the compositor has told this client about the things in this file.
#[derive(Default)]
pub struct More {
    /// The whole-number scale an output announced.
    pub output_scale: Option<i32>,
    /// The latest toplevel configure: width, height, whether fullscreen.
    pub toplevel: Option<(i32, i32, bool)>,
    /// The types of the primary selection offered last, and how many
    /// times a selection was announced.
    pub primary_types: Vec<String>,
    pub primary_selections: u32,
    pub token: Option<String>,
    /// The scale a surface was told to draw at, in 120ths.
    pub preferred_scale: Option<u32>,
    pub pointer_enters: u32,
    pub pointer_motions: u32,
    pub button_serial: Option<u32>,
    /// Each relative motion: dx and dy.
    pub relative: Vec<(f64, f64)>,
    pub pointer_locked: u32,
    pub pointer_unlocked: u32,
    pub pointer_confined: u32,
    /// What a screen copy frame wants for a buffer: width, height, stride.
    pub frame_buffer: Option<(u32, u32, u32)>,
    pub frame_ready: u32,
    pub frame_failed: u32,
    /// A layer surface's latest configure: serial, width, height.
    pub layer_configure: Option<(u32, u32, u32)>,
}

/// A picture of the screen as the compositor copied it.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    xrgb: Vec<u8>,
}

impl Picture {
    /// The colour at a pixel, as 0xRRGGBB.
    pub fn at(&self, x: u32, y: u32) -> u32 {
        let i = ((y * self.width + x) * 4) as usize;
        u32::from_le_bytes([self.xrgb[i], self.xrgb[i + 1], self.xrgb[i + 2], 0])
    }
}

fn scratch_file(bytes: usize) -> Result<File, Failure> {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "sengi-test-picture-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::Relaxed)
    ));
    let file = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    fs::remove_file(&path)?;
    file.set_len(bytes as u64)?;
    Ok(file)
}

impl Client {
    pub fn seat(&mut self) -> Result<WlSeat, Failure> {
        Ok(self.globals.bind(&self.handle, 1..=5, ())?)
    }

    /// The whole-number scale the output announces.
    pub fn output_scale(&mut self) -> Result<Option<i32>, Failure> {
        let _output: WlOutput = self.globals.bind(&self.handle, 2..=4, ())?;
        self.roundtrip()?;
        Ok(self.seen.more.output_scale)
    }

    /// Listens for the pointer.
    pub fn pointer(&mut self) -> Result<WlPointer, Failure> {
        let pointer = self.seat()?.get_pointer(&self.handle, ());
        self.roundtrip()?;
        Ok(pointer)
    }

    /// Makes text of this client the primary selection, as selecting it
    /// with the mouse does. `serial` is that of an input event.
    pub fn select_primary(
        &mut self,
        serial: u32,
    ) -> Result<(ZwpPrimarySelectionDeviceV1, ZwpPrimarySelectionSourceV1), Failure> {
        let (device, manager) = self.primary_device()?;
        let source = manager.create_source(&self.handle, ());
        source.offer("text/plain".into());
        device.set_selection(Some(&source), serial);
        self.roundtrip()?;
        Ok((device, source))
    }

    /// Listens for the primary selection.
    pub fn primary_device(
        &mut self,
    ) -> Result<
        (
            ZwpPrimarySelectionDeviceV1,
            ZwpPrimarySelectionDeviceManagerV1,
        ),
        Failure,
    > {
        let manager: ZwpPrimarySelectionDeviceManagerV1 =
            self.globals.bind(&self.handle, 1..=1, ())?;
        let seat = self.seat()?;
        let device = manager.get_device(&seat, &self.handle, ());
        self.roundtrip()?;
        Ok((device, manager))
    }

    /// Asks for a token to bring a window forward with, naming `surface`
    /// as the one in use and `serial` as proof of it.
    pub fn activation_token(
        &mut self,
        surface: &WlSurface,
        serial: Option<u32>,
    ) -> Result<String, Failure> {
        let activation: XdgActivationV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let token = activation.get_activation_token(&self.handle, ());
        if let Some(serial) = serial {
            token.set_serial(serial, &self.seat()?);
        }
        token.set_surface(surface);
        token.commit();
        self.seen.more.token = None;
        if !self.wait(SOON, |seen| seen.more.token.is_some()) {
            return Err("no token came".into());
        }
        Ok(self.seen.more.token.take().unwrap_or_default())
    }

    /// Asks for `surface` to be brought forward on the strength of `token`.
    pub fn activate(&mut self, token: &str, surface: &WlSurface) -> Result<(), Failure> {
        let activation: XdgActivationV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        activation.activate(token.into(), surface);
        self.roundtrip()
    }

    /// The scale `surface` is told to draw at, in 120ths.
    pub fn preferred_scale(&mut self, surface: &WlSurface) -> Result<Option<u32>, Failure> {
        let manager: WpFractionalScaleManagerV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let _scale = manager.get_fractional_scale(surface, &self.handle, ());
        self.roundtrip()?;
        Ok(self.seen.more.preferred_scale)
    }

    pub fn lock_pointer(
        &mut self,
        surface: &WlSurface,
        pointer: &WlPointer,
    ) -> Result<ZwpLockedPointerV1, Failure> {
        let constraints: ZwpPointerConstraintsV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let locked = constraints.lock_pointer(
            surface,
            pointer,
            None,
            Lifetime::Persistent,
            &self.handle,
            (),
        );
        self.roundtrip()?;
        Ok(locked)
    }

    /// As `lock_pointer`, but only once the pointer is inside this part of
    /// the surface: left, top, width, height.
    pub fn lock_pointer_within(
        &mut self,
        surface: &WlSurface,
        pointer: &WlPointer,
        (x, y, width, height): (i32, i32, i32, i32),
    ) -> Result<ZwpLockedPointerV1, Failure> {
        let constraints: ZwpPointerConstraintsV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let region = self.compositor.create_region(&self.handle, ());
        region.add(x, y, width, height);
        let locked = constraints.lock_pointer(
            surface,
            pointer,
            Some(&region),
            Lifetime::Persistent,
            &self.handle,
            (),
        );
        self.roundtrip()?;
        Ok(locked)
    }

    pub fn confine_pointer(
        &mut self,
        surface: &WlSurface,
        pointer: &WlPointer,
    ) -> Result<ZwpConfinedPointerV1, Failure> {
        let constraints: ZwpPointerConstraintsV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let confined = constraints.confine_pointer(
            surface,
            pointer,
            None,
            Lifetime::Persistent,
            &self.handle,
            (),
        );
        self.roundtrip()?;
        Ok(confined)
    }

    /// Listens for the pointing device's movement as it was made.
    pub fn relative_pointer(
        &mut self,
        pointer: &WlPointer,
    ) -> Result<ZwpRelativePointerV1, Failure> {
        let manager: ZwpRelativePointerManagerV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let relative = manager.get_relative_pointer(pointer, &self.handle, ());
        self.roundtrip()?;
        Ok(relative)
    }

    /// A picture of the first output. An error if the compositor refuses.
    pub fn capture(&mut self) -> Result<Picture, Failure> {
        let manager: ZwlrScreencopyManagerV1 = self.globals.bind(&self.handle, 1..=1, ())?;
        let output = self.outputs().remove(0);
        self.seen.more.frame_buffer = None;
        let (ready, failed) = (self.seen.more.frame_ready, self.seen.more.frame_failed);
        let frame = manager.capture_output(0, &output, &self.handle, ());
        self.wait(SOON, |seen| {
            seen.more.frame_buffer.is_some() || seen.more.frame_failed > failed
        });
        let Some((width, height, stride)) = self.seen.more.frame_buffer else {
            return Err("the compositor would not start a screen copy".into());
        };
        let file = scratch_file((stride * height) as usize)?;
        let pool = self
            .shm
            .create_pool(file.as_fd(), (stride * height) as i32, &self.handle, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Xrgb8888,
            &self.handle,
            (),
        );
        pool.destroy();
        frame.copy(&buffer);
        self.wait(SOON, |seen| {
            seen.more.frame_ready > ready || seen.more.frame_failed > failed
        });
        if self.seen.more.frame_ready == ready {
            return Err("the compositor would not copy the screen".into());
        }
        let mut xrgb = vec![0u8; (stride * height) as usize];
        file.read_exact_at(&mut xrgb, 0)?;
        frame.destroy();
        buffer.destroy();
        Ok(Picture {
            width,
            height,
            xrgb,
        })
    }

    /// A bar of one colour across the top of the screen, on the layer the
    /// session's bar and dock are on.
    pub fn bar(
        &mut self,
        height: u32,
        argb: u32,
    ) -> Result<(WlSurface, ZwlrLayerSurfaceV1), Failure> {
        let shell: ZwlrLayerShellV1 = self.globals.bind(&self.handle, 1..=4, ())?;
        let surface = self.compositor.create_surface(&self.handle, ());
        let bar = shell.get_layer_surface(
            &surface,
            None,
            Layer::Top,
            "test bar".into(),
            &self.handle,
            (),
        );
        bar.set_size(0, height);
        bar.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
        bar.set_exclusive_zone(height as i32);
        surface.commit();
        if !self.wait(SOON, |seen| seen.more.layer_configure.is_some()) {
            return Err("the bar was never configured".into());
        }
        let (serial, width, height) = self.seen.more.layer_configure.take().unwrap_or_default();
        bar.ack_configure(serial);
        self.paint(&surface, width as i32, height as i32, argb)?;
        Ok((surface, bar))
    }

    /// Gives `surface` a buffer of one colour and this size.
    pub fn paint(
        &mut self,
        surface: &WlSurface,
        width: i32,
        height: i32,
        argb: u32,
    ) -> Result<(), Failure> {
        let buffer: WlBuffer = self.buffer(width, height, argb)?;
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, width, height);
        surface.commit();
        self.roundtrip()
    }

    /// Starts dragging from `origin`, with a square of one colour as the
    /// picture that goes along. `serial` is that of the button press.
    pub fn drag(
        &mut self,
        origin: &WlSurface,
        serial: u32,
        argb: u32,
    ) -> Result<(WlDataDevice, WlDataSource, WlSurface), Failure> {
        let manager: WlDataDeviceManager = self.globals.bind(&self.handle, 1..=3, ())?;
        let device = manager.get_data_device(&self.seat()?, &self.handle, ());
        let source = manager.create_data_source(&self.handle, ());
        source.offer("text/plain".into());
        let icon = self.compositor.create_surface(&self.handle, ());
        device.start_drag(Some(&source), origin, Some(&icon), serial);
        self.paint(&icon, 48, 48, argb)?;
        Ok((device, source, icon))
    }
}

impl Dispatch<WlOutput, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Scale { factor } = event {
            seen.more.output_scale = Some(factor);
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Configure {
            width,
            height,
            states,
        } = event
        {
            let fullscreen =
                states.as_chunks::<4>().0.iter().any(|state| {
                    u32::from_ne_bytes(*state) == xdg_toplevel::State::Fullscreen as u32
                });
            seen.more.toplevel = Some((width, height, fullscreen));
        }
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: zwp_primary_selection_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_device_v1::Event::DataOffer { .. } => {
                seen.more.primary_types.clear();
            }
            zwp_primary_selection_device_v1::Event::Selection { id } => {
                if id.is_some() {
                    seen.more.primary_selections += 1;
                } else {
                    seen.more.primary_types.clear();
                }
            }
            _ => {}
        }
    }

    event_created_child!(Seen, ZwpPrimarySelectionDeviceV1, [
        zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE => (ZwpPrimarySelectionOfferV1, ()),
    ]);
}

impl Dispatch<ZwpPrimarySelectionOfferV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpPrimarySelectionOfferV1,
        event: zwp_primary_selection_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = event {
            seen.more.primary_types.push(mime_type);
        }
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            seen.more.token = Some(token);
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            seen.more.preferred_scale = Some(scale);
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { .. } => seen.more.pointer_enters += 1,
            wl_pointer::Event::Motion { .. } => seen.more.pointer_motions += 1,
            wl_pointer::Event::Button {
                serial,
                state: WEnum::Value(wl_pointer::ButtonState::Pressed),
                ..
            } => seen.more.button_serial = Some(serial),
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, .. } = event {
            seen.more.relative.push((dx, dy));
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => seen.more.pointer_locked += 1,
            zwp_locked_pointer_v1::Event::Unlocked => seen.more.pointer_unlocked += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpConfinedPointerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpConfinedPointerV1,
        event: zwp_confined_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_confined_pointer_v1::Event::Confined = event {
            seen.more.pointer_confined += 1;
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                width,
                height,
                stride,
                ..
            } => seen.more.frame_buffer = Some((width, height, stride)),
            zwlr_screencopy_frame_v1::Event::Ready { .. } => seen.more.frame_ready += 1,
            zwlr_screencopy_frame_v1::Event::Failed => seen.more.frame_failed += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            seen.more.layer_configure = Some((serial, width, height));
        }
    }
}

impl Dispatch<WlDataDevice, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WlDataDevice,
        _: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(Seen, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

delegate_noop!(Seen: ZwpPrimarySelectionDeviceManagerV1);
delegate_noop!(Seen: ignore ZwpPrimarySelectionSourceV1);
delegate_noop!(Seen: XdgActivationV1);
delegate_noop!(Seen: WpFractionalScaleManagerV1);
delegate_noop!(Seen: ZwpPointerConstraintsV1);
delegate_noop!(Seen: WlRegion);
delegate_noop!(Seen: ZwpRelativePointerManagerV1);
delegate_noop!(Seen: ZwlrScreencopyManagerV1);
delegate_noop!(Seen: ZwlrLayerShellV1);
delegate_noop!(Seen: WlDataDeviceManager);
delegate_noop!(Seen: ignore WlDataSource);
delegate_noop!(Seen: ignore WlDataOffer);
