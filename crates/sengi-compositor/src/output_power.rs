//! wlr-output-power-management: lets a client switch a display's panel
//! off and on. `sengi-settings --screen off|on` is that client, run by
//! swayidle when the session has been idle for the time set in Settings.
//!
//! Smithay does not ship this protocol, so it is implemented here. The
//! compositor only keeps which outputs are asked to be off; the backend
//! that drives real screens looks at that when it draws. Any input lights
//! everything again, whoever switched it off: a dark screen that a key
//! does not bring back looks like a dead machine. That first key, click
//! or movement only lights the screen; see `State::note_activity`.

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols_wlr::output_power_management::v1::server::{
            zwlr_output_power_manager_v1::{self, ZwlrOutputPowerManagerV1},
            zwlr_output_power_v1::{self, Mode, ZwlrOutputPowerV1},
        },
        wayland_server::{
            backend::GlobalId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
            Resource, WEnum,
        },
    },
};

use crate::state::State;

const VERSION: u32 = 1;

pub struct OutputPowerState {
    _global: GlobalId,
    /// The power objects clients hold, each for one output.
    controls: Vec<(ZwlrOutputPowerV1, Output)>,
    /// The outputs that are switched off.
    off: Vec<Output>,
}

impl OutputPowerState {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            _global: display.create_global::<State, ZwlrOutputPowerManagerV1, ()>(VERSION, ()),
            controls: Vec::new(),
            off: Vec::new(),
        }
    }
}

fn mode(on: bool) -> Mode {
    if on {
        Mode::On
    } else {
        Mode::Off
    }
}

impl State {
    /// Whether `output` has been asked to be dark.
    pub fn output_is_off(&self, output: &Output) -> bool {
        self.output_power.off.contains(output)
    }

    /// Switches `output` off or on and tells everyone who watches it.
    pub fn set_output_power(&mut self, output: &Output, on: bool) {
        let power = &mut self.output_power;
        if power.off.contains(output) != on {
            return;
        }
        if on {
            power.off.retain(|off| off != output);
        } else {
            power.off.push(output.clone());
        }
        power.controls.retain(|(control, _)| control.is_alive());
        for (control, of) in &power.controls {
            if of == output {
                control.mode(mode(on));
            }
        }
        self.request_redraw();
    }

    pub fn any_output_off(&self) -> bool {
        !self.output_power.off.is_empty()
    }

    /// Lights every output that was switched off.
    pub fn wake_outputs(&mut self) {
        for output in std::mem::take(&mut self.output_power.off) {
            // Back in the list for a moment, so that the one place that
            // switches does all of it.
            self.output_power.off.push(output.clone());
            self.set_output_power(&output, true);
        }
    }

    /// An output is gone; nothing of it needs remembering.
    pub fn forget_output_power(&mut self, output: &Output) {
        self.output_power.off.retain(|off| off != output);
        self.output_power.controls.retain(|(control, of)| {
            if of == output && control.is_alive() {
                control.failed();
            }
            of != output && control.is_alive()
        });
    }

    /// One line for the SIGUSR1 report.
    pub fn output_power_status(&self) -> String {
        let outputs: Vec<String> = self
            .space
            .outputs()
            .map(|output| {
                let state = if self.output_is_off(output) {
                    "off"
                } else {
                    "on"
                };
                format!("{} {state}", output.name())
            })
            .collect();
        format!("outputs: {}", outputs.join(", "))
    }
}

impl GlobalDispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputPowerManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _: &()) -> bool {
        crate::security::trusted(&client)
    }
}

impl Dispatch<ZwlrOutputPowerManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrOutputPowerManagerV1,
        request: zwlr_output_power_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_output_power_manager_v1::Request::GetOutputPower { id, output } = request {
            let control = data_init.init(id, ());
            match Output::from_resource(&output) {
                Some(output) => {
                    control.mode(mode(!state.output_is_off(&output)));
                    state.output_power.controls.push((control, output));
                }
                // An output that went away between the client hearing of
                // it and asking.
                None => control.failed(),
            }
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        control: &ZwlrOutputPowerV1,
        request: zwlr_output_power_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_output_power_v1::Request::SetMode { mode } = request {
            let output = state
                .output_power
                .controls
                .iter()
                .find(|(known, _)| known == control)
                .map(|(_, output)| output.clone());
            match (output, mode) {
                (Some(output), WEnum::Value(Mode::On)) => state.set_output_power(&output, true),
                (Some(output), WEnum::Value(Mode::Off)) => state.set_output_power(&output, false),
                _ => {}
            }
        }
    }
}
