//! The test client's side of the clipboard: a program with no window that
//! sets and reads the clipboard through data control, as wl-copy,
//! wl-paste and clipboard managers do.

use std::{
    fs::File,
    io::{Read, Write},
    os::fd::{AsFd, FromRawFd, OwnedFd},
    sync::Arc,
    time::{Duration, Instant},
};

use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

use super::{Client, Failure, Seen};

pub const EXT: &str = "ext_data_control_manager_v1";
pub const WLR: &str = "zwlr_data_control_manager_v1";

/// The text in a UTF-8 text offer.
pub const TEXT: &str = "text/plain;charset=utf-8";

/// What the compositor has told this client about the clipboard.
#[derive(Default)]
pub struct Clip {
    /// Every offer announced, by the mime types it carries.
    offers: Vec<(String, Vec<String>)>,
    /// The offer that is the clipboard now, if there is one.
    current: Option<String>,
    /// How many times the clipboard changed, as this client was told.
    pub selections: u32,
    /// How many of our own sources were told to stop.
    pub cancelled: u32,
    /// Descriptors a hanging source was asked to write to.
    pub held: Vec<OwnedFd>,
}

impl Clip {
    /// The mime types of the clipboard as it is now.
    pub fn types(&self) -> Vec<String> {
        self.current
            .as_ref()
            .and_then(|id| self.offers.iter().find(|(offer, _)| offer == id))
            .map(|(_, types)| types.clone())
            .unwrap_or_default()
    }
}

/// What a source of ours hands over: the same bytes for every type. A
/// source that hangs keeps the descriptor it is asked to write to open and
/// writes nothing, like a program that has stopped answering.
pub struct Payload(pub Arc<Vec<u8>>, pub bool);

/// A data-control device of either kind, and what makes sources for it.
pub enum Control {
    Ext(ExtDataControlManagerV1, ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlManagerV1, ZwlrDataControlDeviceV1),
}

/// A source we made, which keeps the selection ours while it is held.
pub enum Source {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

impl Client {
    /// Listens to the clipboard and can set it, through `ext` or the wlr
    /// protocol.
    pub fn data_control(&mut self, ext: bool) -> Result<Control, Failure> {
        let seat = self.seat()?;
        let control = if ext {
            let manager: ExtDataControlManagerV1 = self.globals.bind(&self.handle, 1..=1, ())?;
            let device = manager.get_data_device(&seat, &self.handle, ());
            Control::Ext(manager, device)
        } else {
            let manager: ZwlrDataControlManagerV1 = self.globals.bind(&self.handle, 1..=1, ())?;
            let device = manager.get_data_device(&seat, &self.handle, ());
            Control::Wlr(manager, device)
        };
        self.roundtrip()?;
        Ok(control)
    }

    /// Makes `content` the clipboard, offered as each of `types`.
    pub fn copy(
        &mut self,
        control: &Control,
        types: &[&str],
        content: &[u8],
    ) -> Result<Source, Failure> {
        self.copy_as(control, types, content, false)
    }

    /// As `copy`, from a program that never answers a request for the text.
    pub fn copy_hanging(&mut self, control: &Control, types: &[&str]) -> Result<Source, Failure> {
        self.copy_as(control, types, b"", true)
    }

    fn copy_as(
        &mut self,
        control: &Control,
        types: &[&str],
        content: &[u8],
        hang: bool,
    ) -> Result<Source, Failure> {
        let payload = Payload(Arc::new(content.to_vec()), hang);
        let source = match control {
            Control::Ext(manager, device) => {
                let source = manager.create_data_source(&self.handle, payload);
                for mime in types {
                    source.offer((*mime).to_string());
                }
                device.set_selection(Some(&source));
                Source::Ext(source)
            }
            Control::Wlr(manager, device) => {
                let source = manager.create_data_source(&self.handle, payload);
                for mime in types {
                    source.offer((*mime).to_string());
                }
                device.set_selection(Some(&source));
                Source::Wlr(source)
            }
        };
        self.roundtrip()?;
        Ok(source)
    }

    /// Clears the clipboard.
    pub fn clear_clipboard(&mut self, control: &Control) -> Result<(), Failure> {
        match control {
            Control::Ext(_, device) => device.set_selection(None),
            Control::Wlr(_, device) => device.set_selection(None),
        }
        self.roundtrip()
    }

    /// Reads the clipboard as `mime`, as a paste does. `None` if there is
    /// no clipboard, or nothing arrives within a few seconds.
    pub fn paste(&mut self, control: &Control, mime: &str) -> Result<Option<Vec<u8>>, Failure> {
        let mut ends = [0 as libc::c_int; 2];
        // SAFETY: pipe2 fills two descriptors, which are then owned.
        let (read, write) = unsafe {
            assert_eq!(libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC), 0);
            (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1]))
        };
        let Some(current) = self.seen.clip.current.clone() else {
            return Ok(None);
        };
        let asked = match control {
            Control::Ext(..) => self.seen.clip_ext.get(&current).map(|offer| {
                offer.receive(mime.to_string(), write.as_fd());
            }),
            Control::Wlr(..) => self.seen.clip_wlr.get(&current).map(|offer| {
                offer.receive(mime.to_string(), write.as_fd());
            }),
        };
        if asked.is_none() {
            return Ok(None);
        }
        drop(write);
        self.roundtrip()?;
        let mut file = File::from(read);
        // SAFETY: fcntl on a descriptor owned by `file`.
        unsafe {
            use std::os::fd::AsRawFd;
            let flags = libc::fcntl(file.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let mut out = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let mut chunk = [0u8; 8192];
            match file.read(&mut chunk) {
                Ok(0) => return Ok(Some(out)),
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= end {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    self.roundtrip()?;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// The clipboard as text, if it holds some.
    pub fn paste_text(&mut self, control: &Control) -> Result<Option<String>, Failure> {
        Ok(self
            .paste(control, TEXT)?
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// The offer a message belongs to, by the id it is known by.
fn name<P: Proxy>(offer: &P) -> String {
    offer.id().to_string()
}

fn hand_over(seen: &mut Seen, payload: &Payload, fd: OwnedFd) {
    if payload.1 {
        seen.clip.held.push(fd);
        return;
    }
    let mut file = File::from(fd);
    let _ = file.write_all(&payload.0);
}

impl Dispatch<ExtDataControlManagerV1, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &ExtDataControlManagerV1,
        _: <ExtDataControlManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_device_v1::Event::DataOffer { id } => {
                seen.clip.offers.push((name(&id), Vec::new()));
                seen.clip_ext.insert(name(&id), id);
            }
            ext_data_control_device_v1::Event::Selection { id } => {
                seen.clip.selections += 1;
                seen.clip.current = id.as_ref().map(name);
            }
            _ => {}
        }
    }

    event_created_child!(Seen, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        offer: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event {
            if let Some((_, types)) = seen
                .clip
                .offers
                .iter_mut()
                .find(|(id, _)| *id == name(offer))
            {
                types.push(mime_type);
            }
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, Payload> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        payload: &Payload,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_source_v1::Event::Send { fd, .. } => hand_over(seen, payload, fd),
            ext_data_control_source_v1::Event::Cancelled => seen.clip.cancelled += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlManagerV1,
        _: <ZwlrDataControlManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::DataOffer { id } => {
                seen.clip.offers.push((name(&id), Vec::new()));
                seen.clip_wlr.insert(name(&id), id);
            }
            zwlr_data_control_device_v1::Event::Selection { id } => {
                seen.clip.selections += 1;
                seen.clip.current = id.as_ref().map(name);
            }
            _ => {}
        }
    }

    event_created_child!(Seen, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: zwlr_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event {
            if let Some((_, types)) = seen
                .clip
                .offers
                .iter_mut()
                .find(|(id, _)| *id == name(offer))
            {
                types.push(mime_type);
            }
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, Payload> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        payload: &Payload,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { fd, .. } => hand_over(seen, payload, fd),
            zwlr_data_control_source_v1::Event::Cancelled => seen.clip.cancelled += 1,
            _ => {}
        }
    }
}
