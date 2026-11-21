//! Connections to the two buses, opened without waking anything up.
//!
//! Left to zbus, the connect runs on a thread of the `blocking` crate's
//! pool, and with blocking 1.7.0 that thread never ends: it wakes twice a
//! second for as long as the process lives. A socket that is already
//! connected keeps the pool from ever starting, so the socket is opened
//! here and handed to zbus's builder.

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};

use zbus::address::transport::{Transport, UnixSocket};
use zbus::blocking::connection::Builder;

fn socket(address: zbus::Address) -> zbus::Result<UnixStream> {
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

/// Opens the socket to the session bus (`DBUS_SESSION_BUS_ADDRESS`, or
/// the usual place). An error if the bus is not on a unix socket or not
/// there.
pub fn session_socket() -> zbus::Result<UnixStream> {
    socket(zbus::Address::session()?)
}

/// A builder for a connection to the session bus, over a socket opened
/// here when that is possible.
pub fn session() -> zbus::Result<Builder<'static>> {
    match session_socket() {
        Ok(socket) => Ok(Builder::async_io_unix_stream(socket)),
        // A bus that is not on a unix socket, or not there at all: let
        // zbus try, and report the error in its own words if it fails.
        Err(_) => Builder::session(),
    }
}

/// Opens the socket to the system bus (`DBUS_SYSTEM_BUS_ADDRESS`, or the
/// usual place). An error if the bus is not on a unix socket or not there.
pub fn system_socket() -> zbus::Result<UnixStream> {
    socket(zbus::Address::system()?)
}

/// A builder for a connection to the system bus, over a socket opened
/// here. There is no falling back to zbus's own connect: a machine without
/// a system bus gets an error and no helper thread.
pub fn system() -> zbus::Result<Builder<'static>> {
    Ok(Builder::async_io_unix_stream(system_socket()?))
}
