//! Decides which app scopes belong to the focused window.
//!
//! The compositor tells us the app id of the focused window, not its process,
//! so scopes are matched by name. `sengi-run` and Flatpak both put the app
//! name in the scope's unit name, and the process names inside the scope are
//! a fallback for anything started another way.

/// One `*.scope` unit under the user's `app.slice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub unit: String,
    /// `comm` of each process in the scope.
    pub processes: Vec<String>,
}

/// The kernel cuts `comm` to this many bytes.
const COMM_LEN: usize = 15;

fn normalise(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Names a window's app id may go by: the id itself and, for reverse-DNS ids
/// like `org.mozilla.firefox`, the last part.
fn keys(app_id: &str) -> Vec<String> {
    let mut keys = vec![normalise(app_id)];
    if let Some((_, last)) = app_id.rsplit_once('.') {
        if !last.is_empty() {
            keys.push(normalise(last));
        }
    }
    keys.dedup();
    keys
}

fn unit_matches(unit: &str, key: &str) -> bool {
    let unit = normalise(unit.strip_suffix(".scope").unwrap_or(unit));
    // Unit names look like app-<name>-<id>, so the name sits between
    // separators. Matching whole segments keeps "foot" from hitting
    // "footclient".
    let padded = format!("_{unit}_");
    padded.contains(&format!("_{key}_"))
}

fn process_matches(comm: &str, key: &str) -> bool {
    let comm = normalise(comm);
    let key: String = key.chars().take(COMM_LEN).collect();
    comm == key
}

/// Units of every scope that looks like it belongs to `app_id`. Two windows
/// of the same app can live in two scopes, and then both are returned.
pub fn scopes_for(app_id: &str, scopes: &[Scope]) -> Vec<String> {
    if app_id.is_empty() {
        return Vec::new();
    }
    let keys = keys(app_id);

    let by_unit: Vec<String> = scopes
        .iter()
        .filter(|scope| keys.iter().any(|key| unit_matches(&scope.unit, key)))
        .map(|scope| scope.unit.clone())
        .collect();
    if !by_unit.is_empty() {
        return by_unit;
    }

    scopes
        .iter()
        .filter(|scope| {
            scope
                .processes
                .iter()
                .any(|comm| keys.iter().any(|key| process_matches(comm, key)))
        })
        .map(|scope| scope.unit.clone())
        .collect()
}

/// What has to change to move the boost from `current` to `wanted`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub boost: Vec<String>,
    pub restore: Vec<String>,
}

pub fn plan(current: &[String], wanted: &[String]) -> Change {
    Change {
        boost: wanted
            .iter()
            .filter(|u| !current.contains(u))
            .cloned()
            .collect(),
        restore: current
            .iter()
            .filter(|u| !wanted.contains(u))
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(unit: &str, processes: &[&str]) -> Scope {
        Scope {
            unit: unit.to_string(),
            processes: processes.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn matches_a_scope_started_by_sengi_run() {
        let scopes = [scope("app-foot-812.scope", &["foot", "bash"])];
        assert_eq!(scopes_for("foot", &scopes), ["app-foot-812.scope"]);
    }

    #[test]
    fn does_not_match_a_longer_name() {
        let scopes = [scope("app-footclient-9.scope", &["footclient"])];
        assert!(scopes_for("foot", &scopes).is_empty());
    }

    #[test]
    fn matches_a_flatpak_scope_by_full_id() {
        let scopes = [scope(
            "app-flatpak-org.mozilla.firefox-4410.scope",
            &["bwrap", "firefox"],
        )];
        assert_eq!(
            scopes_for("org.mozilla.firefox", &scopes),
            ["app-flatpak-org.mozilla.firefox-4410.scope"]
        );
    }

    #[test]
    fn reverse_dns_id_matches_a_plain_scope() {
        let scopes = [scope("app-firefox-77.scope", &["firefox"])];
        assert_eq!(
            scopes_for("org.mozilla.firefox", &scopes),
            ["app-firefox-77.scope"]
        );
    }

    #[test]
    fn falls_back_to_process_names() {
        let scopes = [
            scope("app-launcher_wrapper-5.scope", &["sh", "gimp-2.10"]),
            scope("app-foot-6.scope", &["foot"]),
        ];
        assert_eq!(
            scopes_for("gimp-2.10", &scopes),
            ["app-launcher_wrapper-5.scope"]
        );
    }

    #[test]
    fn long_names_match_the_truncated_comm() {
        let scopes = [scope("app-wrapper-5.scope", &["a-very-long-pro"])];
        assert_eq!(
            scopes_for("a-very-long-program-name", &scopes),
            ["app-wrapper-5.scope"]
        );
    }

    #[test]
    fn every_scope_of_the_app_is_returned() {
        let scopes = [
            scope("app-foot-1.scope", &["foot"]),
            scope("app-foot-2.scope", &["foot"]),
            scope("app-other-3.scope", &["other"]),
        ];
        assert_eq!(
            scopes_for("foot", &scopes),
            ["app-foot-1.scope", "app-foot-2.scope"]
        );
    }

    #[test]
    fn an_empty_app_id_matches_nothing() {
        let scopes = [scope("app--1.scope", &[""])];
        assert!(scopes_for("", &scopes).is_empty());
    }

    #[test]
    fn plan_only_touches_what_changed() {
        let current = ["a.scope".to_string(), "b.scope".to_string()];
        let wanted = ["b.scope".to_string(), "c.scope".to_string()];
        let change = plan(&current, &wanted);
        assert_eq!(change.boost, ["c.scope"]);
        assert_eq!(change.restore, ["a.scope"]);
    }
}
