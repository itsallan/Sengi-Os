//! The StatusNotifierWatcher: the service tray apps register with.
//!
//! Nothing else in Sengi OS provides one, so the bar does. [`Registry`] is
//! the bookkeeping, free of any bus so it can be tested; the two interface
//! types at the bottom put it on the bus under both names apps look for.
//!
//! Any process on the session bus can call the watcher, so a registration
//! is tied to the connection that made it: an app may register itself, or
//! a name it owns, and no connection gets more than a few places.

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
pub const MAX_PER_SENDER: usize = 4;

/// Connections turned away for misbehaving that are remembered at once.
const MAX_BANNED: usize = 64;

/// D-Bus caps names and we cap paths to match.
const MAX_NAME: usize = 255;

pub const KDE_WATCHER: &str = "org.kde.StatusNotifierWatcher";
pub const FDO_WATCHER: &str = "org.freedesktop.StatusNotifierWatcher";
const DBUS: &str = "org.freedesktop.DBus";

/// One registered item: the bus name to call and the object to call it on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
    pub bus: String,
    pub path: String,
}

/// What an app asked to register, before it is known to be its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    /// An item on the caller's own connection.
    Own(Address),
    /// An item under a well-known name, which the caller must own.
    Named(Address),
}

impl Address {
    fn checked(bus: &str, path: &str) -> Option<Address> {
        if bus.len() > MAX_NAME || path.len() > MAX_NAME || bus == DBUS {
            return None;
        }
        BusName::try_from(bus).ok()?;
        ObjectPath::try_from(path).ok()?;
        Some(Address {
            bus: bus.to_string(),
            path: path.to_string(),
        })
    }

    /// The form items are listed in: name and path run together, as
    /// `:1.42/StatusNotifierItem`.
    pub fn service(&self) -> String {
        format!("{}{}", self.bus, self.path)
    }

    /// Reads a registration from `sender`. Apps register in one of two
    /// ways: with a bus name, meaning the item is at the default path
    /// there, or (libayatana and libappindicator) with an object path on
    /// the connection the call came from. Another connection's unique
    /// name is refused outright, and so is the listed form: only a
    /// watcher lists items that way.
    pub fn claim(service: &str, sender: &str) -> Option<Claim> {
        if service.starts_with('/') {
            return Address::checked(sender, service).map(Claim::Own);
        }
        if service.contains('/') {
            return None;
        }
        let address = Address::checked(service, DEFAULT_PATH)?;
        if service == sender {
            Some(Claim::Own(address))
        } else if service.starts_with(':') {
            None
        } else {
            Some(Claim::Named(address))
        }
    }

    /// Reads an item as another watcher lists it: a name, with or without
    /// a path after it.
    pub fn listed(service: &str) -> Option<Address> {
        if service.len() > MAX_NAME * 2 {
            return None;
        }
        match service.find('/') {
            Some(0) => None,
            Some(at) => Address::checked(&service[..at], &service[at..]),
            None => Address::checked(service, DEFAULT_PATH),
        }
    }
}

/// What banning a name did.
#[derive(Debug, PartialEq, Eq)]
pub struct Banned {
    /// The items that went with it.
    pub gone: Vec<Address>,
    /// Whether the name was not banned already.
    pub new: bool,
    /// A name that was forgiven early to make room. Nobody is waiting
    /// for it to leave the bus any more.
    pub evicted: Option<String>,
}

/// Why a registration was turned down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    Malformed,
    Full,
    /// The connection sent something far over a limit and is ignored
    /// until it leaves the bus.
    Banned,
}

#[derive(Debug)]
struct Entry {
    address: Address,
    /// The unique name of the connection that registered it.
    sender: String,
}

/// The items currently registered, in the order they arrived.
#[derive(Debug, Default)]
pub struct Registry {
    items: Vec<Entry>,
    banned: Vec<String>,
}

impl Registry {
    /// Adds an item registered by the connection `sender`. `Ok(None)`
    /// means it was registered already, which apps do freely and is not
    /// an error.
    pub fn register(&mut self, address: Address, sender: &str) -> Result<Option<Address>, Refused> {
        if self.is_banned(sender) || self.is_banned(&address.bus) {
            return Err(Refused::Banned);
        }
        if self.items.iter().any(|item| item.address == address) {
            return Ok(None);
        }
        let theirs = self.items.iter().filter(|i| i.sender == sender).count();
        if self.items.len() >= MAX_ITEMS || theirs >= MAX_PER_SENDER {
            return Err(Refused::Full);
        }
        self.items.push(Entry {
            address: address.clone(),
            sender: sender.to_string(),
        });
        Ok(Some(address))
    }

    /// Adds an item that another watcher listed. Who registered it there
    /// is not known, so it is counted against the name it is under.
    pub fn adopt(&mut self, service: &str) -> Result<Option<Address>, Refused> {
        let address = Address::listed(service).ok_or(Refused::Malformed)?;
        let sender = address.bus.clone();
        self.register(address, &sender)
    }

    /// Removes one item, saying whether it was there.
    pub fn remove(&mut self, address: &Address) -> bool {
        let before = self.items.len();
        self.items.retain(|item| item.address != *address);
        self.items.len() != before
    }

    fn take(&mut self, gone: impl Fn(&Entry) -> bool) -> Vec<Address> {
        let (gone, kept) = std::mem::take(&mut self.items)
            .into_iter()
            .partition(|item| gone(item));
        self.items = kept;
        gone.into_iter().map(|item: Entry| item.address).collect()
    }

    /// Removes every item of a bus name that has lost its owner, and
    /// forgives it if it was banned: the name may be handed out again.
    /// Returns the items and whether a ban was lifted.
    pub fn owner_lost(&mut self, bus: &str) -> (Vec<Address>, bool) {
        let before = self.banned.len();
        self.banned.retain(|name| name != bus);
        let gone = self.take(|item| item.address.bus == bus || item.sender == bus);
        (gone, self.banned.len() != before)
    }

    /// Stops listening to a name: its items go, and nothing can be
    /// registered by it or under it until it has left the bus.
    pub fn ban(&mut self, name: &str) -> Banned {
        let new = !self.is_banned(name);
        let mut evicted = None;
        if new {
            if self.banned.len() >= MAX_BANNED {
                evicted = Some(self.banned.remove(0));
            }
            self.banned.push(name.to_string());
        }
        Banned {
            gone: self.take(|item| item.address.bus == name || item.sender == name),
            new,
            evicted,
        }
    }

    pub fn is_banned(&self, name: &str) -> bool {
        self.banned.iter().any(|banned| banned == name)
    }

    pub fn services(&self) -> Vec<String> {
        self.items.iter().map(|i| i.address.service()).collect()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }
}

/// What the bus-facing watcher shares with the rest of the tray.
#[derive(Clone)]
pub struct Shared {
    pub registry: Arc<Mutex<Registry>>,
    pub events: Sender<Event>,
}

/// Who owns a name right now, or None if nobody or the bus will not say.
async fn name_owner(connection: &zbus::Connection, name: &str) -> Option<String> {
    let reply = connection
        .call_method(
            Some(DBUS),
            "/org/freedesktop/DBus",
            Some(DBUS),
            "GetNameOwner",
            &name,
        )
        .await
        .ok()?;
    reply.body().deserialize::<String>().ok()
}

impl Shared {
    /// Carries out a registration. Returns the service string to announce,
    /// or None for a repeat.
    async fn register(
        &self,
        service: &str,
        header: &Header<'_>,
        connection: &zbus::Connection,
    ) -> zbus::fdo::Result<Option<String>> {
        let malformed =
            || zbus::fdo::Error::InvalidArgs("expected a bus name or an object path".into());
        let sender = header
            .sender()
            .map(|name| name.as_str())
            .ok_or_else(malformed)?;
        let address = match Address::claim(service, sender).ok_or_else(malformed)? {
            Claim::Own(address) => address,
            Claim::Named(address) => {
                // Registering a name is asking us to talk to whoever has
                // it. Only its owner may ask that.
                if name_owner(connection, &address.bus).await.as_deref() != Some(sender) {
                    return Err(zbus::fdo::Error::AccessDenied(
                        "that name belongs to another connection".into(),
                    ));
                }
                address
            }
        };
        let outcome = self
            .registry
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("the tray has stopped".into()))?
            .register(address, sender);
        match outcome {
            Ok(Some(address)) => {
                let service = address.service();
                let _ = self.events.send(Event::Registered(address));
                Ok(Some(service))
            }
            Ok(None) => Ok(None),
            Err(Refused::Malformed) => Err(malformed()),
            Err(Refused::Full) => Err(zbus::fdo::Error::LimitsExceeded("the tray is full".into())),
            Err(Refused::Banned) => Err(zbus::fdo::Error::AccessDenied(
                "this connection is being ignored".into(),
            )),
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
                    #[zbus(connection)] connection: &zbus::Connection,
                    #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
                ) -> zbus::fdo::Result<()> {
                    let registered = self.0.register(service, &header, connection).await?;
                    if let Some(service) = registered {
                        // Whichever name the app used, hosts may be
                        // listening on the other.
                        for interface in [KDE_WATCHER, FDO_WATCHER] {
                            let _ = emitter
                                .emit(interface, "StatusNotifierItemRegistered", &service)
                                .await;
                        }
                    }
                    Ok(())
                }

                /// The bar is the host and is always there, so this has
                /// nothing to record.
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

                // Sent from the registration above, for both interfaces.
                #[zbus(signal)]
                async fn status_notifier_item_registered(
                    emitter: &SignalEmitter<'_>,
                    service: &str,
                ) -> zbus::Result<()>;

                // Sent by the host when an item goes; see
                // `Backend::unregistered`.
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

    /// Registers as the watcher does once any name has been checked.
    fn register(
        registry: &mut Registry,
        service: &str,
        sender: &str,
    ) -> Result<Option<Address>, Refused> {
        match Address::claim(service, sender).ok_or(Refused::Malformed)? {
            Claim::Own(address) | Claim::Named(address) => registry.register(address, sender),
        }
    }

    #[test]
    fn registering_a_path_means_the_caller() {
        let path = "/org/ayatana/NotificationItem/nm_applet";
        assert_eq!(
            Address::claim(path, ":1.42"),
            Some(Claim::Own(address(":1.42", path)))
        );
        assert_eq!(
            address(":1.42", path).service(),
            ":1.42/org/ayatana/NotificationItem/nm_applet"
        );
    }

    #[test]
    fn registering_the_callers_own_name_means_the_default_path() {
        assert_eq!(
            Address::claim(":1.75", ":1.75"),
            Some(Claim::Own(address(":1.75", DEFAULT_PATH)))
        );
    }

    #[test]
    fn a_well_known_name_has_to_be_checked_against_its_owner() {
        let name = "org.kde.StatusNotifierItem-4077-1";
        assert_eq!(
            Address::claim(name, ":1.50"),
            Some(Claim::Named(address(name, DEFAULT_PATH)))
        );
    }

    #[test]
    fn another_connections_unique_name_cannot_be_registered() {
        // One process naming every other connection on the bus as an item
        // would fill the tray and have the bar query them all.
        assert_eq!(Address::claim(":1.6", ":1.5"), None);
        assert_eq!(Address::claim(":1.6/x", ":1.5"), None);
        assert_eq!(Address::claim(":1.5/x", ":1.5"), None);
        assert_eq!(Address::claim("org.example.App/Item", ":1.5"), None);
    }

    #[test]
    fn the_listed_form_is_read_only_when_adopting() {
        for (bus, path) in [
            (":1.42", "/org/ayatana/NotificationItem/x"),
            ("org.example.App", DEFAULT_PATH),
        ] {
            let item = address(bus, path);
            assert_eq!(Address::listed(&item.service()), Some(item));
        }
        assert_eq!(
            Address::listed("org.example.App"),
            Some(address("org.example.App", DEFAULT_PATH))
        );
        assert_eq!(Address::listed("/only/a/path"), None);

        let mut registry = Registry::default();
        let adopted = registry.adopt(":1.42/StatusNotifierItem").unwrap().unwrap();
        assert_eq!(adopted, address(":1.42", DEFAULT_PATH));
        assert_eq!(registry.adopt(":1.42/StatusNotifierItem"), Ok(None));
        assert_eq!(registry.adopt("not a service"), Err(Refused::Malformed));
    }

    #[test]
    fn registering_twice_changes_nothing() {
        let mut registry = Registry::default();
        assert!(register(&mut registry, ":1.5", ":1.5").unwrap().is_some());
        assert_eq!(register(&mut registry, ":1.5", ":1.5"), Ok(None));
        // The same item named the other way.
        assert_eq!(register(&mut registry, DEFAULT_PATH, ":1.5"), Ok(None));
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn losing_the_owner_removes_all_of_its_items() {
        let mut registry = Registry::default();
        register(&mut registry, "/a", ":1.5").unwrap();
        register(&mut registry, "/b", ":1.5").unwrap();
        register(&mut registry, "org.example.Other", ":1.6").unwrap();

        let (gone, _) = registry.owner_lost(":1.5");
        assert_eq!(gone, [address(":1.5", "/a"), address(":1.5", "/b")]);
        assert_eq!(
            registry.services(),
            ["org.example.Other/StatusNotifierItem"]
        );
        assert!(registry.owner_lost(":1.5").0.is_empty());
        assert!(registry.owner_lost(":1.77").0.is_empty());

        // A named item goes when the name does, or when the connection
        // that registered it does.
        assert_eq!(registry.owner_lost("org.example.Other").0.len(), 1);
        register(&mut registry, "org.example.Other", ":1.6").unwrap();
        assert_eq!(registry.owner_lost(":1.6").0.len(), 1);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn a_single_item_can_be_removed() {
        let mut registry = Registry::default();
        register(&mut registry, "/a", ":1.5").unwrap();
        register(&mut registry, "/b", ":1.5").unwrap();
        assert!(registry.remove(&address(":1.5", "/a")));
        assert!(!registry.remove(&address(":1.5", "/a")));
        assert_eq!(registry.services(), [":1.5/b"]);
    }

    #[test]
    fn malformed_registrations_are_refused() {
        for service in [
            "",
            "not a name",
            "nodots",
            "/not//a/path",
            "/trailing/",
            "org.freedesktop.DBus",
            "name.with\nnewline",
            "org.example.\u{202e}App",
        ] {
            assert_eq!(Address::claim(service, ":1.5"), None, "{service:?}");
        }
        let long = format!("/{}", "a".repeat(MAX_NAME));
        assert_eq!(Address::claim(&long, ":1.5"), None);
        let endless = "a.".repeat(100_000);
        assert_eq!(Address::claim(&endless, ":1.5"), None);
        assert_eq!(Address::listed(&endless), None);
    }

    #[test]
    fn one_connection_cannot_fill_the_tray_whatever_names_it_uses() {
        let mut registry = Registry::default();
        // Paths on itself, and well-known names it has gone and taken.
        let attempts: Vec<String> = (0..12)
            .map(|n| match n % 2 {
                0 => format!("/item/{n}"),
                _ => format!("org.example.Name{n}"),
            })
            .collect();
        let accepted = attempts
            .iter()
            .filter(|service| matches!(register(&mut registry, service, ":1.5"), Ok(Some(_))))
            .count();
        assert_eq!(accepted, MAX_PER_SENDER);
        assert_eq!(registry.len(), MAX_PER_SENDER);
        assert_eq!(
            register(&mut registry, "org.example.More", ":1.5"),
            Err(Refused::Full)
        );
        // Others still get in, and a repeat is still not an error.
        assert!(register(&mut registry, "/item/0", ":1.6")
            .unwrap()
            .is_some());
        assert_eq!(register(&mut registry, "/item/0", ":1.5"), Ok(None));
        // When it leaves, all of it leaves, names included.
        assert_eq!(registry.owner_lost(":1.5").0.len(), MAX_PER_SENDER);
    }

    #[test]
    fn the_tray_has_a_size_limit() {
        let mut registry = Registry::default();
        for n in 0..MAX_ITEMS {
            let sender = format!(":1.{n}");
            assert!(register(&mut registry, "/item", &sender).unwrap().is_some());
        }
        assert_eq!(
            register(&mut registry, "/item", ":1.999"),
            Err(Refused::Full)
        );
        // Room again once one leaves.
        registry.owner_lost(":1.0");
        assert!(register(&mut registry, "/item", ":1.999")
            .unwrap()
            .is_some());
    }

    #[test]
    fn a_banned_connection_stays_out_until_it_leaves_the_bus() {
        let mut registry = Registry::default();
        register(&mut registry, "/a", ":1.5").unwrap();
        register(&mut registry, "org.example.Theirs", ":1.5").unwrap();
        register(&mut registry, "/a", ":1.6").unwrap();

        let banned = registry.ban(":1.5");
        assert_eq!(banned.gone.len(), 2);
        assert!(banned.new && banned.evicted.is_none());
        assert_eq!(registry.services(), [":1.6/a"]);
        assert_eq!(register(&mut registry, "/b", ":1.5"), Err(Refused::Banned));
        assert_eq!(
            registry.adopt(":1.5/StatusNotifierItem"),
            Err(Refused::Banned)
        );
        // Banning twice is not news.
        assert!(!registry.ban(":1.5").new);

        // The name is free to be handed to someone else afterwards.
        assert_eq!(registry.owner_lost(":1.5"), (Vec::new(), true));
        assert!(register(&mut registry, "/b", ":1.5").unwrap().is_some());
    }

    #[test]
    fn a_ban_on_a_well_known_name_covers_an_item_adopted_under_it() {
        // In guest mode the item is known only by the name it is listed
        // under; the ban has to be able to name that.
        let mut registry = Registry::default();
        registry
            .adopt("org.example.App/StatusNotifierItem")
            .unwrap();
        assert_eq!(registry.ban(":1.5").gone.len(), 0);
        assert_eq!(registry.ban("org.example.App").gone.len(), 1);
        assert_eq!(
            registry.adopt("org.example.App/StatusNotifierItem"),
            Err(Refused::Banned)
        );
    }

    #[test]
    fn the_list_of_banned_connections_is_bounded() {
        let mut registry = Registry::default();
        let mut evicted = Vec::new();
        for n in 0..MAX_BANNED + 10 {
            evicted.extend(registry.ban(&format!(":1.{n}")).evicted);
        }
        // Each name pushed off the end is reported, so that whoever was
        // watching for it to leave the bus can stop.
        let expected: Vec<String> = (0..10).map(|n| format!(":1.{n}")).collect();
        assert_eq!(evicted, expected);
        assert_eq!(registry.banned.len(), MAX_BANNED);
        assert!(!registry.is_banned(":1.0"));
        assert!(registry.is_banned(&format!(":1.{}", MAX_BANNED + 9)));
    }
}
