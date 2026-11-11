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
use crate::services::{Block, Command, Event, Hardware};
use crate::text::isolate;
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
    /// The network list as laid out. See [`Panel::relist`].
    shown: Vec<Shown>,
    /// The pointer is over the list or the keyboard is in it.
    held: bool,
    /// The keyboard's row, by network and not by position.
    selected: Option<String>,

    bluetooth: Reading<Bluetooth>,
    bluetooth_wanted: Option<bool>,

    volume: Reading<Option<Volume>>,
    volume_echo: Echo,
    brightness_echo: Echo,

    /// What the service said, or failing that what was last set here.
    dnd: Option<bool>,
    /// The service has been asked, and whether it could say.
    dnd_asked: Option<bool>,
}

/// A row of the network list.
struct Shown {
    network: Network,
    /// Out of range since the list was last free to change. The row keeps
    /// its place, empty, so that nothing moves up under the pointer.
    gone: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkRow {
    /// Names the network to [`Panel::choose_network`].
    pub id: String,
    pub gone: bool,
    pub name: String,
    pub secured: bool,
    pub known: bool,
    pub level: i32,
    /// Being joined right now.
    pub busy: bool,
    /// An open network that was clicked: the row asks before joining.
    pub confirm: bool,
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
    /// Index into `rows` of the keyboard's row, or -1.
    pub current: i32,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DndControl {
    /// Not known yet which it will be.
    #[default]
    None,
    /// The service says whether do-not-disturb is on: a switch like the
    /// other tiles have.
    Switch,
    /// It cannot say (an older sengi-notify). A switch would have to
    /// claim a position nobody knows, so there are two buttons instead,
    /// and the one pressed here is marked.
    Buttons,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DndView {
    pub visible: bool,
    pub control: DndControl,
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
            shown: Vec::new(),
            held: false,
            selected: None,
            bluetooth: Reading::Pending,
            bluetooth_wanted: None,
            volume: Reading::Pending,
            volume_echo: Echo::default(),
            brightness_echo: Echo::default(),
            dnd: None,
            dnd_asked: None,
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
            Event::Dnd(answer) => {
                self.dnd_asked = Some(answer.is_some());
                self.dnd = answer.or(self.dnd);
            }
        }
        self.relist();
    }

    /// Lay the network list out again from the latest reading.
    ///
    /// While the list is held, by the pointer being over it or the
    /// keyboard being in it, rows stay where they are: a scan result must
    /// not put a different network under a click or under Return. Networks
    /// that have gone leave an empty row, new ones join at the end, and
    /// the proper order comes back when the list is let go.
    fn relist(&mut self) {
        let mut fresh: Vec<Network> = match &self.wifi {
            Reading::Known(Wifi::On { networks, .. }) => networks
                .iter()
                .filter(|network| !network.connected)
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        if !self.held {
            self.shown.clear();
        }
        for row in &mut self.shown {
            match fresh
                .iter()
                .position(|network| network.id == row.network.id)
            {
                Some(index) => {
                    row.network = fresh.remove(index);
                    row.gone = false;
                }
                None => row.gone = true,
            }
        }
        let room = (2 * wifi::MAX_NETWORKS).saturating_sub(self.shown.len());
        self.shown
            .extend(fresh.into_iter().take(room).map(|network| Shown {
                network,
                gone: false,
            }));
    }

    /// The pointer came over the network list or left it, or the keyboard
    /// focus did. `held` is whether either is there now.
    pub fn hold_list(&mut self, held: bool, focused: bool) {
        self.held = held;
        self.relist();
        let selected_row = self.selected_row();
        if focused && selected_row.is_none() {
            self.selected = self.shown.first().map(|row| row.network.id.clone());
        }
    }

    fn selected_row(&self) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        self.shown.iter().position(|row| row.network.id == selected)
    }

    /// Move the keyboard's row up (-1) or down (1). False at the end of
    /// the list, where the key belongs to whatever is next.
    pub fn move_selection(&mut self, delta: i32) -> bool {
        let target = match self.selected_row() {
            Some(row) => row as i64 + i64::from(delta.signum()),
            None => 0,
        };
        match usize::try_from(target)
            .ok()
            .and_then(|row| self.shown.get(row))
        {
            Some(row) => {
                self.selected = Some(row.network.id.clone());
                true
            }
            None => false,
        }
    }

    /// Return on the keyboard's row.
    pub fn activate_selection(&mut self) -> Option<Command> {
        let selected = self.selected.clone()?;
        // A second, separate press on the row that is asking is the yes.
        if matches!(&self.join, Join::Confirming { id, .. } if *id == selected) {
            self.confirm_network(&selected)
        } else {
            self.choose_network(&selected)
        }
    }

    pub fn toggle_wifi(&mut self, on: bool) -> Option<Command> {
        self.wifi_wanted = Some(on);
        self.join.reset();
        self.selected = None;
        Some(Command::WifiPower(on))
    }

    /// A row was clicked. It is named by its network, so that whatever
    /// the list did between the click and now, this is the network whose
    /// name was under the pointer.
    pub fn choose_network(&mut self, id: &str) -> Option<Command> {
        let row = self
            .shown
            .iter()
            .find(|row| row.network.id == id && !row.gone)?;
        let network = row.network.clone();
        self.join.choose(&network).map(Command::Join)
    }

    /// "Join" in the row of an open network that was clicked.
    pub fn confirm_network(&mut self, id: &str) -> Option<Command> {
        // Only for a network still in range.
        self.shown
            .iter()
            .find(|row| row.network.id == id && !row.gone)?;
        self.join.confirm(id).map(Command::Join)
    }

    pub fn submit_passphrase(&mut self, passphrase: Secret) -> Option<Command> {
        self.join.submit(passphrase).map(Command::Join)
    }

    /// Escape in the passphrase field, or its Cancel button.
    pub fn cancel_prompt(&mut self) {
        self.join.cancel();
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
        // Numbered like a move of the slider, so that a reading taken
        // before the click cannot flip the button back for a moment.
        Some(Command::Mute {
            sequence: self.volume_echo.next(volume.percent).sequence,
            muted: volume.muted,
        })
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
                control: match self.dnd_asked {
                    None => DndControl::None,
                    Some(true) => DndControl::Switch,
                    Some(false) => DndControl::Buttons,
                },
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
            Reading::Known(Wifi::Denied) => off(false, "Wi-Fi is not permitted for this user"),
            Reading::Known(Wifi::Off) => off(true, off_text(self.hardware.wifi_block)),
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
                    (Some(name), _, _) => format!("Connecting to {}\u{2026}", isolate(name)),
                    (None, Link::Connecting, Some(network)) => {
                        format!("Connecting to {}\u{2026}", isolate(&network.name))
                    }
                    (None, Link::Connecting, None) => "Connecting\u{2026}".to_string(),
                    (None, Link::Connected, Some(network)) => network.name.clone(),
                    _ => "Not connected".to_string(),
                };
                let rows: Vec<NetworkRow> = self
                    .shown
                    .iter()
                    .map(|Shown { network, gone }| NetworkRow {
                        id: network.id.clone(),
                        gone: *gone,
                        name: network.name.clone(),
                        secured: network.security != Security::Open,
                        known: network.known,
                        level: wifi::level(network.signal),
                        busy: matches!(&self.join, Join::Joining { id, .. } if *id == network.id),
                        confirm: matches!(&self.join, Join::Confirming { id, .. } if *id == network.id),
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
                    current: self.selected_row().map_or(-1, |row| row as i32),
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
        let name = isolate(name);
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
            Reading::Known(Bluetooth::Denied) => tile(
                false,
                false,
                "Bluetooth is not permitted for this user".to_string(),
            ),
            Reading::Known(Bluetooth::Off) => tile(
                true,
                false,
                off_text(self.hardware.bluetooth_block).to_string(),
            ),
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

/// What a radio that is off says for itself. When rfkill is behind it the
/// switch in the tile will not turn it on, and this is why.
fn off_text(block: Block) -> &'static str {
    match block {
        Block::None => "Off",
        Block::Soft => "Turned off",
        Block::Hard => "Blocked by the hardware switch",
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
            ..Hardware::default()
        }
    }

    /// Click the row at `row` of the list as it is shown.
    fn pick(panel: &mut Panel, row: usize) -> Option<Command> {
        let id = panel.view().wifi.rows.get(row)?.id.clone();
        panel.choose_network(&id)
    }

    fn row_names(panel: &Panel) -> Vec<String> {
        let rows = panel.view().wifi.rows;
        rows.iter()
            .map(|row| {
                if row.gone {
                    format!("({})", row.name)
                } else {
                    row.name.clone()
                }
            })
            .collect()
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

        // Running, but the bus policy keeps this user out: say that, not
        // that the service is missing.
        panel.apply(Event::Wifi(Wifi::Denied));
        panel.apply(Event::Bluetooth(Bluetooth::Denied));
        let view = panel.view();
        assert!(!view.wifi.enabled && !view.bluetooth.enabled);
        assert_eq!(view.wifi.detail, "Wi-Fi is not permitted for this user");
        assert_eq!(
            view.bluetooth.detail,
            "Bluetooth is not permitted for this user"
        );
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
        assert!(pick(&mut panel, 0).is_none());
        let prompt = panel.view().wifi.prompt.unwrap();
        assert_eq!(prompt.name, "Office");
        assert!(!prompt.busy && prompt.problem.is_empty());
        assert!(panel.view().wifi.prompt.is_some());

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
        assert_eq!(view.detail, "Connecting to \u{2068}Office\u{2069}\u{2026}");

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
        assert!(panel.view().wifi.prompt.is_none());
    }

    #[test]
    fn a_known_network_shows_as_busy_in_its_row() {
        let mut panel = Panel::new(laptop(), false);
        let networks = vec![
            network("Home", Security::Passphrase, true, -5000),
            network("Cafe", Security::Open, false, -6000),
        ];
        panel.apply(Event::Wifi(on(Link::Disconnected, networks)));
        assert!(matches!(pick(&mut panel, 0), Some(Command::Join(_))));
        let view = panel.view().wifi;
        assert!(view.rows[0].busy && !view.rows[1].busy);
        assert!(view.prompt.is_none());
        assert_eq!(view.detail, "Connecting to \u{2068}Home\u{2069}\u{2026}");

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
        pick(&mut panel, 0);
        panel.apply(Event::Joined {
            id: "/Home".to_string(),
            result: Err(JoinError::Timeout),
        });
        assert_eq!(
            panel.view().wifi.alert,
            "\u{2068}Home\u{2069} did not answer"
        );
        // Trying something else clears the complaint.
        panel.cancel_prompt();
        assert_eq!(panel.view().wifi.alert, "");
    }

    #[test]
    fn an_open_network_asks_in_its_row_before_joining() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));
        // Cafe is open and new; it is the second row.
        assert!(pick(&mut panel, 1).is_none());
        let view = panel.view().wifi;
        assert!(view.rows[1].confirm && !view.rows[0].confirm);
        assert!(view.prompt.is_none());
        // Clicking the row again changes nothing.
        assert!(pick(&mut panel, 1).is_none());
        assert!(panel.confirm_network("/Office").is_none());

        let Some(Command::Join(request)) = panel.confirm_network("/Cafe") else {
            panic!("expected a join");
        };
        assert_eq!(request.id, "/Cafe");
        let view = panel.view().wifi;
        assert!(view.rows[1].busy && !view.rows[1].confirm);
    }

    #[test]
    fn return_twice_joins_an_open_network_from_the_keyboard() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));
        panel.hold_list(true, true);
        panel.move_selection(1);
        assert!(panel.activate_selection().is_none());
        assert!(panel.view().wifi.rows[1].confirm);
        assert!(matches!(panel.activate_selection(), Some(Command::Join(_))));
    }

    #[test]
    fn an_open_network_that_left_cannot_be_confirmed() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));
        panel.hold_list(true, false);
        pick(&mut panel, 1);
        let mut home = network("Home", Security::Passphrase, true, -5200);
        home.connected = true;
        panel.apply(Event::Wifi(on(Link::Connected, vec![home])));
        assert!(panel.confirm_network("/Cafe").is_none());
    }

    #[test]
    fn networks_that_are_not_listed_cannot_be_chosen() {
        let mut panel = Panel::new(laptop(), false);
        assert!(panel.choose_network("/Office").is_none());
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));
        assert!(panel.choose_network("/Nowhere").is_none());
        assert!(panel.choose_network("").is_none());
        // The connected network is in the header, not the list.
        assert!(panel.choose_network("/Home").is_none());
    }

    fn fresh(name: &str, signal: i16) -> Network {
        network(name, Security::Passphrase, false, signal)
    }

    #[test]
    fn the_list_follows_the_scans_while_nobody_is_at_it() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(
            Link::Disconnected,
            vec![fresh("Bakery", -5000), fresh("Cellar", -7500)],
        )));
        assert_eq!(row_names(&panel), ["Bakery", "Cellar"]);
        panel.apply(Event::Wifi(on(
            Link::Disconnected,
            vec![fresh("Attic", -5000), fresh("Cellar", -5000)],
        )));
        assert_eq!(row_names(&panel), ["Attic", "Cellar"]);
    }

    #[test]
    fn a_held_list_keeps_every_row_where_it_is() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(
            Link::Disconnected,
            vec![
                fresh("Bakery", -5000),
                fresh("Cellar", -7500),
                fresh("Dairy", -8500),
            ],
        )));
        // The pointer is over the list, aiming at Cellar in the second row.
        panel.hold_list(true, false);
        // A scan: a strong newcomer that sorts first, Bakery gone, and
        // Dairy now the strongest of all.
        panel.apply(Event::Wifi(on(
            Link::Disconnected,
            vec![
                fresh("Aardvark", -3000),
                fresh("Cellar", -7500),
                fresh("Dairy", -4000),
            ],
        )));
        assert_eq!(
            row_names(&panel),
            ["(Bakery)", "Cellar", "Dairy", "Aardvark"]
        );
        // The click lands on what was under it, and the signal is current.
        assert_eq!(panel.view().wifi.rows[2].level, 4);
        assert!(pick(&mut panel, 0).is_none(), "a network that has gone");
        pick(&mut panel, 1);
        assert_eq!(panel.view().wifi.prompt.unwrap().name, "Cellar");
        panel.cancel_prompt();

        // A network that comes back takes its old row.
        panel.apply(Event::Wifi(on(
            Link::Disconnected,
            vec![fresh("Bakery", -5000), fresh("Cellar", -7500)],
        )));
        assert_eq!(
            row_names(&panel),
            ["Bakery", "Cellar", "(Dairy)", "(Aardvark)"]
        );

        // Let go, and the list is as a fresh one would be.
        panel.hold_list(false, false);
        assert_eq!(row_names(&panel), ["Bakery", "Cellar"]);
    }

    #[test]
    fn the_keyboard_row_is_a_network_not_a_position() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(
            Link::Disconnected,
            vec![
                fresh("Bakery", -5000),
                fresh("Cellar", -7500),
                fresh("Dairy", -8500),
            ],
        )));
        assert_eq!(panel.view().wifi.current, -1);
        // Tab into the list: the first row.
        panel.hold_list(true, true);
        assert_eq!(panel.view().wifi.current, 0);
        assert!(panel.move_selection(1));
        assert_eq!(panel.view().wifi.current, 1);

        // The keyboard leaves, the list reorders, and comes back: still
        // on Cellar, wherever that now is.
        panel.hold_list(false, false);
        panel.apply(Event::Wifi(on(
            Link::Disconnected,
            vec![
                fresh("Aardvark", -5000),
                fresh("Bakery", -5000),
                fresh("Cellar", -5000),
            ],
        )));
        panel.hold_list(true, true);
        assert_eq!(panel.view().wifi.current, 2);
        panel.activate_selection();
        assert_eq!(panel.view().wifi.prompt.unwrap().name, "Cellar");
        panel.cancel_prompt();

        // The ends of the list hand the key on.
        assert!(!panel.move_selection(1));
        assert!(panel.move_selection(-1));
        assert!(panel.move_selection(-1));
        assert!(!panel.move_selection(-1));
        assert_eq!(panel.view().wifi.current, 0);

        // A selection whose network has left the list starts over.
        panel.hold_list(false, false);
        panel.apply(Event::Wifi(on(
            Link::Disconnected,
            vec![fresh("Zebra", -5000)],
        )));
        panel.hold_list(true, true);
        assert_eq!(panel.view().wifi.current, 0);
        assert_eq!(row_names(&panel), ["Zebra"]);
    }

    #[test]
    fn switching_wifi_off_closes_the_passphrase_prompt() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Wifi(on(Link::Connected, home_and_cafe())));
        pick(&mut panel, 0);
        assert!(panel.view().wifi.prompt.is_some());
        assert!(matches!(
            panel.toggle_wifi(false),
            Some(Command::WifiPower(false))
        ));
        assert!(panel.view().wifi.prompt.is_none());
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
    fn a_radio_held_off_by_rfkill_says_why() {
        let mut hardware = laptop();
        hardware.wifi_block = Block::Hard;
        hardware.bluetooth_block = Block::Soft;
        let mut panel = Panel::new(hardware, false);
        panel.apply(Event::Wifi(Wifi::Off));
        panel.apply(Event::Bluetooth(Bluetooth::Off));
        assert_eq!(panel.view().wifi.detail, "Blocked by the hardware switch");
        assert_eq!(panel.view().bluetooth.detail, "Turned off");

        // The switch is tried and does not take: it springs back, and the
        // reason is still there to read.
        panel.toggle_wifi(true);
        panel.apply(Event::Wifi(Wifi::Off));
        let view = panel.view().wifi;
        assert!(!view.on);
        assert_eq!(view.detail, "Blocked by the hardware switch");

        // The block is lifted while the panel is open.
        panel.apply(Event::Hardware {
            sequence: 0,
            hardware: laptop(),
        });
        assert_eq!(panel.view().wifi.detail, "Off");
    }

    #[test]
    fn bluetooth_names_its_connected_devices() {
        let mut panel = Panel::new(laptop(), false);
        panel.apply(Event::Bluetooth(Bluetooth::On {
            devices: vec!["Headphones".to_string(), "Keyboard".to_string()],
        }));
        let view = panel.view().bluetooth;
        assert!(view.enabled && view.on);
        assert_eq!(
            view.detail,
            "\u{2068}Headphones\u{2069}, \u{2068}Keyboard\u{2069}"
        );
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

        let Some(Command::Mute {
            sequence,
            muted: true,
        }) = panel.toggle_mute()
        else {
            panic!("expected a mute command");
        };
        let view = panel.view().volume;
        assert!(view.muted);
        assert_eq!(view.label, "Muted");
        // A reading that was on its way when the button was clicked does
        // not unmute it; the one taken after does.
        panel.apply(Event::Volume {
            sequence: sequence - 1,
            volume: volume(130, false),
        });
        assert!(panel.view().volume.muted);
        panel.apply(Event::Volume {
            sequence,
            volume: volume(130, true),
        });
        assert!(panel.view().volume.muted);

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
    fn dnd_is_a_switch_showing_what_the_service_says() {
        let mut panel = Panel::new(laptop(), true);
        // Until it has answered there is nothing to press.
        assert_eq!(panel.view().dnd.control, DndControl::None);
        panel.apply(Event::Dnd(Some(true)));
        let view = panel.view().dnd;
        assert_eq!((view.control, view.state), (DndControl::Switch, Some(true)));
        // The switch moves at once and the service confirms.
        assert!(matches!(panel.set_dnd(false), Some(Command::Dnd(false))));
        assert_eq!(panel.view().dnd.state, Some(false));
        panel.apply(Event::Dnd(Some(false)));
        let view = panel.view().dnd;
        assert_eq!(
            (view.control, view.state),
            (DndControl::Switch, Some(false))
        );
    }

    #[test]
    fn dnd_claims_no_state_it_does_not_know() {
        let mut panel = Panel::new(laptop(), true);
        assert_eq!(panel.view().dnd.state, None);
        // A service that cannot say leaves it unknown, and gets buttons.
        panel.apply(Event::Dnd(None));
        assert_eq!(panel.view().dnd.state, None);
        assert_eq!(panel.view().dnd.control, DndControl::Buttons);
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
