//! What the kernel says about network interfaces: which are cabled, which
//! are up, whether rfkill holds the radio off, and the addresses each has.
//!
//! The interface list and rfkill come from `/sys/class`, as in
//! sengi-quick's `system/machine.rs`. Addresses come from `getifaddrs`,
//! which is the kernel's own account whoever configured them:
//! systemd-networkd here, but nothing depends on that.

use std::ffi::CStr;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use super::{Block, Wired, MAX_ADDRESSES, MAX_WIRED};
use crate::text;

/// The kernel's number for Ethernet in an interface's `type` file.
const ARPHRD_ETHER: &str = "1";

/// An interface name the kernel could have given: short and plain. It is
/// compared with what `getifaddrs` returns and shown on the page.
pub fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 16
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '@'))
}

/// One address of one interface, as `getifaddrs` gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub interface: String,
    pub ip: std::net::IpAddr,
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
                    std::net::IpAddr::V4(Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let address = &*(this.ifa_addr as *const libc::sockaddr_in6);
                    std::net::IpAddr::V6(Ipv6Addr::from(address.sin6_addr.s6_addr))
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
            std::net::IpAddr::V4(ip) => !ip.is_loopback() && !ip.is_link_local(),
            std::net::IpAddr::V6(ip) => !ip.is_loopback() && (ip.segments()[0] & 0xffc0) != 0xfe80,
        })
        .collect();
    mine.sort_by_key(|address| address.ip.is_ipv6());
    mine.iter()
        .take(MAX_ADDRESSES)
        .map(|address| address.ip.to_string())
        .collect()
}

/// What `/sys/class/net` says about cabled interfaces, and whether there
/// is any Wi-Fi hardware. Only interfaces with a device behind them count:
/// bridges, tunnels and the like say nothing about the machine's own links.
pub fn read(net: &Path, all: &[Address]) -> (Vec<Wired>, bool) {
    let mut wired = Vec::new();
    let mut wifi = false;
    let mut entries: Vec<_> = fs::read_dir(net)
        .into_iter()
        .flatten()
        .flatten()
        .take(64)
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if !path.join("device").exists() {
            continue;
        }
        let file = |name: &str| fs::read_to_string(path.join(name)).unwrap_or_default();
        if path.join("wireless").exists() || path.join("phy80211").exists() {
            wifi = true;
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // A USB adapter or a tethered phone is Ethernet too, as far as the
        // kernel can tell, and is called wired.
        if !matches!(file("type").trim(), "" | ARPHRD_ETHER) || !plain_name(&name) {
            continue;
        }
        if wired.len() < MAX_WIRED {
            wired.push(Wired {
                up: file("operstate").trim() == "up",
                addresses: addresses_of(all, &name),
                interface: text::clean(&name, 16),
            });
        }
    }
    (wired, wifi)
}

/// Whether rfkill holds the radios of `kind` ("wlan", "bluetooth") off. A
/// machine often has two switches for one radio, the laptop's and the
/// card's; any one of them blocks it.
pub fn block(rfkill: &Path, kind: &str) -> Block {
    let mut found = Block::None;
    for entry in fs::read_dir(rfkill)
        .into_iter()
        .flatten()
        .flatten()
        .take(64)
    {
        let path = entry.path();
        let file = |name: &str| fs::read_to_string(path.join(name)).unwrap_or_default();
        if file("type").trim() != kind {
            continue;
        }
        if file("hard").trim() == "1" {
            return Block::Hard;
        }
        if file("soft").trim() == "1" {
            found = Block::Soft;
        }
    }
    found
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

    #[test]
    fn interface_names() {
        for good in ["eth0", "enp0s31f6", "wlan0", "usb0", "eth0.100"] {
            assert!(plain_name(good), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "name with space",
            "sixteen-chars-xx",
            "é",
        ] {
            assert!(!plain_name(bad), "{bad}");
        }
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
        let all = [address("enp3s0", "10.0.2.15")];
        let (wired, wifi) = read(&tmp.path().join("net"), &all);
        assert!(wifi);
        assert_eq!(
            wired,
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
        let (none, wifi) = read(&tmp.path().join("missing"), &all);
        assert!(none.is_empty() && !wifi);
    }

    #[test]
    fn rfkill_blocks() {
        let tmp = TempDir::new();
        tmp.write("rfkill/rfkill0/type", "bluetooth\n");
        tmp.write("rfkill/rfkill0/soft", "1\n");
        tmp.write("rfkill/rfkill0/hard", "0\n");
        tmp.write("rfkill/rfkill1/type", "wlan\n");
        tmp.write("rfkill/rfkill1/soft", "0\n");
        tmp.write("rfkill/rfkill1/hard", "0\n");
        let rfkill = tmp.path().join("rfkill");
        assert_eq!(block(&rfkill, "wlan"), Block::None);
        assert_eq!(block(&rfkill, "bluetooth"), Block::Soft);
        tmp.write("rfkill/rfkill2/type", "wlan\n");
        tmp.write("rfkill/rfkill2/hard", "1\n");
        assert_eq!(block(&rfkill, "wlan"), Block::Hard);
        assert_eq!(block(&tmp.path().join("missing"), "wlan"), Block::None);
    }
}
