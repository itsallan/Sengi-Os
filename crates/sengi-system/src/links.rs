//! The cabled interfaces one by one, and the addresses each interface has.
//!
//! The interface list comes from `/sys/class/net` by way of
//! [`machine`](crate::machine). Addresses come from `getifaddrs`, which is
//! the kernel's own account whoever configured them: systemd-networkd
//! here, but nothing depends on that.

use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

use crate::machine::{self, LinkKind};

/// How many cabled interfaces are listed.
pub const MAX_WIRED: usize = 8;
/// How many addresses of one interface are shown.
pub const MAX_ADDRESSES: usize = 4;

/// One address of one interface, as `getifaddrs` gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    /// The interface's name.
    pub interface: String,
    /// The address.
    pub ip: IpAddr,
}

/// A cabled interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wired {
    /// The kernel's name for it.
    pub interface: String,
    /// A cable is in and the link is up.
    pub up: bool,
    /// Its addresses worth showing: see [`addresses_of`].
    pub addresses: Vec<String>,
}

/// Every address of every interface. Empty if the kernel will not say.
pub fn all_addresses() -> Vec<Address> {
    let mut found = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list it allocates, or
    // fails and leaves it alone. The list is only read, every pointer is
    // checked for null before use, and it is freed exactly once below.
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return found;
        }
        let mut entry = list;
        // No machine has this many; a bound all the same.
        let mut budget = 4096;
        while !entry.is_null() && budget > 0 {
            budget -= 1;
            let this = &*entry;
            entry = this.ifa_next;
            if this.ifa_addr.is_null() || this.ifa_name.is_null() {
                continue;
            }
            let Ok(name) = CStr::from_ptr(this.ifa_name).to_str() else {
                continue;
            };
            let ip = match i32::from((*this.ifa_addr).sa_family) {
                libc::AF_INET => {
                    let address = &*(this.ifa_addr as *const libc::sockaddr_in);
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let address = &*(this.ifa_addr as *const libc::sockaddr_in6);
                    IpAddr::V6(Ipv6Addr::from(address.sin6_addr.s6_addr))
                }
                _ => continue,
            };
            found.push(Address {
                interface: name.to_string(),
                ip,
            });
        }
        libc::freeifaddrs(list);
    }
    found
}

/// The addresses of `interface` worth showing: IPv4 first, then IPv6
/// that is routable. Link-local addresses say nothing about being online.
pub fn addresses_of(all: &[Address], interface: &str) -> Vec<String> {
    let mut mine: Vec<&Address> = all
        .iter()
        .filter(|address| address.interface == interface)
        .filter(|address| match address.ip {
            IpAddr::V4(ip) => !ip.is_loopback() && !ip.is_link_local(),
            IpAddr::V6(ip) => !ip.is_loopback() && (ip.segments()[0] & 0xffc0) != 0xfe80,
        })
        .collect();
    mine.sort_by_key(|address| address.ip.is_ipv6());
    mine.iter()
        .take(MAX_ADDRESSES)
        .map(|address| address.ip.to_string())
        .collect()
}

/// The cabled interfaces under `net`, which is `/sys/class/net`, by name,
/// each with its addresses out of `all`.
pub fn wired(net: &Path, all: &[Address]) -> Vec<Wired> {
    let mut interfaces = machine::interfaces(net);
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    interfaces
        .into_iter()
        .filter(|interface| {
            interface.kind == LinkKind::Wired && machine::plain_name(&interface.name)
        })
        .take(MAX_WIRED)
        .map(|interface| Wired {
            up: interface.up,
            addresses: addresses_of(all, &interface.name),
            interface: sengi_text::line(&interface.name, 16),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn address(interface: &str, ip: &str) -> Address {
        Address {
            interface: interface.to_string(),
            ip: ip.parse().unwrap(),
        }
    }

    #[test]
    fn the_kernel_can_be_asked_for_addresses() {
        // Whatever this machine has, loopback is among it.
        let all = all_addresses();
        assert!(all.iter().any(|a| a.ip.is_loopback()), "{all:?}");
        assert!(all.iter().all(|a| !a.interface.is_empty()));
    }

    #[test]
    fn addresses_worth_showing() {
        let all = [
            address("eth0", "fe80::1"),
            address("eth0", "2001:db8::5"),
            address("eth0", "192.168.1.20"),
            address("eth0", "169.254.3.4"),
            address("wlan0", "10.0.0.7"),
            address("lo", "127.0.0.1"),
        ];
        assert_eq!(addresses_of(&all, "eth0"), ["192.168.1.20", "2001:db8::5"]);
        assert_eq!(addresses_of(&all, "wlan0"), ["10.0.0.7"]);
        assert!(addresses_of(&all, "lo").is_empty());
        assert!(addresses_of(&all, "eth1").is_empty());
        let many: Vec<Address> = (0..50)
            .map(|i| address("eth0", &format!("10.0.0.{i}")))
            .collect();
        assert_eq!(addresses_of(&many, "eth0").len(), MAX_ADDRESSES);
    }

    fn interface(tmp: &TempDir, name: &str, kind: &str, state: &str, device: bool) {
        tmp.write(&format!("net/{name}/type"), kind);
        tmp.write(&format!("net/{name}/operstate"), state);
        if device {
            tmp.write(&format!("net/{name}/device/placeholder"), "");
        }
    }

    #[test]
    fn cabled_interfaces_are_found_and_the_rest_left_out() {
        let tmp = TempDir::new();
        interface(&tmp, "enp3s0", "1\n", "up\n", true);
        interface(&tmp, "enp4s0", "1\n", "down\n", true);
        interface(&tmp, "wlan0", "1\n", "up\n", true);
        tmp.write("net/wlan0/wireless/placeholder", "");
        interface(&tmp, "lo", "772\n", "unknown\n", false);
        interface(&tmp, "br0", "1\n", "up\n", false);
        interface(&tmp, "wwan0", "65534\n", "up\n", true);
        let net = tmp.path().join("net");
        let all = [address("enp3s0", "10.0.2.15")];
        assert!(machine::interfaces(&net)
            .iter()
            .any(|interface| interface.kind == LinkKind::Wifi));
        assert_eq!(
            wired(&net, &all),
            [
                Wired {
                    interface: "enp3s0".to_string(),
                    up: true,
                    addresses: vec!["10.0.2.15".to_string()]
                },
                Wired {
                    interface: "enp4s0".to_string(),
                    up: false,
                    addresses: vec![]
                }
            ]
        );
        let missing = tmp.path().join("missing");
        assert!(wired(&missing, &all).is_empty());
        assert!(machine::interfaces(&missing).is_empty());
    }
}
