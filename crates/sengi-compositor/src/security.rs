//! wp-security-context: how a sandbox (Flatpak, through its portal) tells
//! the compositor which connections come from confined programs.
//!
//! Such clients are marked `sandboxed` and do not get the globals that
//! reach beyond their own windows: session lock, layer shell,
//! foreign-toplevel management, idle notification, and this one.
//! `trusted` is the test every one of those globals applies.
//!
//! A program that is not sandboxed runs with the user's full rights and
//! is not held back by any of this.

use std::sync::Arc;

use smithay::{
    delegate_security_context,
    reexports::wayland_server::Client,
    wayland::security_context::{
        SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
    },
};

use crate::state::{ClientState, State};

/// Whether `client` may see the globals that are kept from sandboxes.
pub fn trusted(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|data| !data.sandboxed)
}

impl SecurityContextHandler for State {
    fn context_created(&mut self, source: SecurityContextListenerSource, _: SecurityContext) {
        let listening = self.loop_handle.insert_source(source, |stream, _, state| {
            let data = ClientState::of(&stream, true);
            if let Err(err) = state.display_handle.insert_client(stream, Arc::new(data)) {
                crate::say!("rejected a sandboxed client: {err}");
            }
        });
        if let Err(err) = listening {
            crate::say!("cannot listen for a sandbox: {err}");
        }
    }
}

delegate_security_context!(State);
