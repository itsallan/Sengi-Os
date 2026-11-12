//! One Settings window at a time. The open window listens on a socket in
//! `$XDG_RUNTIME_DIR`; a second invocation hands it the page it was asked
//! for and exits, so the quick settings panel and the bar can send the user
//! to a page whether or not Settings is already open.
//!
//! The socket, its lock and the care taken over both are sengi-system's
//! `instance::deliver`. What is here is what goes over it: the name of a
//! page, or nothing.

use std::path::Path;

use sengi_system::instance::{deliver, Delivery, Mailbox, Post};

use crate::cli::Page;

/// A message is a page name, or empty for "just come forward".
const POST: Post = Post {
    socket_name: "sengi-settings.sock",
    lock_name: "sengi-settings.lock",
    max_len: 32,
    valid: is_request,
};

pub enum Claim {
    /// This process is the Settings window.
    Granted(Instance),
    /// A window was open and has been given the page.
    Forwarded,
}

/// A later invocation's request: bring the window forward, at a page if
/// it named one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request(pub Option<Page>);

/// Held by the process that owns the window, for as long as it is open.
pub struct Instance(Mailbox);

/// The request a message stands for, if it is one.
fn parse(message: &[u8]) -> Option<Request> {
    if message.is_empty() {
        return Some(Request(None));
    }
    let name = std::str::from_utf8(message).ok()?;
    Page::ALL
        .into_iter()
        .find(|page| page.name() == name)
        .map(|page| Request(Some(page)))
}

fn is_request(message: &[u8]) -> bool {
    parse(message).is_some()
}

/// Become the one window, or pass `page` to the one that is open.
pub fn claim(runtime_dir: Option<&Path>, page: Option<Page>) -> Claim {
    let message = page.map_or("", Page::name);
    match deliver(runtime_dir, &POST, message.as_bytes()) {
        Delivery::Granted(mailbox) => Claim::Granted(Instance(mailbox)),
        Delivery::Forwarded => Claim::Forwarded,
    }
}

impl Instance {
    /// Call `wake` whenever a request arrives. It runs on another thread,
    /// with the request left for [`Instance::take`]; at once, if one has
    /// come already.
    pub fn on_request(&self, wake: impl Fn() + Send + 'static) {
        self.0.on_message(wake);
    }

    /// The request waiting to be acted on, if any.
    pub fn take(&self) -> Option<Request> {
        self.0.take().as_deref().and_then(parse)
    }

    /// The window has closed. If a request slipped in while it was
    /// closing, that request is returned and the window must come back
    /// for it; otherwise this instance stops taking requests and the next
    /// invocation opens its own window.
    pub fn retire(&self) -> Option<Request> {
        self.0.retire().as_deref().and_then(parse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn requests_are_page_names_or_nothing() {
        assert_eq!(parse(b""), Some(Request(None)));
        for page in Page::ALL {
            assert_eq!(parse(page.name().as_bytes()), Some(Request(Some(page))));
            assert!(page.name().len() <= POST.max_len);
        }
        assert_eq!(parse(b"nowhere"), None);
        assert_eq!(parse(b"Sound"), None);
        assert_eq!(parse(b"\xff\xfe"), None);
    }

    #[test]
    fn a_second_invocation_hands_over_its_page_and_does_not_open() {
        let tmp = TempDir::new();
        let Claim::Granted(first) = claim(Some(tmp.path()), None) else {
            panic!("expected to be granted");
        };
        assert_eq!(first.take(), None);
        assert!(matches!(
            claim(Some(tmp.path()), Some(Page::Sound)),
            Claim::Forwarded
        ));
        assert_eq!(first.take(), Some(Request(Some(Page::Sound))));
        // With no page it only raises the window.
        assert!(matches!(claim(Some(tmp.path()), None), Claim::Forwarded));
        assert_eq!(first.retire(), Some(Request(None)));
        assert_eq!(first.retire(), None);
        // Retired: the next invocation opens its own window.
        assert!(matches!(
            claim(Some(tmp.path()), Some(Page::About)),
            Claim::Granted(_)
        ));
    }
}
