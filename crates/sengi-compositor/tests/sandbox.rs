//! What a sandboxed client is kept from, checked against the real
//! compositor.

mod common;
mod harness;

use common::{Client, LOCK_MANAGER};
use harness::Compositor;
use wayland_protocols::ext::session_lock::v1::client::ext_session_lock_manager_v1::ExtSessionLockManagerV1;

/// The globals that reach beyond a client's own windows.
const PRIVILEGED: [&str; 5] = [
    LOCK_MANAGER,
    "zwlr_layer_shell_v1",
    "zwlr_foreign_toplevel_manager_v1",
    "ext_idle_notifier_v1",
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

    let socket = compositor.dir.join("sandbox-0");
    portal.sandbox(&socket).unwrap();
    let mut confined = Client::connect(Some(&socket)).unwrap();
    for global in PRIVILEGED {
        assert!(
            !confined.has_global(global),
            "{global} is offered inside a sandbox"
        );
    }
    // It is still an ordinary client.
    assert!(confined.has_global("xdg_wm_base"));
    let _window = confined.window(0xff20_b020).unwrap();

    // Asking by number does not get around the missing announcement.
    let name = portal.globals.contents().with_list(|list| {
        list.iter()
            .find(|global| global.interface == LOCK_MANAGER)
            .map(|global| global.name)
    });
    let _forced: ExtSessionLockManagerV1 =
        confined
            .globals
            .registry()
            .bind(name.unwrap(), 1, &confined.handle, ());
    assert!(
        confined.roundtrip().is_err(),
        "binding a hidden global must fail"
    );
}
