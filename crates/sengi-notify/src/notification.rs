//! What a client asks for, and the bounded, cleaned-up notification made
//! from it. Everything a client sends is untrusted: every string is cut to
//! a fixed length and every list to a fixed count before it is kept.

use crate::markup;

/// Longest input looked at for any one string, in bytes. Longer bodies are
/// cut here first so that cleaning them costs a bounded amount of work.
pub const RAW_MAX: usize = 16 * 1024;
pub const APP_MAX: usize = 48;
pub const SUMMARY_MAX: usize = 160;
pub const BODY_MAX: usize = 600;
pub const LABEL_MAX: usize = 24;
/// Action keys are sent back to the client untouched, so one that is too
/// long is refused instead of cut.
pub const KEY_MAX: usize = 128;
/// Buttons that fit across a card.
pub const ACTIONS_MAX: usize = 3;
/// Action pairs and hints looked at before giving up on the rest.
pub const SCAN_MAX: usize = 32;
pub const NAME_MAX: usize = 128;

/// The key the spec gives to the action taken by clicking the card itself.
pub const DEFAULT_ACTION: &str = "default";

/// Shortest time a card with a timeout stays up, in milliseconds. A client
/// asking for less would only make it flicker.
pub const TIMEOUT_MIN: u64 = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub key: String,
    pub label: String,
}

/// A notification ready to show.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Notification {
    pub id: u32,
    pub app: String,
    pub summary: String,
    pub body: String,
    pub actions: Vec<Action>,
    /// Clicking the card invokes the `default` action.
    pub has_default: bool,
    pub urgency: Urgency,
    /// Not kept in the history.
    pub transient: bool,
    /// Milliseconds on screen; `None` stays until dismissed.
    pub timeout: Option<u64>,
    pub category: String,
    pub desktop_entry: String,
    /// When it arrived, as shown on the card.
    pub time: String,
}

/// A hint value, reduced to the types the hints we read can have.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HintValue<'a> {
    Bool(bool),
    Int(i64),
    Str(&'a str),
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Hints {
    pub urgency: Urgency,
    pub transient: bool,
    pub category: String,
    pub desktop_entry: String,
}

/// Reads the hints we act on and ignores the rest, `image-data` included.
pub fn parse_hints<'a>(hints: impl IntoIterator<Item = (&'a str, HintValue<'a>)>) -> Hints {
    let mut parsed = Hints::default();
    for (name, value) in hints.into_iter().take(SCAN_MAX) {
        match (name, value) {
            // A byte by the spec; some clients send a wider integer.
            ("urgency", HintValue::Int(level)) => {
                parsed.urgency = match level {
                    0 => Urgency::Low,
                    2 => Urgency::Critical,
                    _ => Urgency::Normal,
                };
            }
            ("transient", HintValue::Bool(transient)) => parsed.transient = transient,
            ("transient", HintValue::Int(transient)) => parsed.transient = transient != 0,
            ("category", HintValue::Str(category)) => parsed.category = identifier(category),
            ("desktop-entry", HintValue::Str(entry)) => parsed.desktop_entry = identifier(entry),
            _ => {}
        }
    }
    parsed
}

/// Categories and desktop entry names are dotted identifiers. Anything
/// else, a path above all, is dropped whole.
fn identifier(text: &str) -> String {
    let fine = !text.is_empty()
        && text.len() <= NAME_MAX
        && !text.starts_with('.')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if fine {
        text.to_string()
    } else {
        String::new()
    }
}

/// Splits the flat `[key, label, key, label, ...]` list. Returns whether
/// there is a default action, and the buttons to draw.
pub fn parse_actions<'a>(flat: impl IntoIterator<Item = &'a str>) -> (bool, Vec<Action>) {
    let mut has_default = false;
    let mut actions: Vec<Action> = Vec::new();
    let mut flat = flat.into_iter();
    for _ in 0..SCAN_MAX {
        let (Some(key), Some(label)) = (flat.next(), flat.next()) else {
            break;
        };
        if key.is_empty() || key.len() > KEY_MAX {
            continue;
        }
        if key == DEFAULT_ACTION {
            has_default = true;
            continue;
        }
        if actions.len() == ACTIONS_MAX || actions.iter().any(|action| action.key == key) {
            continue;
        }
        let mut label = clean_line(label, LABEL_MAX);
        if label.is_empty() {
            label = clean_line(key, LABEL_MAX);
        }
        if label.is_empty() {
            continue;
        }
        actions.push(Action {
            key: key.to_string(),
            label,
        });
    }
    (has_default, actions)
}

/// How long a card stays up, in milliseconds, or `None` to stay until it
/// is dismissed. `requested` is the client's `expire_timeout`: -1 leaves it
/// to us, 0 means never. Critical notifications never time out.
pub fn timeout(requested: i32, urgency: Urgency, default: u64) -> Option<u64> {
    if urgency == Urgency::Critical {
        return None;
    }
    match requested {
        0 => None,
        wanted if wanted < 0 => Some(default.max(TIMEOUT_MIN)),
        wanted => Some(u64::from(wanted.unsigned_abs()).max(TIMEOUT_MIN)),
    }
}

fn clean_line(text: &str, max: usize) -> String {
    markup::tidy(markup::clip_bytes(text, RAW_MAX), max, false)
}

/// The arguments of a `Notify` call, as they came off the bus.
pub struct Request<'a> {
    pub app_name: &'a str,
    pub summary: &'a str,
    pub body: &'a str,
    pub actions: Vec<&'a str>,
    pub hints: Hints,
    pub expire_timeout: i32,
}

impl Request<'_> {
    /// Cleans and bounds everything. `default_timeout` is in milliseconds.
    pub fn into_notification(self, id: u32, default_timeout: u64) -> Notification {
        let (has_default, actions) = parse_actions(self.actions);
        let body = markup::strip(markup::clip_bytes(self.body, RAW_MAX));
        let mut app = clean_line(self.app_name, APP_MAX);
        if app.is_empty() {
            // "org.example.Mail" at least says Mail.
            let entry = self.hints.desktop_entry.as_str();
            app = entry.rsplit('.').next().unwrap_or(entry).to_string();
        }
        Notification {
            id,
            app,
            summary: clean_line(self.summary, SUMMARY_MAX),
            body: markup::tidy(&body, BODY_MAX, true),
            actions,
            has_default,
            urgency: self.hints.urgency,
            transient: self.hints.transient,
            timeout: timeout(self.expire_timeout, self.hints.urgency, default_timeout),
            category: self.hints.category,
            desktop_entry: self.hints.desktop_entry,
            time: String::new(),
        }
    }
}

/// What `GetCapabilities` answers. Only what is really done: bodies are
/// shown, actions get buttons, and body markup is accepted and stripped.
/// No icons, images, sounds, hyperlinks or persistence.
pub const CAPABILITIES: [&str; 3] = ["actions", "body", "body-markup"];

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(summary: &'a str, body: &'a str) -> Request<'a> {
        Request {
            app_name: "Test",
            summary,
            body,
            actions: Vec::new(),
            hints: Hints::default(),
            expire_timeout: -1,
        }
    }

    #[test]
    fn urgency_is_read_from_any_integer_and_defaults_to_normal() {
        let urgency = |value| parse_hints([("urgency", value)]).urgency;
        assert_eq!(urgency(HintValue::Int(0)), Urgency::Low);
        assert_eq!(urgency(HintValue::Int(1)), Urgency::Normal);
        assert_eq!(urgency(HintValue::Int(2)), Urgency::Critical);
        assert_eq!(urgency(HintValue::Int(3)), Urgency::Normal);
        assert_eq!(urgency(HintValue::Int(-1)), Urgency::Normal);
        assert_eq!(urgency(HintValue::Str("2")), Urgency::Normal);
        assert_eq!(urgency(HintValue::Other), Urgency::Normal);
        assert_eq!(parse_hints([]).urgency, Urgency::Normal);
    }

    #[test]
    fn transient_is_a_boolean_or_a_number() {
        let transient = |value| parse_hints([("transient", value)]).transient;
        assert!(transient(HintValue::Bool(true)));
        assert!(!transient(HintValue::Bool(false)));
        assert!(transient(HintValue::Int(1)));
        assert!(!transient(HintValue::Int(0)));
        assert!(!transient(HintValue::Str("true")));
    }

    #[test]
    fn names_that_are_not_identifiers_are_dropped() {
        let hints = parse_hints([
            ("category", HintValue::Str("im.received")),
            ("desktop-entry", HintValue::Str("org.mozilla.firefox")),
        ]);
        assert_eq!(hints.category, "im.received");
        assert_eq!(hints.desktop_entry, "org.mozilla.firefox");

        for bad in [
            "../../etc/passwd",
            "/etc/passwd",
            "a b",
            ".hidden",
            "",
            "a\0b",
        ] {
            let hints = parse_hints([("desktop-entry", HintValue::Str(bad))]);
            assert_eq!(hints.desktop_entry, "", "{bad:?} was kept");
        }
        let long = "a".repeat(NAME_MAX + 1);
        assert_eq!(
            parse_hints([("category", HintValue::Str(&long))]).category,
            ""
        );
    }

    #[test]
    fn unknown_hints_and_wrong_types_are_ignored() {
        let hints = parse_hints([
            ("image-data", HintValue::Other),
            ("category", HintValue::Int(5)),
            ("desktop-entry", HintValue::Bool(true)),
            ("x-made-up", HintValue::Str("x")),
        ]);
        assert_eq!(hints, Hints::default());
    }

    #[test]
    fn only_the_first_hints_are_looked_at() {
        let mut many = vec![("x-padding", HintValue::Other); SCAN_MAX];
        many.push(("urgency", HintValue::Int(2)));
        assert_eq!(parse_hints(many).urgency, Urgency::Normal);
    }

    #[test]
    fn actions_come_in_pairs_and_default_is_not_a_button() {
        let (has_default, actions) =
            parse_actions(["default", "Open", "reply", "Reply", "mute", "Mute"]);
        assert!(has_default);
        assert_eq!(
            actions,
            vec![
                Action {
                    key: "reply".into(),
                    label: "Reply".into()
                },
                Action {
                    key: "mute".into(),
                    label: "Mute".into()
                },
            ]
        );
        // A key with no label after it is not an action.
        let (has_default, actions) = parse_actions(["reply", "Reply", "orphan"]);
        assert!(!has_default);
        assert_eq!(actions.len(), 1);
        assert_eq!(parse_actions([]), (false, Vec::new()));
    }

    #[test]
    fn actions_are_capped_and_cleaned() {
        let flat: Vec<String> = (0..100)
            .flat_map(|n| [format!("k{n}"), format!("L{n}")])
            .collect();
        let (_, actions) = parse_actions(flat.iter().map(String::as_str));
        assert_eq!(actions.len(), ACTIONS_MAX);

        let long_key = "k".repeat(KEY_MAX + 1);
        let long_label = "L".repeat(500);
        let (_, actions) = parse_actions([
            long_key.as_str(),
            "Too long a key",
            "",
            "Empty key",
            "ok",
            long_label.as_str(),
            "ok",
            "Same key again",
            "bare",
            "",
            "ctl",
            "a\u{202e}b\nc",
        ]);
        let labels: Vec<&str> = actions.iter().map(|action| action.label.as_str()).collect();
        assert_eq!(labels.len(), 3);
        assert_eq!(labels[0].chars().count(), LABEL_MAX);
        assert!(labels[0].ends_with('\u{2026}'));
        assert_eq!(labels[1], "bare");
        assert_eq!(labels[2], "ab c");
        assert_eq!(actions[0].key, "ok");
    }

    #[test]
    fn a_default_action_past_the_scan_limit_is_not_seen() {
        let mut flat: Vec<&str> = Vec::new();
        for _ in 0..SCAN_MAX {
            flat.extend(["same", "Same"]);
        }
        flat.extend(["default", "Open"]);
        assert!(!parse_actions(flat).0);
    }

    #[test]
    fn timeouts_follow_the_spec() {
        // -1 is ours to choose.
        assert_eq!(timeout(-1, Urgency::Normal, 5_000), Some(5_000));
        assert_eq!(timeout(-1, Urgency::Low, 5_000), Some(5_000));
        assert_eq!(timeout(i32::MIN, Urgency::Normal, 5_000), Some(5_000));
        // 0 never expires.
        assert_eq!(timeout(0, Urgency::Normal, 5_000), None);
        // Anything else is taken as given, within reason.
        assert_eq!(timeout(8_000, Urgency::Normal, 5_000), Some(8_000));
        assert_eq!(timeout(1, Urgency::Normal, 5_000), Some(TIMEOUT_MIN));
        assert_eq!(
            timeout(i32::MAX, Urgency::Low, 5_000),
            Some(i32::MAX as u64)
        );
        // Critical stays whatever was asked for.
        assert_eq!(timeout(-1, Urgency::Critical, 5_000), None);
        assert_eq!(timeout(3_000, Urgency::Critical, 5_000), None);
        // A silly default cannot make cards flicker either.
        assert_eq!(timeout(-1, Urgency::Normal, 0), Some(TIMEOUT_MIN));
    }

    #[test]
    fn every_string_is_bounded() {
        let huge = "é".repeat(200_000);
        let mut request = request(&huge, &huge);
        request.app_name = &huge;
        let note = request.into_notification(1, 5_000);
        assert_eq!(note.app.chars().count(), APP_MAX);
        assert_eq!(note.summary.chars().count(), SUMMARY_MAX);
        assert_eq!(note.body.chars().count(), BODY_MAX);
    }

    #[test]
    fn a_nameless_app_is_named_after_its_desktop_entry() {
        let mut nameless = request("s", "b");
        nameless.app_name = "";
        nameless.hints.desktop_entry = "org.example.Mail".into();
        assert_eq!(nameless.into_notification(1, 5_000).app, "Mail");

        let mut named = request("s", "b");
        named.hints.desktop_entry = "org.example.Mail".into();
        assert_eq!(named.into_notification(1, 5_000).app, "Test");

        let mut neither = request("s", "b");
        neither.app_name = "";
        assert_eq!(neither.into_notification(1, 5_000).app, "");
    }

    #[test]
    fn markup_is_only_read_in_the_body() {
        let note = request("<b>5 &lt; 6</b>", "<b>5 &lt; 6</b>").into_notification(1, 5_000);
        assert_eq!(note.summary, "<b>5 &lt; 6</b>");
        assert_eq!(note.body, "5 < 6");
    }

    #[test]
    fn hostile_text_comes_out_plain() {
        let body = "<img src=\"file:///etc/shadow\" alt=\"x\"><script>\u{1b}]0;t\u{7}</script>\
                    &#x202e;evil&#0;<a href=\"javascript:1\">link</a>\n\n\n\n\nend";
        let note = request("line one\nline two\u{0}", body).into_notification(1, 5_000);
        assert_eq!(note.summary, "line one line two");
        assert_eq!(note.body, "x]0;tevillink\n\nend");
    }

    #[test]
    fn the_capabilities_are_the_ones_implemented() {
        assert!(CAPABILITIES.contains(&"body"));
        assert!(CAPABILITIES.contains(&"actions"));
        assert!(CAPABILITIES.contains(&"body-markup"));
        for absent in [
            "icon-static",
            "body-images",
            "body-hyperlinks",
            "persistence",
            "sound",
        ] {
            assert!(!CAPABILITIES.contains(&absent));
        }
    }
}
