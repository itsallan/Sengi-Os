//! Wi-Fi as the panel sees it: the networks in range, which to list and in
//! what order, and the steps of joining one. Nothing here talks to iwd;
//! [`iwd`](crate::iwd) turns what iwd says into these types.

use crate::secret::Secret;

/// How many networks the list holds. A busy building has hundreds in range,
/// and anything past the strongest few dozen is out of practical reach.
pub const MAX_NETWORKS: usize = 24;

/// How a network is secured, as far as joining it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Security {
    /// No passphrase.
    Open,
    /// A shared passphrase: WPA2 or WPA3 personal.
    Passphrase,
    /// 802.1X. iwd only joins these from a provisioning file.
    Enterprise,
    /// WEP and whatever else may turn up. Not joinable.
    Other,
}

impl Security {
    /// From the `Type` property of an iwd network.
    pub fn from_iwd(kind: &str) -> Security {
        match kind {
            "open" => Security::Open,
            "psk" => Security::Passphrase,
            "8021x" => Security::Enterprise,
            _ => Security::Other,
        }
    }
}

/// One network in range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    /// Opaque to everything but the service: iwd's object path.
    pub id: String,
    /// Already cleaned for display.
    pub name: String,
    /// How it is secured.
    pub security: Security,
    /// iwd has joined it before and holds what it needs to do so again.
    pub known: bool,
    /// This is the network the device is on, or is joining.
    pub connected: bool,
    /// Signal strength in hundredths of a dBm, as iwd reports it.
    pub signal: i16,
}

/// Where the device stands with the network it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// On no network.
    Disconnected,
    /// Joining one.
    Connecting,
    /// On a network, roaming between its access points included.
    Connected,
}

/// Wi-Fi as a whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wifi {
    /// iwd is not running or has no device.
    Absent,
    /// iwd is running and the bus policy keeps this user out.
    Denied,
    /// There is a device and it is switched off.
    Off,
    /// Switched on.
    On {
        /// Where the device stands.
        link: Link,
        /// A scan is under way.
        scanning: bool,
        /// Arranged: see [`arrange`].
        networks: Vec<Network>,
    },
}

/// Bars to light for a signal, 1 to 4. iwd gives dBm times 100.
pub fn level(signal: i16) -> i32 {
    match signal {
        -6000.. => 4,
        -7000..=-6001 => 3,
        -8000..=-7001 => 2,
        _ => 1,
    }
}

/// The list as shown: one entry per name and kind of security, the
/// connected network first, then the ones iwd knows, then by signal.
/// Several devices, or a service that repeats itself, must not show the
/// same network twice.
///
/// Signal counts by its level, not its exact value, and equal levels go
/// by name. Readings wobble from scan to scan; the list should not, and a
/// network must not be able to shoulder its way up a row by shouting.
pub fn arrange(mut networks: Vec<Network>) -> Vec<Network> {
    let order = |network: &Network| (!network.connected, !network.known, -level(network.signal));
    networks.sort_by(|a, b| {
        order(a)
            .cmp(&order(b))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut seen = std::collections::HashSet::new();
    // The best of each came first in the sort, so the first is kept.
    networks.retain(|network| seen.insert((network.name.clone(), network.security)));
    networks.truncate(MAX_NETWORKS);
    networks
}

/// Why an attempt to join failed, in iwd's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JoinError {
    /// The network refused, which for a typed passphrase nearly always
    /// means it was wrong.
    Failed,
    /// iwd wanted a passphrase and none had been typed. It asks again for
    /// a network it knows once the stored one has failed.
    NeedsPassphrase,
    /// Another request took over, or the passphrase was not supplied.
    Aborted,
    /// The attempt timed out.
    Timeout,
    /// An enterprise network without a provisioning file.
    NotConfigured,
    /// iwd cannot join this kind of network.
    NotSupported,
    /// The bus policy does not let this user join networks.
    Denied,
    /// The service went away or would not take the request.
    Unavailable,
}

/// What is wrong with a passphrase before or after trying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// Under 8 characters.
    TooShort,
    /// Over 63 characters.
    TooLong,
    /// WPA passphrases are printable ASCII.
    NotPlain,
    /// The network turned it down.
    Rejected,
    /// The one iwd had stored stopped working: it was changed at the
    /// router since.
    Stale,
}

/// WPA's rules: 8 to 63 printable ASCII characters. iwd applies the same
/// and would only answer with a vaguer error.
pub fn check_passphrase(passphrase: &str) -> Result<(), Problem> {
    if !passphrase.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        Err(Problem::NotPlain)
    } else if passphrase.len() < 8 {
        Err(Problem::TooShort)
    } else if passphrase.len() > 63 {
        Err(Problem::TooLong)
    } else {
        Ok(())
    }
}

/// What to ask the service for.
#[derive(Debug)]
pub struct Request {
    /// The ID of the [`Network`] to join.
    pub id: String,
    /// The passphrase the user typed, if one was needed. Without one iwd
    /// uses what it has stored.
    pub passphrase: Option<Secret>,
}

/// Where the user is in joining a network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Join {
    /// Nothing is being joined.
    #[default]
    Idle,
    /// An open network nobody has joined before was picked. Anyone can
    /// put up an open network under any name, and everything sent over it
    /// can be read, so one click is not enough: the row asks first.
    Confirming {
        /// The network's ID.
        id: String,
        /// Its name, for the question.
        name: String,
    },
    /// The passphrase field is open for this network.
    Asking {
        /// The network's ID.
        id: String,
        /// Its name, for the prompt.
        name: String,
        /// What was wrong with the last passphrase, if one was tried.
        problem: Option<Problem>,
    },
    /// The service is working on it.
    Joining {
        /// The network's ID.
        id: String,
        /// Its name.
        name: String,
        /// A passphrase was typed for this attempt.
        typed: bool,
        /// The network takes a passphrase, so one can be asked for if
        /// this attempt shows it is needed.
        secured: bool,
    },
    /// The last attempt failed and there is nothing to retype.
    Failed {
        /// The network's name.
        name: String,
        /// Why it failed.
        error: JoinError,
    },
}

impl Join {
    /// A network in the list was clicked.
    pub fn choose(&mut self, network: &Network) -> Option<Request> {
        if matches!(self, Join::Joining { .. }) || network.connected {
            return None;
        }
        let (id, name) = (network.id.clone(), network.name.clone());
        let direct = network.known;
        let request = direct.then(|| Request {
            id: id.clone(),
            passphrase: None,
        });
        *self = match network.security {
            _ if direct => Join::Joining {
                id,
                name,
                typed: false,
                secured: network.security == Security::Passphrase,
            },
            Security::Open => Join::Confirming { id, name },
            Security::Passphrase => Join::Asking {
                id,
                name,
                problem: None,
            },
            Security::Enterprise => Join::Failed {
                name,
                error: JoinError::NotConfigured,
            },
            _ => Join::Failed {
                name,
                error: JoinError::NotSupported,
            },
        };
        request
    }

    /// "Join" was pressed in the row of the open network being asked about.
    pub fn confirm(&mut self, confirmed: &str) -> Option<Request> {
        let Join::Confirming { id, name } = self else {
            return None;
        };
        if id != confirmed {
            return None;
        }
        let request = Request {
            id: id.clone(),
            passphrase: None,
        };
        *self = Join::Joining {
            id: std::mem::take(id),
            name: std::mem::take(name),
            typed: false,
            secured: false,
        };
        Some(request)
    }

    /// Return was pressed in the passphrase field.
    pub fn submit(&mut self, passphrase: Secret) -> Option<Request> {
        let Join::Asking { id, name, problem } = self else {
            return None;
        };
        if let Err(found) = check_passphrase(passphrase.expose()) {
            *problem = Some(found);
            return None;
        }
        let request = Request {
            id: id.clone(),
            passphrase: Some(passphrase),
        };
        *self = Join::Joining {
            id: std::mem::take(id),
            name: std::mem::take(name),
            typed: true,
            secured: true,
        };
        Some(request)
    }

    /// The passphrase field was dismissed, or a failure acknowledged.
    pub fn cancel(&mut self) {
        if !matches!(self, Join::Joining { .. }) {
            *self = Join::Idle;
        }
    }

    /// The service has finished with the request for `finished_id`.
    pub fn finished(&mut self, finished_id: &str, result: Result<(), JoinError>) {
        let Join::Joining {
            id,
            name,
            typed,
            secured,
        } = self
        else {
            return;
        };
        if id != finished_id {
            return;
        }
        *self = match result {
            Ok(()) | Err(JoinError::Aborted) => Join::Idle,
            // Wrong, whether it was typed just now or stored long ago.
            // Either way the cure is to type the right one.
            Err(JoinError::Failed | JoinError::NeedsPassphrase) if *secured => Join::Asking {
                id: std::mem::take(id),
                name: std::mem::take(name),
                problem: Some(if *typed {
                    Problem::Rejected
                } else {
                    Problem::Stale
                }),
            },
            Err(error) => Join::Failed {
                name: std::mem::take(name),
                error,
            },
        };
    }

    /// Wi-Fi was switched off or went away: nothing is left to join.
    pub fn reset(&mut self) {
        *self = Join::Idle;
    }

    /// The network being asked about or joined.
    pub fn target(&self) -> Option<&str> {
        match self {
            Join::Confirming { id, .. } | Join::Asking { id, .. } | Join::Joining { id, .. } => {
                Some(id)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(name: &str, security: Security, known: bool, signal: i16) -> Network {
        Network {
            id: format!("/{name}/{security:?}/{signal}"),
            name: name.to_string(),
            security,
            known,
            connected: false,
            signal,
        }
    }

    fn names(networks: &[Network]) -> Vec<&str> {
        networks.iter().map(|n| n.name.as_str()).collect()
    }

    #[test]
    fn signal_levels() {
        assert_eq!(level(-3000), 4);
        assert_eq!(level(-6000), 4);
        assert_eq!(level(-6001), 3);
        assert_eq!(level(-7000), 3);
        assert_eq!(level(-7001), 2);
        assert_eq!(level(-8000), 2);
        assert_eq!(level(-8001), 1);
        assert_eq!(level(i16::MIN), 1);
        // Nonsense from the service still lands on a level.
        assert_eq!(level(i16::MAX), 4);
    }

    #[test]
    fn known_networks_come_first_then_the_strongest() {
        use Security::*;
        let arranged = arrange(vec![
            network("Strong stranger", Passphrase, false, -4000),
            network("Weak home", Passphrase, true, -8500),
            network("Cafe", Open, false, -6500),
            network("Office", Passphrase, true, -5000),
        ]);
        assert_eq!(
            names(&arranged),
            ["Office", "Weak home", "Strong stranger", "Cafe"]
        );
    }

    #[test]
    fn a_wobbling_signal_does_not_reorder_the_list() {
        use Security::*;
        let scan = |a: i16, b: i16| {
            arrange(vec![
                network("Bakery", Open, false, a),
                network("Attic", Open, false, b),
            ])
        };
        // Both in the same level: by name, whichever is a little louder.
        assert_eq!(names(&scan(-6100, -6900)), ["Attic", "Bakery"]);
        assert_eq!(names(&scan(-6900, -6100)), ["Attic", "Bakery"]);
        // A whole level apart does count.
        assert_eq!(names(&scan(-5000, -6900)), ["Bakery", "Attic"]);
    }

    #[test]
    fn the_connected_network_leads() {
        use Security::*;
        let mut joined = network("Guest", Open, false, -8900);
        joined.connected = true;
        let arranged = arrange(vec![network("Home", Passphrase, true, -4000), joined]);
        assert_eq!(names(&arranged), ["Guest", "Home"]);
    }

    #[test]
    fn one_entry_per_name_and_security() {
        use Security::*;
        let arranged = arrange(vec![
            network("Mesh", Passphrase, false, -7000),
            network("Mesh", Passphrase, false, -5000),
            network("Mesh", Passphrase, true, -8000),
            // The same name on an open network is a different network.
            network("Mesh", Open, false, -6000),
        ]);
        assert_eq!(arranged.len(), 2);
        // The known one stands for the three.
        assert!(arranged[0].known);
        assert_eq!(arranged[0].signal, -8000);
        assert_eq!(arranged[1].security, Open);
        // Of two that differ only in strength, the stronger level is kept.
        let pair = arrange(vec![
            network("Mesh", Passphrase, false, -8500),
            network("Mesh", Passphrase, false, -5000),
        ]);
        assert_eq!(pair[0].signal, -5000);
    }

    #[test]
    fn equal_networks_keep_a_stable_order() {
        use Security::*;
        let a = arrange(vec![
            network("B", Open, false, -5000),
            network("A", Open, false, -5000),
        ]);
        assert_eq!(names(&a), ["A", "B"]);
    }

    #[test]
    fn the_list_is_capped() {
        let many: Vec<Network> = (0..500)
            .map(|i| network(&format!("net {i:03}"), Security::Open, false, -5000 - i))
            .collect();
        let arranged = arrange(many);
        assert_eq!(arranged.len(), MAX_NETWORKS);
        assert_eq!(arranged[0].name, "net 000");
    }

    #[test]
    fn passphrase_rules() {
        assert_eq!(check_passphrase("1234567"), Err(Problem::TooShort));
        assert_eq!(check_passphrase(""), Err(Problem::TooShort));
        assert_eq!(check_passphrase("12345678"), Ok(()));
        assert_eq!(check_passphrase(&"x".repeat(63)), Ok(()));
        assert_eq!(check_passphrase(&"x".repeat(64)), Err(Problem::TooLong));
        assert_eq!(check_passphrase("pässwörter"), Err(Problem::NotPlain));
        assert_eq!(check_passphrase("line\nbreak!"), Err(Problem::NotPlain));
        assert_eq!(check_passphrase("with space ok"), Ok(()));
    }

    fn secret(text: &str) -> Secret {
        Secret::new(text.to_string())
    }

    #[test]
    fn a_known_network_is_joined_at_once() {
        let home = network("Home", Security::Passphrase, true, -5000);
        let mut join = Join::Idle;
        let request = join.choose(&home).unwrap();
        assert_eq!(request.id, home.id);
        assert!(request.passphrase.is_none());
        assert!(matches!(join, Join::Joining { typed: false, .. }));
        join.finished(&home.id, Ok(()));
        assert_eq!(join, Join::Idle);
    }

    #[test]
    fn a_new_open_network_is_not_joined_on_one_click() {
        let cafe = network("Cafe", Security::Open, false, -5000);
        let mut join = Join::Idle;
        assert!(join.choose(&cafe).is_none());
        assert_eq!(
            join,
            Join::Confirming {
                id: cafe.id.clone(),
                name: "Cafe".to_string()
            }
        );
        // Nor on two: a double click on the row is still not a yes.
        assert!(join.choose(&cafe).is_none());
        assert!(matches!(join, Join::Confirming { .. }));
        // Confirming some other network is not a yes to this one.
        assert!(join.confirm("/elsewhere").is_none());

        let request = join.confirm(&cafe.id).unwrap();
        assert_eq!(request.id, cafe.id);
        assert!(request.passphrase.is_none());
        assert!(matches!(join, Join::Joining { typed: false, .. }));
        // A confirmation is used up.
        assert!(join.confirm(&cafe.id).is_none());
    }

    #[test]
    fn the_question_about_an_open_network_can_be_dropped() {
        let cafe = network("Cafe", Security::Open, false, -5000);
        let office = network("Office", Security::Passphrase, false, -5000);
        let mut join = Join::Idle;
        join.choose(&cafe);
        join.cancel();
        assert_eq!(join, Join::Idle);
        assert!(join.confirm(&cafe.id).is_none());
        // Picking another network drops it too.
        join.choose(&cafe);
        join.choose(&office);
        assert!(join.confirm(&cafe.id).is_none());
        assert!(matches!(join, Join::Asking { .. }));
    }

    #[test]
    fn an_open_network_joined_before_needs_no_asking() {
        let cafe = network("Cafe", Security::Open, true, -5000);
        let mut join = Join::Idle;
        assert!(join.choose(&cafe).unwrap().passphrase.is_none());
    }

    #[test]
    fn a_new_secured_network_asks_for_its_passphrase() {
        let office = network("Office", Security::Passphrase, false, -5000);
        let mut join = Join::Idle;
        assert!(join.choose(&office).is_none());
        assert_eq!(
            join,
            Join::Asking {
                id: office.id.clone(),
                name: "Office".to_string(),
                problem: None
            }
        );
        assert_eq!(join.target(), Some(office.id.as_str()));

        // Too short: nothing is sent and the field stays open.
        assert!(join.submit(secret("short")).is_none());
        assert!(matches!(
            join,
            Join::Asking {
                problem: Some(Problem::TooShort),
                ..
            }
        ));

        let request = join.submit(secret("long enough")).unwrap();
        assert_eq!(request.id, office.id);
        assert_eq!(request.passphrase.unwrap().expose(), "long enough");
        assert!(matches!(join, Join::Joining { typed: true, .. }));

        join.finished(&office.id, Ok(()));
        assert_eq!(join, Join::Idle);
    }

    #[test]
    fn a_wrong_passphrase_reopens_the_field() {
        let office = network("Office", Security::Passphrase, false, -5000);
        let mut join = Join::Idle;
        join.choose(&office);
        join.submit(secret("wrong guess")).unwrap();
        join.finished(&office.id, Err(JoinError::Failed));
        assert_eq!(
            join,
            Join::Asking {
                id: office.id.clone(),
                name: "Office".to_string(),
                problem: Some(Problem::Rejected)
            }
        );
        // And a second try goes out like the first.
        assert!(join.submit(secret("right guess")).is_some());
    }

    #[test]
    fn a_saved_network_whose_passphrase_changed_asks_for_the_new_one() {
        let home = network("Home", Security::Passphrase, true, -5000);
        // iwd tries the stored passphrase and fails; or, having failed
        // before, wants to be told and was not.
        for error in [JoinError::Failed, JoinError::NeedsPassphrase] {
            let mut join = Join::Idle;
            assert!(join.choose(&home).unwrap().passphrase.is_none());
            join.finished(&home.id, Err(error));
            assert_eq!(
                join,
                Join::Asking {
                    id: home.id.clone(),
                    name: "Home".to_string(),
                    problem: Some(Problem::Stale)
                }
            );
            let request = join.submit(secret("the new passphrase")).unwrap();
            assert_eq!(request.id, home.id);
            join.finished(&home.id, Ok(()));
            assert_eq!(join, Join::Idle);
        }
    }

    #[test]
    fn a_saved_open_network_that_fails_has_nothing_to_retype() {
        let cafe = network("Cafe", Security::Open, true, -5000);
        let mut join = Join::Idle;
        join.choose(&cafe);
        join.finished(&cafe.id, Err(JoinError::Failed));
        assert_eq!(
            join,
            Join::Failed {
                name: "Cafe".to_string(),
                error: JoinError::Failed
            }
        );
        join.cancel();
        assert_eq!(join, Join::Idle);
    }

    #[test]
    fn a_timeout_with_a_typed_passphrase_is_not_blamed_on_it() {
        let office = network("Office", Security::Passphrase, false, -5000);
        let mut join = Join::Idle;
        join.choose(&office);
        join.submit(secret("perfectly fine")).unwrap();
        join.finished(&office.id, Err(JoinError::Timeout));
        assert!(matches!(
            join,
            Join::Failed {
                error: JoinError::Timeout,
                ..
            }
        ));
    }

    #[test]
    fn an_aborted_attempt_just_ends() {
        let home = network("Home", Security::Passphrase, true, -5000);
        let mut join = Join::Idle;
        join.choose(&home);
        join.finished(&home.id, Err(JoinError::Aborted));
        assert_eq!(join, Join::Idle);
    }

    #[test]
    fn networks_that_cannot_be_joined_say_so() {
        let mut join = Join::Idle;
        let campus = network("Campus", Security::Enterprise, false, -5000);
        assert!(join.choose(&campus).is_none());
        assert!(matches!(
            join,
            Join::Failed {
                error: JoinError::NotConfigured,
                ..
            }
        ));
        // Once provisioned, iwd knows it and it joins like any other.
        let mut provisioned = campus.clone();
        provisioned.known = true;
        assert!(join.choose(&provisioned).is_some());

        let mut join = Join::Idle;
        let relic = network("Relic", Security::Other, false, -5000);
        assert!(join.choose(&relic).is_none());
        assert!(matches!(
            join,
            Join::Failed {
                error: JoinError::NotSupported,
                ..
            }
        ));
    }

    #[test]
    fn clicks_during_an_attempt_are_ignored() {
        let home = network("Home", Security::Passphrase, true, -5000);
        let cafe = network("Cafe", Security::Open, false, -5000);
        let mut join = Join::Idle;
        join.choose(&home);
        let before = join.clone();
        assert!(join.choose(&cafe).is_none());
        assert!(join.submit(secret("not asked for")).is_none());
        join.cancel();
        assert_eq!(join, before);
        // A result for some other network is not ours.
        join.finished(&cafe.id, Ok(()));
        assert_eq!(join, before);
    }

    #[test]
    fn choosing_another_network_moves_the_question() {
        let office = network("Office", Security::Passphrase, false, -5000);
        let lab = network("Lab", Security::Passphrase, false, -5000);
        let mut join = Join::Idle;
        join.choose(&office);
        join.choose(&lab);
        assert_eq!(join.target(), Some(lab.id.as_str()));
        join.cancel();
        assert_eq!(join, Join::Idle);
        assert!(join.submit(secret("nobody asked")).is_none());
    }

    #[test]
    fn the_connected_network_is_not_joined_again() {
        let mut home = network("Home", Security::Passphrase, true, -5000);
        home.connected = true;
        let mut join = Join::Idle;
        assert!(join.choose(&home).is_none());
        assert_eq!(join, Join::Idle);
    }
}
