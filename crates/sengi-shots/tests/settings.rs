//! Every Settings page, drawn at three window sizes with made-up data.
//! SENGI_RENDER_DIR names where the pictures go.

use sengi_shots::harness::{self, Look, BACKDROP};
use sengi_shots::settings::*;
use slint::{
    ComponentHandle, Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, SharedString, VecModel,
};

fn list<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn item(id: &str, title: &str, detail: &str, icon: &str, level: i32) -> Item {
    Item {
        id: id.into(),
        title: title.into(),
        detail: detail.into(),
        icon: icon.into(),
        level,
        enabled: true,
        ..Default::default()
    }
}

fn info(title: &str, value: &str) -> InfoRow {
    InfoRow {
        title: title.into(),
        value: value.into(),
    }
}

fn pages() -> ModelRc<PageEntry> {
    let rows = [
        ("network", "Network", "signal"),
        ("bluetooth", "Bluetooth", "bluetooth"),
        ("sound", "Sound", "speaker"),
        ("display", "Display", "display"),
        ("wallpaper", "Appearance", "sun"),
        ("power", "Power", "battery"),
        ("date-time", "Date & Time", "clock"),
        ("keyboard", "Keyboard", "keyboard"),
        ("mouse", "Mouse & Touchpad", "mouse"),
        ("printers", "Printers", "printer"),
        ("notifications", "Notifications", "bell"),
        ("updates", "Updates", "arrows-clockwise"),
        ("accessibility", "Accessibility", "person-simple-circle"),
        ("about", "About", "info"),
    ];
    list(
        rows.iter()
            .map(|(name, title, icon)| PageEntry {
                name: (*name).into(),
                title: (*title).into(),
                icon: (*icon).into(),
            })
            .collect(),
    )
}

// The colour of a page behind its cards: bg in tokens/sengi.toml.
fn page_fill() -> [u8; 3] {
    harness::page_colour()
}
// The sidebar and the widest column of a page, in tokens.
// It grows by half of the text scale beyond 1.
fn sidebar() -> usize {
    (200.0 * (1.0 + (harness::Look::from_env().text_scale - 1.0) / 2.0)).round() as usize
}
const COLUMN: usize = 640;
const MARGIN: usize = 24;
const SCROLLBAR: usize = 10;

/// Nothing is drawn in the margins either side of the column of cards: a
/// row or a control that is wider than its card would show there.
fn keeps_to_its_column(picture: &harness::Picture, page: &str, variant: &str, width: usize) {
    let area = width - sidebar();
    let column = (area - MARGIN * 2).min(COLUMN);
    let left = sidebar() + (area - column) / 2;
    let right = left + column;
    let name = format!("{page}/{variant} at {width}");
    // One pixel of slack for the border of a card.
    if let Some(at) = picture.first_other(sidebar() + 2, left - 1, 0, picture.height, page_fill()) {
        panic!("{name}: something is drawn left of the column at {at:?}");
    }
    if let Some(at) =
        picture.first_other(right + 1, width - SCROLLBAR, 0, picture.height, page_fill())
    {
        panic!("{name}: something is drawn right of the column at {at:?}");
    }
}

type Scene = (&'static str, &'static str, Box<dyn Fn(&SettingsWindow)>);

const SIZES: [(u32, u32); 3] = [(720, 480), (888, 600), (1280, 800)];

fn network(w: &SettingsWindow, variant: &str) {
    let d = w.global::<NetworkData>();
    d.set_available(true);
    d.set_powered(variant != "off");
    if variant == "off" {
        d.set_status("Off".into());
        return;
    }
    d.set_status("Connected to Home".into());
    d.set_connected(true);
    d.set_name("Home".into());
    d.set_signal("Excellent".into());
    d.set_security("WPA2 Personal".into());
    d.set_address("192.168.1.23".into());
    d.set_saved(true);
    let mut nets = vec![
        item("a", "Home", "Connected", "signal", 4),
        item(
            "b",
            "Neighbour with an extremely long network name that goes on",
            "WPA2 Personal, saved",
            "signal",
            3,
        ),
        item("c", "Cafe Guest", "Open", "signal", 2),
        item("d", "x", "WPA3", "signal", 1),
    ];
    nets[0].status = "Connected".into();
    nets[1].action = "Join".into();
    nets[1].remove = "Forget".into();
    nets[2].action = "Join".into();
    nets[3].status = "Joining…".into();
    if variant == "many" {
        for n in 0..14 {
            nets.push(item(
                &format!("n{n}"),
                &format!("Network number {n}"),
                "WPA2 Personal",
                "signal",
                1 + n % 4,
            ));
        }
    }
    d.set_networks(list(nets));
    d.set_wired(list(vec![item(
        "w",
        "Wired connection",
        "192.168.1.5",
        "wired",
        0,
    )]));
}

fn bluetooth(w: &SettingsWindow, variant: &str) {
    let d = w.global::<BluetoothData>();
    d.set_available(true);
    d.set_powered(true);
    d.set_status("On".into());
    d.set_searching(variant == "search");
    let mut mine = vec![
        item(
            "1",
            "Allan's headphones with a very long product name",
            "Connected",
            "headphones",
            0,
        ),
        item("2", "Keyboard", "Not connected", "keyboard", 0),
    ];
    mine[0].action = "Disconnect".into();
    mine[0].remove = "Remove".into();
    d.set_mine(list(mine));
    let mut near = vec![item("3", "Speaker", "Ready to pair", "speaker", 0)];
    near[0].action = "Pair".into();
    d.set_nearby(list(near));
    if variant == "blocked" {
        d.set_note("Bluetooth is switched off by a key on this computer.".into());
        d.set_note_warning(true);
        d.set_available(false);
        d.set_mine(list(vec![]));
        d.set_nearby(list(vec![]));
    }
}

fn sound(w: &SettingsWindow, variant: &str) {
    let d = w.global::<SoundData>();
    if variant == "none" {
        d.set_note("No sound device was found.".into());
        return;
    }
    d.set_output(SoundSide {
        present: true,
        device: "Built-in Audio Analog Stereo with a long name here".into(),
        choice: true,
        muted: variant == "muted",
    });
    d.set_input(SoundSide {
        present: true,
        device: "Internal Microphone".into(),
        choice: false,
        muted: false,
    });
    d.set_output_volume(72);
    d.set_input_volume(40);
}

fn display(w: &SettingsWindow, variant: &str) {
    let d = w.global::<DisplayData>();
    d.set_has_backlight(true);
    d.set_brightness_value(60);
    let mut outs = vec![DisplayOutput {
        name: "eDP-1".into(),
        title: "Built-in display".into(),
        connector: "eDP-1".into(),
        enabled: true,
        resolution: "1920x1080".into(),
        refresh: "60 Hz".into(),
        scale: "100%".into(),
        position: "".into(),
        can_position: false,
        can_resolution: true,
        can_refresh: true,
    }];
    if variant == "two" {
        outs.push(DisplayOutput {
            name: "HDMI-A-1".into(),
            title: "Dell U2723QE 27-inch external monitor".into(),
            connector: "HDMI-A-1".into(),
            enabled: false,
            resolution: "3840x2160".into(),
            refresh: "30 Hz".into(),
            scale: "150%".into(),
            position: "".into(),
            can_position: false,
            can_resolution: true,
            can_refresh: false,
        });
    }
    d.set_outputs(list(outs));
}

// The thumbnails the image ships, decoded as the page would draw them.
fn thumb(file: &str) -> Image {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../image/rootfs/usr/share/backgrounds/sengi/thumbs")
        .join(file);
    let mut reader = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()))
        .read_info()
        .unwrap();
    let mut data = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut data).unwrap();
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(frame.width, frame.height);
    buffer
        .make_mut_bytes()
        .copy_from_slice(&data[..frame.buffer_size()]);
    Image::from_rgb8(buffer)
}

fn wallpaper(w: &SettingsWindow, variant: &str) {
    let d = w.global::<WallpaperData>();
    let files = [
        "01-graphite",
        "02-horizon",
        "03-hills",
        "04-tiles",
        "05-sunrise",
        "06-stripes",
    ];
    let chosen = if variant == "solid" { 0 } else { 3 };
    let mut tiles = vec![WallpaperTile {
        path: "solid".into(),
        title: "Solid colour".into(),
        chosen: chosen == 0,
        ..Default::default()
    }];
    for (index, file) in files.iter().enumerate() {
        let title = file[3..].to_string();
        tiles.push(WallpaperTile {
            path: format!("/usr/share/backgrounds/sengi/{file}.png").into(),
            title: format!("{}{}", title[..1].to_uppercase(), &title[1..]).into(),
            image: thumb(&format!("{file}.png")),
            has_image: true,
            chosen: chosen == index + 1,
        });
    }
    d.set_tiles(list(tiles));
    if variant == "solid" {
        d.set_current_title("Solid colour".into());
        d.set_current_detail("The plain desktop colour. It uses the least memory.".into());
    } else {
        d.set_current_title("Tiles".into());
        d.set_current_detail("PNG, 1920 by 1080".into());
        d.set_current_image(thumb("04-tiles.png"));
        d.set_has_current_image(true);
    }
    if variant == "missing" {
        d.set_current_title("Solid colour".into());
        d.set_current_detail("The plain desktop colour, until a picture is chosen.".into());
        d.set_has_current_image(false);
        d.set_notice(
            "The chosen picture cannot be shown: /home/allan/Pictures/beach.jpg is not there."
                .into(),
        );
        d.set_path_error("/home/allan/Pictures/notes.txt is not a PNG or JPEG picture".into());
    }
    let row = |name: &str, detail: &str, usable: bool, chosen: bool| PictureRow {
        path: format!("/home/allan/Pictures/{name}").into(),
        title: name.into(),
        detail: detail.into(),
        usable,
        chosen,
    };
    d.set_pictures(list(vec![
        row(
            "beach.jpg",
            "JPEG, 4000 by 2667. It takes about 43 MB of memory while it is the wallpaper.",
            true,
            false,
        ),
        row("logo.png", "PNG, 512 by 512", true, false),
        row("scan.jpg", "This file is too big, over 64 MB", false, false),
    ]));
}

fn power(w: &SettingsWindow, variant: &str) {
    let d = w.global::<PowerData>();
    d.set_dim("After 2 minutes".into());
    d.set_blank("After 5 minutes".into());
    d.set_lock("Never".into());
    d.set_lock_on_sleep(true);
    d.set_buttons(list(vec![info("Power button", "Sleep")]));
    if variant == "battery" {
        // A laptop has a lid.
        d.set_has_lid(true);
        d.set_lid("Sleep".into());
        d.set_has_battery(true);
        d.set_battery(list(vec![
            info("Charge", "64%"),
            info("State", "Discharging"),
            info("Time left", "3 hours 12 minutes"),
            info("Health", "91%"),
        ]));
        d.set_idle_note("The change could not reach the running session.".into());
    }
}

fn datetime(w: &SettingsWindow) {
    let d = w.global::<DateTimeData>();
    d.set_zone("Africa/Kigali".into());
    d.set_own_zone(true);
    d.set_now("Tuesday 25 November, 21:04".into());
    d.set_twelve_hour(false);
    d.set_system(list(vec![
        info("System time zone", "Etc/UTC"),
        info("Network time", "Synchronised"),
    ]));
}

fn keyboard(w: &SettingsWindow, variant: &str) {
    let d = w.global::<KeyboardData>();
    let mut layouts = vec![item("us", "English (US)", "us", "keyboard", 0)];
    if variant == "several" {
        layouts.push(item(
            "fr",
            "French (Rwanda, with a very long variant name)",
            "fr",
            "keyboard",
            0,
        ));
        layouts.push(item("de", "German", "de", "keyboard", 0));
        d.set_several(true);
        d.set_switch("Alt + Shift".into());
        // Under sengi-compositor there is a sheet of shortcuts to show.
        d.set_can_show_shortcuts(true);
    }
    for l in layouts.iter_mut() {
        l.remove = "Remove".into();
    }
    d.set_layouts(list(layouts));
}

fn pointer(w: &SettingsWindow) {
    let d = w.global::<PointerData>();
    d.set_mouse(PointerSide {
        natural_scroll: false,
        left_handed: false,
        tap_to_click: false,
        disable_while_typing: false,
    });
    d.set_touchpad(PointerSide {
        natural_scroll: true,
        left_handed: false,
        tap_to_click: true,
        disable_while_typing: true,
    });
    d.set_mouse_speed(0);
    d.set_touchpad_speed(30);
}

fn printers(w: &SettingsWindow, variant: &str) {
    let d = w.global::<PrintersData>();
    d.set_available(true);
    if variant == "missing" || variant == "missing-live" || variant == "installing" {
        d.set_available(false);
        d.set_can_install(true);
        d.set_install_blocked(variant == "missing-live");
        d.set_install_detail(
            if variant == "missing-live" {
                "Printing can be installed after Sengi OS is installed on this computer. This live system runs from memory and has no room for it."
            } else {
                "Adds CUPS, the print system: about 87 MB on disk and 25 MB to download. It needs an internet connection and your password, and nothing of it runs until something prints."
            }
            .into(),
        );
        if variant == "installing" {
            d.set_busy(true);
            d.set_busy_text(
                "Installing printing support. This can take a few minutes\u{2026}".into(),
            );
        }
        return;
    }
    if variant == "none" {
        d.set_summary("No printers added yet".into());
        return;
    }
    d.set_summary("2 printers \u{b7} Office Laser is the default".into());
    let mut rows = vec![
        item(
            "Office_Laser",
            "Office Laser",
            "Idle \u{b7} 192.168.1.50",
            "printer",
            0,
        ),
        item(
            "Garage_Brother_HL-L2350DW_series_with_a_very_long_name",
            "Garage Brother HL-L2350DW series with a very long name",
            "Offline, unable to connect to printer \u{b7} On the network",
            "printer",
            0,
        ),
    ];
    for row in &mut rows {
        row.action = "Manage".into();
    }
    rows[0].status = "Default".into();
    rows[0].checked = true;
    d.set_printers(list(rows));
    d.set_has_chosen(true);
    d.set_chosen_title("Office Laser".into());
    d.set_chosen_detail("Idle \u{b7} 192.168.1.50".into());
    d.set_chosen_default(true);
    if variant == "queue" {
        let mut jobs = vec![
            item(
                "Office_Laser-12",
                "Job 12 on Office Laser",
                "allan \u{b7} 2 KB \u{b7} Thu 26 Nov 2026 10:00:00 AM CET",
                "file",
                0,
            ),
            item(
                "Office_Laser-13",
                "Job 13 on Office Laser",
                "allan \u{b7} 1.4 MB \u{b7} Thu 26 Nov 2026 10:02:10 AM CET",
                "file",
                0,
            ),
        ];
        for job in &mut jobs {
            job.remove = "Cancel".into();
        }
        d.set_jobs(list(jobs));
        d.set_note("A test page was sent to Office Laser.".into());
        d.set_note_warning(false);
    }
    if variant == "busy" {
        d.set_busy(true);
        d.set_busy_text("Adding Office Laser\u{2026}".into());
    }
    if variant == "offline-chosen" {
        d.set_chosen_title("Garage Brother HL-L2350DW series with a very long name".into());
        d.set_chosen_detail("Offline, unable to connect to printer \u{b7} On the network".into());
        d.set_chosen_default(false);
    }
    if variant.starts_with("add") {
        d.set_adding(true);
        d.set_searching(variant == "add-searching");
        d.set_searched(variant != "add-searching");
        if variant == "add-found" {
            let mut found = vec![
                item(
                    "0",
                    "Brother HL-L2350DW series",
                    "On the network",
                    "printer",
                    0,
                ),
                item(
                    "1",
                    "EPSON ET-2850 Series",
                    "epson-et2850.local",
                    "printer",
                    0,
                ),
            ];
            for row in &mut found {
                row.action = "Add".into();
            }
            d.set_found(list(found));
        }
        if variant == "add-problem" {
            d.set_address_problem(
                "That is not a printer address. Use a name or a number such as 192.168.1.50."
                    .into(),
            );
        }
    }
}

fn updates(w: &SettingsWindow, variant: &str) {
    let d = w.global::<UpdatesData>();
    let row = |title: &str, detail: &str, tag: &str| UpdateRow {
        title: title.into(),
        detail: detail.into(),
        tag: tag.into(),
    };
    let lists = || {
        d.set_system_title("System packages (3)".into());
        d.set_system(list(vec![
            row(
                "openssl",
                "3.0.15-1~deb13u1 to 3.0.16-1~deb13u1",
                "Security",
            ),
            row(
                "libssl3t64",
                "3.0.15-1~deb13u1 to 3.0.16-1~deb13u1",
                "Security",
            ),
            row("tzdata", "2024a-4 to 2025b-1", ""),
        ]));
        d.set_apps_title("Apps (2)".into());
        d.set_apps(list(vec![
            row("GIMP", "2.10.38 \u{b7} 112.4 MB", ""),
            row("Firefox", "128.0 \u{b7} 60.1 MB", ""),
        ]));
        d.set_can_update(true);
        d.set_badge(5);
    };
    match variant {
        "idle" => {
            d.set_headline("Check for updates".into());
            d.set_subline("Not checked yet".into());
        }
        "checking" => {
            d.set_checking(true);
            d.set_headline("Looking for updates".into());
            d.set_subline("Step 1 of 2 \u{b7} 50% \u{b7} Retrieving file 1 of 2".into());
        }
        "list" => {
            lists();
            d.set_headline("5 updates available".into());
            d.set_subline(
                "3 system packages (2 security), 2 apps \u{b7} 36 MB to download \u{b7} Checked 5 minutes ago"
                    .into(),
            );
        }
        "progress" => {
            lists();
            d.set_working(true);
            d.set_headline("5 updates available".into());
            d.set_subline(
                "3 system packages (2 security), 2 apps \u{b7} Checked 5 minutes ago".into(),
            );
            d.set_phase("Downloading system updates".into());
            d.set_percent(45);
            d.set_progress_line(
                "Step 2 of 6 \u{b7} 45% \u{b7} 16 MB of 36 MB \u{b7} Retrieving file 2 of 6".into(),
            );
            d.set_can_cancel(true);
        }
        "restart" => {
            d.set_headline("Up to date".into());
            d.set_subline("Checked just now".into());
            d.set_restart_needed(true);
        }
        "failed" => {
            d.set_headline("Check for updates".into());
            d.set_subline("Not checked yet".into());
            d.set_note(
                "System updates could not be checked. E: Could not resolve 'deb.debian.org'".into(),
            );
        }
        _ => {
            d.set_live(true);
            d.set_headline("Not on the live system".into());
            d.set_note_warning(false);
            d.set_note(
                "Updates are available after Sengi OS is installed. This live system runs from memory and has nothing on a disk to update."
                    .into(),
            );
        }
    }
}

fn about(w: &SettingsWindow) {
    let d = w.global::<AboutData>();
    d.set_version("0.8.0".into());
    d.set_system(list(vec![info("Kernel", "6.12.113")]));
    d.set_hardware(list(vec![
        info(
            "Processor",
            "Intel(R) Core(TM) i5-7200U CPU @ 2.50GHz (4 threads)",
        ),
        info("Memory", "16 GB"),
        info("Graphics", "Intel (i915), NVIDIA (nouveau)"),
        info("Disk", "Live system, held in memory: 5.1 GB free of 7.8 GB"),
    ]));
    d.set_memory_used("312 MB in use".into());
}

/// The Updates page at the largest text, dark and light, where its top
/// row has least room: the title and the buttons must not share a line.
#[test]
fn updates_at_extra_large_text() {
    harness::install();
    for light in [false, true] {
        let look = Look {
            light,
            ..Look::PLAIN.scaled(1.4)
        };
        for variant in ["idle", "list", "progress", "live"] {
            for (width, height) in [(720u32, 480u32), (888, 600)] {
                let w = themed_look(SettingsWindow::new().unwrap(), look);
                w.set_pages(pages());
                w.set_page(SharedString::from("updates"));
                updates(&w, variant);
                w.show().unwrap();
                let win = harness::window();
                harness::resize(&win, width, height);
                harness::advance(300);
                let picture = harness::draw(&win, BACKDROP);
                let mode = if light { "-light" } else { "" };
                picture.save_as(&format!(
                    "settings-updates-{variant}-extra-large{mode}-{width}x{height}.png"
                ));
                w.hide().unwrap();
            }
        }
    }
}

#[test]
fn every_page() {
    harness::install();
    let scenes: Vec<Scene> = vec![
        ("network", "connected", Box::new(|w| network(w, "on"))),
        ("network", "many", Box::new(|w| network(w, "many"))),
        ("network", "off", Box::new(|w| network(w, "off"))),
        ("bluetooth", "on", Box::new(|w| bluetooth(w, "on"))),
        (
            "bluetooth",
            "blocked",
            Box::new(|w| bluetooth(w, "blocked")),
        ),
        ("sound", "on", Box::new(|w| sound(w, "on"))),
        ("sound", "none", Box::new(|w| sound(w, "none"))),
        ("display", "one", Box::new(|w| display(w, "one"))),
        ("display", "two", Box::new(|w| display(w, "two"))),
        (
            "wallpaper",
            "picture",
            Box::new(|w| wallpaper(w, "picture")),
        ),
        ("wallpaper", "solid", Box::new(|w| wallpaper(w, "solid"))),
        (
            "wallpaper",
            "missing",
            Box::new(|w| wallpaper(w, "missing")),
        ),
        ("power", "desktop", Box::new(|w| power(w, "desktop"))),
        ("power", "battery", Box::new(|w| power(w, "battery"))),
        ("date-time", "on", Box::new(datetime)),
        ("keyboard", "one", Box::new(|w| keyboard(w, "one"))),
        ("keyboard", "several", Box::new(|w| keyboard(w, "several"))),
        ("mouse", "on", Box::new(pointer)),
        ("printers", "none", Box::new(|w| printers(w, "none"))),
        ("printers", "list", Box::new(|w| printers(w, "list"))),
        (
            "printers",
            "offline-chosen",
            Box::new(|w| printers(w, "offline-chosen")),
        ),
        ("printers", "queue", Box::new(|w| printers(w, "queue"))),
        ("printers", "busy", Box::new(|w| printers(w, "busy"))),
        (
            "printers",
            "add-searching",
            Box::new(|w| printers(w, "add-searching")),
        ),
        (
            "printers",
            "add-found",
            Box::new(|w| printers(w, "add-found")),
        ),
        (
            "printers",
            "add-none",
            Box::new(|w| printers(w, "add-none")),
        ),
        (
            "printers",
            "add-problem",
            Box::new(|w| printers(w, "add-problem")),
        ),
        ("printers", "missing", Box::new(|w| printers(w, "missing"))),
        (
            "printers",
            "missing-live",
            Box::new(|w| printers(w, "missing-live")),
        ),
        (
            "printers",
            "installing",
            Box::new(|w| printers(w, "installing")),
        ),
        (
            "notifications",
            "on",
            Box::new(|w| {
                let d = w.global::<NotificationsData>();
                d.set_available(true);
            }),
        ),
        (
            "notifications",
            "off",
            Box::new(|w| {
                let d = w.global::<NotificationsData>();
                d.set_note("The notification service is not running.".into());
            }),
        ),
        ("updates", "idle", Box::new(|w| updates(w, "idle"))),
        ("updates", "checking", Box::new(|w| updates(w, "checking"))),
        ("updates", "list", Box::new(|w| updates(w, "list"))),
        ("updates", "progress", Box::new(|w| updates(w, "progress"))),
        ("updates", "restart", Box::new(|w| updates(w, "restart"))),
        ("updates", "failed", Box::new(|w| updates(w, "failed"))),
        ("updates", "live", Box::new(|w| updates(w, "live"))),
        (
            "accessibility",
            "default",
            Box::new(|w| {
                let d = w.global::<AccessibilityData>();
                d.set_text_size(1);
            }),
        ),
        (
            "accessibility",
            "all-on",
            Box::new(|w| {
                let d = w.global::<AccessibilityData>();
                d.set_text_size(3);
                d.set_high_contrast(true);
                d.set_reduce_motion(true);
                d.set_cursor_size(2);
            }),
        ),
        (
            "wallpaper",
            "high-contrast",
            Box::new(|w| {
                wallpaper(w, "picture");
                w.global::<WallpaperData>()
                    .set_theme("high-contrast".into());
            }),
        ),
        ("about", "on", Box::new(about)),
    ];
    for (page, variant, fill) in &scenes {
        for (width, height) in SIZES {
            let w = themed(SettingsWindow::new().unwrap());
            w.set_pages(pages());
            w.set_page(SharedString::from(*page));
            fill(&w);
            w.show().unwrap();
            let win = harness::window();
            harness::resize(&win, width, height);
            harness::advance(300);
            let picture = harness::draw(&win, BACKDROP);
            picture.save(&format!("settings-{page}-{variant}-{width}x{height}.png"));
            if harness::has_inter() {
                keeps_to_its_column(&picture, page, variant, width as usize);
            }
            w.hide().unwrap();
        }
    }
}

#[test]
fn focus_hover_dialog_chooser() {
    harness::install();
    // The sidebar has the keyboard on the chosen row, and the pointer is on another.
    let w = themed(SettingsWindow::new().unwrap());
    w.set_pages(pages());
    w.set_page("network".into());
    network(&w, "on");
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 888, 600);
    w.invoke_focus_sidebar();
    harness::pointer(&win, 60.0, 133.0);
    harness::advance(300);
    harness::draw(&win, BACKDROP).save("settings-sidebar-focus-hover-888x600.png");
    // Down into the list of networks.
    for _ in 0..3 {
        harness::key(&win, "\t");
    }
    harness::advance(300);
    harness::draw(&win, BACKDROP).save("settings-tab-focus-888x600.png");
    w.hide().unwrap();

    let w = themed(SettingsWindow::new().unwrap());
    w.set_pages(pages());
    w.set_page("bluetooth".into());
    bluetooth(&w, "on");
    let d = w.global::<DialogState>();
    d.set_open(true);
    d.set_spec(DialogSpec {
        title: "Pair with Allan's headphones?".into(),
        body: "Check that this number is shown on the other device too.".into(),
        code: "482 913".into(),
        confirm: "Pair".into(),
        cancel: "Cancel".into(),
        ..Default::default()
    });
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 888, 600);
    harness::advance(300);
    harness::draw(&win, BACKDROP).save("settings-dialog-code-888x600.png");
    w.hide().unwrap();

    let w = themed(SettingsWindow::new().unwrap());
    w.set_pages(pages());
    w.set_page("network".into());
    network(&w, "on");
    let d = w.global::<DialogState>();
    d.set_open(true);
    d.set_spec(DialogSpec {
        title: "Join Neighbour with an extremely long network name that goes on?".into(),
        body: "Type the passphrase of the network.".into(),
        input: true,
        masked: true,
        placeholder: "8 to 63 characters".into(),
        problem: "That passphrase was not accepted".into(),
        confirm: "Join".into(),
        cancel: "Cancel".into(),
        minimum: 8,
        ..Default::default()
    });
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 720, 480);
    harness::advance(300);
    harness::draw(&win, BACKDROP).save("settings-dialog-input-720x480.png");
    w.hide().unwrap();

    let w = themed(SettingsWindow::new().unwrap());
    w.set_pages(pages());
    w.set_page("date-time".into());
    datetime(&w);
    let d = w.global::<DialogState>();
    d.set_open(true);
    d.set_spec(DialogSpec {
        title: "Forget Home?".into(),
        body: "The saved passphrase is removed from this computer. This cannot be undone.".into(),
        confirm: "Forget".into(),
        cancel: "Cancel".into(),
        danger: true,
        ..Default::default()
    });
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 888, 600);
    harness::advance(300);
    harness::draw(&win, BACKDROP).save("settings-dialog-danger-888x600.png");
    w.hide().unwrap();

    let w = themed(SettingsWindow::new().unwrap());
    w.set_pages(pages());
    w.set_page("date-time".into());
    datetime(&w);
    let c = w.global::<ChooserState>();
    c.set_open(true);
    c.set_title("Time zone".into());
    c.set_searchable(true);
    c.set_placeholder("Search".into());
    c.set_empty("No time zone matches".into());
    c.set_items(list(
        (0..30)
            .map(|n| {
                let mut i = item(
                    &format!("z{n}"),
                    &format!("Africa/City number {n}"),
                    "UTC+2",
                    "",
                    0,
                );
                i.checked = n == 3;
                i
            })
            .collect(),
    ));
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, 888, 600);
    harness::advance(300);
    harness::draw(&win, BACKDROP).save("settings-chooser-888x600.png");
    w.hide().unwrap();
}

/// The runs of rows, top to bottom, that are not page background just
/// inside the left edge of the column: one per card.
fn cards(picture: &harness::Picture, width: usize) -> Vec<(usize, usize)> {
    let area = width - sidebar();
    let column = (area - MARGIN * 2).min(COLUMN);
    let x = sidebar() + (area - column) / 2 + 3;
    let mut runs = Vec::new();
    let mut start = None;
    for y in 0..picture.height {
        match (picture.at(x, y) != page_fill(), start) {
            (true, None) => start = Some(y),
            (false, Some(from)) => {
                runs.push((from, y));
                start = None;
            }
            _ => {}
        }
    }
    // The title's first letter is not a card.
    runs.retain(|(top, bottom)| bottom - top >= 30);
    runs
}

/// The About page has four cards (name, kernel, hardware, memory). The
/// kernel card holds one row, with its value in it, and is not taller than
/// a row; the hardware card holds four.
fn about_is_laid_out(picture: &harness::Picture, width: usize, what: &str) {
    let runs = cards(picture, width);
    let needed = if harness::Look::from_env().text_scale > 1.0 {
        2
    } else {
        3
    };
    assert!(runs.len() >= needed, "{what}: found {} cards", runs.len());
    let (top, bottom) = runs[1];
    assert!(
        bottom - top <= (56.0 * harness::Look::from_env().text_scale.max(1.0)).ceil() as usize,
        "{what}: the kernel card is {} px tall",
        bottom - top
    );
    let area = width - sidebar();
    let column = (area - MARGIN * 2).min(COLUMN);
    let right = sidebar() + (area - column) / 2 + column;
    let card = picture.at(right - 4, top + 3);
    assert!(
        picture
            .first_other(right - 160, right - 14, top + 8, bottom - 8, card)
            .is_some(),
        "{what}: the kernel value is not in the kernel row"
    );
    let (top, bottom) = runs[2];
    assert!(
        bottom - top >= 4 * 40,
        "{what}: the hardware card is {} px tall",
        bottom - top
    );
}

/// The About page is opened empty and filled a moment later, when the
/// worker thread has read the system, and again when Measure again is
/// pressed. However it got there, every row is where it should be.
#[test]
fn about_rows_stay_with_their_values() {
    harness::install();
    if !harness::has_inter() {
        return;
    }
    for (width, height) in SIZES {
        let name = |how: &str| format!("about {how} at {width}");
        let open = || {
            let w = themed(SettingsWindow::new().unwrap());
            w.set_pages(pages());
            w.set_page("about".into());
            w
        };

        let w = open();
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, width, height);
        harness::advance(300);
        harness::draw(&win, BACKDROP);
        about(&w);
        harness::advance(300);
        let late = harness::draw(&win, BACKDROP);
        late.save(&format!("settings-about-late-{width}x{height}.png"));
        about_is_laid_out(&late, width as usize, &name("filled late"));
        w.hide().unwrap();

        // Short values first, long ones after.
        let w = open();
        let d = w.global::<AboutData>();
        d.set_system(list(vec![info("Kernel", "")]));
        d.set_hardware(list(vec![
            info("Processor", "Unknown"),
            info("Memory", "Unknown"),
            info("Graphics", "Unknown"),
            info("Disk", "Unknown"),
        ]));
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, width, height);
        harness::advance(300);
        harness::draw(&win, BACKDROP);
        about(&w);
        harness::advance(300);
        about_is_laid_out(
            &harness::draw(&win, BACKDROP),
            width as usize,
            &name("again"),
        );
        w.hide().unwrap();

        // Filled while the window is still tiny, then grown.
        let w = open();
        w.show().unwrap();
        let win = harness::window();
        harness::resize(&win, 120, 80);
        harness::advance(300);
        about(&w);
        harness::advance(300);
        harness::resize(&win, width, height);
        harness::advance(300);
        about_is_laid_out(
            &harness::draw(&win, BACKDROP),
            width as usize,
            &name("grown"),
        );
        w.hide().unwrap();
    }
}
