//! The system bus, shared by the Wi-Fi, Bluetooth and brightness code.
//!
//! One connection, opened by whichever worker needs it first. Every call
//! carries the no-auto-start flag: the panel shows what is running and
//! never causes iwd or BlueZ to be started on a machine without the
//! hardware.
//!
//! What comes back from `GetManagedObjects` is copied into plain types
//! here, so the code that interprets it can be tested without a bus.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::thread;

use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::export::serde::Serialize;
use zbus::fdo::ManagedObjects;
use zbus::message::Type as MessageType;
use zbus::names::BusName;
use zbus::proxy::MethodFlags;
use zbus::zvariant::{DynamicDeserialize, DynamicType, Value};
use zbus::MatchRule;

use crate::connect;
use crate::services::Notify;

/// A service can describe any number of objects. This is far more than
/// iwd or BlueZ have on a real machine, and bounds the work on the rest.
const MAX_OBJECTS: usize = 1024;
/// Signals waiting to be looked at. Each only says "read again", so
/// losing some to a full queue costs nothing.
const SIGNAL_QUEUE: usize = 16;

/// A property value, of the few kinds the panel reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prop {
    /// A boolean.
    Bool(bool),
    /// A string.
    Text(String),
    /// An object path.
    Path(String),
    /// Any of the integer types, widened.
    Int(i64),
    /// Anything else. Its value is not kept.
    Other,
}

/// The properties of one interface of one object, by name.
pub type Props = HashMap<String, Prop>;

/// One object a service exports, as `GetManagedObjects` lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Object {
    /// Its object path.
    pub path: String,
    /// Its interfaces and their properties, by interface name.
    pub interfaces: HashMap<String, Props>,
}

impl Object {
    /// The properties of interface `name`, if the object has it.
    pub fn interface(&self, name: &str) -> Option<&Props> {
        self.interfaces.get(name)
    }
}

/// The boolean property `name`, if it is there and is one.
pub fn flag(props: &Props, name: &str) -> Option<bool> {
    match props.get(name)? {
        Prop::Bool(value) => Some(*value),
        _ => None,
    }
}

/// The string property `name`, if it is there and is one.
pub fn text<'a>(props: &'a Props, name: &str) -> Option<&'a str> {
    match props.get(name)? {
        Prop::Text(value) => Some(value),
        _ => None,
    }
}

/// The object path property `name`, if it is there and is one.
pub fn path<'a>(props: &'a Props, name: &str) -> Option<&'a str> {
    match props.get(name)? {
        Prop::Path(value) => Some(value),
        _ => None,
    }
}

/// The whole number `name` holds, if it holds one.
pub fn int(props: &Props, name: &str) -> Option<i64> {
    match props.get(name)? {
        Prop::Int(value) => Some(*value),
        _ => None,
    }
}

fn prop(value: &Value<'_>) -> Prop {
    match value {
        Value::Bool(value) => Prop::Bool(*value),
        Value::Str(value) => Prop::Text(value.as_str().to_string()),
        Value::ObjectPath(value) => Prop::Path(value.as_str().to_string()),
        Value::U8(value) => Prop::Int(i64::from(*value)),
        Value::I16(value) => Prop::Int(i64::from(*value)),
        Value::U16(value) => Prop::Int(i64::from(*value)),
        Value::I32(value) => Prop::Int(i64::from(*value)),
        Value::U32(value) => Prop::Int(i64::from(*value)),
        Value::I64(value) => Prop::Int(*value),
        Value::U64(value) => i64::try_from(*value).map_or(Prop::Other, Prop::Int),
        _ => Prop::Other,
    }
}

fn converted(all: &HashMap<String, zbus::zvariant::OwnedValue>) -> Props {
    all.iter()
        .take(MAX_OBJECTS)
        .map(|(key, value)| (key.clone(), prop(value)))
        .collect()
}

/// What a service said when asked for its objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Nobody owns the name: the service is not running.
    Absent,
    /// It is running and the bus will not let this user talk to it.
    Denied,
    /// It answered with these objects.
    Objects(Vec<Object>),
}

/// The objects that have any of `interfaces`, with only those interfaces,
/// sorted by path so that "the first adapter" means the same thing each
/// time, and no more than `MAX_OBJECTS` of them.
///
/// The cut comes after the choosing. BlueZ lists an object for every
/// service and characteristic of every device it has seen, and a cut made
/// first could drop the adapter itself.
fn keep(objects: impl IntoIterator<Item = Object>, interfaces: &[&str]) -> Vec<Object> {
    let mut kept: Vec<Object> = objects
        .into_iter()
        .filter_map(|mut object| {
            object
                .interfaces
                .retain(|name, _| interfaces.contains(&name.as_str()));
            (!object.interfaces.is_empty()).then_some(object)
        })
        .collect();
    kept.sort_by(|a, b| a.path.cmp(&b.path));
    kept.truncate(MAX_OBJECTS);
    kept
}

fn objects(managed: ManagedObjects, interfaces: &[&str]) -> Vec<Object> {
    let converted = managed.into_iter().map(|(path, all)| Object {
        path: path.as_str().to_string(),
        interfaces: all
            .into_iter()
            // Nothing is copied out of interfaces nobody will read.
            .filter(|(name, _)| interfaces.contains(&name.as_str()))
            .map(|(name, props)| {
                let props = props
                    .iter()
                    .map(|(key, value)| (key.clone(), prop(value)))
                    .collect();
                (name.as_str().to_string(), props)
            })
            .collect(),
    });
    keep(converted, interfaces)
}

/// Whether a call failed because the bus policy, or polkit behind the
/// service, forbids it.
pub fn denied(error: &zbus::Error) -> bool {
    matches!(error, zbus::Error::FDO(inner) if matches!(**inner,
        zbus::fdo::Error::AccessDenied(_)
        | zbus::fdo::Error::AuthFailed(_)
        | zbus::fdo::Error::InteractiveAuthorizationRequired(_)))
        || matches!(
            error_name(error),
            Some(
                "org.freedesktop.DBus.Error.AccessDenied"
                    | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired"
                    | "org.freedesktop.PolicyKit1.Error.NotAuthorized"
            )
        )
}

/// The system bus connection, made on first use and shared by the
/// services.
#[derive(Default)]
pub struct Bus {
    connection: OnceLock<Option<Connection>>,
}

impl Bus {
    /// The connection, made on first use. `None` when there is no system
    /// bus to connect to; that is not retried for as long as this lives.
    pub fn connection(&self) -> Option<&Connection> {
        self.connection
            .get_or_init(
                || match connect::system().and_then(|builder| builder.build()) {
                    Ok(connection) => Some(connection),
                    Err(error) => {
                        eprintln!("{}: no system bus: {error}", crate::program_name());
                        None
                    }
                },
            )
            .as_ref()
    }

    /// The unique name of whoever owns `service`, if anyone does. Asking
    /// the bus this never starts the service.
    pub fn owner(&self, service: &str) -> Option<String> {
        let name = BusName::try_from(service).ok()?;
        let owner = DBusProxy::new(self.connection()?)
            .ok()?
            .get_name_owner(name)
            .ok()?;
        Some(owner.as_str().to_string())
    }

    /// Calls a method and reads the reply as `R`. The call carries the
    /// no-auto-start flag, so it fails rather than start `service`.
    pub fn call<B, R>(
        &self,
        service: &str,
        path: &str,
        interface: &str,
        method: &str,
        body: &B,
    ) -> zbus::Result<R>
    where
        B: Serialize + DynamicType,
        R: for<'d> DynamicDeserialize<'d>,
    {
        let connection = self
            .connection()
            .ok_or_else(|| zbus::Error::Failure("no system bus".to_string()))?;
        let proxy = Proxy::new(
            connection,
            service.to_string(),
            path.to_string(),
            interface.to_string(),
        )?;
        proxy
            .call_with_flags(method, MethodFlags::NoAutoStart.into(), body)?
            .ok_or_else(|| zbus::Error::Failure("no reply".to_string()))
    }

    /// The objects of `service` that have any of `interfaces`.
    pub fn objects(&self, service: &str, interfaces: &[&str]) -> Listing {
        if self.owner(service).is_none() {
            return Listing::Absent;
        }
        let managed: zbus::Result<ManagedObjects> = self.call(
            service,
            "/",
            "org.freedesktop.DBus.ObjectManager",
            "GetManagedObjects",
            &(),
        );
        match managed {
            Ok(managed) => Listing::Objects(objects(managed, interfaces)),
            Err(error) if denied(&error) => Listing::Denied,
            // It left the bus between the two calls, or is not what it
            // claims to be. Either way there is nothing to show.
            Err(_) => Listing::Absent,
        }
    }

    /// Every property of one interface of one object. `None` when the
    /// service is not running or will not say. Like every other call, this
    /// does not start the service.
    pub fn properties(&self, service: &str, path: &str, interface: &str) -> Option<Props> {
        self.owner(service)?;
        let all: HashMap<String, zbus::zvariant::OwnedValue> = self
            .call(
                service,
                path,
                "org.freedesktop.DBus.Properties",
                "GetAll",
                &(interface,),
            )
            .ok()?;
        Some(converted(&all))
    }

    /// As [`Bus::properties`], for a service that is meant to be started
    /// by being asked: timedated is not kept running, it is brought up
    /// for each question and leaves again. The one exception to the rule
    /// that nothing here starts a service; never to be used for iwd or
    /// BlueZ.
    pub fn properties_of_helper(
        &self,
        service: &str,
        path: &str,
        interface: &str,
    ) -> Option<Props> {
        let proxy = Proxy::new(
            self.connection()?,
            service.to_string(),
            path.to_string(),
            "org.freedesktop.DBus.Properties",
        )
        .ok()?;
        let all: HashMap<String, zbus::zvariant::OwnedValue> =
            proxy.call("GetAll", &(interface,)).ok()?;
        Some(converted(&all))
    }

    /// Sets a boolean property through `org.freedesktop.DBus.Properties`.
    pub fn set_flag(
        &self,
        service: &str,
        path: &str,
        interface: &str,
        property: &str,
        value: bool,
    ) -> zbus::Result<()> {
        self.call(
            service,
            path,
            "org.freedesktop.DBus.Properties",
            "Set",
            &(interface, property, Value::Bool(value)),
        )
    }

    /// Call `changed` whenever `service` emits any signal, and when it
    /// appears on or leaves the bus.
    pub fn watch(&self, service: &'static str, changed: Notify) {
        let Some(connection) = self.connection() else {
            return;
        };
        let changed = Arc::new(changed);
        let signals = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(service)
            .map(|rule| rule.build());
        let comings = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender("org.freedesktop.DBus")
            .and_then(|rule| rule.interface("org.freedesktop.DBus"))
            .and_then(|rule| rule.member("NameOwnerChanged"))
            .and_then(|rule| rule.arg(0, service))
            .map(|rule| rule.build());
        for rule in [signals, comings] {
            let stream = rule.and_then(|rule| {
                MessageIterator::for_match_rule(rule, connection, Some(SIGNAL_QUEUE))
            });
            match stream {
                Ok(stream) => {
                    let changed = changed.clone();
                    thread::spawn(move || {
                        for message in stream {
                            if message.is_err() {
                                break;
                            }
                            changed();
                        }
                    });
                }
                Err(error) => {
                    eprintln!("{}: cannot watch {service}: {error}", crate::program_name())
                }
            }
        }
    }
}

/// The D-Bus name of the error a call failed with, when it was the
/// service that refused.
pub fn error_name(error: &zbus::Error) -> Option<&str> {
    match error {
        zbus::Error::MethodError(name, _, _) => Some(name.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::testing::object;
    use super::*;

    #[test]
    fn only_objects_with_a_wanted_interface_are_kept() {
        let mut both = object("/b", "wanted.One", &[("Powered", Prop::Bool(true))]);
        both.interfaces
            .insert("other.Thing".to_string(), Props::new());
        let kept = keep(
            [
                object("/c", "other.Thing", &[]),
                both,
                object("/a", "wanted.Two", &[]),
            ],
            &["wanted.One", "wanted.Two"],
        );
        let paths: Vec<&str> = kept.iter().map(|object| object.path.as_str()).collect();
        assert_eq!(paths, ["/a", "/b"]);
        // And of those, only the wanted interfaces.
        assert_eq!(kept[1].interfaces.len(), 1);
        assert_eq!(
            flag(&kept[1].interfaces["wanted.One"], "Powered"),
            Some(true)
        );
    }

    #[test]
    fn a_crowd_of_other_objects_does_not_push_the_adapter_out() {
        // A Bluetooth adapter among thousands of GATT objects, last in
        // whatever order the service happened to send them.
        let mut crowd: Vec<Object> = (0..5000)
            .map(|i| {
                object(
                    &format!("/dev/{i}/char"),
                    "org.bluez.GattCharacteristic1",
                    &[],
                )
            })
            .collect();
        crowd.push(object("/hci0", "org.bluez.Adapter1", &[]));
        let kept = keep(crowd, &["org.bluez.Adapter1"]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, "/hci0");
    }

    #[test]
    fn the_list_is_still_bounded() {
        let many = (0..3000).map(|i| object(&format!("/n/{i:05}"), "wanted", &[]));
        let kept = keep(many, &["wanted"]);
        assert_eq!(kept.len(), MAX_OBJECTS);
        assert_eq!(kept[0].path, "/n/00000");
    }

    #[test]
    fn a_policy_refusal_is_told_from_other_failures() {
        let refused = zbus::Error::FDO(Box::new(zbus::fdo::Error::AccessDenied(
            "not allowed".to_string(),
        )));
        assert!(denied(&refused));
        let gone = zbus::Error::FDO(Box::new(zbus::fdo::Error::ServiceUnknown(
            "no such name".to_string(),
        )));
        assert!(!denied(&gone));
        assert!(!denied(&zbus::Error::Failure("no system bus".to_string())));
    }
}

/// Builders of made-up bus objects, for tests here and, with the
/// `testing` feature, in the programs that read objects through this
/// crate.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::*;

    /// Builds an object list the way a test wants to read it.
    pub fn object(path: &str, interface: &str, props: &[(&str, Prop)]) -> Object {
        Object {
            path: path.to_string(),
            interfaces: HashMap::from([(
                interface.to_string(),
                props
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.clone()))
                    .collect(),
            )]),
        }
    }

    pub fn text(value: &str) -> Prop {
        Prop::Text(value.to_string())
    }

    pub fn path(value: &str) -> Prop {
        Prop::Path(value.to_string())
    }

    /// `base` with the interfaces of `extra` added: one object that has
    /// several.
    pub fn merged(mut base: Object, extra: Object) -> Object {
        base.interfaces.extend(extra.interfaces);
        base
    }
}
