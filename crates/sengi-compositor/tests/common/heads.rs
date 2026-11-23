//! The test client's side of wlr-output-management: the heads it was told
//! of, configurations it builds and sends, and what each answered. Also
//! the `wl_output` of every output by name, which says what the compositor
//! tells ordinary clients.
#![allow(dead_code)]

use std::time::Duration;

use wayland_client::{
    backend::ObjectId,
    delegate_noop, event_created_child,
    protocol::wl_output::{self, WlOutput},
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

use super::{Client, Failure, Seen};

const SOON: Duration = Duration::from_secs(5);

pub struct ModeInfo {
    pub proxy: ZwlrOutputModeV1,
    pub size: (i32, i32),
    pub refresh: i32,
    pub preferred: bool,
}

pub struct HeadInfo {
    pub proxy: ZwlrOutputHeadV1,
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub physical: Option<(i32, i32)>,
    pub modes: Vec<ModeInfo>,
    pub enabled: bool,
    pub current: Option<ObjectId>,
    pub position: (i32, i32),
    pub transform: Option<wl_output::Transform>,
    pub scale: f64,
    pub adaptive_sync: Option<u32>,
}

impl HeadInfo {
    /// The size of the mode in use, if the head lists modes.
    pub fn current_size(&self) -> Option<(i32, i32, i32)> {
        let id = self.current.as_ref()?;
        self.modes
            .iter()
            .find(|mode| mode.proxy.id() == *id)
            .map(|mode| (mode.size.0, mode.size.1, mode.refresh))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
}

/// What `wl_output` said of one output.
#[derive(Default, Clone, Debug)]
pub struct Plain {
    pub name: String,
    pub size: Option<(i32, i32)>,
    pub scale: i32,
    pub position: (i32, i32),
}

#[derive(Default)]
pub struct HeadsSeen {
    pub heads: Vec<HeadInfo>,
    pub serial: Option<u32>,
    pub dones: u32,
    pub outcomes: Vec<Outcome>,
    pub manager_finished: bool,
    pub plain: Vec<(WlOutput, Plain)>,
}

/// What to do with one head in a configuration.
#[derive(Default, Clone)]
pub struct Op {
    pub name: String,
    pub off: bool,
    pub mode: Option<(i32, i32, i32)>,
    pub custom: Option<(i32, i32, i32)>,
    pub position: Option<(i32, i32)>,
    pub scale: Option<f64>,
    pub transform: Option<wl_output::Transform>,
    pub sync: Option<bool>,
}

impl Op {
    /// The head, as it is.
    pub fn keep(name: &str) -> Op {
        Op {
            name: name.into(),
            ..Op::default()
        }
    }

    pub fn off(name: &str) -> Op {
        Op {
            name: name.into(),
            off: true,
            ..Op::default()
        }
    }

    pub fn scale(mut self, scale: f64) -> Op {
        self.scale = Some(scale);
        self
    }

    pub fn at(mut self, x: i32, y: i32) -> Op {
        self.position = Some((x, y));
        self
    }

    pub fn mode(mut self, w: i32, h: i32, refresh: i32) -> Op {
        self.mode = Some((w, h, refresh));
        self
    }

    pub fn custom(mut self, w: i32, h: i32, refresh: i32) -> Op {
        self.custom = Some((w, h, refresh));
        self
    }

    pub fn transform(mut self, transform: wl_output::Transform) -> Op {
        self.transform = Some(transform);
        self
    }
}

impl Client {
    /// Binds the output manager, as Settings does, and waits for the heads.
    pub fn manager(&mut self) -> Result<ZwlrOutputManagerV1, Failure> {
        let manager: ZwlrOutputManagerV1 = self.globals.bind(&self.handle, 1..=4, ())?;
        for _ in 0..3 {
            self.roundtrip()?;
        }
        Ok(manager)
    }

    /// Reads what the compositor has said, without asking for more.
    pub fn settle(&mut self) -> Result<(), Failure> {
        for _ in 0..2 {
            self.roundtrip()?;
        }
        Ok(())
    }

    pub fn head(&self, name: &str) -> &HeadInfo {
        self.seen
            .heads
            .heads
            .iter()
            .find(|head| head.name == name)
            .unwrap_or_else(|| panic!("no head {name}"))
    }

    pub fn head_names(&self) -> Vec<String> {
        self.seen
            .heads
            .heads
            .iter()
            .map(|head| head.name.clone())
            .collect()
    }

    /// Makes, sends and answers a configuration against `serial` (the
    /// latest one if `None`). `ops` name each head; with `apply` false it
    /// is only tested.
    pub fn configure(
        &mut self,
        manager: &ZwlrOutputManagerV1,
        serial: Option<u32>,
        ops: &[Op],
        apply: bool,
    ) -> Result<Outcome, Failure> {
        let serial = serial.or(self.seen.heads.serial).ok_or("no serial yet")?;
        let configuration = manager.create_configuration(serial, &self.handle, ());
        self.send_ops(&configuration, ops);
        let before = self.seen.heads.outcomes.len();
        if apply {
            configuration.apply();
        } else {
            configuration.test();
        }
        let waited = self.wait(SOON, |seen| seen.heads.outcomes.len() > before);
        if !waited {
            return Err("the compositor never answered the configuration".into());
        }
        let outcome = *self.seen.heads.outcomes.last().expect("one was seen");
        configuration.destroy();
        self.settle()?;
        Ok(outcome)
    }

    /// Builds a configuration and leaves it unsent.
    pub fn half_configuration(
        &mut self,
        manager: &ZwlrOutputManagerV1,
        ops: &[Op],
    ) -> Result<ZwlrOutputConfigurationV1, Failure> {
        let serial = self.seen.heads.serial.ok_or("no serial yet")?;
        let configuration = manager.create_configuration(serial, &self.handle, ());
        self.send_ops(&configuration, ops);
        self.roundtrip()?;
        Ok(configuration)
    }

    pub fn send_ops(&mut self, configuration: &ZwlrOutputConfigurationV1, ops: &[Op]) {
        for op in ops {
            let head = self.head(&op.name).proxy.clone();
            if op.off {
                configuration.disable_head(&head);
                continue;
            }
            let settings: ZwlrOutputConfigurationHeadV1 =
                configuration.enable_head(&head, &self.handle, ());
            if let Some((w, h, refresh)) = op.mode {
                let mode = self
                    .head(&op.name)
                    .modes
                    .iter()
                    .find(|mode| mode.size == (w, h) && mode.refresh == refresh)
                    .unwrap_or_else(|| panic!("{} has no {w}x{h}@{refresh}", op.name))
                    .proxy
                    .clone();
                settings.set_mode(&mode);
            }
            if let Some((w, h, refresh)) = op.custom {
                settings.set_custom_mode(w, h, refresh);
            }
            if let Some((x, y)) = op.position {
                settings.set_position(x, y);
            }
            if let Some(scale) = op.scale {
                settings.set_scale(scale);
            }
            if let Some(transform) = op.transform {
                settings.set_transform(transform);
            }
            if let Some(sync) = op.sync {
                use wayland_protocols_wlr::output_management::v1::client::zwlr_output_head_v1::AdaptiveSyncState;
                settings.set_adaptive_sync(if sync {
                    AdaptiveSyncState::Enabled
                } else {
                    AdaptiveSyncState::Disabled
                });
            }
        }
    }

    /// Every output as `wl_output` shows it, by name.
    pub fn plain_outputs(&mut self) -> Result<Vec<Plain>, Failure> {
        // What went away since is heard of first.
        self.settle()?;
        self.seen.heads.plain.clear();
        let names: Vec<u32> = self.globals.contents().with_list(|list| {
            list.iter()
                .filter(|global| global.interface == "wl_output")
                .map(|global| global.name)
                .collect()
        });
        for (index, name) in names.into_iter().enumerate() {
            let proxy: WlOutput = self.globals.registry().bind(name, 4, &self.handle, index);
            self.seen.heads.plain.push((proxy, Plain::default()));
        }
        self.settle()?;
        Ok(self
            .seen
            .heads
            .plain
            .iter()
            .map(|(_, plain)| plain.clone())
            .collect())
    }

    /// A picture of the output called `name`.
    pub fn capture_named(&mut self, name: &str) -> Result<super::apps::Picture, Failure> {
        let output = self.output_named(name)?;
        self.capture_of(&output, 0)
    }

    /// The `wl_output` object of the output called `name`.
    pub fn output_named(&mut self, name: &str) -> Result<WlOutput, Failure> {
        self.plain_outputs()?;
        self.seen
            .heads
            .plain
            .iter()
            .find(|(_, plain)| plain.name == name)
            .map(|(proxy, _)| proxy.clone())
            .ok_or_else(|| format!("no wl_output named {name}").into())
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => seen.heads.heads.push(HeadInfo {
                proxy: head,
                name: String::new(),
                description: String::new(),
                make: String::new(),
                model: String::new(),
                serial: String::new(),
                physical: None,
                modes: Vec::new(),
                enabled: false,
                current: None,
                position: (0, 0),
                transform: None,
                scale: 1.0,
                adaptive_sync: None,
            }),
            zwlr_output_manager_v1::Event::Done { serial } => {
                seen.heads.serial = Some(serial);
                seen.heads.dones += 1;
            }
            zwlr_output_manager_v1::Event::Finished => seen.heads.manager_finished = true,
            _ => {}
        }
    }

    event_created_child!(Seen, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        proxy: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_output_head_v1::Event::Finished = event {
            seen.heads.heads.retain(|head| head.proxy != *proxy);
            return;
        }
        let Some(head) = seen
            .heads
            .heads
            .iter_mut()
            .find(|head| head.proxy == *proxy)
        else {
            return;
        };
        match event {
            zwlr_output_head_v1::Event::Name { name } => head.name = name,
            zwlr_output_head_v1::Event::Description { description } => {
                head.description = description;
            }
            zwlr_output_head_v1::Event::PhysicalSize { width, height } => {
                head.physical = Some((width, height));
            }
            zwlr_output_head_v1::Event::Mode { mode } => head.modes.push(ModeInfo {
                proxy: mode,
                size: (0, 0),
                refresh: 0,
                preferred: false,
            }),
            zwlr_output_head_v1::Event::Enabled { enabled } => head.enabled = enabled != 0,
            zwlr_output_head_v1::Event::CurrentMode { mode } => head.current = Some(mode.id()),
            zwlr_output_head_v1::Event::Position { x, y } => head.position = (x, y),
            zwlr_output_head_v1::Event::Transform { transform } => {
                head.transform = transform.into_result().ok();
            }
            zwlr_output_head_v1::Event::Scale { scale } => head.scale = scale,
            zwlr_output_head_v1::Event::Make { make } => head.make = make,
            zwlr_output_head_v1::Event::Model { model } => head.model = model,
            zwlr_output_head_v1::Event::SerialNumber { serial_number } => {
                head.serial = serial_number;
            }
            zwlr_output_head_v1::Event::AdaptiveSync { state } => {
                head.adaptive_sync = match state {
                    WEnum::Value(value) => Some(value as u32),
                    WEnum::Unknown(raw) => Some(raw),
                };
            }
            _ => {}
        }
    }

    event_created_child!(Seen, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        proxy: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut modes = seen
            .heads
            .heads
            .iter_mut()
            .flat_map(|head| head.modes.iter_mut());
        let Some(mode) = modes.find(|mode| mode.proxy == *proxy) else {
            return;
        };
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => mode.size = (width, height),
            zwlr_output_mode_v1::Event::Refresh { refresh } => mode.refresh = refresh,
            zwlr_output_mode_v1::Event::Preferred => mode.preferred = true,
            zwlr_output_mode_v1::Event::Finished => {
                for head in &mut seen.heads.heads {
                    head.modes.retain(|mode| mode.proxy != *proxy);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        seen.heads.outcomes.push(match event {
            zwlr_output_configuration_v1::Event::Succeeded => Outcome::Succeeded,
            zwlr_output_configuration_v1::Event::Failed => Outcome::Failed,
            zwlr_output_configuration_v1::Event::Cancelled => Outcome::Cancelled,
            _ => return,
        });
    }
}

impl Dispatch<WlOutput, usize> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some((_, plain)) = seen.heads.plain.get_mut(*index) else {
            return;
        };
        match event {
            wl_output::Event::Name { name } => plain.name = name,
            wl_output::Event::Geometry { x, y, .. } => plain.position = (x, y),
            wl_output::Event::Mode {
                flags,
                width,
                height,
                ..
            } => {
                let current = flags
                    .into_result()
                    .is_ok_and(|flags| flags.contains(wl_output::Mode::Current));
                if current {
                    plain.size = Some((width, height));
                }
            }
            wl_output::Event::Scale { factor } => plain.scale = factor,
            _ => {}
        }
    }
}

delegate_noop!(Seen: ZwlrOutputConfigurationHeadV1);

/// Reads a screenshot the compositor saved: 8-bit RGB, deflate stored
/// blocks, as `src/png.rs` writes them. Gives the width, the height and
/// the pixels as 0xRRGGBB.
pub fn read_png(path: &std::path::Path) -> (u32, u32, Vec<u32>) {
    let data = std::fs::read(path).expect("a screenshot");
    let be = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
    let (width, height) = (be(16), be(20));
    let mut at = 8;
    let mut zlib = Vec::new();
    while at + 8 <= data.len() {
        let len = be(at) as usize;
        if &data[at + 4..at + 8] == b"IDAT" {
            zlib.extend_from_slice(&data[at + 8..at + 8 + len]);
        }
        at += 12 + len;
    }
    let mut raw = Vec::new();
    let mut at = 2;
    loop {
        let last = zlib[at] & 1 == 1;
        let len = u16::from_le_bytes([zlib[at + 1], zlib[at + 2]]) as usize;
        raw.extend_from_slice(&zlib[at + 5..at + 5 + len]);
        at += 5 + len;
        if last {
            break;
        }
    }
    let row = width as usize * 3 + 1;
    let pixels = (0..height as usize)
        .flat_map(|y| {
            let line = &raw[y * row + 1..(y + 1) * row];
            (0..width as usize)
                .map(|x| {
                    let p = &line[x * 3..x * 3 + 3];
                    u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2])
                })
                .collect::<Vec<_>>()
        })
        .collect();
    (width, height, pixels)
}
