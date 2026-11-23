//! What a sandboxed client is kept from, checked against the real
//! compositor.

mod common;
mod harness;

use common::{Client, LOCK_MANAGER};
use harness::Compositor;
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

/// The globals that reach beyond a client's own windows.
const IDLE_NOTIFIER: &str = "ext_idle_notifier_v1";
const PRIVILEGED: [&str; 6] = [
    "zwp_idle_inhibit_manager_v1",
    common::apps::SCREENCOPY,
    "zwlr_layer_shell_v1",
    "zwlr_foreign_toplevel_manager_v1",
    IDLE_NOTIFIER,
    "wp_security_context_manager_v1",
];

#[test]
fn a_sandboxed_client_does_not_see_the_privileged_globals() {
    let compositor = Compositor::start(None);
    let mut portal = compositor.connect();
    for global in PRIVILEGED {
        assert!(
            portal.has_global(global),
            "{global} is missing outside a sandbox"
        );
    }
    // The session lock is for the locker the compositor starts, and for
    // nobody who connects, in a sandbox or out of one.
    assert!(!portal.has_global(LOCK_MANAGER));

    let socket = compositor.dir.join("sandbox-0");
    portal.sandbox(&socket).unwrap();
    let mut confined = Client::connect(Some(&socket)).unwrap();
    for global in PRIVILEGED {
        assert!(
            !confined.has_global(global),
            "{global} is offered inside a sandbox"
        );
    }
    assert!(!confined.has_global(LOCK_MANAGER));
    // It is still an ordinary client.
    assert!(confined.has_global("xdg_wm_base"));
    let _window = confined.window(0xff20_b020).unwrap();

    // Asking by number does not get around the missing announcement.
    let name = portal.globals.contents().with_list(|list| {
        list.iter()
            .find(|global| global.interface == IDLE_NOTIFIER)
            .map(|global| global.name)
    });
    let _forced: ExtIdleNotifierV1 =
        confined
            .globals
            .registry()
            .bind(name.unwrap(), 1, &confined.handle, ());
    assert!(
        confined.roundtrip().is_err(),
        "binding a hidden global must fail"
    );
}

#[test]
fn a_client_that_set_up_a_sandbox_is_forgotten_when_it_disconnects() {
    let compositor = Compositor::start(None);
    let mut portal = compositor.connect();
    portal.sandbox(&compositor.dir.join("sandbox-1")).unwrap();
    assert_eq!(compositor.control("contexts"), "ok 1");
    drop(portal);
    let end = std::time::Instant::now() + harness::SOON;
    while compositor.control("contexts") != "ok 0" && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        compositor.control("contexts"),
        "ok 0",
        "the count of a client that is gone is kept"
    );
}
