//! `sengi-settings --screen off|on`: switch every display's panel off or
//! back on, through the compositor's `wlr-output-power-management`.
//!
//! This is what `swayidle` runs when the "switch the screen off" time is
//! up, and again when a key is pressed. It is here, and not a separate
//! program, because the base image has nothing else that speaks the
//! protocol and this one already has a Wayland client in it.

use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1;
use wayland_protocols_wlr::output_power_management::v1::client::zwlr_output_power_v1::{
    self, Mode, ZwlrOutputPowerV1,
};

#[derive(Default)]
struct State {
    manager: Option<ZwlrOutputPowerManagerV1>,
    outputs: Vec<WlOutput>,
    /// An output refused: it does not support being switched.
    failed: bool,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut State,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<State>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            if interface == ZwlrOutputPowerManagerV1::interface().name && state.manager.is_none() {
                state.manager = Some(registry.bind(name, 1, queue, ()));
            } else if interface == WlOutput::interface().name && state.outputs.len() < 16 {
                state.outputs.push(registry.bind(name, 1, queue, ()));
            }
        }
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        _: &mut State,
        _: &WlOutput,
        _: <WlOutput as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn event(
        _: &mut State,
        _: &ZwlrOutputPowerManagerV1,
        _: <ZwlrOutputPowerManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<ZwlrOutputPowerV1, ()> for State {
    fn event(
        state: &mut State,
        _: &ZwlrOutputPowerV1,
        event: zwlr_output_power_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let zwlr_output_power_v1::Event::Failed = event {
            state.failed = true;
        }
    }
}

/// Switch every output on or off. An error says what was missing.
pub fn switch(on: bool) -> Result<(), String> {
    let lost = |error: wayland_client::DispatchError| format!("the compositor went away: {error}");
    let connection =
        Connection::connect_to_env().map_err(|error| format!("no compositor: {error}"))?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    let mut state = State::default();
    connection.display().get_registry(&handle, ());
    queue.roundtrip(&mut state).map_err(lost)?;
    let manager = state
        .manager
        .clone()
        .ok_or("the compositor cannot switch displays off")?;
    let mode = if on { Mode::On } else { Mode::Off };
    for output in &state.outputs {
        manager.get_output_power(output, &handle, ()).set_mode(mode);
    }
    // The setting belongs to the output, not to this client: it stays
    // when the connection closes.
    queue.roundtrip(&mut state).map_err(lost)?;
    if state.failed {
        return Err("a display cannot be switched off".to_string());
    }
    Ok(())
}
