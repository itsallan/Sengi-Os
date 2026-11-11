//! The state of the open panel and what it shows.
//!
//! [`Panel`] takes [`Event`]s from the workers and the user's actions, and
//! hands back [`Command`]s for the workers and a [`View`] for the window.
//! It never touches the window or a service, so every rule about what is
//! shown when can be tested here.

use crate::backlight::{self, Backlight};
use crate::battery::Battery;
use crate::bluetooth::{self, Bluetooth};
use crate::secret::Secret;
use crate::services::{Command, Event, Hardware};
use crate::volume::{Echo, Volume, MAX_PERCENT};
use crate::wifi::{self, Join, JoinError, Link, Network, Problem, Security, Wifi};

/// A reading that has not arrived yet, has, or will not.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reading<T> {
    Pending,
    Known(T),
}

pub struct Panel {
    hardware: Hardware,
    dnd_available: bool,

    wifi: Reading<Wifi>,
    /// Where the Wi-Fi switch was last put, until the service catches up.
    wifi_wanted: Option<bool>,
    join: Join,

    bluetooth: Reading<Bluetooth>,
    bluetooth_wanted: Option<bool>,

    volume: Reading<Option<Volume>>,
    volume_echo: Echo,
    brightness_echo: Echo,

    /// `None` until the service says, or the panel sets it. sengi-notify
    /// cannot be asked, so in practice this is what was last pressed here.
    dnd: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkRow {
    pub name: String,
    pub secured: bool,
    pub known: bool,
    pub level: i32,
    /// Being joined right now.
    pub busy: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prompt {
    pub name: String,
    /// What was wrong with the last passphrase, or empty.
    pub problem: String,
    /// Sent, waiting for the network's answer.
    pub busy: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiView {
    /// False greys the tile out: no adapter, or not known yet.
    pub enabled: bool,
    pub on: bool,
    pub detail: String,
    /// Signal of the connected network, 1 to 4, or 0 for none.
    pub level: i32,
    /// "Wired, connected", shown as a line of its own.
    pub wired: bool,
    pub rows: Vec<NetworkRow>,
    /// Shown in place of an empty list.
    pub note: String,
    /// Why the last attempt to join failed, or empty.
    pub alert: String,
    pub prompt: Option<Prompt>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwitchView {
    pub visible: bool,
    pub enabled: bool,
    pub on: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SliderView {
    pub visible: bool,
    pub enabled: bool,
    pub value: i32,
    pub minimum: i32,
    pub label: String,
    pub muted: bool,
}

/// Do-not-disturb is two buttons, not a switch: a switch would have to
/// claim a position the panel does not know when it opens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DndView {
    pub visible: bool,
    pub state: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatteryView {
    pub visible: bool,
    pub percent: i32,
    pub label: String,
    pub detail: String,
    pub charging: bool,
    pub low: bool,
    pub warning: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct View {
    pub wifi: WifiView,
    pub bluetooth: SwitchView,
    pub volume: SliderView,
    pub brightness: SliderView,
    pub dnd: DndView,
    pub battery: BatteryView,
}

impl Panel {
    /// `hardware` is read before the panel opens, so that tiles for things
    /// the machine does not have never flash up.
    pub fn new(hardware: Hardware, dnd_available: bool) -> Panel {
        Panel {
            hardware,
            dnd_available,
            wifi: Reading::Pending,
            wifi_wanted: None,
            join: Join::Idle,
            bluetooth: Reading::Pending,
            bluetooth_wanted: None,
            volume: Reading::Pending,
            volume_echo: Echo::default(),
            brightness_echo: Echo::default(),
            dnd: None,
        }
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Wifi(wifi) => {
                if !matches!(wifi, Wifi::On { .. }) {
                    self.join.reset();
                }
                self.wifi = Reading::Known(wifi);
                self.wifi_wanted = None;
            }
            Event::Joined { id, result } => self.join.finished(&id, result),
            Event::Bluetooth(bluetooth) => {
                self.bluetooth = Reading::Known(bluetooth);
                self.bluetooth_wanted = None;
            }
            Event::Volume { sequence, volume } => {
                // A reading from before the slider's last move would pull
                // the knob back under the pointer.
                if self.volume_echo.current(sequence) || volume.is_none() {
                    self.volume = Reading::Known(volume);
                }
            }
            Event::Hardware { sequence, hardware } => {
                let mine = self.hardware.backlight.take();
                self.hardware = hardware;
                if !self.brightness_echo.current(sequence) && self.hardware.backlight.is_some() {
                    self.hardware.backlight = mine;
                }
            }
            Event::Dnd(Some(on)) => self.dnd = Some(on),
            Event::Dnd(None) => {}
        }
    }

    pub fn toggle_wifi(&mut self, on: bool) -> Option<Command> {
        self.wifi_wanted = Some(on);
        self.join.reset();
        Some(Command::WifiPower(on))
    }

    /// The networks in the list, in the order of [`WifiView::rows`].
    fn listed(&self) -> impl Iterator<Item = &Network> {
        let networks = match &self.wifi {
            Reading::Known(Wifi::On { networks, .. }) => networks.as_slice(),
            _ => &[],
        };
        networks.iter().filter(|network| !network.connected)
    }

    pub fn choose_network(&mut self, row: usize) -> Option<Command> {
        let network = self.listed().nth(row)?.clone();
        self.join.choose(&network).map(Command::Join)
    }

    pub fn submit_passphrase(&mut self, passphrase: Secret) -> Option<Command> {
        self.join.submit(passphrase).map(Command::Join)
    }

    /// Escape in the passphrase field, or its Cancel button.
    pub fn cancel_prompt(&mut self) {
        self.join.cancel();
    }

    /// Whether Escape has something smaller than the panel to close.
    pub fn prompt_open(&self) -> bool {
        matches!(self.join, Join::Asking { .. })
    }

    pub fn toggle_bluetooth(&mut self, on: bool) -> Option<Command> {
        self.bluetooth_wanted = Some(on);
        Some(Command::BluetoothPower(on))
    }

    pub fn set_volume(&mut self, percent: i32) -> Option<Command> {
        let Reading::Known(Some(volume)) = &mut self.volume else {
            return None;
        };
        volume.percent = percent.clamp(0, MAX_PERCENT as i32) as u32;
        Some(Command::Volume(self.volume_echo.next(volume.percent)))
    }

    pub fn toggle_mute(&mut self) -> Option<Command> {
        let Reading::Known(Some(volume)) = &mut self.volume else {
            return None;
        };
        volume.muted = !volume.muted;
        Some(Command::Mute(volume.muted))
    }

    pub fn set_brightness(&mut self, percent: i32) -> Option<Command> {
        let backlight = self.hardware.backlight.as_mut()?;
        let percent = percent.clamp(backlight::MIN_PERCENT as i32, 100) as u32;
        backlight.brightness = backlight.raw(percent);
        Some(Command::Brightness(self.brightness_echo.next(percent)))
    }

    pub fn set_dnd(&mut self, on: bool) -> Option<Command> {
        if !self.dnd_available {
            return None;
        }
        self.dnd = Some(on);
        Some(Command::Dnd(on))
    }

    pub fn view(&self) -> View {
        View {
            wifi: self.wifi_view(),
            bluetooth: self.bluetooth_view(),
            volume: self.volume_view(),
            brightness: brightness_view(self.hardware.backlight.as_ref()),
            dnd: DndView {
                visible: self.dnd_available,
                state: self.dnd,
            },
            battery: battery_view(self.hardware.battery.as_ref()),
        }
    }

    fn wifi_view(&self) -> WifiView {
        let wired = self.hardware.wired;
        let off = |enabled: bool, detail: &str| WifiView {
            enabled,
            on: enabled && self.wifi_wanted == Some(true),
            detail: detail.to_string(),
            wired,
            ..WifiView::default()
        };
        match &self.wifi {
            // Without a wireless device there is nothing to wait for.
            Reading::Pending if self.hardware.wifi_device => off(false, ""),
            Reading::Pending => off(false, "No Wi-Fi adapter"),
            Reading::Known(Wifi::Absent) if self.hardware.wifi_device => {
                off(false, "Wi-Fi service is not running")
            }
            Reading::Known(Wifi::Absent) => off(false, "No Wi-Fi adapter"),
            Reading::Known(Wifi::Off) => off(true, "Off"),
            Reading::Known(Wifi::On {
                link,
                scanning,
                networks,
            }) => {
                let (link, scanning) = (*link, *scanning);
                let connected = networks.iter().find(|network| network.connected);
                let joining = match &self.join {
                    Join::Joining { name, .. } => Some(name.as_str()),
                    _ => None,
                };
                let detail = match (joining, link, connected) {
                    (Some(name), _, _) => format!("Connecting to {name}\u{2026}"),
                    (None, Link::Connecting, Some(network)) => {
                        format!("Connecting to {}\u{2026}", network.name)
                    }
                    (None, Link::Connecting, None) => "Connecting\u{2026}".to_string(),
                    (None, Link::Connected, Some(network)) => network.name.clone(),
                    _ => "Not connected".to_string(),
                };
                let rows: Vec<NetworkRow> = self
                    .listed()
                    .map(|network| NetworkRow {
                        name: network.name.clone(),
                        secured: network.security != Security::Open,
                        known: network.known,
                        level: wifi::level(network.signal),
                        busy: matches!(&self.join, Join::Joining { id, .. } if *id == network.id),
                    })
                    .collect();
                let note = match (rows.is_empty(), scanning) {
                    (false, _) => "",
                    (true, true) => "Looking for networks\u{2026}",
                    (true, false) if connected.is_some() => "No other networks in range",
                    (true, false) => "No networks in range",
                };
                WifiView {
                    enabled: true,
                    on: self.wifi_wanted.unwrap_or(true),
                    detail,
                    level: match (link, connected) {
                        (Link::Connected, Some(network)) => wifi::level(network.signal),
                        _ => 0,
                    },
                    wired,
                    rows,
                    note: note.to_string(),
                    alert: self.alert(),
                    prompt: self.prompt(),
                }
            }
        }
    }

    fn prompt(&self) -> Option<Prompt> {
        match &self.join {
            Join::Asking { name, problem, .. } => Some(Prompt {
                name: name.clone(),
                problem: problem.map(problem_text).unwrap_or_default().to_string(),
                busy: false,
            }),
            Join::Joining {
                name, typed: true, ..
            } => Some(Prompt {
                name: name.clone(),
                problem: String::new(),
                busy: true,
            }),
            _ => None,
        }
    }

    fn alert(&self) -> String {
        let Join::Failed { name, error } = &self.join else {
            return String::new();
        };
        match error {
            JoinError::Failed | JoinError::Aborted | JoinError::NeedsPassphrase => {
                format!("Could not connect to {name}")
            }
            JoinError::Timeout => format!("{name} did not answer"),
            JoinError::NotConfigured => format!("{name} has to be set up before it can be joined"),
            JoinError::NotSupported => format!("{name} uses security that is not supported"),
            JoinError::Unavailable => "Wi-Fi is not available".to_string(),
        }
    }

    fn bluetooth_view(&self) -> SwitchView {
        let tile = |enabled: bool, on: bool, detail: String| SwitchView {
            visible: true,
            enabled,
            on: self.bluetooth_wanted.filter(|_| enabled).unwrap_or(on),
            detail,
        };
        match &self.bluetooth {
            Reading::Pending if self.hardware.bluetooth_device => tile(false, false, String::new()),
            Reading::Known(Bluetooth::Absent) if self.hardware.bluetooth_device => {
                tile(false, false, "Bluetooth service is not running".to_string())
            }
            Reading::Pending | Reading::Known(Bluetooth::Absent) => {
                tile(false, false, "No Bluetooth adapter".to_string())
            }
            Reading::Known(Bluetooth::Off) => tile(true, false, "Off".to_string()),
            Reading::Known(Bluetooth::On { devices }) => {
                tile(true, true, bluetooth::summary(devices))
            }
        }
    }

    fn volume_view(&self) -> SliderView {
        let tile = |enabled: bool, label: &str| SliderView {
            visible: true,
            enabled,
            label: label.to_string(),
            ..SliderView::default()
        };
        match &self.volume {
            Reading::Pending => tile(false, ""),
            Reading::Known(None) => tile(false, "No output"),
            Reading::Known(Some(volume)) => SliderView {
                value: volume.percent.min(MAX_PERCENT) as i32,
                muted: volume.muted,
                ..tile(
                    true,
                    &if volume.muted {
                        "Muted".to_string()
                    } else {
                        format!("{}%", volume.percent)
                    },
                )
            },
        }
    }
}

fn problem_text(problem: Problem) -> &'static str {
    match problem {
        Problem::TooShort => "A passphrase has at least 8 characters",
        Problem::TooLong => "A passphrase has at most 63 characters",
        Problem::NotPlain => "Only unaccented letters, digits and symbols",
        Problem::Rejected => "That passphrase was not accepted",
        Problem::Stale => "The saved passphrase no longer works",
    }
}

/// The brightness tile exists only on a machine with a backlight.
fn brightness_view(backlight: Option<&Backlight>) -> SliderView {
    let Some(backlight) = backlight else {
        return SliderView::default();
    };
    let percent = backlight.percent().max(backlight::MIN_PERCENT);
    SliderView {
        visible: true,
        enabled: true,
        value: percent as i32,
        minimum: backlight::MIN_PERCENT as i32,
        label: format!("{percent}%"),
        muted: false,
    }
}

/// And the battery row only on a machine with a battery.
fn battery_view(battery: Option<&Battery>) -> BatteryView {
    let Some(battery) = battery else {
        return BatteryView::default();
    };
    BatteryView {
        visible: true,
        percent: battery.percent as i32,
        label: format!("{}%", battery.percent),
        detail: battery.detail(),
        charging: battery.charging(),
        low: battery.low(),
        warning: battery.warning(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::battery::Charge;

    fn network(name: &str, security: Security, known: bool, signal: i16) -> Network {
        Network {
            id: format!("/{name}"),
            name: name.to_string(),
            security,
            known,
            connected: false,
            signal,
        }
    }

    fn on(link: Link, networks: Vec<Network>) -> Wifi {
        Wifi::On {
            link,
            scanning: false,
            networks: wifi::arrange(networks),
        }
    }

    fn laptop() -> Hardware {
        Hardware {
            battery: Some(Battery {
                percent: 64,
                charge: Charge::Discharging,
                minutes: Some(185),
            }),
            backlight: Some(Backlight {
                name: "intel_backlight".to_string(),
                brightness: 500,
                max: 1000,
            }),
            wired: false,
            wifi_device: true,
            bluetooth_device: true,
        }
    }

    fn home_and_cafe() -> Vec<Network> {
        let mut home = network("Home", Security::Passphrase, true, -5200);
        home.connected = true;
        vec![
            home,
            network("Cafe", Security::Open, false, -7400),
            network("Office", Security::Passphrase, false, -6100),
        ]
    }

    #[test]
    fn a_machine_with_nothing_shows_what_is_missing_at_once() {
        let panel = Panel::new(Hardware::default(), false);
        let view = panel.view();
        assert!(!view.wifi.enabled);
        assert_eq!(view.wifi.detail, "No Wi-Fi adapter");
        assert!(!view.wifi.wired);
        assert!(view.bluetooth.visible && !view.bluetooth.enabled);
        assert_eq!(view.bluetooth.detail, "No Bluetooth adapter");
        assert!(view.volume.visible && !view.volume.enabled);
        assert!(!view.brightness.visible);
        assert!(!view.dnd.visible);
        assert!(!view.battery.visible);
    }

    #[test]
    fn a_wired_machine_says_so() {
        let hardware = Hardware {
            wired: true,
            ..Hardware::default()
        };
        let mut panel = Panel::new(hardware, false);
        assert!(panel.view().wifi.wired);
        panel.apply(Event::Wifi(Wifi::Absent));
        let view = panel.view();
        assert!(view.wifi.wired && !view.wifi.enabled);
        assert_eq!(view.wifi.detail, "No Wi-Fi adapter");
    }

    #[test]
    fn tiles_wait_for_services_the_hardware_promises() {
        let mut panel = Panel::new(laptop(), true);
        let view = panel.view();
        assert_eq!(view.wifi.detail, "");
        assert!(!view.wifi.enabled);
        assert_eq!(view.bluetooth.detail, "");
        assert!(view.brightness.visible && view.battery.visible && view.dnd.visible);

        // The device is there but the service never came up.
        panel.apply(Event::Wifi(Wifi::Absent));
        panel.apply(Event::Bluetooth(Bluetooth::Absent));
        let view = panel.view();
        assert_eq!(view.wifi.detail, "Wi-Fi service is not running");
        assert_eq!(view.bluetooth.detail, "Bluetooth service is not running");
    }

    #[test]
    fn the_connected_network_is_in_the_header_not_the_list() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));
        let view = panel.view().wifi;
        assert!(view.enabled && view.on);
        assert_eq!(view.detail, "Home");
        assert_eq!(view.level, 4);
        let names: Vec<_> = view.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["Office", "Cafe"]);
        assert!(view.rows[0].secured && !view.rows[1].secured);
        assert_eq!((view.rows[0].level, view.rows[1].level), (3, 2));
        assert_eq!(view.note, "");
    }

    #[test]
    fn an_empty_list_explains_itself() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(Wifi::On {
            link: Link::Disconnected,
            scanning: true,
            networks: Vec::new(),
        }));
        assert_eq!(panel.view().wifi.note, "Looking for networks\u{2026}");
        assert_eq!(panel.view().wifi.detail, "Not connected");
        panel.apply(Event::Wifi(on(Link::Disconnected, Vec::new())));
        assert_eq!(panel.view().wifi.note, "No networks in range");
        let mut home = network("Home", Security::Passphrase, true, -5000);
        home.connected = true;
        panel.apply(Event::Wifi(on(Link::Connected, vec![home])));
        assert_eq!(panel.view().wifi.note, "No other networks in range");
    }

    #[test]
    fn joining_a_new_secured_network_end_to_end() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));

        // Office is row 0 and needs a passphrase.
        assert!(panel.choose_network(0).is_none());
        let prompt = panel.view().wifi.prompt.unwrap();
        assert_eq!(prompt.name, "Office");
        assert!(!prompt.busy && prompt.problem.is_empty());
        assert!(panel.prompt_open());

        assert!(panel
            .submit_passphrase(Secret::new("short".to_string()))
            .is_none());
        assert_eq!(
            panel.view().wifi.prompt.unwrap().problem,
            "A passphrase has at least 8 characters"
        );

        let Some(Command::Join(request)) =
            panel.submit_passphrase(Secret::new("wrong passphrase".to_string()))
        else {
            panic!("expected a join");
        };
        assert_eq!(request.id, "/Office");
        assert_eq!(request.passphrase.unwrap().expose(), "wrong passphrase");
        let view = panel.view().wifi;
        assert!(view.prompt.unwrap().busy);
        assert_eq!(view.detail, "Connecting to Office\u{2026}");

        panel.apply(Event::Joined {
            id: "/Office".to_string(),
            result: Err(JoinError::Failed),
        });
        let prompt = panel.view().wifi.prompt.unwrap();
        assert!(!prompt.busy);
        assert_eq!(prompt.problem, "That passphrase was not accepted");

        assert!(panel
            .submit_passphrase(Secret::new("right passphrase".to_string()))
            .is_some());
        panel.apply(Event::Joined {
            id: "/Office".to_string(),
            result: Ok(()),
        });
        assert!(panel.view().wifi.prompt.is_none());
        assert!(!panel.prompt_open());
    }

    #[test]
    fn a_known_network_shows_as_busy_in_its_row() {
        let mut panel = Panel::new(laptop(), false);
        let networks = vec![
            network("Home", Security::Passphrase, true, -5000),
            network("Cafe", Security::Open, false, -6000),
        ];
        panel.apply(Event::Wifi(on(Link::Disconnected, networks)));
        assert!(matches!(panel.choose_network(0), Some(Command::Join(_))));
        let view = panel.view().wifi;
        assert!(view.rows[0].busy && !view.rows[1].busy);
        assert!(view.prompt.is_none());
        assert_eq!(view.detail, "Connecting to Home\u{2026}");

        // The stored passphrase is refused: the field opens for a new one.
        panel.apply(Event::Joined {
            id: "/Home".to_string(),
            result: Err(JoinError::Failed),
        });
        let prompt = panel.view().wifi.prompt.unwrap();
        assert_eq!(prompt.name, "Home");
        assert_eq!(prompt.problem, "The saved passphrase no longer works");
        panel.cancel_prompt();

        // Anything else is reported and left at that.
        panel.choose_network(0);
        panel.apply(Event::Joined {
            id: "/Home".to_string(),
            result: Err(JoinError::Timeout),
        });
        assert_eq!(panel.view().wifi.alert, "Home did not answer");
        // Trying something else clears the complaint.
        panel.cancel_prompt();
        assert_eq!(panel.view().wifi.alert, "");
    }

    #[test]
    fn rows_that_do_not_exist_cannot_be_chosen() {
        let mut panel = Panel::new(laptop(), false);
        assert!(panel.choose_network(0).is_none());
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));
        assert!(panel.choose_network(2).is_none());
        assert!(panel.choose_network(usize::MAX).is_none());
    }

    #[test]
    fn switching_wifi_off_closes_the_passphrase_prompt() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));
        panel.choose_network(0);
        assert!(panel.prompt_open());
        assert!(matches!(
            panel.toggle_wifi(false),
            Some(Command::WifiPower(false))
        ));
        assert!(!panel.prompt_open());
        // The switch stays where it was put until the service answers.
        assert!(!panel.view().wifi.on);
        panel.apply(Event::Wifi(Wifi::Off));
        let view = panel.view().wifi;
        assert!(view.enabled && !view.on);
        assert_eq!(view.detail, "Off");
        assert!(view.rows.is_empty());
    }

    #[test]
    fn a_switch_that_did_not_take_springs_back() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(Wifi::Off));
        panel.toggle_wifi(true);
        assert!(panel.view().wifi.on);
        panel.apply(Event::Wifi(Wifi::Off));
        assert!(!panel.view().wifi.on);

        panel.apply(Event::Bluetooth(Bluetooth::Off));
        panel.toggle_bluetooth(true);
        assert!(panel.view().bluetooth.on);
        panel.apply(Event::Bluetooth(Bluetooth::Off));
        assert!(!panel.view().bluetooth.on);
    }

    #[test]
    fn bluetooth_names_its_connected_devices() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Bluetooth(Bluetooth::On {
            devices: vec!["Headphones".to_string(), "Keyboard".to_string()],
        }));
        let view = panel.view().bluetooth;
        assert!(view.enabled && view.on);
        assert_eq!(view.detail, "Headphones, Keyboard");
    }

    fn volume(percent: u32, muted: bool) -> Option<Volume> {
        Some(Volume { percent, muted })
    }

    #[test]
    fn volume_follows_the_slider_not_stale_readings() {
        let mut panel = Panel::new(laptop(), false);
        assert!(panel.set_volume(10).is_none(), "nothing to control yet");
        panel.apply(Event::Volume {
            sequence: 0,
            volume: volume(40, false),
        });
        assert_eq!(panel.view().volume.value, 40);
        assert_eq!(panel.view().volume.label, "40%");

        let Some(Command::Volume(first)) = panel.set_volume(50) else {
            panic!("expected a volume command");
        };
        let Some(Command::Volume(second)) = panel.set_volume(60) else {
            panic!("expected a volume command");
        };
        // The worker reports the first while the slider is at the second.
        panel.apply(Event::Volume {
            sequence: first.sequence,
            volume: volume(50, false),
        });
        assert_eq!(panel.view().volume.value, 60);
        panel.apply(Event::Volume {
            sequence: second.sequence,
            volume: volume(60, false),
        });
        assert_eq!(panel.view().volume.value, 60);
        // Later something else turns it down.
        panel.apply(Event::Volume {
            sequence: second.sequence,
            volume: volume(20, false),
        });
        assert_eq!(panel.view().volume.value, 20);
    }

    #[test]
    fn volume_edges() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Volume {
            sequence: 0,
            volume: volume(130, false),
        });
        let view = panel.view().volume;
        assert_eq!((view.value, view.label.as_str()), (100, "130%"));

        assert!(matches!(panel.toggle_mute(), Some(Command::Mute(true))));
        let view = panel.view().volume;
        assert!(view.muted);
        assert_eq!(view.label, "Muted");

        let Some(Command::Volume(step)) = panel.set_volume(900) else {
            panic!("expected a volume command");
        };
        assert_eq!(step.value, 100);

        // The sink goes away, whatever the slider was doing.
        panel.set_volume(30);
        panel.apply(Event::Volume {
            sequence: 0,
            volume: None,
        });
        let view = panel.view().volume;
        assert!(view.visible && !view.enabled);
        assert_eq!(view.label, "No output");
        assert!(panel.toggle_mute().is_none());
    }

    #[test]
    fn brightness_has_a_floor_and_ignores_stale_readings() {
        let mut panel = Panel::new(laptop(), false);
        let view = panel.view().brightness;
        assert_eq!((view.value, view.minimum), (50, 5));

        let Some(Command::Brightness(step)) = panel.set_brightness(0) else {
            panic!("expected a brightness command");
        };
        assert_eq!(step.value, 5);
        assert_eq!(panel.view().brightness.value, 5);

        // A poll from before the change does not move the slider back.
        panel.apply(Event::Hardware {
            sequence: 0,
            hardware: laptop(),
        });
        assert_eq!(panel.view().brightness.value, 5);
        let mut after = laptop();
        after.backlight.as_mut().unwrap().brightness = 50;
        panel.apply(Event::Hardware {
            sequence: step.sequence,
            hardware: after,
        });
        assert_eq!(panel.view().brightness.value, 5);

        // No backlight, no slider, no command.
        let mut panel = Panel::new(Hardware::default(), false);
        assert!(panel.set_brightness(50).is_none());
    }

    #[test]
    fn battery_row() {
        let panel = Panel::new(laptop(), false);
        let view = panel.view().battery;
        assert_eq!(view.label, "64%");
        assert_eq!(view.detail, "3 h 05 min remaining");
        assert!(!view.low && !view.warning && !view.charging);

        let mut hardware = laptop();
        hardware.battery = Some(Battery {
            percent: 7,
            charge: Charge::Discharging,
            minutes: Some(12),
        });
        let view = Panel::new(hardware, false).view().battery;
        assert!(view.low);
        assert_eq!(view.detail, "12 min remaining");
    }

    #[test]
    fn dnd_claims_no_state_it_does_not_know() {
        let mut panel = Panel::new(laptop(), true);
        assert_eq!(panel.view().dnd.state, None);
        // A service that cannot say leaves it unknown.
        panel.apply(Event::Dnd(None));
        assert_eq!(panel.view().dnd.state, None);
        assert!(matches!(panel.set_dnd(true), Some(Command::Dnd(true))));
        assert_eq!(panel.view().dnd.state, Some(true));
        panel.apply(Event::Dnd(None));
        assert_eq!(panel.view().dnd.state, Some(true));
        // One that can is believed.
        panel.apply(Event::Dnd(Some(false)));
        assert_eq!(panel.view().dnd.state, Some(false));

        let mut panel = Panel::new(laptop(), false);
        assert!(panel.set_dnd(true).is_none());
        assert!(!panel.view().dnd.visible);
    }
}
