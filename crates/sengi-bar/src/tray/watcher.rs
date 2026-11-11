//! The StatusNotifierWatcher: the service tray apps register with.
//!
//! Nothing else in Sengi OS provides one, so the bar does. [`Registry`] is
//! the bookkeeping, free of any bus so it can be tested; the two interface
//! types at the bottom put it on the bus under both names apps look for.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use zbus::message::Header;
use zbus::names::BusName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::ObjectPath;

use super::host::Event;

/// Where an item lives when it registers with only a bus name.
pub const DEFAULT_PATH: &str = "/StatusNotifierItem";

/// Most items the tray will hold. A desktop has a handful.
pub const MAX_ITEMS: usize = 24;

/// Most items one connection may register, so a single misbehaving app
/// cannot fill the tray on its own.
pub const MAX_PER_BUS: usize = 4;

/// D-Bus caps names and we cap paths to match.
const MAX_NAME: usize = 255;

/// One registered item: the bus name to call and the object to call it on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
    pub bus: String,
    pub path: String,
}

impl Address {
    /// The form items are listed in: name and path run together, as
    /// `:1.42/StatusNotifierItem`.
    pub fn service(&self) -> String {
        format!("{}{}", self.bus, self.path)
    }

    /// Reads a registration. Apps register in one of two ways: with a bus
    /// name, meaning the item is at the default path there, or (libayatana
    /// and libappindicator) with an object path on the connection the call
    /// came from. The listed form is accepted too, for items learnt from
    /// another watcher.
    pub fn parse(service: &str, sender: Option<&str>) -> Option<Address> {
        if service.len() > MAX_NAME * 2 {
            return None;
        }
        let (bus, path) = if service.starts_with('/') {
            (sender?, service)
        } else {
            match service.find('/') {
                Some(at) => service.split_at(at),
                None => (service, DEFAULT_PATH),
            }
        };
        if bus.len() > MAX_NAME || path.len() > MAX_NAME {
            return None;
        }
        // The bus's own name is not an app.
        if bus == "org.freedesktop.DBus" {
            return None;
        }
        BusName::try_from(bus).ok()?;
        ObjectPath::try_from(path).ok()?;
        Some(Address {
            bus: bus.to_string(),
            path: path.to_string(),
        })
    }
}

/// Why a registration was turned down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Malformed,
    Full,
}

/// The items currently registered, in the order they arrived.
#[derive(Debug, Default)]
pub struct Registry {
    items: Vec<Address>,
}

impl Registry {
    /// Adds an item. `Ok(None)` means it was registered already, which
    /// apps do freely and is not an error.
    pub fn register(
        &mut self,
        service: &str,
        sender: Option<&str>,
    ) -> Result<Option<Address>, Refused> {
        let address = Address::parse(service, sender).ok_or(Refused::Malformed)?;
        if self.items.contains(&address) {
            return Ok(None);
        }
        let same_bus = self.items.iter().filter(|i| i.bus == address.bus).count();
        if self.items.len() >= MAX_ITEMS || same_bus >= MAX_PER_BUS {
            return Err(Refused::Full);
        }
        self.items.push(address.clone());
        Ok(Some(address))
    }

    /// Removes one item, saying whether it was there.
    pub fn remove(&mut self, address: &Address) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item != address);
        self.items.len() != before
    }

    /// Removes every item of a bus name that has lost its owner.
    pub fn owner_lost(&mut self, bus: &str) -> Vec<Address> {
        let (gone, kept) = std::mem::take(&mut self.items)
            .into_iter()
            .partition(|item| item.bus == bus);
        self.items = kept;
        gone
    }

    pub fn services(&self) -> Vec<String> {
        self.items.iter().map(Address::service).collect()
    }

    #[cfg(test)]
    pub fn items(&self) -> &[Address] {
        &self.items
    }
}

/// What the bus-facing watcher shares with the rest of the tray.
#[derive(Clone)]
pub struct Shared {
    pub registry: Arc<Mutex<Registry>>,
    pub events: Sender<Event>,
}

impl Shared {
    fn register(&self, service: &str, header: &Header<'_>) -> zbus::fdo::Result<Option<String>> {
        let sender = header.sender().map(|name| name.as_str());
        let outcome = self
            .registry
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("the tray has stopped".into()))?
            .register(service, sender);
        match outcome {
            Ok(Some(address)) => {
                let service = address.service();
                let _ = self.events.send(Event::Registered(address));
                Ok(Some(service))
            }
            Ok(None) => Ok(None),
            Err(Refused::Malformed) => Err(zbus::fdo::Error::InvalidArgs(
                "expected a bus name or an object path".into(),
            )),
            Err(Refused::Full) => Err(zbus::fdo::Error::LimitsExceeded("the tray is full".into())),
        }
    }

    fn services(&self) -> Vec<String> {
        self.registry
            .lock()
            .map(|registry| registry.services())
            .unwrap_or_default()
    }
}

/// The same watcher under the name everything uses in practice (KDE's) and
/// the one the freedesktop draft gave it.
macro_rules! watcher_interface {
    ($module:ident, $type:ident, $name:literal) => {
        // zbus writes more ways to send each signal than we call.
        #[allow(dead_code)]
        mod $module {
            use super::*;

            pub struct $type(pub Shared);

            #[zbus::interface(name = $name)]
            impl $type {
                async fn register_status_notifier_item(
                    &self,
                    service: &str,
                    #[zbus(header)] header: Header<'_>,
                    #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
                ) -> zbus::fdo::Result<()> {
                    if let Some(service) = self.0.register(service, &header)? {
                        let _ = Self::status_notifier_item_registered(&emitter, &service).await;
                    }
                    Ok(())
                }

                /// The bar is the host and is always there, so this has nothing
                /// to record.
                async fn register_status_notifier_host(
                    &self,
                    _service: &str,
                    #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
                ) {
                    let _ = Self::status_notifier_host_registered(&emitter).await;
                }

                #[zbus(property)]
                fn registered_status_notifier_items(&self) -> Vec<String> {
                    self.0.services()
                }

                #[zbus(property)]
                fn is_status_notifier_host_registered(&self) -> bool {
                    true
                }

                #[zbus(property)]
                fn protocol_version(&self) -> i32 {
                    0
                }

                #[zbus(signal)]
                async fn status_notifier_item_registered(
                    emitter: &SignalEmitter<'_>,
                    service: &str,
                ) -> zbus::Result<()>;

                // Sent by the host when an item goes; see `Backend::unregistered`.
                #[zbus(signal)]
                async fn status_notifier_item_unregistered(
                    emitter: &SignalEmitter<'_>,
                    service: &str,
                ) -> zbus::Result<()>;

                #[zbus(signal)]
                async fn status_notifier_host_registered(
                    emitter: &SignalEmitter<'_>,
                ) -> zbus::Result<()>;
            }
        }
        pub use $module::$type;
    };
}

watcher_interface!(kde, KdeWatcher, "org.kde.StatusNotifierWatcher");
watcher_interface!(
    freedesktop,
    FreedesktopWatcher,
    "org.freedesktop.StatusNotifierWatcher"
);

#[cfg(test)]
mod tests {
    use super::*;

    fn address(bus: &str, path: &str) -> Address {
        Address {
            bus: bus.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn registering_a_bus_name_means_the_default_path() {
        let mut registry = Registry::default();
        let added = registry
            .register("org.kde.StatusNotifierItem-4077-1", Some(":1.50"))
            .unwrap()
            .unwrap();
        assert_eq!(
            added,
            address("org.kde.StatusNotifierItem-4077-1", DEFAULT_PATH)
        );
        assert_eq!(
            registry.services(),
            ["org.kde.StatusNotifierItem-4077-1/StatusNotifierItem"]
        );
        // A unique name works the same way.
        let added = registry.register(":1.75", Some(":1.75")).unwrap().unwrap();
        assert_eq!(added.service(), ":1.75/StatusNotifierItem");
    }

    #[test]
    fn registering_a_path_means_the_caller() {
        let mut registry = Registry::default();
        let path = "/org/ayatana/NotificationItem/nm_applet";
        let added = registry.register(path, Some(":1.42")).unwrap().unwrap();
        assert_eq!(added, address(":1.42", path));
        assert_eq!(
            registry.services(),
            [":1.42/org/ayatana/NotificationItem/nm_applet"]
        );
        // With no caller there is nothing to attach the path to.
        assert_eq!(registry.register(path, None), Err(Refused::Malformed));
    }

    #[test]
    fn the_listed_form_parses_back() {
        for (bus, path) in [
            (":1.42", "/org/ayatana/NotificationItem/x"),
            ("org.example.App", DEFAULT_PATH),
        ] {
            let item = address(bus, path);
            assert_eq!(Address::parse(&item.service(), None), Some(item));
        }
    }

    #[test]
    fn registering_twice_changes_nothing() {
        let mut registry = Registry::default();
        assert!(registry.register(":1.5", Some(":1.5")).unwrap().is_some());
        assert_eq!(registry.register(":1.5", Some(":1.5")), Ok(None));
        // The same item named in the other two ways.
        assert_eq!(registry.register(DEFAULT_PATH, Some(":1.5")), Ok(None));
        assert_eq!(
            registry.register(":1.5/StatusNotifierItem", Some(":1.9")),
            Ok(None)
        );
        assert_eq!(registry.items().len(), 1);
    }

    #[test]
    fn losing_the_owner_removes_all_of_its_items() {
        let mut registry = Registry::default();
        registry.register("/a", Some(":1.5")).unwrap();
        registry.register("/b", Some(":1.5")).unwrap();
        registry
            .register("org.example.Other", Some(":1.6"))
            .unwrap();

        let gone = registry.owner_lost(":1.5");
        assert_eq!(gone, [address(":1.5", "/a"), address(":1.5", "/b")]);
        assert_eq!(
            registry.services(),
            ["org.example.Other/StatusNotifierItem"]
        );
        assert!(registry.owner_lost(":1.5").is_empty());
        assert!(registry.owner_lost(":1.77").is_empty());

        // A well-known name is tracked by that name, not by who registered.
        assert!(registry.owner_lost(":1.6").is_empty());
        assert_eq!(registry.owner_lost("org.example.Other").len(), 1);
        assert!(registry.items().is_empty());
    }

    #[test]
    fn a_single_item_can_be_removed() {
        let mut registry = Registry::default();
        registry.register("/a", Some(":1.5")).unwrap();
        registry.register("/b", Some(":1.5")).unwrap();
        assert!(registry.remove(&address(":1.5", "/a")));
        assert!(!registry.remove(&address(":1.5", "/a")));
        assert_eq!(registry.services(), [":1.5/b"]);
    }

    #[test]
    fn malformed_registrations_are_refused() {
        let mut registry = Registry::default();
        for service in [
            "",
            "not a name",
            "nodots",
            ":1.5/not//a/path",
            "/trailing/",
            "org.freedesktop.DBus",
            "name.with\nnewline",
            "org.example.\u{202e}App",
        ] {
            assert_eq!(
                registry.register(service, Some(":1.5")),
                Err(Refused::Malformed),
                "{service:?}"
            );
        }
        let long = format!("/{}", "a".repeat(MAX_NAME));
        assert_eq!(
            registry.register(&long, Some(":1.5")),
            Err(Refused::Malformed)
        );
        let endless = "a.".repeat(100_000);
        assert_eq!(
            registry.register(&endless, Some(":1.5")),
            Err(Refused::Malformed)
        );
        assert!(registry.items().is_empty());
    }

    #[test]
    fn one_connection_cannot_fill_the_tray() {
        let mut registry = Registry::default();
        for n in 0..MAX_PER_BUS {
            assert!(registry
                .register(&format!("/item/{n}"), Some(":1.5"))
                .unwrap()
                .is_some());
        }
        assert_eq!(
            registry.register("/item/more", Some(":1.5")),
            Err(Refused::Full)
        );
        // Others still get in, and a repeat is still not an error.
        assert!(registry
            .register("/item/0", Some(":1.6"))
            .unwrap()
            .is_some());
        assert_eq!(registry.register("/item/0", Some(":1.5")), Ok(None));
    }

    #[test]
    fn the_tray_has_a_size_limit() {
        let mut registry = Registry::default();
        for n in 0..MAX_ITEMS {
            let sender = format!(":1.{n}");
            assert!(registry.register("/item", Some(&sender)).unwrap().is_some());
        }
        assert_eq!(
            registry.register("/item", Some(":1.999")),
            Err(Refused::Full)
        );
        // Room again once one leaves.
        registry.owner_lost(":1.0");
        assert!(registry
            .register("/item", Some(":1.999"))
            .unwrap()
            .is_some());
    }
}
