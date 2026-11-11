//! The Slint platform: hands out software-rendered windows and lets other
//! threads reach the loop through Slint's own `invoke_from_event_loop`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Clipboard, EventLoopProxy, Platform, WindowAdapter};
use slint::{EventLoopError, PlatformError};
use smithay_client_toolkit::reexports::calloop::channel::Sender;

/// Windows Slint has created that no surface has claimed yet.
pub(crate) type Unclaimed = Rc<RefCell<Vec<Rc<MinimalSoftwareWindow>>>>;

pub(crate) enum ProxyEvent {
    Invoke(Box<dyn FnOnce() + Send>),
    Quit,
}

pub(crate) struct LayerPlatform {
    pub unclaimed: Unclaimed,
    pub started: Instant,
    pub proxy: Sender<ProxyEvent>,
    /// Copy and paste work inside the program only. Nothing is offered to
    /// other clients.
    pub clipboard: RefCell<Option<String>>,
}

impl Platform for LayerPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        self.unclaimed.borrow_mut().push(window.clone());
        Ok(window)
    }

    fn duration_since_start(&self) -> Duration {
        self.started.elapsed()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy(Mutex::new(self.proxy.clone()))))
    }

    fn set_clipboard_text(&self, text: &str, clipboard: Clipboard) {
        if clipboard == Clipboard::DefaultClipboard {
            *self.clipboard.borrow_mut() = Some(text.to_owned());
        }
    }

    fn clipboard_text(&self, clipboard: Clipboard) -> Option<String> {
        if clipboard == Clipboard::DefaultClipboard {
            self.clipboard.borrow().clone()
        } else {
            None
        }
    }
}

struct Proxy(Mutex<Sender<ProxyEvent>>);

impl Proxy {
    fn send(&self, event: ProxyEvent) -> Result<(), EventLoopError> {
        let sender = self
            .0
            .lock()
            .map_err(|_| EventLoopError::EventLoopTerminated)?;
        sender
            .send(event)
            .map_err(|_| EventLoopError::EventLoopTerminated)
    }
}

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        self.send(ProxyEvent::Quit)
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), EventLoopError> {
        self.send(ProxyEvent::Invoke(event))
    }
}
