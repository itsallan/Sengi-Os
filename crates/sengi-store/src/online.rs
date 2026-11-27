//! Whether the network has come back, from the kernel's routing tables, and
//! how soon to ask Flathub again. Nothing here talks to the network, and
//! nothing here depends on what configures it.

use std::fs;
use std::path::Path;
use std::time::Duration;

/// Where the tables are.
pub const PROC_NET: &str = "/proc/net";

/// A route that is up, and one that goes nowhere.
const RTF_UP: u32 = 0x1;
const RTF_REJECT: u32 = 0x200;

/// How often the routes are looked at while Flathub is out of reach.
pub const LOOK_EVERY: Duration = Duration::from_secs(3);

/// The waits between tries with a route that has not changed: the names or
/// the other end may take a moment after the link is up.
const WAITS: [u64; 5] = [5, 15, 30, 60, 120];

/// The default routes there are now, one line each, in order: the interface
/// and the gateway. Empty when there is no way out.
pub fn default_routes(proc_net: &Path) -> Vec<String> {
    let mut routes = Vec::new();
    // Iface Destination Gateway Flags RefCnt Use Metric Mask ...
    for line in read(&proc_net.join("route")).lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() > 7 && f[1] == "00000000" && f[7] == "00000000" && usable(f[3]) {
            routes.push(format!("{} {}", f[0], f[2]));
        }
    }
    // destination prefix source prefix next-hop metric refs use flags name
    for line in read(&proc_net.join("ipv6_route")).lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() > 9
            && f[1] == "00"
            && f[0].bytes().all(|b| b == b'0')
            && usable(f[8])
            && f[9] != "lo"
        {
            routes.push(format!("{} {}", f[9], f[4]));
        }
    }
    routes.sort();
    routes.dedup();
    routes
}

fn usable(flags: &str) -> bool {
    u32::from_str_radix(flags, 16).is_ok_and(|f| f & RTF_UP != 0 && f & RTF_REJECT == 0)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// When to try again. It is told the routes every [`LOOK_EVERY`] and says
/// whether now is the time: soon after a route appears or changes, and
/// after longer and longer waits while one stays. Without a route it never
/// is.
#[derive(Debug, Default)]
pub struct Retry {
    routes: Vec<String>,
    /// Time since the last try, as counted by the looks.
    waited: Duration,
    tries: usize,
}

impl Retry {
    /// Starts counting after a failed try, with the routes of that moment.
    pub fn failed(&mut self, routes: Vec<String>) {
        self.routes = routes;
        self.waited = Duration::ZERO;
    }

    /// Starts over: it worked, or the user asked.
    pub fn reset(&mut self) {
        *self = Retry::default();
    }

    pub fn due(&mut self, routes: Vec<String>, since_last_look: Duration) -> bool {
        self.waited = self.waited.saturating_add(since_last_look);
        let changed = routes != self.routes;
        self.routes = routes;
        if self.routes.is_empty() {
            return false;
        }
        if changed {
            self.tries = 0;
            return true;
        }
        let wait = Duration::from_secs(WAITS[self.tries.min(WAITS.len() - 1)]);
        if self.waited < wait {
            return false;
        }
        self.tries += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n";

    fn tables(tag: &str, v4: &str, v6: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("sengi-store-net-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("route"), format!("{HEADER}{v4}")).unwrap();
        fs::write(dir.join("ipv6_route"), v6).unwrap();
        dir
    }

    #[test]
    fn a_default_route_is_found_and_a_local_one_is_not() {
        let dir = tables(
            "v4",
            "wlan0\t00000000\t0102A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
             wlan0\t0002A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n",
            "",
        );
        assert_eq!(default_routes(&dir), ["wlan0 0102A8C0"]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rejected_and_loopback_routes_do_not_count() {
        let zero = "0".repeat(32);
        let dir = tables(
            "v6",
            "",
            &format!(
                "{zero} 00 {zero} 00 {zero} ffffffff 00000001 00000000 00200200       lo\n\
                 {zero} 00 {zero} 00 fe800000000000000000000000000001 00000400 00000001 00000000 00000003    wlan0\n\
                 fe800000000000000000000000000000 40 {zero} 00 {zero} 00000100 00000001 00000000 00000001    wlan0\n"
            ),
        );
        assert_eq!(
            default_routes(&dir),
            ["wlan0 fe800000000000000000000000000001"]
        );
        let _ = fs::remove_dir_all(dir);
        assert!(default_routes(Path::new("/nonexistent")).is_empty());
    }

    #[test]
    fn a_route_that_appears_is_tried_at_once_and_then_seldom() {
        let wifi = || vec!["wlan0 1".to_string()];
        let mut retry = Retry::default();
        retry.failed(Vec::new());
        for _ in 0..100 {
            assert!(!retry.due(Vec::new(), LOOK_EVERY));
        }
        assert!(retry.due(wifi(), LOOK_EVERY));
        retry.failed(wifi());
        // Not again at the next look.
        assert!(!retry.due(wifi(), LOOK_EVERY));
        assert!(retry.due(wifi(), LOOK_EVERY));
        let mut tries = 0;
        retry.failed(wifi());
        // An hour of looks, each failed try starting the count again.
        for _ in 0..1200 {
            if retry.due(wifi(), LOOK_EVERY) {
                tries += 1;
                retry.failed(wifi());
            }
        }
        assert!((25..=40).contains(&tries), "{tries} tries in an hour");
    }

    #[test]
    fn another_network_is_tried_at_once() {
        let mut retry = Retry::default();
        retry.failed(vec!["wlan0 1".into()]);
        assert!(retry.due(vec!["eth0 2".into()], LOOK_EVERY));
    }
}
