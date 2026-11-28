use super::*;
use crate::flatpak::testing::{quick, Fake};
use crate::icons::testing::appstream;
use std::fs;

const LISTING: &str = "org.gimp.GIMP\\tGIMP\\tImages\\norg.x.Y\\tY\\t\\n";

/// Firefox, GIMP (named as Flathub names it) and something that is not
/// recommended.
const FLATHUB: &str = "org.mozilla.firefox\\tFirefox\\tWeb browser\\n\
    org.gimp.GIMP\\tGNU Image Manipulation Program\\tImages\\n\
    org.x.Other\\tOther\\t\\n";

const REMOTES: &str = "*remotes*) printf 'flathub\\thttps://dl.flathub.org/repo/\\tuser\\n';;";

/// What flatpak said on a laptop that was not connected.
const NO_NAMES: &str = "echo 'error: Unable to load summary from remote flathub: While fetching \
    https://dl.flathub.org/repo/summary.idx: [6] Could not resolve hostname' >&2; exit 1";

/// A 2 by 2 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72, 0xb6, 0x0d,
    0x24, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x38, 0x61, 0xa3, 0xf1,
    0x1f, 0x84, 0x19, 0x60, 0x0c, 0x00, 0x4c, 0xfc, 0x08, 0xad, 0xb3, 0xb5, 0x7d, 0xd1, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// A fake flatpak that answers like the real one for what the store asks,
/// and fails the install if `install_exit` is not 0.
fn fake(tag: &str, install_exit: u8, extra: &str) -> Fake {
    Fake::new(
        tag,
        &format!(
            "case \"$*\" in\n{REMOTES}\n\
             *remote-ls*) printf '{LISTING}';;\n\
             *\" install \"*) echo 'error: no space left on device' >&2; exit {install_exit};;\n\
             {extra}\n\
             esac"
        ),
    )
}

fn flathub_fake(tag: &str) -> Fake {
    Fake::new(
        tag,
        &format!("case \"$*\" in\n{REMOTES}\n*remote-ls*) printf '{FLATHUB}';;\nesac"),
    )
}

fn offline_fake(tag: &str) -> Fake {
    Fake::new(
        tag,
        &format!("case \"$*\" in\n{REMOTES}\n*) {NO_NAMES};;\nesac"),
    )
}

/// The real decoding, in this process: the test binary is not a decoder.
fn decode() -> Decode {
    Box::new(sengi_apps::icons::load)
}

fn context(fake: &Fake) -> Shared {
    let mut context = Context::new(fake.flatpak(quick()), fake.dir.join("data"), None, decode());
    context.proc_net = fake.dir.join("net");
    Arc::new(context)
}

fn wait_until(shared: &Shared, done: impl Fn(&Data) -> bool) {
    for _ in 0..400 {
        if done(&shared.lock()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("it did not come to pass");
}

fn names(data: &Data) -> Vec<String> {
    view(data, None).cards.into_iter().map(|c| c.name).collect()
}

fn connect(fake: &Fake) {
    let net = fake.dir.join("net");
    fs::create_dir_all(&net).unwrap();
    fs::write(
        net.join("route"),
        "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
         wlan0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\n",
    )
    .unwrap();
}

/// Flatpak's appstream tree for `ids`, with real pictures in it.
fn unpack(shared: &Shared, commit: &str, ids: &[&str]) {
    let icons: Vec<(&str, &str)> = ids.iter().map(|id| ("128x128", *id)).collect();
    let folder = appstream(&shared.data_home, commit, &icons);
    for id in ids {
        fs::write(folder.join(format!("icons/128x128/{id}.png")), PNG).unwrap();
    }
}

fn has_icon(data: &Data, id: &str) -> bool {
    view(data, None)
        .cards
        .iter()
        .any(|c| c.id == id && c.icon.is_some())
}

#[test]
fn load_fills_the_catalog() {
    let fake = fake("app-load", 0, "");
    let shared = context(&fake);
    load(shared.clone());
    let data = shared.lock();
    assert_eq!(data.catalog.len(), 2);
    assert!(!data.loading);
    assert!(!data.message.error);
    assert!(view(&data, None).banner.is_empty());
}

#[test]
fn featured_apps_get_their_icons_when_the_appstream_data_arrives() {
    let fake = offline_fake("app-icons-late");
    let shared = context(&fake);
    load(shared.clone());
    wait_until(&shared, |d| !d.decoding);
    {
        let data = shared.lock();
        let v = view(&data, None);
        assert!(v.cards.iter().all(|c| c.icon.is_none()));
        assert!(v.cards.iter().all(|c| !c.glyph.is_empty()));
    }
    // Flatpak unpacks it later, whoever asked.
    unpack(&shared, "one", &["org.mozilla.firefox", "org.gimp.GIMP"]);
    let mut last = None;
    look(&shared, &mut last, online::LOOK_EVERY);
    wait_until(&shared, |d| {
        has_icon(d, "org.mozilla.firefox") && has_icon(d, "org.gimp.GIMP")
    });
}

#[test]
fn an_icon_missing_from_old_data_is_looked_for_in_new_data() {
    let fake = flathub_fake("app-icons-new");
    let shared = context(&fake);
    unpack(&shared, "one", &["org.gimp.GIMP"]);
    load(shared.clone());
    wait_until(&shared, |d| has_icon(d, "org.gimp.GIMP") && !d.decoding);
    assert!(!has_icon(&shared.lock(), "org.mozilla.firefox"));
    unpack(&shared, "two", &["org.gimp.GIMP", "org.mozilla.firefox"]);
    render(&shared, None);
    wait_until(&shared, |d| has_icon(d, "org.mozilla.firefox"));
    // Search and a category show the same pictures.
    search(&shared, "firefox");
    assert!(has_icon(&shared.lock(), "org.mozilla.firefox"));
    search(&shared, "");
    open_category(&shared, 0);
    wait_until(&shared, |d| has_icon(d, "org.mozilla.firefox"));
}

#[test]
fn icons_arrive_when_the_refresh_deploys_them_and_then_fails() {
    let fake = Fake::new(
        "app-icons-failed-refresh",
        &format!(
            "case \"$*\" in\n{REMOTES}\n\
             *--appstream*) d=\"$(dirname \"$0\")/data/flatpak/appstream/flathub/x86_64\"\n\
             mkdir -p \"$d/c/icons/128x128\"; ln -sfn c \"$d/active\"\n\
             cp \"$(dirname \"$0\")/png\" \"$d/c/icons/128x128/org.mozilla.firefox.png\"\n\
             sleep 0.3; exit 1;;\n\
             *remote-ls*) printf '{FLATHUB}';;\n\
             esac"
        ),
    );
    fs::write(fake.dir.join("png"), PNG).unwrap();
    let shared = context(&fake);
    load(shared.clone());
    wait_until(&shared, |d| has_icon(d, "org.mozilla.firefox"));
}

#[test]
fn the_icon_size_follows_the_screen() {
    let fake = flathub_fake("app-scale");
    let shared = context(&fake);
    unpack(&shared, "one", &["org.mozilla.firefox"]);
    load(shared.clone());
    wait_until(&shared, |d| {
        d.icons
            .get("org.mozilla.firefox")
            .is_some_and(|p| p.size == 48)
    });
    scaled(&shared, 1.5);
    wait_until(&shared, |d| {
        d.icons
            .get("org.mozilla.firefox")
            .is_some_and(|p| p.size == 72)
    });
    scaled(&shared, 4.0);
    assert_eq!(shared.lock().icon_pixels, MAX_ICON_PIXELS);
}

#[test]
fn offline_there_is_a_calm_banner_and_no_error_line() {
    let fake = offline_fake("app-offline-banner");
    let shared = context(&fake);
    load(shared.clone());
    let data = shared.lock();
    let v = view(&data, None);
    assert_eq!(
        v.banner,
        "You\u{2019}re offline. Connect to the internet to browse apps."
    );
    assert!(v.banner_retry && !v.banner_busy);
    assert!(v.message.text.is_empty() && !v.message.error);
    assert_eq!(v.tiles.len(), data.featured.groups.len());
    assert!(v.cards.len() >= 8);
    assert!(v
        .cards
        .iter()
        .all(|c| c.unavailable && c.summary.is_empty()));
    assert!(v.cards.iter().any(|c| c.name == "Firefox"));
    drop(data);
    install(&shared, "org.mozilla.firefox");
    std::thread::sleep(Duration::from_millis(100));
    assert!(!fake.calls().iter().any(|c| c.contains(" install ")));
}

#[test]
fn the_list_is_asked_for_again_when_the_network_comes_back() {
    let fake = Fake::new(
        "app-comeback",
        &format!(
            "[ -e \"$(dirname \"$0\")/net/route\" ] || {{ case \"$*\" in\n{REMOTES}\n*) {NO_NAMES};;\nesac; exit 0; }}\n\
             case \"$*\" in\n{REMOTES}\n*remote-ls*) printf '{FLATHUB}';;\nesac"
        ),
    );
    let shared = context(&fake);
    load(shared.clone());
    let mut last = None;
    let calls = fake.calls().len();
    for _ in 0..20 {
        look(&shared, &mut last, online::LOOK_EVERY);
    }
    assert_eq!(fake.calls().len(), calls, "asked without a network");
    connect(&fake);
    look(&shared, &mut last, online::LOOK_EVERY);
    wait_until(&shared, |d| !d.loading && !d.catalog.is_empty());
    let data = shared.lock();
    assert!(data.problem.is_none());
    assert!(view(&data, None).banner.is_empty());
}

#[test]
fn a_network_that_is_there_but_fails_is_not_asked_at_every_look() {
    let fake = offline_fake("app-throttle");
    connect(&fake);
    let shared = context(&fake);
    load(shared.clone());
    let calls = fake.calls().len();
    let mut last = None;
    look(&shared, &mut last, online::LOOK_EVERY);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fake.calls().len(), calls);
}

#[test]
fn retry_asks_at_once_and_says_it_is_busy() {
    let fake = offline_fake("app-retry");
    let shared = context(&fake);
    load(shared.clone());
    let calls = fake.calls().len();
    retry_now(&shared);
    wait_until(&shared, |d| !d.loading && fake.calls().len() > calls);
    let mut data = shared.lock();
    data.loading = true;
    let v = view(&data, None);
    assert!(v.banner_busy && !v.banner_retry);
}

#[test]
fn a_list_from_last_time_is_kept_when_the_refresh_cannot_reach_flathub() {
    let fake = Fake::new(
        "app-stale",
        &format!(
            "case \"$*\" in\n{REMOTES}\n\
             *--cached*) printf '{FLATHUB}';;\n\
             *--appstream*) {NO_NAMES};;\n\
             esac"
        ),
    );
    let shared = context(&fake);
    unpack(&shared, "old", &["org.mozilla.firefox"]);
    load(shared.clone());
    wait_until(&shared, |d| has_icon(d, "org.mozilla.firefox"));
    let data = shared.lock();
    let v = view(&data, None);
    assert!(v.banner.contains("from last time"), "{}", v.banner);
    assert!(v.cards.iter().all(|c| !c.unavailable));
}

#[test]
fn a_failed_install_says_why_in_plain_words_until_the_user_acts() {
    let fake = fake("app-fail", 1, "");
    let shared = context(&fake);
    load(shared.clone());
    install(&shared, "org.gimp.GIMP");
    wait_until(&shared, |d| d.sticky);
    {
        let mut data = shared.lock();
        assert!(data.message.error);
        assert_eq!(
            data.message.text,
            "Could not install GIMP. The disk is full."
        );
        assert_eq!(data.rows.state("org.gimp.GIMP"), State::Failed);
        // A note from a thread of ours does not wipe it out.
        Context::note(&mut data, Message::info(UPDATING));
        assert!(data.message.error);
    }
    search(&shared, "gi");
    let data = shared.lock();
    assert!(!data.sticky && data.message.text.is_empty());
}

#[test]
fn an_install_shows_its_progress_on_the_card_and_then_none() {
    let fake = Fake::new(
        "app-progress",
        &format!(
            "case \"$*\" in\n{REMOTES}\n\
             *remote-ls*) printf 'org.gimp.GIMP\\tGIMP\\tImages\\n';;\n\
             *\" install \"*) sed '/2\\/2.* 44%/q' '{}/../sengi-text/tests/fixtures/install.out'; sleep 1;;\n\
             esac",
            env!("CARGO_MANIFEST_DIR")
        ),
    );
    let shared = context(&fake);
    load(shared.clone());
    install(&shared, "org.gimp.GIMP");
    wait_until(&shared, |d| {
        d.progress.as_ref().is_some_and(|p| p.percent == Some(44))
    });
    search(&shared, "gimp");
    {
        let data = shared.lock();
        let v = view(&data, None);
        let card = v.cards.iter().find(|c| c.id == "org.gimp.GIMP").unwrap();
        let p = card.progress.as_ref().unwrap();
        assert_eq!(p.step, Some((2, 2)));
        assert_eq!(p.detail(), "Step 2 of 2 \u{b7} up to 300.1 MB");
        assert!(p.pace().ends_with(" left"), "{}", p.pace());
    }
    wait_until(&shared, |d| {
        d.rows.state("org.gimp.GIMP") == State::Installed
    });
    assert!(shared.lock().progress.is_none());
}

#[test]
fn open_runs_only_an_installed_app() {
    let fake = fake("app-open", 0, "");
    let shared = context(&fake);
    load(shared.clone());
    open(&shared, "org.gimp.GIMP");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!fake.calls().iter().any(|c| c.starts_with("run ")));
    install(&shared, "org.gimp.GIMP");
    wait_until(&shared, |d| {
        d.rows.state("org.gimp.GIMP") == State::Installed
    });
    open(&shared, "org.gimp.GIMP");
    wait_until(&shared, |_| {
        fake.calls().iter().any(|c| c == "run org.gimp.GIMP")
    });
    assert!(shared.lock().message.text.contains("Opening GIMP"));
}

#[test]
fn an_app_installed_elsewhere_is_not_installed_again() {
    let fake = Fake::new(
        "app-elsewhere",
        &format!(
            "case \"$*\" in\n{REMOTES}\n\
             *remote-ls*) printf 'org.gimp.GIMP\\tGIMP\\tImages\\n';;\n\
             list*) echo org.gimp.GIMP;;\n\
             *\" install \"*) exit 1;;\n\
             esac"
        ),
    );
    let shared = context(&fake);
    shared.lock().catalog = vec![App::new("org.gimp.GIMP", "GIMP", "")];
    install(&shared, "org.gimp.GIMP");
    wait_until(&shared, |d| {
        d.rows.state("org.gimp.GIMP") == State::Installed
    });
    assert!(!fake.calls().iter().any(|c| c.contains(" install ")));
}

#[test]
fn offline_typing_does_not_ask_for_the_list_at_every_key() {
    let fake = offline_fake("app-offline-typing");
    let shared = context(&fake);
    load(shared.clone());
    let calls = fake.calls().len();
    search(&shared, "f");
    wait_until(&shared, |d| !d.loading);
    let after_first = fake.calls().len();
    assert!(after_first > calls);
    for text in ["fi", "fir", "fire"] {
        search(&shared, text);
    }
    open_category(&shared, 0);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fake.calls().len(), after_first);
}

#[test]
fn the_start_page_has_the_categories_and_what_flathub_has_of_each() {
    let fake = flathub_fake("app-start");
    let shared = context(&fake);
    load(shared.clone());
    let data = shared.lock();
    let v = view(&data, None);
    assert!(v.page == StorePage::Start);
    assert_eq!(v.tiles.len(), data.featured.groups.len());
    assert_eq!(v.tiles[0].0, "Browsers");
    assert_eq!(names(&data), ["Firefox", "GNU Image Manipulation Program"]);
    assert_eq!(v.cards[0].summary, "Web browser");
    assert!(v.cards.iter().all(|c| !c.unavailable));
}

#[test]
fn a_category_a_search_and_the_way_back() {
    let fake = flathub_fake("app-pages");
    let shared = context(&fake);
    load(shared.clone());
    open_category(&shared, 0);
    {
        let data = shared.lock();
        let v = view(&data, None);
        assert!(v.page == StorePage::Category);
        assert_eq!(v.title, "Browsers");
        assert!(v.tiles.is_empty());
        assert_eq!(names(&data), ["Firefox"]);
    }
    // A search looks everywhere, also from a category, and the welcome
    // wizard's word finds GIMP by its ID.
    search(&shared, "gimp");
    {
        let data = shared.lock();
        assert!(view(&data, None).page == StorePage::Search);
        assert_eq!(names(&data), ["GNU Image Manipulation Program"]);
    }
    search(&shared, "zzzz");
    assert_eq!(
        view(&shared.lock(), None).hint,
        "No app is named \u{201c}zzzz\u{201d}."
    );
    search(&shared, "");
    assert!(view(&shared.lock(), None).page == StorePage::Category);
    back(&shared);
    assert!(view(&shared.lock(), None).page == StorePage::Start);
    open_category(&shared, 99);
    assert!(view(&shared.lock(), None).page == StorePage::Start);
}

#[test]
fn the_keys_move_through_the_tiles_and_then_the_cards() {
    let fake = flathub_fake("app-keys");
    let shared = context(&fake);
    load(shared.clone());
    let tiles = shared.lock().featured.groups.len() as i32;
    let go = |from, key| step(&shared, from, key, 3, 2);
    assert_eq!(go(-1, 3), 0);
    assert_eq!(go(0, 1), 1);
    assert_eq!(go(0, 3), 3);
    // Down from the last row of tiles into the cards, and back.
    assert_eq!(go(tiles - 3, 3), tiles);
    assert_eq!(go(tiles, 2), tiles - 3);
    assert_eq!(go(tiles + 1, 1), tiles + 1);
    assert_eq!(go(0, 2), -1);
    // A category has cards only.
    open_category(&shared, 0);
    assert_eq!(go(0, 3), 0);
    assert_eq!(go(0, 2), -1);
    assert_eq!(go(0, 9), 0);
}

#[test]
fn a_source_that_is_not_flathub_stops_the_load() {
    let fake = Fake::new(
        "app-bad-remote",
        "case \"$*\" in *remotes*) printf 'flathub\\thttp://evil/\\tuser\\n';; esac",
    );
    let shared = context(&fake);
    load(shared.clone());
    let data = shared.lock();
    assert_eq!(data.problem, Some(Problem::Source));
    assert!(data.catalog.is_empty());
    assert!(!view(&data, None).banner.contains("offline"));
    assert!(!fake.calls().iter().any(|c| c.contains("remote-ls")));
}

const GIMP: &str = "org.gimp.GIMP";
const OWN_GIMP: &str = "org.gimp.GIMP\tuser\n";

/// GIMP and Y on the remote, `list` printing the file `installed` in the
/// fake's folder, which is `$d`, and one branch of GIMP to remove; `extra`
/// answers the rest.
fn removing_fake(tag: &str, installed: &str, extra: &str) -> Fake {
    let fake = Fake::new(
        tag,
        &format!(
            "d=\"$(dirname \"$0\")\"\n\
             case \"$*\" in\n{REMOTES}\n\
             *remote-ls*) printf '{LISTING}';;\n\
             list*) cat \"$d/installed\";;\n\
             *--columns=ref) echo org.gimp.GIMP/x86_64/stable;;\n\
             {extra}\n\
             esac"
        ),
    );
    fs::write(fake.dir.join("installed"), installed).unwrap();
    fake
}

/// What takes GIMP off the list, as removing it does.
const REMOVES: &str = "*\"--app org.gimp.GIMP/x86_64/stable\") : >\"$d/installed\";;";

fn called(fake: &Fake, part: &str) -> bool {
    fake.calls().iter().any(|c| c.contains(part))
}

/// Presses Remove and waits for the question.
fn asked(shared: &Shared) {
    ask_remove(shared, GIMP);
    wait_until(shared, |d| d.asking.is_some());
}

#[test]
fn a_cancelled_install_is_no_error_and_may_be_tried_again() {
    let fake = removing_fake(
        "app-cancel",
        "",
        "*\" install \"*) [ -e \"$d/again\" ] && exit 0; touch \"$d/again\"; exec sleep 30;;",
    );
    let shared = context(&fake);
    load(shared.clone());
    install(&shared, GIMP);
    wait_until(&shared, |_| fake.dir.join("again").exists());
    // Another app's Cancel does nothing.
    cancel(&shared, "org.x.Y");
    assert!(!view(&shared.lock(), None)
        .cards
        .iter()
        .any(|c| c.cancelling));
    cancel(&shared, GIMP);
    wait_until(&shared, |d| d.rows.state(GIMP) == State::Available);
    {
        let data = shared.lock();
        assert!(!data.message.error && !data.sticky);
        assert_eq!(data.message.text, "Cancelled. GIMP was not installed.");
        assert!(data.stop.is_none() && data.progress.is_none());
    }
    install(&shared, GIMP);
    wait_until(&shared, |d| d.rows.state(GIMP) == State::Installed);
    assert!(shared.lock().rows.may_remove(GIMP));
}

#[test]
fn a_cancel_that_comes_too_late_says_the_app_is_there() {
    let fake = removing_fake(
        "app-cancel-late",
        "",
        "*\" install \"*) printf 'org.gimp.GIMP\\tuser\\n' >\"$d/installed\"; \
         touch \"$d/deployed\"; exec sleep 30;;",
    );
    let shared = context(&fake);
    load(shared.clone());
    install(&shared, GIMP);
    wait_until(&shared, |_| fake.dir.join("deployed").exists());
    cancel(&shared, GIMP);
    wait_until(&shared, |d| d.rows.state(GIMP) == State::Installed);
    let data = shared.lock();
    assert_eq!(
        data.message.text,
        "GIMP was installed before it could be cancelled."
    );
    assert!(!data.message.error && data.rows.may_remove(GIMP));
}

#[test]
fn closing_stops_an_install_and_starts_nothing_more() {
    let fake = removing_fake(
        "app-close-install",
        "",
        "*\" install \"*) touch \"$d/started\"; exec sleep 30;;",
    );
    let shared = context(&fake);
    load(shared.clone());
    install(&shared, GIMP);
    wait_until(&shared, |_| fake.dir.join("started").exists());
    close(&shared);
    wait_until(&shared, |d| d.rows.state(GIMP) == State::Available);
    assert!(!shared.lock().message.error);
    let calls = fake.calls().len();
    install(&shared, GIMP);
    wait_until(&shared, |d| !d.rows.busy());
    assert_eq!(fake.calls().len(), calls);
}

#[test]
fn remove_asks_first_and_names_the_app() {
    let fake = removing_fake("app-ask", OWN_GIMP, "");
    let shared = context(&fake);
    load(shared.clone());
    assert!(view(&shared.lock(), None).remove_title.is_empty());
    asked(&shared);
    {
        let v = view(&shared.lock(), None);
        assert_eq!(v.remove_title, "Remove GIMP?");
        assert!(v.remove_body.contains("settings and data are kept"));
    }
    keep(&shared);
    assert!(view(&shared.lock(), None).remove_title.is_empty());
    // A yes that was never asked for does nothing.
    confirm_remove(&shared);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!called(&fake, "uninstall"));
    assert!(shared.lock().rows.may_remove(GIMP));
}

#[test]
fn a_confirmed_removal_takes_the_app_and_then_the_unused_parts() {
    let fake = removing_fake("app-remove", OWN_GIMP, REMOVES);
    let shared = context(&fake);
    load(shared.clone());
    asked(&shared);
    confirm_remove(&shared);
    wait_until(&shared, |d| d.rows.state(GIMP) == State::Available);
    let uninstalls: Vec<String> = fake
        .calls()
        .into_iter()
        .filter(|c| c.contains("uninstall"))
        .collect();
    assert_eq!(
        uninstalls,
        [
            "--user uninstall -y --noninteractive --app org.gimp.GIMP/x86_64/stable",
            "--user uninstall --unused -y --noninteractive",
        ]
    );
    assert!(!called(&fake, "delete-data"));
    let data = shared.lock();
    assert_eq!(data.message.text, "GIMP was removed.");
    assert!(!data.message.error);
}

#[test]
fn an_open_app_is_not_removed() {
    let fake = removing_fake("app-remove-open", OWN_GIMP, "ps*) echo org.gimp.GIMP;;");
    let shared = context(&fake);
    load(shared.clone());
    ask_remove(&shared, GIMP);
    wait_until(&shared, |d| d.sticky);
    let data = shared.lock();
    assert!(data.asking.is_none() && data.message.error);
    assert_eq!(data.message.text, "GIMP is open. Close it, then remove it.");
    assert!(data.rows.may_remove(GIMP));
    assert!(!called(&fake, "uninstall"));
}

#[test]
fn an_app_opened_while_asked_is_not_removed() {
    let fake = removing_fake(
        "app-remove-opened",
        OWN_GIMP,
        "ps*) cat \"$d/open\" 2>/dev/null;;",
    );
    let shared = context(&fake);
    load(shared.clone());
    asked(&shared);
    fs::write(fake.dir.join("open"), "org.gimp.GIMP\n").unwrap();
    confirm_remove(&shared);
    wait_until(&shared, |d| !d.rows.busy());
    assert!(shared.lock().message.text.contains("Close it"));
    assert!(shared.lock().rows.may_remove(GIMP));
    assert!(!called(&fake, "uninstall"));
}

#[test]
fn a_system_app_is_never_offered_for_removal() {
    let fake = removing_fake("app-system", "org.gimp.GIMP\tsystem\n", "");
    let shared = context(&fake);
    load(shared.clone());
    search(&shared, "gimp");
    {
        let v = view(&shared.lock(), None);
        assert!(v.cards[0].state == State::Installed && !v.cards[0].removable);
    }
    ask_remove(&shared, GIMP);
    assert!(view(&shared.lock(), None).remove_title.is_empty());
    confirm_remove(&shared);
    std::thread::sleep(Duration::from_millis(100));
    assert!(!called(&fake, "uninstall"));
}

#[test]
fn a_removal_that_fails_says_so_and_the_app_stays() {
    let fake = removing_fake(
        "app-remove-fail",
        OWN_GIMP,
        "*uninstall*) echo 'error: something broke' >&2; exit 1;;",
    );
    let shared = context(&fake);
    load(shared.clone());
    asked(&shared);
    confirm_remove(&shared);
    wait_until(&shared, |d| d.sticky);
    let data = shared.lock();
    assert_eq!(
        data.message.text,
        "Could not remove GIMP. Something went wrong. Try again."
    );
    assert!(data.message.error && data.rows.may_remove(GIMP));
    assert!(!called(&fake, "--unused"));
}

#[test]
fn closing_waits_for_a_removal() {
    let fake = removing_fake(
        "app-close-remove",
        OWN_GIMP,
        "*--unused*) touch \"$d/tidied\";;\n\
         *uninstall*) touch \"$d/started\"; sleep 1; touch \"$d/removed\";;",
    );
    let shared = context(&fake);
    load(shared.clone());
    asked(&shared);
    confirm_remove(&shared);
    wait_until(&shared, |_| fake.dir.join("started").exists());
    close(&shared);
    assert!(fake.dir.join("removed").exists());
    assert!(fake.dir.join("tidied").exists());
}

#[test]
fn nothing_is_removed_while_an_app_is_installed() {
    let fake = removing_fake(
        "app-remove-busy",
        OWN_GIMP,
        "*\" install \"*) exec sleep 30;;",
    );
    let shared = context(&fake);
    load(shared.clone());
    install(&shared, "org.x.Y");
    ask_remove(&shared, GIMP);
    {
        let data = shared.lock();
        assert!(data.asking.is_none());
        assert_eq!(data.message.text, BUSY);
    }
    assert!(!called(&fake, "uninstall"));
    close(&shared);
}
