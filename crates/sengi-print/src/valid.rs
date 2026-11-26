//! What may be passed to the CUPS tools. Names, addresses and job numbers
//! come from outside: from the network, from a text field, from whatever
//! another program called its printer. The helper checks them again as
//! root, with these same functions, before any of them is an argument.
//!
//! The patterns are strict on purpose. A name that is refused here is
//! never worth the risk of being taken for an option, a path or a second
//! argument.

/// The longest name CUPS accepts.
pub const MAX_NAME: usize = 127;
/// The longest device address the helper takes.
pub const MAX_URI: usize = 512;

/// The schemes a printer may be added with. `dnssd` is what discovery
/// finds, `ipp` and `ipps` are typed in, and `socket` and `lpd` are for
/// the older printers that only take a raw port.
const SCHEMES: [&str; 5] = ["ipp", "ipps", "dnssd", "socket", "lpd"];
/// The schemes a person may type an address with.
const TYPED_SCHEMES: [&str; 4] = ["ipp", "ipps", "socket", "lpd"];

/// A printer or class name: letters, digits, dot, underscore, plus and
/// hyphen, starting with a letter or digit, so that it can never be read
/// as an option. CUPS itself refuses spaces, slashes and `#`.
pub fn printer_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= MAX_NAME
        && first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// A job as `cancel` names it: the printer, a hyphen and the number.
pub fn job_id(id: &str) -> bool {
    let Some((printer, number)) = id.rsplit_once('-') else {
        return false;
    };
    printer_name(printer)
        && !number.is_empty()
        && number.len() <= 9
        && number.chars().all(|c| c.is_ascii_digit())
}

fn uri_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '.' | '_' | '~' | '%' | ':' | '/' | '?' | '=' | '&' | '-')
}

fn split_scheme(uri: &str) -> Option<(String, &str)> {
    let (scheme, rest) = uri.split_once("://")?;
    Some((scheme.to_ascii_lowercase(), rest))
}

/// A device address a printer may be added with: a scheme from the
/// short list, then a host or a name, in plain characters. No user name,
/// no password, no quote, no space.
pub fn device_uri(uri: &str) -> bool {
    if uri.is_empty() || uri.len() > MAX_URI || !uri.chars().all(uri_char) {
        return false;
    }
    let Some((scheme, rest)) = split_scheme(uri) else {
        return false;
    };
    if !SCHEMES.contains(&scheme.as_str()) {
        return false;
    }
    // Something must follow, and it must be a host or an encoded name,
    // not a path or a query on its own.
    rest.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '%')
}

/// Why an address that was typed was not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddressError {
    Empty,
    /// The scheme is not one of ours.
    Scheme,
    /// Not a host name or an IPv4 address, or a port out of range.
    Host,
}

impl AddressError {
    pub fn message(&self) -> &'static str {
        match self {
            AddressError::Empty => "Type the address of the printer.",
            AddressError::Scheme => {
                "Only addresses that start with ipp://, ipps://, socket:// or lpd:// can be used."
            }
            AddressError::Host => {
                "That is not a printer address. Use a name or a number such as 192.168.1.50."
            }
        }
    }
}

fn host_ok(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = host.split('.').collect();
    // Four numbers are an address; any label with a letter makes it a name.
    if labels.len() == 4
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()))
    {
        return labels
            .iter()
            .all(|l| l.len() <= 3 && l.parse::<u16>().is_ok_and(|n| n <= 255));
    }
    // Numbers alone, but not four of them, are a mistyped address.
    if labels.iter().all(|l| l.chars().all(|c| c.is_ascii_digit())) {
        return false;
    }
    labels.iter().all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// The device address for what a person typed: a name or an IPv4
/// address, with or without a port, becomes `ipp://HOST/ipp/print`, the
/// path every driverless printer answers on; a whole address is kept if
/// its scheme is one a person may type.
pub fn address_to_uri(input: &str) -> Result<String, AddressError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(AddressError::Empty);
    }
    if input.contains("://") {
        let Some((scheme, _)) = split_scheme(input) else {
            return Err(AddressError::Host);
        };
        if !TYPED_SCHEMES.contains(&scheme.as_str()) {
            return Err(AddressError::Scheme);
        }
        return if device_uri(input) {
            Ok(input.to_string())
        } else {
            Err(AddressError::Host)
        };
    }
    let (host, port) = match input.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (input, None),
    };
    if !host_ok(host) {
        return Err(AddressError::Host);
    }
    let port = match port {
        None => None,
        Some(port) => {
            let valid = !port.is_empty()
                && port.len() <= 5
                && port.chars().all(|c| c.is_ascii_digit())
                && port.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n));
            if !valid {
                return Err(AddressError::Host);
            }
            Some(port)
        }
    };
    Ok(match port {
        Some(port) => format!("ipp://{host}:{port}/ipp/print"),
        None => format!("ipp://{host}/ipp/print"),
    })
}

/// A queue name for a printer called `display`: its words joined with
/// underscores, nothing else but letters, digits, dots and hyphens.
pub fn name_from_display(display: &str) -> String {
    let mut out = String::new();
    let mut pending = false;
    for c in display.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
            if pending && !out.is_empty() {
                out.push('_');
            }
            pending = false;
            out.push(c);
        } else if c.is_whitespace() || c == '_' {
            pending = true;
        }
    }
    let trimmed = out.trim_matches(|c| matches!(c, '.' | '-' | '_'));
    let mut name: String = trimmed.chars().take(40).collect();
    while name.ends_with(['.', '-', '_']) {
        name.pop();
    }
    if name.is_empty() {
        "Printer".to_string()
    } else {
        name
    }
}

/// `base`, or `base_2`, `base_3` and so on, so that it is not one of
/// `taken`. CUPS does not tell names apart by case, and nor does this.
pub fn unique_name(base: &str, taken: &[String]) -> String {
    let used = |name: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(name));
    if !used(base) {
        return base.to_string();
    }
    (2..1000)
        .map(|n| format!("{base}_{n}"))
        .find(|name| !used(name))
        .unwrap_or_else(|| base.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_printer_names_are_taken() {
        for name in ["Brother_HL-L2350DW", "HP.LaserJet", "a", "Office2", "x+y"] {
            assert!(printer_name(name), "{name}");
        }
    }

    #[test]
    fn unsafe_printer_names_are_refused() {
        for name in [
            "", "-x", "-E", "--help", ".hidden", "_x", "a b", "a/b", "a#b", "a;b", "a$(b)", "a`b`",
            "a\nb", "a\tb", "a\"b", "a'b", "a\\b", "a=b", "ä", "../etc",
        ] {
            assert!(!printer_name(name), "{name:?}");
        }
        assert!(printer_name(&"a".repeat(127)));
        assert!(!printer_name(&"a".repeat(128)));
    }

    #[test]
    fn job_numbers_are_a_name_a_hyphen_and_digits() {
        assert!(job_id("Office-12"));
        assert!(job_id("Brother_HL-L2350DW-104"));
        for bad in [
            "",
            "12",
            "-12",
            "Office",
            "Office-",
            "Office-1a",
            "Office-1234567890",
            "Off ice-1",
            "Office-1;ls",
            "-a-1",
            "Office-+1",
        ] {
            assert!(!job_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn device_addresses_need_a_known_scheme_and_plain_characters() {
        for good in [
            "ipp://192.168.1.50/ipp/print",
            "ipps://printer.local:631/ipp/print",
            "dnssd://Brother%20HL-L2350DW%20series._ipp._tcp.local/?uuid=e3248000-80ce-11db-8000-3c2af4a0b8b5",
            "socket://10.0.0.7:9100",
            "lpd://10.0.0.7/queue",
        ] {
            assert!(device_uri(good), "{good}");
        }
        for bad in [
            "",
            "file:///etc/passwd",
            "file:///dev/null",
            "http://example.com/",
            "usb://HP/LaserJet",
            "ipp:/missing",
            "ipp://",
            "ipp:///path",
            "ipp://host/a b",
            "ipp://user:pw@host/",
            "ipp://host/$(id)",
            "ipp://host/`id`",
            "ipp://host/;ls",
            "ipp://host/\"x",
            "-v ipp://host",
            "ipp://host/\n",
            "ipp://[::1]/",
            "IPP",
        ] {
            assert!(!device_uri(bad), "{bad:?}");
        }
        assert!(!device_uri(&format!("ipp://{}", "a".repeat(600))));
    }

    #[test]
    fn a_typed_name_or_number_becomes_an_ipp_address() {
        assert_eq!(
            address_to_uri(" 192.168.1.50 ").as_deref(),
            Ok("ipp://192.168.1.50/ipp/print")
        );
        assert_eq!(
            address_to_uri("printer.local").as_deref(),
            Ok("ipp://printer.local/ipp/print")
        );
        assert_eq!(
            address_to_uri("brw3c2af4:631").as_deref(),
            Ok("ipp://brw3c2af4:631/ipp/print")
        );
        assert_eq!(
            address_to_uri("ipps://office.example.com/ipp/print").as_deref(),
            Ok("ipps://office.example.com/ipp/print")
        );
        assert_eq!(
            address_to_uri("socket://10.0.0.7:9100").as_deref(),
            Ok("socket://10.0.0.7:9100")
        );
    }

    #[test]
    fn a_typed_address_that_is_not_one_is_refused_in_words() {
        assert_eq!(address_to_uri("  "), Err(AddressError::Empty));
        assert_eq!(
            address_to_uri("file:///etc/passwd"),
            Err(AddressError::Scheme)
        );
        assert_eq!(
            address_to_uri("dnssd://x._ipp._tcp.local/"),
            Err(AddressError::Scheme)
        );
        for bad in [
            "999.1.1.1",
            "1.2.3",
            "-bad.local",
            "bad-.local",
            "a b",
            "host:0",
            "host:70000",
            "host:",
            "host:80a",
            "ho$t",
            "host/path",
            "::1",
            "[::1]",
            "host;rm",
            "ipp://host/a b",
        ] {
            assert!(address_to_uri(bad).is_err(), "{bad:?}");
        }
        assert!(!AddressError::Host.message().is_empty());
    }

    #[test]
    fn queue_names_are_made_from_what_a_printer_calls_itself() {
        assert_eq!(
            name_from_display("Brother HL-L2350DW series"),
            "Brother_HL-L2350DW_series"
        );
        assert_eq!(
            name_from_display("  --Office  printer!!  "),
            "Office_printer"
        );
        assert_eq!(name_from_display("Küche"), "Kche");
        assert_eq!(name_from_display("???"), "Printer");
        assert_eq!(name_from_display(""), "Printer");
        assert!(name_from_display(&"x".repeat(100)).len() <= 40);
        for display in ["a/b", "$(id)", "-rf", "x y", "\u{202e}evil"] {
            assert!(printer_name(&name_from_display(display)), "{display}");
        }
    }

    #[test]
    fn names_that_are_taken_get_a_number_whatever_their_case() {
        let taken = vec!["Office".to_string(), "office_2".to_string()];
        assert_eq!(unique_name("Home", &taken), "Home");
        assert_eq!(unique_name("office", &taken), "office_3");
        assert_eq!(unique_name("OFFICE", &[]), "OFFICE");
    }
}
