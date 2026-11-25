//! The little JSON this program speaks: one flat object to a line, with
//! text, number, true, false and null values, and nothing nested. The
//! helper writes it and Settings reads it, so both ends are here and are
//! tested against each other. Anything that is not exactly that is
//! refused rather than guessed at.

use std::iter::Peekable;
use std::str::Chars;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

/// The longest line the reader looks at.
pub const MAX_LINE: usize = 64 * 1024;

/// `text` as a JSON string, quotes included.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A flat object being written.
pub struct Object {
    text: String,
}

impl Default for Object {
    fn default() -> Object {
        Object::new()
    }
}

impl Object {
    pub fn new() -> Object {
        Object {
            text: String::from("{"),
        }
    }

    fn key(&mut self, key: &str) {
        if self.text.len() > 1 {
            self.text.push(',');
        }
        self.text.push_str(&quote(key));
        self.text.push(':');
    }

    pub fn str(mut self, key: &str, value: &str) -> Object {
        self.key(key);
        self.text.push_str(&quote(value));
        self
    }

    pub fn num(mut self, key: &str, value: f64) -> Object {
        self.key(key);
        if value.is_finite() {
            self.text.push_str(&format!("{value}"));
        } else {
            self.text.push('0');
        }
        self
    }

    pub fn bool(mut self, key: &str, value: bool) -> Object {
        self.key(key);
        self.text.push_str(if value { "true" } else { "false" });
        self
    }

    pub fn finish(mut self) -> String {
        self.text.push('}');
        self.text
    }
}

/// The fields of one line, in order.
pub type Fields = Vec<(String, Value)>;

pub fn get_str<'a>(fields: &'a Fields, key: &str) -> Option<&'a str> {
    fields.iter().find_map(|(k, v)| match v {
        Value::Str(text) if k == key => Some(text.as_str()),
        _ => None,
    })
}

pub fn get_num(fields: &Fields, key: &str) -> Option<f64> {
    fields.iter().find_map(|(k, v)| match v {
        Value::Num(n) if k == key => Some(*n),
        _ => None,
    })
}

pub fn get_bool(fields: &Fields, key: &str) -> Option<bool> {
    fields.iter().find_map(|(k, v)| match v {
        Value::Bool(b) if k == key => Some(*b),
        _ => None,
    })
}

struct Reader<'a> {
    chars: Peekable<Chars<'a>>,
}

impl Reader<'_> {
    fn space(&mut self) {
        while self.chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.chars.next();
        }
    }

    fn expect(&mut self, want: char) -> Option<()> {
        (self.chars.next()? == want).then_some(())
    }

    fn word(&mut self, rest: &str) -> Option<()> {
        for want in rest.chars() {
            self.expect(want)?;
        }
        Some(())
    }

    fn hex4(&mut self) -> Option<u32> {
        let mut value = 0;
        for _ in 0..4 {
            value = value * 16 + self.chars.next()?.to_digit(16)?;
        }
        Some(value)
    }

    fn string(&mut self) -> Option<String> {
        self.expect('"')?;
        let mut out = String::new();
        loop {
            match self.chars.next()? {
                '"' => return Some(out),
                '\\' => match self.chars.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let first = self.hex4()?;
                        let code = if (0xd800..0xdc00).contains(&first) {
                            // A pair: the second half must follow.
                            self.expect('\\')?;
                            self.expect('u')?;
                            let second = self.hex4()?;
                            if !(0xdc00..0xe000).contains(&second) {
                                return None;
                            }
                            0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                        } else {
                            first
                        };
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    _ => return None,
                },
                c => out.push(c),
            }
        }
    }

    fn value(&mut self) -> Option<Value> {
        match *self.chars.peek()? {
            '"' => self.string().map(Value::Str),
            't' => self.word("true").map(|_| Value::Bool(true)),
            'f' => self.word("false").map(|_| Value::Bool(false)),
            'n' => self.word("null").map(|_| Value::Null),
            '-' | '0'..='9' => {
                let mut text = String::new();
                while let Some(c) = self.chars.peek() {
                    if matches!(c, '0'..='9' | '-' | '+' | '.' | 'e' | 'E') {
                        text.push(*c);
                        self.chars.next();
                    } else {
                        break;
                    }
                }
                let number: f64 = text.parse().ok()?;
                number.is_finite().then_some(Value::Num(number))
            }
            _ => None,
        }
    }
}

/// One line as its fields, or `None` if it is not a flat object.
pub fn parse(line: &str) -> Option<Fields> {
    if line.len() > MAX_LINE {
        return None;
    }
    let mut reader = Reader {
        chars: line.trim().chars().peekable(),
    };
    reader.expect('{')?;
    let mut fields = Vec::new();
    reader.space();
    if reader.chars.peek() == Some(&'}') {
        reader.chars.next();
    } else {
        loop {
            reader.space();
            let key = reader.string()?;
            reader.space();
            reader.expect(':')?;
            reader.space();
            let value = reader.value()?;
            fields.push((key, value));
            reader.space();
            match reader.chars.next()? {
                ',' => continue,
                '}' => break,
                _ => return None,
            }
        }
    }
    reader.space();
    reader.chars.peek().is_none().then_some(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_written_is_read_back() {
        let line = Object::new()
            .str("event", "progress")
            .num("percent", 45.5)
            .bool("ok", true)
            .str("detail", "a \"quoted\" name\twith\\slash\u{1}")
            .finish();
        let fields = parse(&line).expect("parses");
        assert_eq!(get_str(&fields, "event"), Some("progress"));
        assert_eq!(get_num(&fields, "percent"), Some(45.5));
        assert_eq!(get_bool(&fields, "ok"), Some(true));
        assert_eq!(
            get_str(&fields, "detail"),
            Some("a \"quoted\" name\twith\\slash\u{1}")
        );
        assert_eq!(get_str(&fields, "missing"), None);
        // Asking for the wrong type finds nothing.
        assert_eq!(get_num(&fields, "event"), None);
    }

    #[test]
    fn a_line_has_no_newline_in_it() {
        let line = Object::new().str("m", "one\ntwo\r\nthree").finish();
        assert!(!line.contains('\n') && !line.contains('\r'));
    }

    #[test]
    fn escapes_and_pairs() {
        let fields = parse(r#"{"a":"é😀\/"}"#).unwrap();
        assert_eq!(get_str(&fields, "a"), Some("\u{e9}\u{1f600}/"));
        // A lone half of a pair is refused.
        assert!(parse(r#"{"a":"\ud83d"}"#).is_none());
        assert!(parse(r#"{"a":"\q"}"#).is_none());
    }

    #[test]
    fn only_flat_objects_are_taken() {
        assert_eq!(parse("{}").unwrap().len(), 0);
        assert!(parse(r#"{"a":{"b":1}}"#).is_none());
        assert!(parse(r#"{"a":[1]}"#).is_none());
        assert!(parse(r#"{"a":1} x"#).is_none());
        assert!(parse(r#"{"a":1,}"#).is_none());
        assert!(parse(r#"{"a" 1}"#).is_none());
        assert!(parse("").is_none());
        assert!(parse("not json").is_none());
        assert!(parse(r#"{"a":1e999}"#).is_none());
        assert!(parse(&format!(r#"{{"a":"{}"}}"#, "x".repeat(MAX_LINE))).is_none());
    }

    #[test]
    fn numbers_and_literals() {
        let fields = parse(r#" { "n" : -3.5e1 , "t":true, "f":false, "z":null } "#).unwrap();
        assert_eq!(get_num(&fields, "n"), Some(-35.0));
        assert_eq!(get_bool(&fields, "t"), Some(true));
        assert_eq!(get_bool(&fields, "f"), Some(false));
        assert_eq!(fields[3].1, Value::Null);
    }

    #[test]
    fn a_number_that_is_not_one_is_not_written() {
        let line = Object::new().num("x", f64::NAN).finish();
        assert_eq!(line, r#"{"x":0}"#);
    }
}
