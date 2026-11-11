//! The Slint platform: hands out software-rendered windows and lets other
//! threads reach the loop through Slint's own `invoke_from_event_loop`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    pub quits: QuitGuard,
    /// Copy and paste work inside the program only. Nothing is offered to
    /// other clients.
    pub clipboard: RefCell<Option<String>>,
}

impl Platform for LayerPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        self.unclaimed.borrow_mut().push(window.clone());
        Ok(window)
    }

    fn duration_since_start(&self) -> Duration {
        self.started.elapsed()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(Proxy {
            sender: Mutex::new(self.proxy.clone()),
            quits: self.quits.clone(),
        }))
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

/// Slint asks the loop to quit when the last shown window is hidden. The
/// backend hides a component to let go of it when its output is unplugged,
/// and that must not end the program, so quits asked for while the guard
/// is held are dropped.
#[derive(Clone, Default)]
pub(crate) struct QuitGuard(Arc<AtomicBool>);

impl QuitGuard {
    /// Runs `action` with quit requests switched off.
    pub(crate) fn hold<T>(&self, action: impl FnOnce() -> T) -> T {
        self.0.store(true, Ordering::SeqCst);
        let result = action();
        self.0.store(false, Ordering::SeqCst);
        result
    }

    fn held(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

struct Proxy {
    sender: Mutex<Sender<ProxyEvent>>,
    quits: QuitGuard,
}

impl Proxy {
    fn send(&self, event: ProxyEvent) -> Result<(), EventLoopError> {
        let sender = self
            .sender
            .lock()
            .map_err(|_| EventLoopError::EventLoopTerminated)?;
        sender
            .send(event)
            .map_err(|_| EventLoopError::EventLoopTerminated)
    }
}

impl EventLoopProxy for Proxy {
    fn quit_event_loop(&self) -> Result<(), EventLoopError> {
        if self.quits.held() {
            return Ok(());
        }
        self.send(ProxyEvent::Quit)
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), EventLoopError> {
        self.send(ProxyEvent::Invoke(event))
    }
}
