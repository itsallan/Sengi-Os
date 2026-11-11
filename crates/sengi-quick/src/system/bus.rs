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
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::sync::{Arc, OnceLock};
use std::thread;

use zbus::address::transport::{Transport, UnixSocket};
use zbus::blocking::connection::Builder;
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::export::serde::Serialize;
use zbus::fdo::ManagedObjects;
use zbus::message::Type as MessageType;
use zbus::names::BusName;
use zbus::proxy::MethodFlags;
use zbus::zvariant::{DynamicDeserialize, DynamicType, Value};
use zbus::MatchRule;

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
    Bool(bool),
    Text(String),
    Path(String),
    Other,
}

pub type Props = HashMap<String, Prop>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Object {
    pub path: String,
    pub interfaces: HashMap<String, Props>,
}

impl Object {
    pub fn interface(&self, name: &str) -> Option<&Props> {
        self.interfaces.get(name)
    }
}

pub fn flag(props: &Props, name: &str) -> Option<bool> {
    match props.get(name)? {
        Prop::Bool(value) => Some(*value),
        _ => None,
    }
}

pub fn text<'a>(props: &'a Props, name: &str) -> Option<&'a str> {
    match props.get(name)? {
        Prop::Text(value) => Some(value),
        _ => None,
    }
}

pub fn path<'a>(props: &'a Props, name: &str) -> Option<&'a str> {
    match props.get(name)? {
        Prop::Path(value) => Some(value),
        _ => None,
    }
}

fn prop(value: &Value<'_>) -> Prop {
    match value {
        Value::Bool(value) => Prop::Bool(*value),
        Value::Str(value) => Prop::Text(value.as_str().to_string()),
        Value::ObjectPath(value) => Prop::Path(value.as_str().to_string()),
        _ => Prop::Other,
    }
}

/// Sorted by path, so "the first adapter" means the same thing each time.
fn objects(managed: ManagedObjects) -> Vec<Object> {
    let mut objects: Vec<Object> = managed
        .into_iter()
        .take(MAX_OBJECTS)
        .map(|(path, interfaces)| Object {
            path: path.as_str().to_string(),
            interfaces: interfaces
                .into_iter()
                .map(|(name, props)| {
                    let props = props
                        .iter()
                        .map(|(key, value)| (key.clone(), prop(value)))
                        .collect();
                    (name.as_str().to_string(), props)
                })
                .collect(),
        })
        .collect();
    objects.sort_by(|a, b| a.path.cmp(&b.path));
    objects
}

/// Opens the socket to the system bus ourselves.
///
/// Left to zbus, the connect runs on a thread of the `blocking` crate's
/// pool, which then wakes twice a second for as long as the process lives.
/// A socket that is already connected keeps the pool from ever starting.
/// sengi-notify does the same for the session bus.
fn system_socket() -> zbus::Result<UnixStream> {
    let address = zbus::Address::system()?;
    let Transport::Unix(unix) = address.transport() else {
        return Err(zbus::Error::Unsupported);
    };
    let stream = match unix.path() {
        UnixSocket::File(path) => UnixStream::connect(path)?,
        UnixSocket::Abstract(name) => {
            UnixStream::connect_addr(&SocketAddr::from_abstract_name(name.as_encoded_bytes())?)?
        }
        _ => return Err(zbus::Error::Unsupported),
    };
    Ok(stream)
}

fn connect() -> zbus::Result<Connection> {
    Builder::async_io_unix_stream(system_socket()?).build()
}

#[derive(Default)]
pub struct Bus {
    connection: OnceLock<Option<Connection>>,
}

impl Bus {
    /// The connection, made on first use. `None` when there is no system
    /// bus to connect to; that is not retried while the panel is open.
    pub fn connection(&self) -> Option<&Connection> {
        self.connection
            .get_or_init(|| match connect() {
                Ok(connection) => Some(connection),
                Err(error) => {
                    eprintln!("sengi-quick: no system bus: {error}");
                    None
                }
            })
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

    /// Everything `service` exports, or `None` if it is not running.
    pub fn objects(&self, service: &str) -> Option<Vec<Object>> {
        self.owner(service)?;
        let managed: ManagedObjects = self
            .call(
                service,
                "/",
                "org.freedesktop.DBus.ObjectManager",
                "GetManagedObjects",
                &(),
            )
            .ok()?;
        Some(objects(managed))
    }

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
                Err(error) => eprintln!("sengi-quick: cannot watch {service}: {error}"),
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
}
