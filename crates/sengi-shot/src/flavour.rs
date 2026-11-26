// The data-control protocols are two copies of one another: the ext one and
// the older wlr one. This file is included once for each, inside a module
// that names the generated types it needs: Manager, Device, Source, Offer,
// SourceEvent and DATA_OFFER_OPCODE.

use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{
    event_created_child, globals::GlobalList, Connection, Dispatch, EventQueue, Proxy,
    QueueHandle,
};

use super::{Held, State, MIME};

/// Takes the clipboard with a source that offers the picture. The objects
/// it returns keep the selection ours for as long as they are held.
pub fn offer(
    globals: &GlobalList,
    queue: &mut EventQueue<State>,
    state: &mut State,
    seat: &WlSeat,
) -> Result<Held, String> {
    let qh = queue.handle();
    let manager: Manager = globals
        .bind(&qh, 1..=1, ())
        .map_err(|e| format!("cannot bind the clipboard protocol: {e}"))?;
    let device = manager.get_data_device(seat, &qh, ());
    let source = manager.create_data_source(&qh, ());
    source.offer(MIME.to_string());
    device.set_selection(Some(&source));
    queue
        .roundtrip(state)
        .map_err(|e| format!("the compositor dropped us: {e}"))?;
    Ok(Held::new((manager, device, source)))
}

impl Dispatch<Manager, ()> for State {
    fn event(
        _: &mut Self,
        _: &Manager,
        _: <Manager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<Device, ()> for State {
    fn event(
        _: &mut Self,
        _: &Device,
        _: <Device as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(State, Device, [
        DATA_OFFER_OPCODE => (Offer, ()),
    ]);
}

// Offers other programs make are of no interest, and are not kept.
impl Dispatch<Offer, ()> for State {
    fn event(
        _: &mut Self,
        offer: &Offer,
        _: <Offer as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if offer.is_alive() {
            offer.destroy();
        }
    }
}

impl Dispatch<Source, ()> for State {
    fn event(
        state: &mut Self,
        _: &Source,
        event: SourceEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            SourceEvent::Send { mime_type, fd } => {
                if mime_type == MIME {
                    state.requests.push(fd);
                }
            }
            SourceEvent::Cancelled => state.cancelled = true,
            _ => {}
        }
    }
}
