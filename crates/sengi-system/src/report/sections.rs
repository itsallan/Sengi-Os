//! What the report says about the machine. Each function reads one subject
//! and writes it as plain lines; nothing here needs to worry about names
//! and addresses, which are taken out of everything afterwards.
//!
//! Serial numbers are not read at all where they have files of their own,
//! and neither is a display's EDID, which carries one.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::command::{self, Ran};
use super::redact::plain;
use super::{
    read, Out, Radios, Sources, COMMAND_LIMIT, IWD_DEVICE, IWD_KNOWN, IWD_NETWORK, IWD_STATION,
};
use crate::bus::{self, Listing, Object};
use crate::commands;

/// The most of a command's output that is kept.
const MAX_OUTPUT: usize = 512 * 1024;
/// The most entries taken from any one directory.
const MAX_ENTRIES: usize = 256;

/// The names in a directory, sorted.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.truncate(MAX_ENTRIES);
    names
}

/// What a link such as `device/driver` points at, by its last part.
fn link_name(path: &Path) -> Option<String> {
    let target = fs::read_link(path).ok()?;
    Some(target.file_name()?.to_string_lossy().into_owned())
}

/// The files of `dir` among `names` that can be read, as `name value`.
fn listed(dir: &Path, names: &[&str]) -> String {
    names
        .iter()
        .filter_map(|name| Some(format!("{name} {}", read(&dir.join(name))?)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let megabytes = bytes as f64 / MB;
    if megabytes >= 1024.0 {
        format!("{:.1} GB", megabytes / 1024.0)
    } else {
        format!("{megabytes:.0} MB")
    }
}

/// Whether a program's complaint is that it was not allowed.
pub(crate) fn refused(error: &str) -> bool {
    [
        "Permission denied",
        "Operation not permitted",
        "insufficient permissions",
        "Access denied",
        "not authorized",
    ]
    .iter()
    .any(|words| error.contains(words))
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("it did not say why")
}

/// Run a command and return what it printed. When there is nothing to
/// return, why is written down as something that could not be read.
pub(crate) fn command(
    sources: &Sources,
    out: &mut Out,
    what: &str,
    program: &str,
    args: &[&str],
) -> Option<String> {
    let why = match command::run(&sources.path, program, args, COMMAND_LIMIT, MAX_OUTPUT) {
        Ran::Done {
            code: Some(0), out, ..
        } => return Some(out),
        Ran::Done {
            code,
            out: printed,
            err,
        } => {
            if refused(&err) {
                format!("refused: {}", first_line(&err))
            } else if !printed.trim().is_empty() {
                // Some say what there is and then end unhappily.
                return Some(printed);
            } else {
                let code = code.map_or("a signal".to_string(), |c| format!("status {c}"));
                // What stopped it is usually the last thing it said.
                let said = err.lines().rev().map(str::trim).find(|l| !l.is_empty());
                format!(
                    "{program} ended with {code}: {}",
                    said.unwrap_or("it did not say why")
                )
            }
        }
        Ran::Missing => format!("{program} is not installed"),
        Ran::NotStarted(error) => format!("{program} could not be started: {error}"),
        Ran::TimedOut { out: printed } => {
            if !printed.trim().is_empty() {
                out.line(printed.trim_end());
            }
            format!(
                "{program} did not finish within {} s",
                COMMAND_LIMIT.as_secs()
            )
        }
    };
    out.unread(what, &why);
    None
}

pub fn system(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let release = read(&sources.etc.join("os-release")).unwrap_or_default();
    let name = release.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?;
        Some(value.trim_matches(['"', '\'']).to_string())
    });
    out.fact("Operating system", name);
    let cmdline = read(&sources.proc.join("cmdline"));
    let live = cmdline
        .as_deref()
        .map(|line| line.split_whitespace().any(|word| word == "boot=sengi"));
    out.fact(
        "Running",
        live.map(|live| {
            if live {
                "the live system, from the install medium".to_string()
            } else {
                "an installed system".to_string()
            }
        }),
    );
    let dmi = sources.sys.join("class/dmi/id");
    for (label, files) in [
        ("Computer", &["sys_vendor", "product_name"][..]),
        ("Board", &["board_vendor", "board_name"][..]),
        (
            "Firmware",
            &["bios_vendor", "bios_version", "bios_date"][..],
        ),
    ] {
        let words: Vec<String> = files
            .iter()
            .filter_map(|file| read(&dmi.join(file)))
            .collect();
        out.fact(label, (!words.is_empty()).then(|| words.join(" ")));
    }
    let efi = sources.sys.join("firmware/efi");
    out.fact(
        "Started through",
        Some(if efi.exists() { "UEFI" } else { "BIOS" }.to_string()),
    );
    if efi.exists() {
        // Four bytes of attributes, then the value.
        let secure = fs::read(efi.join("efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c"))
            .ok()
            .and_then(|bytes| bytes.get(4).copied());
        out.fact(
            "Secure boot",
            secure.map(|value| if value == 1 { "on" } else { "off" }.to_string()),
        );
    }
    out.fact("Architecture", Some(std::env::consts::ARCH.to_string()));
    out.fact("Kernel", read(&sources.proc.join("version")));
    out.fact("Kernel command line", cmdline);
    out.fact(
        "Kernel taint",
        read(&sources.proc.join("sys/kernel/tainted")),
    );
    let up = read(&sources.proc.join("uptime"))
        .and_then(|text| text.split('.').next()?.parse::<u64>().ok());
    out.fact(
        "Running for",
        up.map(|seconds| format!("{} h {} min", seconds / 3600, seconds / 60 % 60)),
    );
    out
}

/// The programs of this user that are running, by the name the kernel keeps
/// of each, which is its first fifteen letters.
fn running(proc: &Path) -> Vec<(String, u32)> {
    // SAFETY: getuid cannot fail and touches nothing.
    let own = unsafe { libc::getuid() };
    fs::read_dir(proc)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            if entry.metadata().ok()?.uid() != own {
                return None;
            }
            Some((read(&entry.path().join("comm"))?, pid))
        })
        .take(4096)
        .collect()
}

pub fn desktop(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let running = running(&sources.proc);
    let found = |name: &str| {
        let kept: String = name.chars().take(15).collect();
        running.iter().find(|(comm, _)| *comm == kept).map(|p| p.1)
    };
    let compositors: Vec<(&str, u32)> = ["sengi-compositor", "labwc"]
        .into_iter()
        .filter_map(|name| Some((name, found(name)?)))
        .collect();
    let names: Vec<&str> = compositors.iter().map(|c| c.0).collect();
    out.fact(
        "Compositor in use",
        Some(if names.is_empty() {
            "none found for this user".to_string()
        } else {
            names.join(", ")
        }),
    );
    if sources.run.join("sengi-os/compositor-failed").exists() {
        out.line("sengi-compositor failed earlier since start-up, so labwc was asked for");
    }
    for (from, path) in [
        ("for every start", sources.etc.join("sengi-os/compositor")),
        ("until restart", sources.run.join("sengi-os.compositor")),
    ] {
        if let Some(word) = read(&path) {
            out.line(format!("Compositor chosen {from}: {word}"));
        }
    }
    if let Some(&(_, pid)) = compositors.first() {
        // Only the one variable: the rest of an environment is private.
        let environment = fs::read(sources.proc.join(pid.to_string()).join("environ"));
        let renderer = environment.ok().and_then(|bytes| {
            bytes.split(|&b| b == 0).find_map(|pair| {
                let value = pair.strip_prefix(b"WLR_RENDERER=")?;
                Some(String::from_utf8_lossy(value).into_owned())
            })
        });
        out.fact(
            "Renderer asked for",
            Some(renderer.unwrap_or_else(|| "none, the compositor chooses".to_string())),
        );
    }
    for name in [
        "sengi-bar",
        "sengi-dock",
        "sengi-desktop",
        "sengi-focus",
        "sengi-notify",
        "swaybg",
        "pipewire",
        "wireplumber",
    ] {
        let state = if found(name).is_some() {
            "running"
        } else {
            "not running"
        };
        out.line(format!("{name}: {state}"));
    }
    out
}

pub fn memory(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let cpuinfo = read(&sources.proc.join("cpuinfo")).unwrap_or_default();
    let field = |name: &str| {
        cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == name).then(|| value.trim().to_string())
        })
    };
    out.fact("Processor", field("model name"));
    out.fact("Cores", field("cpu cores"));
    let threads = cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count();
    out.fact("Threads", (threads > 0).then(|| threads.to_string()));
    out.fact("Microcode", field("microcode"));
    out.fact("Load", read(&sources.proc.join("loadavg")));
    let meminfo = read(&sources.proc.join("meminfo")).unwrap_or_default();
    let kib = |name: &str| {
        meminfo.lines().find_map(|line| {
            let rest = line.strip_prefix(name)?.strip_prefix(':')?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
    };
    for (label, key) in [
        ("Memory", "MemTotal"),
        ("Memory available", "MemAvailable"),
        ("Swap", "SwapTotal"),
        ("Swap free", "SwapFree"),
    ] {
        out.fact(label, kib(key).map(|kb| size(kb.saturating_mul(1024))));
    }
    out.line("Swap areas:");
    out.line(read(&sources.proc.join("swaps")).unwrap_or_else(|| "none".to_string()));
    let block = sources.sys.join("block");
    for name in entries(&block).iter().filter(|n| n.starts_with("zram")) {
        let dir = block.join(name);
        let number = |text: Option<&str>| text.and_then(|t| t.parse::<u64>().ok());
        let disksize = number(read(&dir.join("disksize")).as_deref()).unwrap_or(0);
        let stats = read(&dir.join("mm_stat")).unwrap_or_default();
        let mut stats = stats.split_whitespace();
        let stored = number(stats.next()).unwrap_or(0);
        let compressed = number(stats.next()).unwrap_or(0);
        out.line(format!(
            "{name}: size {}, holds {} in {}, algorithm {}",
            size(disksize),
            size(stored),
            size(compressed),
            read(&dir.join("comp_algorithm")).unwrap_or_default()
        ));
    }
    out
}

/// A field of `/proc/mounts`, where a space is written `\040`.
fn mount_field(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", " ")
        .replace("\\134", "\\")
}

/// Size and free space of the file system at `path`.
// The fields are 64 bits wide here and narrower on other machines.
#[allow(clippy::useless_conversion)]
fn space(path: &str) -> Option<(u64, u64)> {
    let path = std::ffi::CString::new(path).ok()?;
    // SAFETY: statvfs is given a valid C string and a zeroed structure of
    // its own type to fill, which is only read if the call succeeded.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        stat
    };
    let unit = u64::from(stat.f_frsize);
    Some((
        u64::from(stat.f_blocks).saturating_mul(unit),
        u64::from(stat.f_bavail).saturating_mul(unit),
    ))
}

pub fn disks(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let block = sources.sys.join("block");
    let sectors = |path: &Path| {
        read(&path.join("size"))
            .and_then(|text| text.parse::<u64>().ok())
            .map_or_else(
                || "size unknown".to_string(),
                |n| size(n.saturating_mul(512)),
            )
    };
    for name in entries(&block) {
        if name.starts_with("loop") || name.starts_with("ram") || name.starts_with("zram") {
            continue;
        }
        let dir = block.join(&name);
        let mut facts = vec![sectors(&dir)];
        // Where the kernel keeps it says how it is connected.
        let place = fs::read_link(&dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        for (part, words) in [
            ("/usb", "USB"),
            ("/nvme", "NVMe"),
            ("/ata", "SATA"),
            ("/mmc", "memory card"),
            ("/virtio", "virtual"),
        ] {
            if place.contains(part) {
                facts.push(words.to_string());
                break;
            }
        }
        match read(&dir.join("queue/rotational")).as_deref() {
            Some("0") => facts.push("no spinning parts".to_string()),
            Some("1") => facts.push("spinning".to_string()),
            _ => {}
        }
        if read(&dir.join("removable")).as_deref() == Some("1") {
            facts.push("removable".to_string());
        }
        if read(&dir.join("ro")).as_deref() == Some("1") {
            facts.push("read only".to_string());
        }
        facts.extend(read(&dir.join("device/model")));
        out.line(format!("{name}: {}", facts.join(", ")));
        for part in entries(&dir) {
            if dir.join(&part).join("partition").exists() {
                out.line(format!("  {part}: {}", sectors(&dir.join(&part))));
            }
        }
    }
    out.line("");
    out.line("File systems in use:");
    let mounts = read(&sources.proc.join("mounts")).unwrap_or_default();
    let mut seen = Vec::new();
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let (Some(device), Some(place), Some(kind), Some(options)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let place = mount_field(place);
        // Asking a server that has gone how full it is can hang, and its
        // address is nobody's business.
        let remote = matches!(kind, "nfs" | "nfs4" | "cifs" | "smb3" | "9p" | "sshfs")
            || (kind.starts_with("fuse.") && !device.starts_with("/dev/"));
        let wanted = remote || device.starts_with("/dev/") || place == "/";
        if !wanted || seen.contains(&place) || seen.len() >= 64 {
            continue;
        }
        let mode = options.split(',').next().unwrap_or_default();
        if remote {
            out.line(format!(
                "a network or user file system ({kind}, {mode}), not measured"
            ));
        } else {
            let room = space(&place).map_or_else(
                || "space unknown".to_string(),
                |(total, free)| format!("{} free of {}", size(free), size(total)),
            );
            let device = if device.starts_with("/dev/") {
                device
            } else {
                "memory"
            };
            out.line(format!("{place}: {device}, {kind}, {mode}, {room}"));
        }
        seen.push(place);
    }
    out
}

/// The kind of a PCI function, from the top half of its class number.
fn pci_class(class: &str) -> &'static str {
    match class.get(..4).unwrap_or_default() {
        "0100" | "0101" | "0104" | "0106" => "storage",
        "0108" => "NVMe storage",
        "0200" => "Ethernet",
        "0280" => "network, usually Wi-Fi",
        "0300" => "graphics",
        "0302" => "3D graphics",
        "0380" => "display",
        "0401" => "sound",
        "0403" => "sound",
        "0480" => "multimedia",
        "0500" | "0580" => "memory",
        "0600" => "host bridge",
        "0601" => "ISA bridge",
        "0604" => "PCI bridge",
        "0680" => "bridge",
        "0700" | "0780" => "communication",
        "0805" => "card reader",
        "0880" => "system",
        "0c03" => "USB controller",
        "0c05" => "SMBus",
        "0c80" => "serial bus",
        "1180" => "signal processing",
        "ff00" => "other",
        _ => "other",
    }
}

/// The makers a laptop's parts usually have; the image has no PCI database.
fn maker(vendor: &str) -> &'static str {
    match vendor {
        "8086" => " Intel",
        "10de" => " NVIDIA",
        "1002" | "1022" => " AMD",
        "10ec" => " Realtek",
        "168c" | "17cb" => " Qualcomm Atheros",
        "14e4" => " Broadcom",
        "14c3" => " MediaTek",
        "144d" => " Samsung",
        "15b7" => " SanDisk",
        "1c5c" => " SK hynix",
        "1b4b" => " Marvell",
        "1af4" | "1b36" => " Virtio/QEMU",
        "1234" => " QEMU",
        _ => "",
    }
}

pub fn pci(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let devices = sources.sys.join("bus/pci/devices");
    for address in entries(&devices) {
        let dir = devices.join(&address);
        let id = |file: &str| {
            read(&dir.join(file))
                .map(|v| v.trim_start_matches("0x").to_string())
                .unwrap_or_else(|| "????".to_string())
        };
        let vendor = id("vendor");
        let class = id("class");
        let driver = link_name(&dir.join("driver")).unwrap_or_else(|| "none".to_string());
        out.line(format!(
            "{address} {vendor}:{} (subsystem {}:{}) {} [{class}]{}, driver: {driver}",
            id("device"),
            id("subsystem_vendor"),
            id("subsystem_device"),
            pci_class(&class),
            maker(&vendor),
        ));
    }
    if out.text.is_empty() {
        out.line("none listed");
    }
    out
}

pub fn usb(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let devices = sources.sys.join("bus/usb/devices");
    let all = entries(&devices);
    // A name with a colon is one interface of a device.
    for name in all.iter().filter(|name| !name.contains(':')) {
        let dir = devices.join(name);
        let Some(vendor) = read(&dir.join("idVendor")) else {
            continue;
        };
        let words: Vec<String> = ["manufacturer", "product"]
            .iter()
            .filter_map(|file| read(&dir.join(file)))
            .collect();
        out.line(format!(
            "{name} {vendor}:{} {}, {} Mbit/s, class {}",
            read(&dir.join("idProduct")).unwrap_or_default(),
            words.join(" "),
            read(&dir.join("speed")).unwrap_or_else(|| "?".to_string()),
            read(&dir.join("bDeviceClass")).unwrap_or_default(),
        ));
        for interface in all.iter().filter(|i| {
            i.strip_prefix(name.as_str())
                .is_some_and(|rest| rest.starts_with(':'))
        }) {
            let dir = devices.join(interface);
            out.line(format!(
                "  interface class {}, driver: {}",
                read(&dir.join("bInterfaceClass")).unwrap_or_default(),
                link_name(&dir.join("driver")).unwrap_or_else(|| "none".to_string())
            ));
        }
    }
    if out.text.is_empty() {
        out.line("none listed");
    }
    out
}

/// The part of `wpctl status` that says what sound goes in and out where,
/// and the names of the Bluetooth devices in it. The list of connected
/// programs, which gives the account and the computer, is left out, and so
/// are the streams, which are named after what is being played.
pub(crate) fn sound_tree(status: &str) -> (String, Vec<String>) {
    let mut kept = String::new();
    let mut names = Vec::new();
    let mut started = false;
    let mut streams = false;
    for line in status.lines() {
        started |= line.trim_end() == "Audio";
        if !started {
            continue;
        }
        // A heading of the tree, or a part of it at the margin.
        if line.contains("─ ") || !line.starts_with([' ', '│']) {
            streams = line.contains("Streams:");
        }
        if streams {
            continue;
        }
        if line.contains("[bluez") {
            let words = line.trim_start_matches(|c: char| " │├└─*".contains(c));
            if let Some((_, rest)) = words.split_once(". ") {
                if let Some((name, _)) = rest.rsplit_once('[') {
                    names.push(name.trim().to_string());
                }
            }
        }
        kept.push_str(line);
        kept.push('\n');
    }
    (kept, names)
}

pub fn sound(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let asound = sources.proc.join("asound");
    out.line("Sound cards the kernel has (/proc/asound/cards):");
    match read(&asound.join("cards")) {
        Some(cards) => out.line(cards),
        None => out.unread("Sound cards", "there is no /proc/asound/cards"),
    }
    for card in entries(&asound).iter().filter(|n| n.starts_with("card")) {
        for codec in entries(&asound.join(card)) {
            if !codec.starts_with("codec") {
                continue;
            }
            let text = read(&asound.join(card).join(&codec)).unwrap_or_default();
            if let Some(line) = text.lines().find(|l| l.starts_with("Codec:")) {
                out.line(format!("{card} {codec}: {line}"));
            }
        }
    }
    let modules = read(&sources.proc.join("modules")).unwrap_or_default();
    let sound: Vec<&str> = modules
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| name.starts_with("snd"))
        .collect();
    out.line(format!("Sound modules loaded: {}", sound.join(" ")));
    out.line("");
    out.line("PipeWire (wpctl status):");
    if let Some(status) = command(
        sources,
        &mut out,
        "PipeWire's devices",
        commands::WPCTL,
        &["status"],
    ) {
        let (tree, names) = sound_tree(&status);
        out.bluetooth_names = names;
        if tree.is_empty() {
            out.unread("PipeWire's devices", "wpctl printed no list of them");
        } else {
            out.line(tree.trim_end());
        }
    }
    out
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

/// The state of Wi-Fi as iwd has it, without the name of any network.
fn wifi_state(out: &mut Out, objects: &[Object]) {
    let count = |interface: &str| {
        objects
            .iter()
            .filter(|o| o.interface(interface).is_some())
            .count()
    };
    for object in objects {
        let Some(device) = object.interface(IWD_DEVICE) else {
            continue;
        };
        let station = object.interface(IWD_STATION);
        out.line(format!(
            "  {}: switched on {}, mode {}, state {}",
            bus::text(device, "Name").unwrap_or("device"),
            yes_no(bus::flag(device, "Powered")),
            bus::text(device, "Mode").unwrap_or("unknown"),
            station
                .and_then(|s| bus::text(s, "State"))
                .unwrap_or("unknown"),
        ));
    }
    out.line(format!(
        "  networks in range: {}, networks remembered: {}",
        count(IWD_NETWORK),
        count(IWD_KNOWN)
    ));
}

/// The state of Bluetooth as BlueZ has it, with no names or addresses.
fn bluetooth_state(out: &mut Out, objects: &[Object]) {
    let adapters = objects
        .iter()
        .filter_map(|o| o.interface(crate::bluez::ADAPTER));
    for (number, adapter) in adapters.enumerate() {
        out.line(format!(
            "  adapter {number}: switched on {}",
            yes_no(bus::flag(adapter, "Powered"))
        ));
    }
    let devices: Vec<_> = objects
        .iter()
        .filter_map(|o| o.interface(crate::bluez::DEVICE))
        .collect();
    let paired = devices
        .iter()
        .filter(|d| bus::flag(d, "Paired") == Some(true))
        .count();
    out.line(format!("  devices paired: {paired}"));
    for device in devices
        .iter()
        .filter(|d| bus::flag(d, "Connected") == Some(true))
    {
        out.line(format!(
            "  connected: a device of the kind {}",
            bus::text(device, "Icon").unwrap_or("unknown")
        ));
    }
}

pub fn network(sources: &Sources, radios: &Radios) -> Out {
    let mut out = Out::default();
    out.line("Network interfaces:");
    let net = sources.sys.join("class/net");
    for name in entries(&net) {
        let dir = net.join(&name);
        let kind = if dir.join("wireless").exists() || dir.join("phy80211").exists() {
            "Wi-Fi"
        } else if dir.join("device").exists() {
            "wired or modem"
        } else {
            "virtual"
        };
        let driver = link_name(&dir.join("device/driver")).unwrap_or_else(|| "none".to_string());
        out.line(format!(
            "  {name}: {kind}, driver {driver}, state {}, link {}",
            read(&dir.join("operstate")).unwrap_or_else(|| "unknown".to_string()),
            // Reading carrier fails while an interface is down.
            yes_no(read(&dir.join("carrier")).map(|c| c == "1")),
        ));
    }
    out.line("Radio switches (rfkill):");
    let rfkill = sources.sys.join("class/rfkill");
    let switches = entries(&rfkill);
    for switch in &switches {
        let dir = rfkill.join(switch);
        let blocked = |file: &str| yes_no(read(&dir.join(file)).map(|v| v == "1"));
        out.line(format!(
            "  {} ({}): blocked by software {}, by a switch {}",
            read(&dir.join("name")).unwrap_or_else(|| switch.clone()),
            read(&dir.join("type")).unwrap_or_default(),
            blocked("soft"),
            blocked("hard"),
        ));
    }
    if switches.is_empty() {
        out.line("  none: no radio is known to the kernel");
    }
    for (label, listing) in [
        ("Wi-Fi (iwd)", &radios.wifi),
        ("Bluetooth (BlueZ)", &radios.bluetooth),
    ] {
        match listing {
            Listing::Absent => out.line(format!(
                "{label}: the service is not running or did not answer"
            )),
            Listing::Denied => out.unread(label, "refused: this account may not ask it"),
            Listing::Objects(objects) => {
                out.line(format!("{label}:"));
                if label.starts_with("Wi-Fi") {
                    wifi_state(&mut out, objects);
                } else {
                    bluetooth_state(&mut out, objects);
                }
            }
        }
    }
    out.line("Network services:");
    let units = [
        "iwd.service",
        "bluetooth.service",
        "systemd-networkd.service",
        "systemd-resolved.service",
    ];
    // is-active ends unhappily when any of them is not active, which is an
    // answer like any other.
    let mut args = vec!["is-active"];
    args.extend(units);
    if let Some(states) = command(sources, &mut out, "Network services", "systemctl", &args) {
        for (unit, state) in units.iter().zip(states.lines()) {
            out.line(format!("  {unit}: {state}"));
        }
    }
    out
}

pub fn graphics(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let drm = sources.sys.join("class/drm");
    let all = entries(&drm);
    for card in all
        .iter()
        .filter(|n| n.starts_with("card") && !n.contains('-'))
    {
        let device = drm.join(card).join("device");
        let uevent = read(&device.join("uevent")).unwrap_or_default();
        let field = |name: &str| {
            uevent
                .lines()
                .find_map(|l| l.strip_prefix(name)?.strip_prefix('='))
                .unwrap_or("unknown")
        };
        out.line(format!(
            "{card}: driver {}, PCI {}, {}, boot display {}, power {}",
            field("DRIVER"),
            field("PCI_ID"),
            field("PCI_SLOT_NAME"),
            yes_no(read(&device.join("boot_vga")).map(|v| v == "1")),
            read(&device.join("power/runtime_status")).unwrap_or_else(|| "unknown".to_string()),
        ));
        let connectors = all
            .iter()
            .filter_map(|n| Some((n, n.strip_prefix(card.as_str())?.strip_prefix('-')?)));
        for (dir, connector) in connectors {
            let dir = drm.join(dir);
            let file = |name: &str| read(&dir.join(name)).unwrap_or_else(|| "unknown".to_string());
            let modes = read(&dir.join("modes")).unwrap_or_default();
            out.line(format!(
                "  {connector}: {}, {}, power {}, preferred mode {}",
                file("status"),
                file("enabled"),
                file("dpms"),
                modes.lines().next().unwrap_or("none"),
            ));
        }
    }
    if out.text.is_empty() {
        out.line("no graphics device is known to the kernel");
    }
    let backlight = sources.sys.join("class/backlight");
    for name in entries(&backlight) {
        out.line(format!(
            "backlight {name}: {}",
            listed(
                &backlight.join(&name),
                &["type", "brightness", "max_brightness"]
            )
        ));
    }
    out
}

pub fn power(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    let supplies = sources.sys.join("class/power_supply");
    let names = entries(&supplies);
    // serial_number is not among them.
    const FILES: &[&str] = &[
        "type",
        "scope",
        "online",
        "present",
        "status",
        "capacity",
        "capacity_level",
        "health",
        "technology",
        "manufacturer",
        "model_name",
        "cycle_count",
        "energy_full_design",
        "energy_full",
        "energy_now",
        "charge_full_design",
        "charge_full",
        "charge_now",
        "voltage_now",
        "power_now",
        "current_now",
    ];
    for name in &names {
        out.line(format!("{name}: {}", listed(&supplies.join(name), FILES)));
    }
    if names.is_empty() {
        out.line("no battery or power adapter is known to the kernel");
    }
    for (label, file) in [
        ("Sleep states", "power/state"),
        ("Kind of sleep", "power/mem_sleep"),
        ("Platform profile", "firmware/acpi/platform_profile"),
        (
            "Processor frequency driver",
            "devices/system/cpu/cpu0/cpufreq/scaling_driver",
        ),
        (
            "Processor frequency governor",
            "devices/system/cpu/cpu0/cpufreq/scaling_governor",
        ),
        (
            "Processor energy preference",
            "devices/system/cpu/cpu0/cpufreq/energy_performance_preference",
        ),
    ] {
        out.fact(label, read(&sources.sys.join(file)));
    }
    out
}

pub fn failed_units(sources: &Sources, _: &Radios) -> Out {
    let mut out = Out::default();
    for (label, user) in [("System services", false), ("Your own services", true)] {
        let mut args = vec!["--failed", "--no-pager", "--plain", "--no-legend", "--full"];
        if user {
            args.insert(0, "--user");
        }
        out.line(format!("{label}:"));
        if let Some(list) = command(sources, &mut out, label, "systemctl", &args) {
            let list = plain_lines(&list);
            out.line(if list.is_empty() {
                "  none failed".to_string()
            } else {
                list
            });
        }
    }
    out
}

fn plain_lines(text: &str) -> String {
    text.lines()
        .map(plain)
        .filter(|line| !line.trim().is_empty())
        .take(64)
        .map(|line| format!("  {}", line.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}
