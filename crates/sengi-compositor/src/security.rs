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

/// How many security contexts one client may set up.
const MAX_CONTEXTS: u32 = 32;

/// Whether `client` may see the globals that are kept from sandboxes.
/// Xwayland is not trusted, by design: its client data is Smithay's
/// `XWaylandClientData`, which has no `ClientState`, so this is false for it.
/// It stands for every X11 program at once, so it must not get the layer
/// shell, session lock or window management. Do not "fix" that here.
pub fn trusted(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(|data| !data.sandboxed)
}

impl State {
    /// Drops the counts of clients that have disconnected, so that the map
    /// does not grow with every short-lived program that asked for one.
    pub fn forget_gone_security_contexts(&mut self) {
        if self.security_contexts.is_empty() {
            return;
        }
        let backend = self.display_handle.backend_handle();
        self.security_contexts
            .retain(|client, _| backend.get_client_data(client.clone()).is_ok());
    }
}

impl SecurityContextHandler for State {
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        // Each context is a socket this process listens on from then on.
        let made = self
            .security_contexts
            .entry(context.creator_client_id)
            .or_insert(0);
        *made += 1;
        if *made > MAX_CONTEXTS {
            if *made == MAX_CONTEXTS + 1 {
                crate::say!("a client asked for more than {MAX_CONTEXTS} security contexts");
            }
            return;
        }
        let listening = self.loop_handle.insert_source(source, |stream, _, state| {
            let data = ClientState::of(true);
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
