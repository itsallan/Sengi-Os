//! Every Settings page, drawn at three window sizes with made-up data.
//! SENGI_RENDER_DIR names where the pictures go.

use sengi_shots::harness::{self, BACKDROP};
use sengi_shots::settings::*;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

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
        ("power", "Power", "battery"),
        ("date-time", "Date & Time", "clock"),
        ("keyboard", "Keyboard", "keyboard"),
        ("mouse", "Mouse & Touchpad", "mouse"),
        ("notifications", "Notifications", "bell"),
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
const PAGE: [u8; 3] = [0x16, 0x17, 0x1b];
// The sidebar and the widest column of a page, in tokens.
const SIDEBAR: usize = 200;
const COLUMN: usize = 640;
const MARGIN: usize = 24;
const SCROLLBAR: usize = 10;

/// Nothing is drawn in the margins either side of the column of cards: a
/// row or a control that is wider than its card would show there.
fn keeps_to_its_column(picture: &harness::Picture, page: &str, variant: &str, width: usize) {
    let area = width - SIDEBAR;
    let column = (area - MARGIN * 2).min(COLUMN);
    let left = SIDEBAR + (area - column) / 2;
    let right = left + column;
    let name = format!("{page}/{variant} at {width}");
    // One pixel of slack for the border of a card.
    if let Some(at) = picture.first_other(SIDEBAR + 2, left - 1, 0, picture.height, PAGE) {
        panic!("{name}: something is drawn left of the column at {at:?}");
    }
    if let Some(at) = picture.first_other(right + 1, width - SCROLLBAR, 0, picture.height, PAGE) {
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
        item("b", "Neighbour with an extremely long network name that goes on", "WPA2 Personal, saved", "signal", 3),
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
            nets.push(item(&format!("n{n}"), &format!("Network number {n}"), "WPA2 Personal", "signal", 1 + n % 4));
        }
    }
    d.set_networks(list(nets));
    d.set_wired(list(vec![item("w", "Wired connection", "192.168.1.5", "wired", 0)]));
}

fn bluetooth(w: &SettingsWindow, variant: &str) {
    let d = w.global::<BluetoothData>();
    d.set_available(true);
    d.set_powered(true);
    d.set_status("On".into());
    d.set_searching(variant == "search");
    let mut mine = vec![
        item("1", "Allan's headphones with a very long product name", "Connected", "headphones", 0),
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
            can_resolution: true,
            can_refresh: false,
        });
    }
    d.set_outputs(list(outs));
}

fn power(w: &SettingsWindow, variant: &str) {
    let d = w.global::<PowerData>();
    d.set_blank("After 5 minutes".into());
    d.set_lock("After 10 minutes".into());
    d.set_lock_on_sleep(true);
    d.set_buttons(list(vec![
        info("Power button", "Ask what to do"),
        info("Closing the lid", "Suspend"),
    ]));
    if variant == "battery" {
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
        layouts.push(item("fr", "French (Rwanda, with a very long variant name)", "fr", "keyboard", 0));
        layouts.push(item("de", "German", "de", "keyboard", 0));
        d.set_several(true);
        d.set_switch("Alt + Shift".into());
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

fn about(w: &SettingsWindow) {
    let d = w.global::<AboutData>();
    d.set_version("0.8.0".into());
    d.set_system(list(vec![
        info("Version", "0.8.0"),
        info("Kernel", "6.12.57+deb13-amd64"),
        info("Disk", "62 GB free of 238 GB"),
    ]));
    d.set_hardware(list(vec![
        info("Computer", "ASUSTeK COMPUTER INC. UX410UQK"),
        info("Processor", "Intel Core i5-7200U (2 cores)"),
        info("Memory", "15.5 GB"),
        info("Graphics", "NVIDIA GeForce 940MX"),
    ]));
    d.set_memory_used("312 MB in use".into());
}

#[test]
fn every_page() {
    harness::install();
    let scenes: Vec<Scene> = vec![
        ("network", "connected", Box::new(|w| network(w, "on"))),
        ("network", "many", Box::new(|w| network(w, "many"))),
        ("network", "off", Box::new(|w| network(w, "off"))),
        ("bluetooth", "on", Box::new(|w| bluetooth(w, "on"))),
        ("bluetooth", "blocked", Box::new(|w| bluetooth(w, "blocked"))),
        ("sound", "on", Box::new(|w| sound(w, "on"))),
        ("sound", "none", Box::new(|w| sound(w, "none"))),
        ("display", "one", Box::new(|w| display(w, "one"))),
        ("display", "two", Box::new(|w| display(w, "two"))),
        ("power", "desktop", Box::new(|w| power(w, "desktop"))),
        ("power", "battery", Box::new(|w| power(w, "battery"))),
        ("date-time", "on", Box::new(datetime)),
        ("keyboard", "one", Box::new(|w| keyboard(w, "one"))),
        ("keyboard", "several", Box::new(|w| keyboard(w, "several"))),
        ("mouse", "on", Box::new(pointer)),
        ("notifications", "on", Box::new(|w| {
            let d = w.global::<NotificationsData>();
            d.set_available(true);
        })),
        ("notifications", "off", Box::new(|w| {
            let d = w.global::<NotificationsData>();
            d.set_note("The notification service is not running.".into());
        })),
        ("about", "on", Box::new(about)),
    ];
    for (page, variant, fill) in &scenes {
        for (width, height) in SIZES {
            let w = SettingsWindow::new().unwrap();
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
    let w = SettingsWindow::new().unwrap();
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

    let w = SettingsWindow::new().unwrap();
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

    let w = SettingsWindow::new().unwrap();
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

    let w = SettingsWindow::new().unwrap();
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

    let w = SettingsWindow::new().unwrap();
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
                let mut i = item(&format!("z{n}"), &format!("Africa/City number {n}"), "UTC+2", "", 0);
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
