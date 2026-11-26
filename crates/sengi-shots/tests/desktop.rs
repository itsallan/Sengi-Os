//! The bar, the dock, quick settings, notifications, the Store and the
//! launcher, with realistic and awkward data.

use sengi_shots::harness::{self, BACKDROP};
use sengi_shots::{bar, dock, launcher, notify, quick, store};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

fn list<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn swatch(size: u32, rgb: [u8; 3]) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(size, size);
    for pixel in buffer.make_mut_slice() {
        *pixel = Rgba8Pixel {
            r: rgb[0],
            g: rgb[1],
            b: rgb[2],
            a: 255,
        };
    }
    Image::from_rgba8(buffer)
}

fn show<T: ComponentHandle>(w: &T, width: u32, height: u32, name: &str, backdrop: [u8; 3]) {
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, width, height);
    harness::advance(400);
    harness::draw(&win, backdrop).save(name);
    w.hide().unwrap();
}

fn bars() {
    for variant in ["plain", "tray", "many", "low", "offline"] {
        for width in [800u32, 1366] {
            let w = bar::themed(bar::Bar::new().unwrap());
            w.set_clock("Tue 25 Nov  9:41".into());
            w.set_network("Home".into());
            w.set_wifi(true);
            w.set_wifi_name("Home".into());
            w.set_online(true);
            w.set_volume("72%".into());
            w.set_battery("64%".into());
            w.set_output_width(width as f32);
            let entry =
                |id: i32, letter: &str, rgb: Option<[u8; 3]>, attention: bool| bar::TrayEntry {
                    id,
                    icon: rgb.map(|c| swatch(16, c)).unwrap_or_default(),
                    has_icon: rgb.is_some(),
                    letter: letter.into(),
                    label: format!("Tray app {id}").into(),
                    attention,
                };
            match variant {
                "tray" => w.set_tray(list(vec![
                    entry(1, "S", Some([200, 80, 60]), false),
                    entry(2, "D", Some([60, 160, 90]), true),
                    entry(3, "T", None, false),
                ])),
                "many" => w.set_tray(list(
                    (0..14)
                        .map(|n| {
                            entry(
                                n,
                                "A",
                                if n % 2 == 0 {
                                    Some([70, 100, 200])
                                } else {
                                    None
                                },
                                false,
                            )
                        })
                        .collect(),
                )),
                "low" => {
                    w.set_battery("9%".into());
                    w.set_battery_low(true);
                    w.set_muted(true);
                    w.set_volume("Muted".into());
                }
                "offline" => {
                    w.set_network("Offline".into());
                    w.set_wifi(false);
                    w.set_online(false);
                    w.set_battery("19%".into());
                    w.set_battery_warning(true);
                }
                _ => {}
            }
            show(
                &w,
                width,
                28,
                &format!("bar-{variant}-{width}.png"),
                BACKDROP,
            );
        }
    }
}

fn tray_menu() {
    let row = |id: i32, label: &str| bar::MenuRow {
        id,
        label: label.into(),
        enabled: true,
        ..Default::default()
    };
    let mut rows = vec![row(1, "Open Signal"), row(2, "Mute notifications")];
    rows[1].check = true;
    rows[1].on = true;
    let mut sep = row(3, "");
    sep.separator = true;
    rows.push(sep);
    let mut off = row(4, "Disabled item");
    off.enabled = false;
    rows.push(off);
    let mut sub = row(5, "More options with a long label that goes on and on");
    sub.submenu = true;
    rows.push(sub);
    let mut radio = row(6, "Away");
    radio.radio = true;
    radio.on = true;
    rows.push(radio);
    rows.push(row(7, "Quit"));
    let w = bar::themed(bar::TrayMenu::new().unwrap());
    w.set_panels(list(vec![bar::MenuPanel {
        rows: list(rows),
        marks: true,
        open_id: -1,
        selected_id: 1,
        request_id: -1,
        left: 900.0,
        right: 920.0,
        top: 28.0,
    }]));
    show(&w, 1366, 400, "traymenu-1366.png", BACKDROP);
}

fn docks() {
    for (variant, n) in [("five", 5usize), ("two", 2), ("twelve", 12)] {
        let w = dock::themed(dock::Dock::new().unwrap());
        let mut items = vec![
            dock::DockItem {
                name: "Search".into(),
                icon: dock::DockIcon::Search,
                present: true,
                ..Default::default()
            },
            dock::DockItem {
                name: "Terminal".into(),
                icon: dock::DockIcon::Terminal,
                present: true,
                running: true,
                focused: true,
                windows: 2,
                ..Default::default()
            },
        ];
        let colours = [[200, 80, 60], [60, 160, 90], [70, 100, 200], [220, 180, 60]];
        for k in 2..n {
            let mut item = dock::DockItem {
                name: format!("Application {k}").into(),
                present: true,
                letter: "Q".into(),
                ..Default::default()
            };
            if k % 3 == 0 {
                item.icon = dock::DockIcon::Letter;
            } else {
                item.icon = dock::DockIcon::Image;
                item.image = swatch(40, colours[k % 4]);
            }
            item.running = k == 2;
            item.launching = k == 4;
            items.push(item);
        }
        w.set_items(list(items));
        show(&w, 1366, 120, &format!("dock-{variant}-1366.png"), BACKDROP);
    }
}

/// A right click on a tile opens the menu above it, and it goes when the
/// pointer has left it.
fn dock_menu() {
    use slint::platform::{PointerEventButton, WindowEvent};
    use slint::LogicalPosition;

    let w = dock::themed(dock::Dock::new().unwrap());
    let tile = |name: &str, k: usize| dock::DockItem {
        name: name.into(),
        icon: dock::DockIcon::Image,
        image: swatch(40, [[200, 80, 60], [60, 160, 90], [70, 100, 200]][k % 3]),
        present: true,
        running: k == 1,
        ..Default::default()
    };
    w.set_items(list(vec![
        dock::DockItem {
            name: "Search".into(),
            icon: dock::DockIcon::Search,
            present: true,
            ..Default::default()
        },
        tile("Files", 0),
        tile("Notes", 1),
        tile("Mail", 2),
    ]));
    w.on_context_clicked({
        let weak = w.as_weak();
        move |index| {
            let w = weak.upgrade().unwrap();
            let rows: Vec<slint::SharedString> = ["Open", "Remove from Dock"]
                .into_iter()
                .map(Into::into)
                .collect();
            w.set_menu_rows(list(rows));
            w.set_menu_index(index);
        }
    });
    let (width, height) = (w.get_dock_width(), w.get_dock_height());
    w.show().unwrap();
    let win = harness::window();
    harness::resize(&win, width.ceil() as u32, height.ceil() as u32);
    harness::advance(400);

    let slot = w.get_slot_width();
    let x = w.get_side_padding() + slot * 2.5;
    let y = height - w.get_panel_height() / 2.0;
    let at = LogicalPosition::new(x, y);
    win.dispatch_event(WindowEvent::PointerMoved { position: at });
    win.dispatch_event(WindowEvent::PointerPressed {
        position: at,
        button: PointerEventButton::Right,
    });
    win.dispatch_event(WindowEvent::PointerReleased {
        position: at,
        button: PointerEventButton::Right,
    });
    harness::advance(400);
    assert_eq!(w.get_menu_index(), 2, "the menu opens on the tile");
    assert!(w.get_menu_height() > 0.0);
    let inside = harness::draw(&win, BACKDROP);
    inside.save("dock-menu-1366.png");
    // The menu sits above the panel, over the tile that was clicked.
    let menu_y =
        (height - w.get_panel_height() - w.get_menu_gap() - w.get_menu_height() / 2.0) as usize;
    let menu_x = (w.get_menu_x() + w.get_menu_width() / 2.0) as usize;
    assert_ne!(inside.at(menu_x, menu_y), BACKDROP, "the menu is drawn");

    // Away from the menu and its tile it closes by itself.
    let away = LogicalPosition::new(2.0, 2.0);
    win.dispatch_event(WindowEvent::PointerMoved { position: away });
    harness::advance(600);
    assert_eq!(
        w.get_menu_index(),
        -1,
        "the menu closes when the pointer leaves"
    );
    w.hide().unwrap();
}

fn card(
    serial: i32,
    app: &str,
    summary: &str,
    body: &str,
    critical: bool,
    actions: &[(&str, &str)],
) -> notify::Card {
    notify::Card {
        serial,
        app: app.into(),
        time: "9:41".into(),
        summary: summary.into(),
        body: body.into(),
        critical,
        actions: list(
            actions
                .iter()
                .map(|(k, l)| notify::ActionItem {
                    key: (*k).into(),
                    label: (*l).into(),
                })
                .collect(),
        ),
        leaving: false,
    }
}

fn history(n: usize) -> ModelRc<notify::HistoryItem> {
    list(
        (0..n)
            .map(|k| notify::HistoryItem {
                key: k as i32,
                app: ["Files", "Calendar", "Battery"][k % 3].into(),
                time: format!("9:{:02}", 40 - k).into(),
                summary: if k == 1 {
                    "A very long summary line that cannot possibly fit in the width of one notification".into()
                } else {
                    format!("Summary {k}").into()
                },
                body: if k % 2 == 0 {
                    "Some body text that wraps onto a second line when the card is narrow enough to need it.".into()
                } else {
                    "".into()
                },
                critical: k == 2,
                ..Default::default()
            })
            .collect(),
    )
}

fn notifications() {
    let w = notify::themed(notify::NotifyWindow::new().unwrap());
    w.set_cards(list(vec![
        card(
            1,
            "Files",
            "Copy finished",
            "report.pdf was copied to Documents.",
            false,
            &[],
        ),
        card(
            2,
            "Calendar",
            "Meeting in 5 minutes with a title that is really quite long indeed",
            "Team stand-up, room 4, bring the notes from last week and a pen.",
            false,
            &[("open", "Open"), ("snooze", "Snooze")],
        ),
        card(
            3,
            "Battery",
            "Battery low",
            "9% left. Plug in the charger.",
            true,
            &[("settings", "Power settings")],
        ),
    ]));
    w.set_queued(2);
    show(&w, 400, 480, "notify-cards-400x480.png", BACKDROP);
    for (n, label) in [(0usize, "empty"), (3, "three"), (12, "many")] {
        let w = notify::themed(notify::NotifyWindow::new().unwrap());
        w.set_history(history(n));
        w.set_centre_open(true);
        w.set_dnd(n == 3);
        w.set_centre_focus(if n == 3 { 2 } else { -1 });
        show(
            &w,
            1366,
            768,
            &format!("notify-centre-{label}-1366x768.png"),
            BACKDROP,
        );
    }
}

fn nets() -> Vec<quick::NetworkItem> {
    let n = |id: &str, name: &str, secured: bool, known: bool, level: i32| quick::NetworkItem {
        id: id.into(),
        name: name.into(),
        secured,
        known,
        level,
        ..Default::default()
    };
    let mut list = vec![
        n("a", "Home", true, true, 4),
        n(
            "b",
            "Neighbour with an extremely long network name that goes on",
            true,
            false,
            3,
        ),
        n("c", "Cafe Guest", false, false, 2),
        n("d", "Old router", true, true, 1),
    ];
    list[2].confirm = true;
    list[3].busy = true;
    list
}

fn quicks() {
    let scenes = ["full", "networks", "passphrase", "off", "empty", "low"];
    for variant in scenes {
        for (width, height) in [(800u32, 600u32), (1366, 768)] {
            let w = quick::themed(quick::QuickWindow::new().unwrap());
            w.set_wifi_enabled(true);
            w.set_wifi_on(true);
            w.set_wifi_detail("Home".into());
            w.set_wifi_level(4);
            w.set_bluetooth_enabled(true);
            w.set_bluetooth_on(true);
            w.set_bluetooth_detail("1 device connected".into());
            w.set_volume_enabled(true);
            w.set_volume(72);
            w.set_volume_label("72%".into());
            w.set_brightness_visible(true);
            w.set_brightness(60);
            w.set_brightness_label("60%".into());
            w.set_dnd_visible(true);
            w.set_dnd_switch(true);
            w.set_battery_visible(true);
            w.set_battery_percent(64);
            w.set_battery_label("64%".into());
            w.set_battery_detail("3 hours 12 minutes left".into());
            match variant {
                "networks" => {
                    w.set_networks(list(nets()));
                    w.set_network_current(1);
                    w.set_network_confirming(2);
                    w.set_link_text("Wired, connected".into());
                }
                "passphrase" => {
                    w.set_networks(list(nets()));
                    w.set_prompt_open(true);
                    w.set_prompt_network(
                        "Neighbour with an extremely long network name that goes on".into(),
                    );
                    w.set_prompt_problem("Wrong passphrase".into());
                }
                "off" => {
                    w.set_wifi_on(false);
                    w.set_wifi_detail("Off".into());
                    w.set_bluetooth_on(false);
                    w.set_bluetooth_detail("Off".into());
                    w.set_muted(true);
                    w.set_dnd_on(true);
                }
                "empty" => {
                    w.set_wifi_enabled(false);
                    w.set_wifi_on(false);
                    w.set_wifi_detail("No Wi-Fi adapter".into());
                    w.set_bluetooth_enabled(false);
                    w.set_bluetooth_detail("Not available".into());
                    w.set_volume_enabled(false);
                    w.set_volume_label("No sound device".into());
                    w.set_brightness_visible(false);
                    w.set_battery_visible(false);
                }
                "low" => {
                    w.set_battery_percent(9);
                    w.set_battery_low(true);
                    w.set_battery_label("9%".into());
                    w.set_battery_detail("Plug in the charger".into());
                    w.set_wifi_note("No networks in range".into());
                    w.set_wifi_alert("Could not join Home: the passphrase was refused".into());
                }
                _ => {}
            }
            show(
                &w,
                width,
                height,
                &format!("quick-{variant}-{width}x{height}.png"),
                BACKDROP,
            );
        }
    }
}

fn stores() {
    let row =
        |name: &str, summary: &str, state: store::RowState, rgb: Option<[u8; 3]>| store::Row {
            id: name.into(),
            name: name.into(),
            summary: summary.into(),
            icon: rgb.map(|c| swatch(40, c)).unwrap_or_default(),
            has_icon: rgb.is_some(),
            letter: name.chars().next().unwrap().to_string().into(),
            state,
            percent: -1,
            detail: "".into(),
            heading: false,
            unavailable: false,
        };
    use store::RowState::*;
    for variant in ["results", "empty", "error", "many"] {
        for (width, height) in [(800u32, 600u32), (1024, 768), (1920, 1080)] {
            let w = store::themed(store::StoreWindow::new().unwrap());
            let g = w.global::<store::Store>();
            match variant {
                "results" | "many" => {
                    let mut rows = vec![
                        row("Firefox", "Fast, private and safe web browser from Mozilla", Available, Some([200, 80, 60])),
                        row("GIMP", "Create images and edit photographs with a very long summary that keeps on going past the end of the row", Installed, Some([60, 160, 90])),
                        row("Inkscape", "Vector graphics", Installing, None),
                        row("A rather long application name that will not fit on one row of the list", "Short", Failed, Some([70, 100, 200])),
                        row("VLC", "", Available, None),
                    ];
                    rows[2].percent = 45;
                    rows[2].detail = "Installing 2 of 4 \u{b7} 12.3 MB \u{b7} 4.1 MB/s".into();
                    if variant == "many" {
                        for n in 0..20 {
                            rows.push(row(
                                &format!("App {n}"),
                                "Does something useful",
                                Available,
                                None,
                            ));
                        }
                    }
                    g.set_rows(list(rows));
                    g.set_message("Installing Inkscape…".into());
                }
                "empty" => {
                    g.set_hint("No app matches “zzzz”. Try another word.".into());
                }
                _ => {
                    g.set_hint("Flathub could not be reached.".into());
                    g.set_message("Installing Inkscape failed: no network".into());
                    g.set_message_is_error(true);
                }
            }
            show(
                &w,
                width,
                height,
                &format!("store-{variant}-{width}x{height}.png"),
                BACKDROP,
            );
        }
    }
}

/// The start page: the recommended apps by group, with Flathub reached
/// and without it.
fn featured_stores() {
    let labels: Vec<slint::SharedString> = {
        [
            "All",
            "Featured",
            "Browsers",
            "Office",
            "Media",
            "Graphics",
            "Communication",
            "Utilities",
            "Development",
            "Screen recording",
            "Notes",
        ]
        .iter()
        .map(|c| c.to_string().into())
        .collect()
    };
    use store::RowState::*;
    let row =
        |name: &str, summary: &str, state: store::RowState, rgb: Option<[u8; 3]>| store::Row {
            id: name.into(),
            name: name.into(),
            summary: summary.into(),
            icon: rgb.map(|c| swatch(40, c)).unwrap_or_default(),
            has_icon: rgb.is_some(),
            letter: name.chars().next().unwrap().to_string().into(),
            state,
            percent: -1,
            detail: "".into(),
            heading: false,
            unavailable: false,
        };
    let heading = |name: &str| store::Row {
        heading: true,
        unavailable: true,
        ..row(name, "", Available, None)
    };
    for variant in ["featured", "offline", "group"] {
        for (width, height) in [(888u32, 600u32), (720, 480)] {
            let w = store::themed(store::StoreWindow::new().unwrap());
            let g = w.global::<store::Store>();
            g.set_chips(list(labels.clone()));
            g.set_chips_first(list(labels[..6].to_vec()));
            g.set_chips_second(list(labels[6..].to_vec()));
            let mut rows = match variant {
                "featured" => vec![
                    heading("Browsers"),
                    row(
                        "Firefox",
                        "Fast, Private & Safe Web Browser",
                        Available,
                        Some([220, 100, 40]),
                    ),
                    row(
                        "Chromium",
                        "The open-source web browser behind Chrome",
                        Installed,
                        Some([70, 110, 200]),
                    ),
                    heading("Office"),
                    row(
                        "LibreOffice",
                        "Comprehensive, professional-quality productivity suite",
                        Available,
                        Some([60, 160, 90]),
                    ),
                    heading("Media"),
                    row(
                        "VLC",
                        "Play any video or audio file",
                        Installing,
                        Some([230, 140, 30]),
                    ),
                    row(
                        "mpv",
                        "A free, open source, and cross-platform media player",
                        Available,
                        None,
                    ),
                    heading("Graphics"),
                    row(
                        "GIMP",
                        "Create images and edit photographs",
                        Available,
                        Some([150, 120, 90]),
                    ),
                    row(
                        "Inkscape",
                        "Vector graphics editor",
                        Available,
                        Some([90, 90, 90]),
                    ),
                    row("Krita", "Digital painting", Failed, None),
                ],
                "group" => vec![
                    heading("Screen recording"),
                    row(
                        "OBS Studio",
                        "Live streaming and screen recording",
                        Available,
                        Some([60, 60, 70]),
                    ),
                    row("Kooha", "Elegantly record your screen", Available, None),
                ],
                _ => {
                    let mut rows = vec![heading("Browsers")];
                    for name in ["Firefox", "Chromium"] {
                        rows.push(row(name, "", Available, None));
                    }
                    rows.push(heading("Office"));
                    rows.push(row("LibreOffice", "", Available, None));
                    rows.push(heading("Media"));
                    for name in ["VLC", "mpv"] {
                        rows.push(row(name, "", Available, None));
                    }
                    for r in &mut rows {
                        r.unavailable = true;
                    }
                    rows
                }
            };
            if variant == "featured" {
                rows[6].percent = 45;
                rows[6].detail = "Installing 2 of 4 \u{b7} 12.3 MB".into();
                g.set_message("Installing VLC".into());
            }
            g.set_current(1);
            g.set_rows(list(rows));
            if variant == "offline" {
                g.set_notice(
                    "Not connected to Flathub. These apps can be installed once you are online."
                        .into(),
                );
            }
            if variant == "group" {
                g.set_chip(9);
            }
            show(
                &w,
                width,
                height,
                &format!("store-{variant}-{width}x{height}.png"),
                BACKDROP,
            );
        }
    }
}

fn launchers() {
    let seg = |text: &str, matched: bool| launcher::Segment {
        text: text.into(),
        matched,
    };
    let rows: Vec<launcher::Row> = (0..24)
        .map(|n| launcher::Row {
            name: list(vec![
                seg("Fi", true),
                seg(&format!("refox number {n} with a long name"), false),
            ]),
            detail: if n % 2 == 0 {
                "Web Browser".into()
            } else {
                "".into()
            },
            title: format!("Firefox {n}").into(),
            icon: if n % 3 != 0 {
                swatch(48, [200, 80, 60])
            } else {
                Image::default()
            },
            has_icon: n % 3 != 0,
            letter: "F".into(),
        })
        .collect();
    for variant in ["grid", "list", "menu", "empty", "error"] {
        let w = launcher::themed(launcher::LauncherWindow::new().unwrap());
        w.set_grid_capable(true);
        match variant {
            "grid" => {
                w.set_grid(true);
                w.set_icons(true);
                w.set_rows(list(rows[..24].to_vec()));
            }
            "list" => {
                w.set_icons(true);
                w.set_selected(2);
                w.set_rows(list(rows[..8].to_vec()));
                w.set_total_rows(20);
                w.set_query("fi".into());
            }
            "menu" => {
                w.set_compact(true);
                w.set_grid_capable(false);
                w.set_prompt("".into());
                w.set_rows(list(rows[..4].to_vec()));
            }
            "empty" => {
                w.set_query("zzz".into());
            }
            _ => {
                w.set_rows(list(rows[..3].to_vec()));
                w.set_error("Could not start Firefox 0: no such file".into());
            }
        }
        show(
            &w,
            1280,
            720,
            &format!("launcher-{variant}-1280x720.png"),
            BACKDROP,
        );
    }
    let w = launcher::themed(launcher::LauncherWindow::new().unwrap());
    w.set_grid_capable(true);
    w.set_grid(true);
    w.set_icons(true);
    w.set_rows(list(rows[..24].to_vec()));
    show(&w, 800, 600, "launcher-grid-800x600.png", BACKDROP);
}

#[test]
fn desktop() {
    harness::install();
    bars();
    tray_menu();
    docks();
    dock_menu();
    notifications();
    quicks();
    stores();
    featured_stores();
    launchers();
}
