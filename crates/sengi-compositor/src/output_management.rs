//! wlr-output-management-unstable-v1, version 4: lets a client such as
//! Settings or wlr-randr list the outputs and change them.
//!
//! Smithay does not ship this protocol, so it is implemented here. Every
//! head of `State::heads` is described to each client that bound the
//! manager, switched on or off; whenever something about them changes the
//! difference is sent and a new serial goes out with `done`. A client
//! changes things by making a configuration against a serial, naming every
//! head, and applying or testing it. All of that is checked and put in
//! place at once, in `State::apply_plan`, or refused: the answer is
//! `succeeded`, `failed`, or `cancelled` when the serial is out of date.
//! Nothing is changed before `apply`, so a client that goes away half way
//! through leaves nothing behind.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use smithay::{
    output::{Mode as OutputMode, Output},
    reexports::{
        wayland_protocols_wlr::output_management::v1::server::{
            zwlr_output_configuration_head_v1::{
                self, Error as HeadError, ZwlrOutputConfigurationHeadV1,
            },
            zwlr_output_configuration_v1::{self, Error as ConfigError, ZwlrOutputConfigurationV1},
            zwlr_output_head_v1::{AdaptiveSyncState, ZwlrOutputHeadV1},
            zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
            zwlr_output_mode_v1::ZwlrOutputModeV1,
        },
        wayland_server::{
            backend::GlobalId, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
            Resource, WEnum,
        },
    },
    utils::Transform,
};

use crate::{
    outputs::{scale_key, size_is_sane, HeadPlan},
    state::State,
};

/// The newest version, which is what Settings binds.
const VERSION: u32 = 4;
/// The newest version of the mode object.
const MODE_VERSION: u32 = 3;

/// What a client has been told about a head.
#[derive(Clone, PartialEq, Debug)]
struct Snap {
    enabled: bool,
    mode: Option<(i32, i32, i32)>,
    position: (i32, i32),
    transform: Transform,
    scale: u32,
}

struct SentHead {
    output: Output,
    resource: ZwlrOutputHeadV1,
    modes: Vec<(OutputMode, ZwlrOutputModeV1)>,
    sent: Snap,
}

struct Manager {
    resource: ZwlrOutputManagerV1,
    heads: Vec<SentHead>,
}

#[derive(Default)]
pub struct OutputManagementState {
    global: Option<GlobalId>,
    managers: Vec<Manager>,
    /// What every manager's last `done` carried.
    pub serial: u32,
    /// How things stood at that serial, to tell when it must go up.
    snapshot: Vec<(Output, Snap)>,
}

impl OutputManagementState {
    pub fn new(display: &DisplayHandle) -> Self {
        Self {
            global: Some(display.create_global::<State, ZwlrOutputManagerV1, ()>(VERSION, ())),
            ..Self::default()
        }
    }

    pub fn has_global(&self) -> bool {
        self.global.is_some()
    }
}

/// A configuration a client is building.
pub struct Config {
    serial: u32,
    heads: Vec<(Output, Option<Arc<Mutex<Change>>>)>,
    used: Arc<AtomicBool>,
}

/// The settings of one enabled head in a configuration. Unset ones leave
/// the head as it is.
pub struct Change {
    output: Output,
    used: Arc<AtomicBool>,
    mode: Option<OutputMode>,
    custom_mode: bool,
    position: Option<(i32, i32)>,
    transform: Option<Transform>,
    scale: Option<f64>,
    adaptive_sync: bool,
}

fn snap(plan: &HeadPlan) -> Snap {
    Snap {
        enabled: plan.enabled,
        mode: plan
            .enabled
            .then_some((plan.mode.size.w, plan.mode.size.h, plan.mode.refresh)),
        position: plan.position,
        transform: plan.transform,
        scale: scale_key(plan.scale),
    }
}

/// Describes `output` to the client of `manager`, from the start.
fn send_head(
    display: &DisplayHandle,
    manager: &ZwlrOutputManagerV1,
    state: &State,
    output: &Output,
    snap: &Snap,
) -> Option<SentHead> {
    let client = manager.client()?;
    let head = client
        .create_resource::<ZwlrOutputHeadV1, Output, State>(
            display,
            manager.version(),
            output.clone(),
        )
        .ok()?;
    manager.head(&head);
    head.name(output.name());
    head.description(output.description());
    let physical = output.physical_properties();
    if physical.size.w > 0 && physical.size.h > 0 {
        head.physical_size(physical.size.w, physical.size.h);
    }
    if head.version() >= 2 {
        if !physical.make.is_empty() {
            head.make(physical.make.clone());
        }
        if !physical.model.is_empty() {
            head.model(physical.model.clone());
        }
        if let Some(known) = state.head(output).filter(|known| !known.serial.is_empty()) {
            head.serial_number(known.serial.clone());
        }
    }
    let mut modes = Vec::new();
    // A head that takes any size has no modes to list.
    if state.head(output).is_some_and(|known| !known.any_size) {
        let preferred = output.preferred_mode();
        for mode in output.modes() {
            let version = manager.version().min(MODE_VERSION);
            let Ok(object) = client
                .create_resource::<ZwlrOutputModeV1, (Output, OutputMode), State>(
                    display,
                    version,
                    (output.clone(), mode),
                )
            else {
                continue;
            };
            head.mode(&object);
            object.size(mode.size.w, mode.size.h);
            if mode.refresh > 0 {
                object.refresh(mode.refresh);
            }
            if preferred == Some(mode) {
                object.preferred();
            }
            modes.push((mode, object));
        }
    }
    let mut sent = SentHead {
        output: output.clone(),
        resource: head,
        modes,
        sent: Snap {
            enabled: false,
            mode: None,
            position: (i32::MIN, i32::MIN),
            transform: Transform::Normal,
            scale: 0,
        },
    };
    // Whatever differs from "nothing" is the whole state.
    sent.send_changes(snap);
    if sent.resource.version() >= 4 {
        sent.resource.adaptive_sync(AdaptiveSyncState::Disabled);
    }
    Some(sent)
}

impl SentHead {
    /// Sends what `now` has that differs from what was sent. Returns
    /// whether anything was.
    fn send_changes(&mut self, now: &Snap) -> bool {
        if &self.sent == now {
            return false;
        }
        let head = &self.resource;
        let before = &self.sent;
        if before.enabled != now.enabled || before.position == (i32::MIN, i32::MIN) {
            head.enabled(i32::from(now.enabled));
        }
        if now.enabled {
            let fresh = !before.enabled || before.position == (i32::MIN, i32::MIN);
            if fresh || before.mode != now.mode {
                let current = now.mode.and_then(|(w, h, refresh)| {
                    self.modes.iter().find(|(mode, _)| {
                        mode.size.w == w && mode.size.h == h && mode.refresh == refresh
                    })
                });
                if let Some((_, object)) = current {
                    head.current_mode(object);
                }
            }
            if fresh || before.position != now.position {
                head.position(now.position.0, now.position.1);
            }
            if fresh || before.transform != now.transform {
                head.transform(now.transform.into());
            }
            if fresh || before.scale != now.scale {
                head.scale(f64::from(now.scale) / 1000.0);
            }
        }
        self.sent = now.clone();
        true
    }

    fn finish(self) {
        for (_, mode) in &self.modes {
            mode.finished();
        }
        self.resource.finished();
    }
}

impl State {
    /// The outputs changed in some way a client could see: tells every
    /// client that manages them, with a new serial.
    pub fn output_management_changed(&mut self) {
        if !self.output_management.has_global() {
            return;
        }
        let now: Vec<(Output, Snap)> = self
            .current_plan()
            .iter()
            .map(|plan| (plan.output.clone(), snap(plan)))
            .collect();
        let mut om = std::mem::take(&mut self.output_management);
        if om.snapshot != now {
            om.serial = om.serial.wrapping_add(1);
            om.snapshot.clone_from(&now);
        }
        let display = self.display_handle.clone();
        om.managers.retain(|manager| manager.resource.is_alive());
        for manager in &mut om.managers {
            let mut any = false;
            let (kept, gone): (Vec<SentHead>, Vec<SentHead>) = std::mem::take(&mut manager.heads)
                .into_iter()
                .filter(|sent| sent.resource.is_alive())
                .partition(|sent| now.iter().any(|(output, _)| output == &sent.output));
            manager.heads = kept;
            for sent in gone {
                sent.finish();
                any = true;
            }
            for (output, snap) in &now {
                match manager.heads.iter_mut().find(|sent| &sent.output == output) {
                    Some(sent) => any |= sent.send_changes(snap),
                    None => {
                        if let Some(sent) =
                            send_head(&display, &manager.resource, self, output, snap)
                        {
                            manager.heads.push(sent);
                            any = true;
                        }
                    }
                }
            }
            if any {
                manager.resource.done(om.serial);
            }
        }
        self.output_management = om;
    }
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for State {
    fn bind(
        state: &mut Self,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrOutputManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(resource, ());
        let mut heads = Vec::new();
        let plan = state.current_plan();
        for head in &plan {
            if heads.len() >= crate::outputs::MAX_HEADS {
                break;
            }
            if let Some(sent) = send_head(display, &manager, state, &head.output, &snap(head)) {
                heads.push(sent);
            }
        }
        manager.done(state.output_management.serial);
        state.output_management.managers.push(Manager {
            resource: manager,
            heads,
        });
    }

    fn can_view(client: Client, _: &()) -> bool {
        crate::security::trusted(&client)
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ZwlrOutputManagerV1,
        request: zwlr_output_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zwlr_output_manager_v1::Request::CreateConfiguration { id, serial } => {
                data_init.init(
                    id,
                    Mutex::new(Config {
                        serial,
                        heads: Vec::new(),
                        used: Arc::new(AtomicBool::new(false)),
                    }),
                );
            }
            zwlr_output_manager_v1::Request::Stop => {
                let om = &mut state.output_management;
                if let Some(at) = om.managers.iter().position(|m| &m.resource == manager) {
                    let gone = om.managers.remove(at);
                    for head in gone.heads {
                        head.finish();
                    }
                }
                manager.finished();
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputHeadV1, Output> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _head: &ZwlrOutputHeadV1,
        _request: <ZwlrOutputHeadV1 as Resource>::Request,
        _data: &Output,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Release: the object is destroyed, and is found dead at the next
        // change.
    }
}

impl Dispatch<ZwlrOutputModeV1, (Output, OutputMode)> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _mode: &ZwlrOutputModeV1,
        _request: <ZwlrOutputModeV1 as Resource>::Request,
        _data: &(Output, OutputMode),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, Mutex<Config>> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationV1,
        request: zwlr_output_configuration_v1::Request,
        data: &Mutex<Config>,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let Ok(mut config) = data.lock() else { return };
        match request {
            zwlr_output_configuration_v1::Request::EnableHead { id, head } => {
                // The new object is made first, whatever is wrong with the
                // request: one left without data panics the server if the
                // client ever sends to it.
                let output = head_output(&head);
                let change = Arc::new(Mutex::new(Change {
                    output: output
                        .clone()
                        .unwrap_or_else(|| head_output_of_nothing(&config)),
                    used: config.used.clone(),
                    mode: None,
                    custom_mode: false,
                    position: None,
                    transform: None,
                    scale: None,
                    adaptive_sync: false,
                }));
                data_init.init(id, change.clone());
                let Some(output) = output else {
                    return;
                };
                if config.used.load(Ordering::SeqCst) {
                    resource.post_error(ConfigError::AlreadyUsed, "this configuration was used");
                    return;
                }
                if config.heads.iter().any(|(known, _)| known == &output) {
                    resource.post_error(
                        ConfigError::AlreadyConfiguredHead,
                        "the head was configured twice",
                    );
                    return;
                }
                config.heads.push((output, Some(change)));
            }
            zwlr_output_configuration_v1::Request::DisableHead { head } => {
                if config.used.load(Ordering::SeqCst) {
                    resource.post_error(ConfigError::AlreadyUsed, "this configuration was used");
                    return;
                }
                let Some(output) = head_output(&head) else {
                    return;
                };
                if config.heads.iter().any(|(known, _)| known == &output) {
                    resource.post_error(
                        ConfigError::AlreadyConfiguredHead,
                        "the head was configured twice",
                    );
                    return;
                }
                config.heads.push((output, None));
            }
            zwlr_output_configuration_v1::Request::Apply => {
                finish(state, resource, &mut config, true);
            }
            zwlr_output_configuration_v1::Request::Test => {
                finish(state, resource, &mut config, false);
            }
            _ => {}
        }
    }
}

/// Stands in for the output of a head that has gone, in the settings of
/// a head that is refused anyway.
fn head_output_of_nothing(_config: &Config) -> Output {
    Output::new(
        String::new(),
        smithay::output::PhysicalProperties {
            size: (0, 0).into(),
            subpixel: smithay::output::Subpixel::Unknown,
            make: String::new(),
            model: String::new(),
        },
    )
}

/// The output a head object stands for, if it is still one.
fn head_output(head: &ZwlrOutputHeadV1) -> Option<Output> {
    head.data::<Output>().cloned()
}

/// `apply` or `test`: the configuration is complete, so it is answered.
fn finish(
    state: &mut State,
    resource: &ZwlrOutputConfigurationV1,
    config: &mut Config,
    apply: bool,
) {
    if config.used.swap(true, Ordering::SeqCst) {
        resource.post_error(ConfigError::AlreadyUsed, "this configuration was used");
        return;
    }
    // Made against what is no longer so: a head came or went, or somebody
    // else changed things since. The client asks again.
    if config.serial != state.output_management.serial {
        resource.cancelled();
        return;
    }
    let known: Vec<Output> = state
        .heads()
        .iter()
        .map(|head| head.output.clone())
        .collect();
    if config
        .heads
        .iter()
        .any(|(output, _)| !known.contains(output))
    {
        resource.cancelled();
        return;
    }
    if known
        .iter()
        .any(|output| !config.heads.iter().any(|(named, _)| named == output))
    {
        resource.post_error(
            ConfigError::UnconfiguredHead,
            "a head was left out of the configuration",
        );
        return;
    }

    let mut plan = state.current_plan();
    let mut wants_sync = false;
    for entry in &mut plan {
        let Some((_, change)) = config
            .heads
            .iter()
            .find(|(output, _)| output == &entry.output)
        else {
            continue;
        };
        let Some(change) = change else {
            entry.enabled = false;
            continue;
        };
        let Ok(change) = change.lock() else {
            resource.failed();
            return;
        };
        entry.enabled = true;
        if let Some(mode) = change.mode {
            entry.mode = mode;
        }
        if let Some(position) = change.position {
            entry.position = position;
            entry.position_set = true;
        }
        if let Some(transform) = change.transform {
            entry.transform = transform;
        }
        if let Some(scale) = change.scale {
            entry.scale = scale;
        }
        wants_sync |= change.adaptive_sync;
    }
    if wants_sync {
        crate::say!("outputs: adaptive sync is not supported");
        resource.failed();
        return;
    }
    let result = if apply {
        state.apply_plan(plan)
    } else {
        state.test_plan(plan)
    };
    match result {
        Ok(()) => {
            if apply {
                state.output_layout_set_by_client();
            }
            resource.succeeded();
        }
        Err(why) => {
            crate::say!("outputs: {why}");
            resource.failed();
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, Arc<Mutex<Change>>> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &ZwlrOutputConfigurationHeadV1,
        request: zwlr_output_configuration_head_v1::Request,
        data: &Arc<Mutex<Change>>,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_output_configuration_head_v1::Request;
        let Ok(mut change) = data.lock() else { return };
        if change.used.load(Ordering::SeqCst) {
            resource.post_error(HeadError::AlreadySet, "the configuration was used already");
            return;
        }
        let already = |set: bool| {
            if set {
                resource.post_error(HeadError::AlreadySet, "the property was set already");
            }
            set
        };
        match request {
            Request::SetMode { mode } => {
                let Some((output, mode)) = mode.data::<(Output, OutputMode)>() else {
                    return;
                };
                if already(change.mode.is_some()) {
                    return;
                }
                if *output != change.output {
                    resource.post_error(HeadError::InvalidMode, "that mode is of another head");
                    return;
                }
                change.mode = Some(*mode);
            }
            Request::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                if already(change.mode.is_some()) {
                    return;
                }
                if !size_is_sane(width, height) || !(0..=1_000_000).contains(&refresh) {
                    resource.post_error(HeadError::InvalidCustomMode, "that is no mode");
                    return;
                }
                // No refresh named: the head keeps its own.
                let refresh = if refresh > 0 {
                    refresh
                } else {
                    change.output.current_mode().map_or(0, |mode| mode.refresh)
                };
                change.mode = Some(OutputMode {
                    size: (width, height).into(),
                    refresh,
                });
                change.custom_mode = true;
            }
            Request::SetPosition { x, y } => {
                if !already(change.position.is_some()) {
                    change.position = Some((x, y));
                }
            }
            Request::SetTransform { transform } => {
                if already(change.transform.is_some()) {
                    return;
                }
                match transform {
                    WEnum::Value(value) => change.transform = Some(Transform::from(value)),
                    WEnum::Unknown(_) => {
                        resource.post_error(HeadError::InvalidTransform, "no such transform");
                    }
                }
            }
            Request::SetScale { scale } => {
                if already(change.scale.is_some()) {
                    return;
                }
                if scale.is_finite() && scale > 0.0 {
                    change.scale = Some(scale);
                } else {
                    resource.post_error(HeadError::InvalidScale, "a scale is above zero");
                }
            }
            Request::SetAdaptiveSync { state } => match state {
                WEnum::Value(AdaptiveSyncState::Enabled) => change.adaptive_sync = true,
                WEnum::Value(AdaptiveSyncState::Disabled) => {}
                _ => resource.post_error(
                    HeadError::InvalidAdaptiveSyncState,
                    "adaptive sync is on or off",
                ),
            },
            _ => {}
        }
    }
}
