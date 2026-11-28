//! Taking what identifies a person or a machine out of the report's text.
//!
//! Two kinds of thing are replaced. Names this computer knows (the
//! accounts, the computer's name, Wi-Fi and Bluetooth names, the labels
//! of drives) are looked for as whole words. Things with a recognisable
//! form (addresses, serial numbers, unique identifiers, paths into a home
//! folder) are found by that form. Everything found on a line is marked
//! first and replaced at the end, so that replacing one thing never hides
//! another from being found.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Account and computer names that every installation of the kind has.
/// They say nothing about anyone, and replacing the word "live" wherever
/// it stands would only spoil the report.
const COMMON: &[&str] = &["root", "live", "sengios", "localhost", "(none)"];

/// A stretch of a line to be replaced, in characters.
#[derive(Debug, Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
    with: &'static str,
}

struct Literal {
    text: Vec<char>,
    with: &'static str,
    any_case: bool,
}

/// What to look for, and the doing of it.
#[derive(Default)]
pub struct Redactor {
    literals: Vec<Literal>,
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

impl Redactor {
    fn add(&mut self, text: &str, with: &'static str, any_case: bool) {
        // A single letter is no name, and replacing it would leave nothing
        // to read.
        let text = plain(text);
        let text = text.trim();
        if text.chars().count() < 2 {
            return;
        }
        self.literals.push(Literal {
            text: text.chars().collect(),
            with,
            any_case,
        });
    }

    /// An account's name.
    pub fn user(&mut self, name: &str) {
        if !COMMON.iter().any(|c| name.trim().eq_ignore_ascii_case(c)) {
            self.add(name, "<user>", true);
        }
    }

    /// The name of the person an account is for: as a whole, and each
    /// word of it that is long enough to be a name.
    pub fn full_name(&mut self, name: &str) {
        let name = name.trim();
        // What the live image calls its account.
        if name.eq_ignore_ascii_case("Sengi OS") {
            return;
        }
        self.add(name, "<name>", true);
        for part in name.split_whitespace() {
            if part.chars().count() >= 3 {
                self.add(part, "<name>", true);
            }
        }
    }

    /// The computer's name, and the first part of it if it has a domain,
    /// which is how most messages say it.
    pub fn computer(&mut self, name: &str) {
        let short = name.split('.').next().unwrap_or_default();
        for name in [name, short] {
            if !COMMON.iter().any(|c| name.trim().eq_ignore_ascii_case(c)) {
                self.add(name, "<computer>", true);
            }
        }
    }

    /// A Wi-Fi network's name.
    pub fn network(&mut self, name: &str) {
        self.add(name, "<network>", false);
    }

    /// A Bluetooth device's name, this computer's among them.
    pub fn bluetooth(&mut self, name: &str) {
        // This computer's own Bluetooth name is its host name, and the
        // default one is nobody's.
        if !COMMON.iter().any(|c| name.trim().eq_ignore_ascii_case(c)) {
            self.add(name, "<bluetooth-device>", false);
        }
    }

    /// The label of a drive or a volume.
    pub fn volume(&mut self, label: &str) {
        self.add(label, "<volume>", false);
    }

    /// A home folder that is somewhere other than under `/home`.
    pub fn home(&mut self, path: &str) {
        let path = path.trim_end_matches('/');
        if path.len() > 1 && path != "/root" && !path.starts_with("/home/") {
            self.add(path, "~", false);
        }
    }

    /// Every line of `text`, redacted.
    pub fn text(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for line in text.lines() {
            out.push_str(&self.line(line));
            out.push('\n');
        }
        out
    }

    /// One line, redacted. Control characters are dropped and the `\x20`
    /// escapes of unit names and logs are read first, so that a name
    /// cannot pass by being written another way.
    pub fn line(&self, line: &str) -> String {
        let line: Vec<char> = plain(line).chars().collect();
        let low: Vec<char> = line.iter().map(|&c| fold(c)).collect();
        let mut spans = Vec::new();
        self.names(&line, &low, &mut spans);
        paths(&line, &mut spans);
        keyed(&line, &low, &mut spans);
        addresses(&line, &low, &mut spans);
        identifiers(&line, &low, &mut spans);
        replace(&line, spans)
    }

    fn names(&self, line: &[char], low: &[char], spans: &mut Vec<Span>) {
        for literal in &self.literals {
            let needle: Vec<char> = if literal.any_case {
                literal.text.iter().map(|&c| fold(c)).collect()
            } else {
                literal.text.clone()
            };
            let hay = if literal.any_case { low } else { line };
            let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric());
            let mut from = 0;
            while let Some(at) = find(hay, from, &needle) {
                let end = at + needle.len();
                // A whole word: "ann" is not part of "scanning".
                let joined_before = word(needle.first()) && at > 0 && word(line.get(at - 1));
                let joined_after = word(needle.last()) && word(line.get(end));
                if !joined_before && !joined_after {
                    spans.push(Span {
                        start: at,
                        end,
                        with: literal.with,
                    });
                }
                from = at + 1;
            }
        }
    }
}

/// `line` without escape sequences and characters that draw nothing, and
/// with `\xNN` escapes read as the bytes they stand for.
pub fn plain(line: &str) -> String {
    let mut shown = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
        } else if c == '\t' {
            shown.push(' ');
        } else if !sengi_text::hidden(c) {
            shown.push(c);
        }
    }
    unescape(&shown)
}

fn unescape(text: &str) -> String {
    if !text.contains("\\x") {
        return text.to_string();
    }
    let byte = |at: &[u8]| match at {
        [b'\\', b'x', high, low, ..] => {
            let high = char::from(*high).to_digit(16)?;
            let low = char::from(*low).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()
        }
        _ => None,
    };
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        match byte(&bytes[at..]) {
            // A control character stays written as it was.
            Some(value) if value >= 0x20 && value != 0x7f => {
                out.push(value);
                at += 4;
            }
            _ => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out)
        .chars()
        .filter(|&c| !sengi_text::hidden(c))
        .collect()
}

fn find(hay: &[char], from: usize, needle: &[char]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&at| hay[at..at + needle.len()] == *needle)
}

/// Every place `word` starts in `hay`.
fn each(hay: &[char], word: &str) -> Vec<usize> {
    let needle: Vec<char> = word.chars().collect();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = find(hay, from, &needle) {
        found.push(at);
        from = at + 1;
    }
    found
}

fn starts(line: &[char], at: usize, word: &str) -> bool {
    word.chars()
        .enumerate()
        .all(|(i, c)| line.get(at + i) == Some(&c))
}

/// Whether nothing of a word stands straight before `at`.
fn opens_word(line: &[char], at: usize) -> bool {
    at == 0 || !line[at - 1].is_alphanumeric()
}

/// The first place from `from` on where `ends` holds, or the line's end.
fn until(line: &[char], from: usize, ends: impl Fn(char) -> bool) -> usize {
    (from..line.len())
        .find(|&at| ends(line[at]))
        .unwrap_or(line.len())
}

fn skip(line: &[char], from: usize, what: impl Fn(char) -> bool) -> usize {
    until(line, from, |c| !what(c))
}

fn push(spans: &mut Vec<Span>, start: usize, end: usize, with: &'static str) {
    if end > start {
        spans.push(Span { start, end, with });
    }
}

/// Where a path that may have spaces in it can be said to end.
fn path_end(line: &[char], from: usize) -> usize {
    let mut end = until(line, from, |c| "'\":)],;|".contains(c));
    while end > from && line[end - 1].is_whitespace() {
        end -= 1;
    }
    end
}

/// Home folders, and the places drives are mounted, which are named after
/// the account and the drive.
fn paths(line: &[char], spans: &mut Vec<Span>) {
    for at in each(line, "/home/") {
        let name = at + "/home/".len();
        let end = until(line, name, |c| {
            c.is_whitespace() || "/'\":)],;|".contains(c)
        });
        if end == name {
            continue;
        }
        // Where settings and caches are says what is wrong. Any other file
        // in a home folder has a name its owner chose.
        let kept = ["/.config/", "/.cache/", "/.local/state/"]
            .iter()
            .any(|dir| starts(line, end, dir));
        if !kept && line.get(end) == Some(&'/') && path_end(line, end + 1) > end + 1 {
            push(spans, at, path_end(line, end), "~/<file>");
        } else {
            push(spans, at, end, "~");
        }
    }
    for prefix in ["/media/", "/run/media/"] {
        for at in each(line, prefix) {
            // At the start of a path, not a folder called media inside one.
            if at == 0 || !(line[at - 1].is_alphanumeric() || "/._-".contains(line[at - 1])) {
                let start = at + prefix.len();
                push(spans, start, path_end(line, start), "<volume>");
            }
        }
    }
    // The same places as systemd writes them in the names of mount units.
    for prefix in ["media-", "run-media-"] {
        for at in each(line, prefix) {
            if opens_word(line, at) {
                let start = at + prefix.len();
                for suffix in [".mount", ".automount"] {
                    let needle: Vec<char> = suffix.chars().collect();
                    if let Some(end) = find(line, start, &needle) {
                        push(spans, start, end, "<volume>");
                    }
                }
            }
        }
    }
}

/// Values that say what they are: `UUID=...`, `SerialNumber: ...`,
/// `SSID='...'` and the like.
fn keyed(line: &[char], low: &[char], spans: &mut Vec<Span>) {
    let ends_value = |c: char| c.is_whitespace() || ",;)]\"'".contains(c);

    for (key, with) in [("uuid=", "<uuid>"), ("label=", "<volume>")] {
        for at in each(low, key) {
            let mut start = at + key.len();
            let end = match line.get(start) {
                Some(&quote @ ('"' | '\'')) => {
                    start += 1;
                    until(line, start, |c| c == quote)
                }
                _ => until(line, start, ends_value),
            };
            push(spans, start, end, with);
        }
    }

    for (key, with) in [
        ("by-uuid", "<uuid>"),
        ("by-partuuid", "<uuid>"),
        ("by-id", "<id>"),
        ("by-label", "<volume>"),
        ("by-partlabel", "<volume>"),
        ("udisks2/drives", "<id>"),
    ] {
        for at in each(low, key) {
            let joint = at + key.len();
            if !matches!(line.get(joint), Some('/' | '-')) {
                continue;
            }
            let start = joint + 1;
            let mut end = until(line, start, |c| c.is_whitespace() || "\"'".contains(c));
            for unit in [".device", ".mount", ".service", ".swap"] {
                let needle: Vec<char> = unit.chars().collect();
                if let Some(found) = find(low, start, &needle) {
                    end = end.min(found);
                }
            }
            push(spans, start, end, with);
        }
    }

    for key in ["wwn", "naa.", "eui."] {
        for at in each(low, key) {
            if !opens_word(line, at) {
                continue;
            }
            let start = skip(line, at + key.len(), |c| " -=:".contains(c));
            let end = until(line, start, |c| !c.is_ascii_alphanumeric());
            if end >= start + 8 {
                push(spans, start, end, "<serial>");
            }
        }
    }

    serials(line, low, spans);

    for at in each(low, "ssid") {
        // "ssid" and "essid" as words, not the end of "classid".
        let essid = at > 0 && low[at - 1] == 'e' && opens_word(line, at - 1);
        if !(opens_word(line, at) || essid) {
            continue;
        }
        let mut start = skip(line, at + "ssid".len(), char::is_whitespace);
        let separated = matches!(line.get(start), Some(':' | '='));
        if separated {
            start = skip(line, start + 1, char::is_whitespace);
        }
        match line.get(start) {
            Some(&quote @ ('"' | '\'')) => {
                let end = until(line, start + 1, |c| c == quote);
                push(spans, start + 1, end, "<network>");
            }
            _ if separated => {
                push(spans, start, until(line, start, |c| c == ','), "<network>");
            }
            _ => {}
        }
    }
    // systemd-networkd: "wlan0: Connected WiFi access point: name (address)".
    for at in each(low, "access point: ") {
        let start = at + "access point: ".len();
        let end = (start..line.len())
            .rev()
            .find(|&i| starts(line, i, " ("))
            .unwrap_or(line.len());
        push(spans, start, end, "<network>");
    }
    // The kernel names a Bluetooth keyboard or mouse as it appears.
    if !each(low, "bluetooth hid").is_empty() {
        for at in each(line, "[") {
            // Not the time stamp at the start of a kernel line.
            if at > 0 {
                let end = until(line, at + 1, |c| c == ']');
                let stamp = line[at + 1..end]
                    .iter()
                    .all(|&c| c.is_ascii_digit() || c == '.' || c == ' ');
                if !stamp {
                    push(spans, at + 1, end, "<bluetooth-device>");
                }
            }
        }
    }
    for at in each(low, "input: ") {
        let start = at + "input: ".len();
        let needle: Vec<char> = " as /devices/virtual/".chars().collect();
        if let Some(end) = find(low, start, &needle) {
            push(spans, start, end, "<bluetooth-device>");
        }
    }
}

fn serials(line: &[char], low: &[char], spans: &mut Vec<Span>) {
    for key in [
        "serialnumber",
        "serial number",
        "serial_number",
        "serial no",
        "serialno",
        "serial#",
        "iserial",
        "id_serial_short",
        "id_serial",
        "s/n",
        "serial",
    ] {
        // The bare word is also the name of a kind of port.
        let bare = key == "serial";
        for at in each(low, key) {
            if !opens_word(line, at) {
                continue;
            }
            let mut start = at + key.len();
            if line.get(start) == Some(&'.') {
                start += 1;
            }
            start = skip(line, start, char::is_whitespace);
            let separator = line.get(start).copied().filter(|c| ":=#".contains(*c));
            if let Some(separator) = separator {
                start = skip(line, start, |c| ":=# ".contains(c));
                let end = if separator == '=' {
                    until(line, start, |c| c.is_whitespace() || ",;".contains(c))
                } else {
                    until(line, start, |c| c == ',')
                };
                if !bare || line[start..end].iter().any(char::is_ascii_digit) {
                    push(spans, start, end, "<serial>");
                }
                continue;
            }
            // lsusb puts the descriptor's number before the serial itself.
            if key == "iserial" {
                push(spans, start, line.len(), "<serial>");
                continue;
            }
            if starts(low, start, "is ") {
                start += 3;
            }
            let end = until(line, start, |c| c.is_whitespace() || ",;".contains(c));
            let wanted = if bare {
                starts(low, start, "0x")
            } else {
                line[start..end].iter().any(char::is_ascii_digit)
            };
            if wanted {
                push(spans, start, end, "<serial>");
            }
        }
    }
}

fn hex(line: &[char], at: usize) -> bool {
    line.get(at).is_some_and(char::is_ascii_hexdigit)
}

/// Hardware and internet addresses.
fn addresses(line: &[char], low: &[char], spans: &mut Vec<Span>) {
    let alnum = |at: usize| line.get(at).is_some_and(|c| c.is_alphanumeric());

    // Six or more pairs with the same mark between them: a MAC address,
    // with colons, dashes or, in bus names, underscores.
    for at in 0..line.len() {
        if !(opens_word(line, at) && hex(line, at) && hex(line, at + 1)) {
            continue;
        }
        let Some(&mark @ (':' | '-' | '_')) = line.get(at + 2) else {
            continue;
        };
        let mut end = at + 2;
        let mut pairs = 1;
        while line.get(end) == Some(&mark)
            && hex(line, end + 1)
            && hex(line, end + 2)
            && !alnum(end + 3)
        {
            end += 3;
            pairs += 1;
        }
        if pairs >= 6 {
            push(spans, at, end, "<mac>");
        }
    }
    // Interfaces udev names after their address.
    for prefix in ["enx", "wlx", "wwx"] {
        for at in each(low, prefix) {
            let start = at + prefix.len();
            if (start..start + 12).all(|i| hex(line, i)) {
                push(spans, start, start + 12, "<mac>");
            }
        }
    }

    let mut at = 0;
    while at < line.len() {
        let end = until(line, at, |c| !(c.is_ascii_digit() || c == '.'));
        if end == at {
            at += 1;
            continue;
        }
        let mut last = end;
        while last > at && line[last - 1] == '.' {
            last -= 1;
        }
        if (at == 0 || !(line[at - 1].is_alphanumeric() || line[at - 1] == '.')) && !alnum(last) {
            let text: String = line[at..last].iter().collect();
            if let Ok(address) = text.parse::<Ipv4Addr>() {
                let nobody =
                    address.is_unspecified() || address.is_loopback() || address.octets()[0] == 255;
                let before: String = low[..at].iter().collect();
                let version = before
                    .trim_end_matches([' ', ':', '='])
                    .ends_with("version");
                if !nobody && !version {
                    push(spans, at, last, "<ip>");
                }
            }
        }
        at = end;
    }

    let mut at = 0;
    while at < line.len() {
        let end = until(line, at, |c| {
            !(c.is_ascii_hexdigit() || c == ':' || c == '.')
        });
        if end == at {
            at += 1;
            continue;
        }
        let joined = |c: &char| c.is_alphanumeric() || *c == '_';
        if !line.get(end).is_some_and(joined) {
            // The address may follow a word that ends in hex letters, as
            // in "inet6:fe80::1", so it is tried from after each colon.
            let from = (at..end).filter(|&i| {
                if i == at {
                    i == 0 || !joined(&line[i - 1])
                } else {
                    line[i - 1] == ':'
                }
            });
            for start in from {
                if let Some(last) = ipv6(line, start, end) {
                    push(spans, start, last, "<ip>");
                    break;
                }
            }
        }
        at = end;
    }
}

/// Where an IPv6 address that starts at `start` stops, if one does and is
/// somebody's.
fn ipv6(line: &[char], start: usize, end: usize) -> Option<usize> {
    let mut last = end;
    while last > start && line[last - 1] == '.' {
        last -= 1;
    }
    // With one colon left over from the words after it, or without.
    let shorter = (last > start && line[last - 1] == ':').then(|| last - 1);
    [Some(last), shorter].into_iter().flatten().find(|&last| {
        let text: String = line[start..last].iter().collect();
        text.matches(':').count() >= 2
            && text
                .parse::<Ipv6Addr>()
                .is_ok_and(|address| !address.is_unspecified() && !address.is_loopback())
    })
}

/// Unique identifiers and the serial numbers inside device names.
fn identifiers(line: &[char], low: &[char], spans: &mut Vec<Span>) {
    for at in 0..line.len() {
        if at > 0 && line[at - 1].is_ascii_hexdigit() {
            continue;
        }
        let mut end = at;
        let whole = [8, 4, 4, 4, 12].iter().enumerate().all(|(group, &size)| {
            if group > 0 {
                if !matches!(line.get(end), Some('-' | '_')) {
                    return false;
                }
                end += 1;
            }
            let found = (end..end + size).all(|i| hex(line, i));
            end += size;
            found
        });
        if whole && !hex(line, end) {
            push(spans, at, end, "<uuid>");
        }
    }

    // Long runs of hex: machine and boot numbers, volume numbers, serials.
    let mut at = 0;
    while at < line.len() {
        let end = until(line, at, |c| !c.is_alphanumeric());
        if end == at {
            at += 1;
            continue;
        }
        let word = &line[at..end];
        if word.len() >= 12
            && word.iter().all(char::is_ascii_hexdigit)
            && word.iter().any(char::is_ascii_digit)
        {
            push(spans, at, end, "<id>");
        }
        at = end;
    }

    // Sound devices on USB carry the serial number in their node names:
    // alsa_output.usb-Maker_Product_SERIAL-00.analog-stereo.
    for prefix in ["alsa_card.usb-", "alsa_input.usb-", "alsa_output.usb-"] {
        for at in each(low, prefix) {
            let start = at + prefix.len();
            let word = until(line, start, char::is_whitespace);
            let end = (start..word)
                .rev()
                .find(|&i| {
                    line[i] == '-'
                        && line.get(i + 1).is_some_and(char::is_ascii_digit)
                        && line.get(i + 2).is_some_and(char::is_ascii_digit)
                })
                .unwrap_or(word);
            push(spans, start, end, "<usb-device>");
        }
    }
}

/// `line` with the spans replaced; those that overlap become one.
fn replace(line: &[char], mut spans: Vec<Span>) -> String {
    spans.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out = String::with_capacity(line.len());
    let mut done = 0;
    let mut spans = spans.into_iter().peekable();
    while let Some(mut span) = spans.next() {
        while let Some(next) = spans.peek().filter(|next| next.start < span.end) {
            span.end = span.end.max(next.end);
            spans.next();
        }
        out.extend(&line[done..span.start]);
        out.push_str(span.with);
        done = span.end.min(line.len());
    }
    out.extend(&line[done..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Redactor {
        let mut redactor = Redactor::default();
        redactor.user("allan");
        redactor.full_name("Allan Kimani Smith");
        redactor.computer("zenbook-of-allan");
        redactor.network("Smith Family 5G");
        redactor.network("Café Nyumbani");
        redactor.bluetooth("Allan's AirPods");
        redactor.bluetooth("MX Master 3");
        redactor.volume("HOLIDAY PICS");
        redactor
    }

    /// Nothing on this list may come through, whatever the line.
    fn gone(line: &str, secrets: &[&str]) -> String {
        let out = known().line(line);
        for secret in secrets {
            assert!(
                !out.to_lowercase().contains(&secret.to_lowercase()),
                "{secret:?} is still in {out:?}, from {line:?}"
            );
        }
        out
    }

    fn same(line: &str) {
        assert_eq!(known().line(line), line);
    }

    #[test]
    fn user_names_and_full_names() {
        assert_eq!(
            gone("Started session 2 of user allan.", &["allan"]),
            "Started session 2 of user <user>."
        );
        gone("ALLAN logged in; uid of Allan is 1000", &["allan"]);
        gone("gecos=Allan Kimani Smith", &["allan", "kimani", "smith"]);
        gone("owner Kimani", &["kimani"]);
        gone("run-user-allan.slice", &["allan"]);
        // Part of another word is not the name.
        let mut redactor = Redactor::default();
        redactor.user("ann");
        assert_eq!(redactor.line("scanning channel"), "scanning channel");
        assert_eq!(redactor.line("ann_x ann"), "<user>_x <user>");
    }

    #[test]
    fn the_accounts_every_system_has_are_left() {
        let mut redactor = Redactor::default();
        redactor.user("live");
        redactor.user("root");
        redactor.full_name("Sengi OS");
        redactor.computer("sengios");
        redactor.bluetooth("sengios");
        let line = "Sengi OS live system on sengios, run by root";
        assert_eq!(redactor.line(line), line);
    }

    #[test]
    fn home_paths() {
        assert_eq!(
            gone(
                "cannot open /home/allan/Documents/tax 2025.pdf: denied",
                &["allan", "tax", "2025.pdf"]
            ),
            "cannot open ~/<file>: denied"
        );
        assert_eq!(gone("HOME=/home/allan", &["allan"]), "HOME=~");
        assert_eq!(
            gone(
                "read /home/allan/.config/sengi-os/settings.toml",
                &["allan"]
            ),
            "read ~/.config/sengi-os/settings.toml"
        );
        // Somebody else's home too, and what is in the Trash.
        assert_eq!(
            gone(
                "'/home/wanjiru/.local/share/Trash/files/passport.jpg' gone",
                &["wanjiru", "passport"]
            ),
            "'~/<file>' gone"
        );
        gone("home-allan-Videos.mount: failed", &["allan"]);
        let mut redactor = Redactor::default();
        redactor.home("/srv/people/otieno");
        assert_eq!(redactor.line("in /srv/people/otieno today"), "in ~ today");
    }

    #[test]
    fn drive_mount_points() {
        assert_eq!(
            gone(
                "Mounted /dev/sdb1 at /media/allan/HOLIDAY PICS.",
                &["holiday", "allan"]
            ),
            "Mounted /dev/sdb1 at /media/<volume>"
        );
        gone("/run/media/njoroge/Salary: busy", &["njoroge", "salary"]);
        gone(
            r"media-njoroge-My\x20Stick.mount: Failed",
            &["njoroge", "stick"],
        );
        same("/usr/share/sounds/media/bell.oga");
    }

    #[test]
    fn host_names() {
        assert_eq!(
            gone("zenbook-of-allan systemd[1]: ready", &["zenbook", "allan"]),
            "<computer> systemd[1]: ready"
        );
        gone("announcing Zenbook-Of-Allan.local", &["zenbook", "allan"]);
        let mut redactor = Redactor::default();
        redactor.computer("wangari.example.org");
        assert_eq!(
            redactor.line("wangari asked wangari.example.org"),
            "<computer> asked <computer>"
        );
    }

    #[test]
    fn wifi_network_names() {
        gone("connected to Smith Family 5G", &["smith", "family"]);
        gone("joined Café Nyumbani.", &["nyumbani"]);
        gone(r"joined Caf\xc3\xa9 Nyumbani", &["nyumbani"]);
        // Names nobody told us of, said in a way that shows they are names.
        gone("wlan0: SSID='Neighbour Net' rejected", &["neighbour"]);
        gone("brcmf: ssid=OtherPlace", &["otherplace"]);
        assert_eq!(
            gone(
                "wlan0: Connected WiFi access point: Mama's (Corner) Wifi (aa:bb:cc:00:11:22)",
                &["mama", "corner", "aa:bb"]
            ),
            "wlan0: Connected WiFi access point: <network> (<mac>)"
        );
    }

    #[test]
    fn mac_addresses() {
        for line in [
            "wlan0: authenticate with 3c:58:c2:9a:01:fe",
            "address 3C-58-C2-9A-01-FE",
            "/org/bluez/hci0/dev_3C_58_C2_9A_01_FE",
            "bluez_output.3C_58_C2_9A_01_FE.1",
            "link/ether 3c:58:c2:9a:01:fe brd ff:ff:ff:ff:ff:ff",
            "renamed from eth0 to enx3c58c29a01fe",
            "wlx3C58C29A01FE: up",
            "hid-3c:58:c2:9a:01:fe-battery",
        ] {
            let out = gone(line, &["58:c2", "58-c2", "58_c2", "58c29a"]);
            assert!(out.contains("<mac>"), "{out}");
        }
    }

    #[test]
    fn ip_addresses() {
        for line in [
            "DHCPv4 address 192.168.1.23/24 via 192.168.1.1",
            "Using DNS server 8.8.8.8.",
            "peer [10.0.0.5]:443",
            "fe80::3e58:c2ff:fe9a:1fe%wlan0",
            "inet6:2001:db8:85a3::8a2e:370:7334",
            "address:fe80::1: unreachable",
            "[2c0f:fe38:2000:1234:5678:9abc:def0:1111]:53",
            "::ffff:192.168.1.23",
        ] {
            let out = gone(
                line,
                &["192.168", "8.8.8.8", "10.0.0.5", "fe80", "2001:db8", "2c0f"],
            );
            assert!(out.contains("<ip>"), "{out}");
        }
    }

    #[test]
    fn serial_numbers() {
        for line in [
            "usb 1-3: SerialNumber: 4C530001230101117392",
            "usb 1-3: SerialNumber: WD-WX12 AB34",
            "ata1.00: serial number: S21PNSAG517",
            "Serial Number S21PNSAG517",
            "ID_SERIAL_SHORT=S21PNSAG517",
            "disk serial=S21PNSAG517, size 256 GB",
            "S/N: S21PNSAG517",
            "iSerial 3 S21PNSAG517",
            "mmc0: card serial 0x5121f00d",
            "wwn-0x5001b448b0c3a2f1",
            "nvme0n1: eui.0025385b71b0e1a2",
            "/dev/disk/by-id/ata-Samsung_SSD_850_S21PNSAG517-part1",
            r"dev-disk-by\x2did-ata\x2dSamsung_S21PNSAG517.device: timed out",
            "/org/freedesktop/UDisks2/drives/Samsung_SSD_850_S21PNSAG517",
            "alsa_output.usb-Logitech_Headset_S21PNSAG517-00.analog-stereo",
        ] {
            gone(
                line,
                &[
                    "4C5300",
                    "WX12",
                    "AB34",
                    "S21PNSAG517",
                    "5121f00d",
                    "5001b448",
                    "0025385b",
                ],
            );
        }
    }

    #[test]
    fn unique_identifiers() {
        for line in [
            "BOOT_IMAGE=/vmlinuz root=UUID=0b6f3a1e-9d2c-4e7a-8f51-2c9a7e4d1b30 ro",
            "resume=PARTUUID=0b6f3a1e-02",
            "root=LABEL=KimaniData ro",
            "EXT4-fs (sda2): mounted filesystem 0b6f3a1e-9d2c-4e7a-8f51-2c9a7e4d1b30 r/w",
            r"systemd-fsck@dev-disk-by\x2duuid-0b6f3a1e\x2d9d2c\x2d4e7a\x2d8f51\x2d2c9a7e4d1b30.service",
            "/dev/disk/by-uuid/9A3C-11FE",
            "machine-id 0b6f3a1e9d2c4e7a8f512c9a7e4d1b30",
            "ntfs volume 01D8A4C2B7E19F30",
        ] {
            gone(
                line,
                &["0b6f3a1e", "2c9a7e4d", "KimaniData", "9A3C", "01D8A4C2"],
            );
        }
    }

    #[test]
    fn bluetooth_names() {
        gone("bluetoothd: Allan's AirPods disconnected", &["airpods"]);
        gone("  55. MX Master 3   [vol: 0.40]", &["mx master"]);
        assert_eq!(
            gone(
                "[   41.2] hid-generic 0005:046D:B023.0003: input,hidraw1: BLUETOOTH HID v0.11 Mouse [Njeri Mouse] on 3c:58:c2:9a:01:fe",
                &["njeri", "58:c2"]
            ),
            "[   41.2] hid-generic 0005:046D:B023.0003: input,hidraw1: BLUETOOTH HID v0.11 Mouse [<bluetooth-device>] on <mac>"
        );
        gone(
            "input: Baraka Keyboard as /devices/virtual/misc/uhid/0005:046D/input/input9",
            &["baraka"],
        );
    }

    #[test]
    fn two_things_at_once_hide_nothing() {
        // A network named after an address, and an address inside a name.
        let mut redactor = Redactor::default();
        redactor.network("cc");
        redactor.network("Printer 10.1.2.3 room");
        assert_eq!(redactor.line("aa:bb:cc:dd:ee:ff"), "<mac>");
        assert_eq!(
            redactor.line("on Printer 10.1.2.3 room now"),
            "on <network> now"
        );
        // Escape sequences and invisible characters cannot split a name.
        let out = gone("al\u{200b}lan and al\u{1b}[31mlan", &["allan", "lan"]);
        assert_eq!(out, "<user> and <user>");
    }

    #[test]
    fn what_helps_find_a_fault_stays() {
        for line in [
            "0000:00:02.0 8086:5916 (1043:1ee0) driver: i915",
            "[    2.514321] iwlwifi 0000:01:00.0: Direct firmware load for iwlwifi-8265-36.ucode failed with error -2",
            "iwlwifi 0000:01:00.0: loaded firmware version 36.ca7b901d.0 8265-36.ucode op_mode iwlmvm",
            "i915 0000:00:02.0: [drm] Finished loading DMC firmware i915/kbl_dmc_ver1_04.bin (v1.4)",
            "pci 0000:00:1f.2: reg 0x10: [mem 0xdf000000-0xdf003fff]",
            "usb 1-1: new high-speed USB device number 2 using xhci_hcd",
            "serial8250: ttyS0 at I/O 0x3f8 (irq = 4, base_baud = 115200) is a 16550A",
            "Linux version 6.12.48+deb13-amd64, rcS 12:34:56",
            "RTL8168 driver version 8.051.02",
            "firmware version: 1.2.3.4",
            "listening on 127.0.0.1 and ::1, mask 255.255.255.0",
            "alsa_output.pci-0000_00_1f.3.analog-stereo",
            "getty@tty1.service user@1000.service: failed",
            "std::sync::mpsc ACPI: \\_SB_.PCI0::GFX0",
            "ACPI Error: AE_NOT_FOUND (20240827/psargs-332)",
        ] {
            same(line);
        }
    }

    #[test]
    fn odd_input_does_no_harm() {
        let redactor = known();
        for line in [
            "",
            "/home/",
            "UUID=",
            "serial:",
            "SSID='",
            "access point: ",
            "[",
            "BLUETOOTH HID [",
            ":::::::",
            "aa:bb:",
            "enx",
            "\\x",
            "\\x2",
            "\u{1b}[",
            "/media/",
            "wwn-",
            "alsa_card.usb-",
        ] {
            redactor.line(line);
        }
        let long = "a:".repeat(4000);
        redactor.line(&long);
    }
}
