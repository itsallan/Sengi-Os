//! greetd's protocol, as greetd-ipc(7) has it: a 32-bit length in the
//! machine's byte order, then that many bytes of JSON, each way.
//!
//! Requests are written out by hand and replies are read by a parser that
//! knows no more JSON than a reply can hold. That keeps a JSON library
//! out of a program that handles a password, and it lets the one request
//! that carries the password be built in a buffer of a known size that is
//! overwritten once it has been sent.

use std::fmt;
use std::io::{self, Read, Write};

/// No reply is anywhere near this long. A length beyond it is not greetd.
const MAX_REPLY: usize = 64 * 1024;

/// One request, framed and ready to send. Overwritten when dropped,
/// because one kind of request has a password in it.
pub struct Request(Vec<u8>);

impl Request {
    /// Starts a login for `username`.
    pub fn create_session(username: &str) -> Request {
        let mut request = Request::begin("create_session", 16 + escaped_len(username));
        request.raw(b",\"username\":");
        request.string(username);
        request.end()
    }

    /// Answers a question. `None` for a message that was no question.
    pub fn post_auth_message_response(response: Option<&str>) -> Request {
        let room = response.map_or(0, |response| 16 + escaped_len(response));
        let mut request = Request::begin("post_auth_message_response", room);
        if let Some(response) = response {
            request.raw(b",\"response\":");
            request.string(response);
        }
        request.end()
    }

    /// Asks for the session to be started with `cmd` once this program
    /// has exited.
    pub fn start_session(cmd: &[String], env: &[String]) -> Request {
        let room: usize = cmd
            .iter()
            .chain(env)
            .map(|word| escaped_len(word) + 1)
            .sum();
        let mut request = Request::begin("start_session", 24 + room);
        request.raw(b",\"cmd\":");
        request.array(cmd);
        request.raw(b",\"env\":");
        request.array(env);
        request.end()
    }

    pub fn cancel_session() -> Request {
        Request::begin("cancel_session", 0).end()
    }

    /// All the room is set aside here, so that nothing written later
    /// moves the buffer and leaves a copy of it behind.
    fn begin(kind: &str, room: usize) -> Request {
        let mut bytes = Vec::with_capacity(4 + 16 + kind.len() + room);
        bytes.extend_from_slice(&[0; 4]);
        let mut request = Request(bytes);
        request.raw(b"{\"type\":");
        request.string(kind);
        request
    }

    fn end(mut self) -> Request {
        self.raw(b"}");
        let length = (self.0.len() - 4) as u32;
        self.0[..4].copy_from_slice(&length.to_ne_bytes());
        self
    }

    fn raw(&mut self, bytes: &[u8]) {
        assert!(
            self.0.len() + bytes.len() <= self.0.capacity(),
            "a request outgrew the room set aside for it"
        );
        self.0.extend_from_slice(bytes);
    }

    fn string(&mut self, text: &str) {
        self.raw(b"\"");
        for &byte in text.as_bytes() {
            match byte {
                b'"' => self.raw(b"\\\""),
                b'\\' => self.raw(b"\\\\"),
                0..=0x1f => {
                    const HEX: &[u8; 16] = b"0123456789abcdef";
                    let escape = [
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        HEX[usize::from(byte >> 4)],
                        HEX[usize::from(byte & 15)],
                    ];
                    self.raw(&escape);
                }
                _ => self.raw(&[byte]),
            }
        }
        self.raw(b"\"");
    }

    fn array(&mut self, words: &[String]) {
        self.raw(b"[");
        for (index, word) in words.iter().enumerate() {
            if index > 0 {
                self.raw(b",");
            }
            self.string(word);
        }
        self.raw(b"]");
    }

    /// The frame: the length and the JSON after it.
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn send(&self, to: &mut impl Write) -> io::Result<()> {
        to.write_all(&self.0)?;
        to.flush()
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        for byte in self.0.iter_mut() {
            // Volatile, so the wipe of a buffer about to be freed is kept.
            // SAFETY: a valid, exclusive reference to one byte.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Request(..)")
    }
}

/// How many bytes `text` takes inside a JSON string, quotes included.
fn escaped_len(text: &str) -> usize {
    2 + text
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' => 2,
            0..=0x1f => 6,
            _ => 1,
        })
        .sum::<usize>()
}

/// What an `auth_message` wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asks {
    /// An answer that may be shown while it is typed.
    Visible,
    /// An answer that may not.
    Secret,
    /// Nothing: it informs.
    Info,
    /// Nothing: it reports a problem.
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Success,
    Error {
        /// `auth_error`: most likely a wrong password. Otherwise something
        /// else went wrong.
        auth: bool,
        description: String,
    },
    AuthMessage {
        asks: Asks,
        message: String,
    },
}

#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    /// Not a reply greetd-ipc(7) describes.
    Protocol(&'static str),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Io(error) => write!(f, "{error}"),
            ReadError::Protocol(what) => write!(f, "not a greetd reply: {what}"),
        }
    }
}

impl From<io::Error> for ReadError {
    fn from(error: io::Error) -> ReadError {
        ReadError::Io(error)
    }
}

/// Reads one reply.
pub fn read_reply(from: &mut impl Read) -> Result<Reply, ReadError> {
    let mut length = [0u8; 4];
    from.read_exact(&mut length)?;
    let length = u32::from_ne_bytes(length) as usize;
    if length > MAX_REPLY {
        return Err(ReadError::Protocol("too long"));
    }
    let mut payload = vec![0u8; length];
    from.read_exact(&mut payload)?;
    let text = std::str::from_utf8(&payload).map_err(|_| ReadError::Protocol("not UTF-8"))?;
    parse_reply(text).map_err(ReadError::Protocol)
}

/// Reads the JSON of one reply.
pub fn parse_reply(json: &str) -> Result<Reply, &'static str> {
    let fields = Parser::new(json).object()?;
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    match field("type").ok_or("no type")? {
        "success" => Ok(Reply::Success),
        "error" => Ok(Reply::Error {
            auth: match field("error_type").ok_or("an error of no type")? {
                "auth_error" => true,
                "error" => false,
                _ => return Err("an unknown type of error"),
            },
            description: field("description").unwrap_or_default().to_string(),
        }),
        "auth_message" => Ok(Reply::AuthMessage {
            asks: match field("auth_message_type").ok_or("a message of no type")? {
                "visible" => Asks::Visible,
                "secret" => Asks::Secret,
                "info" => Asks::Info,
                "error" => Asks::Error,
                _ => return Err("an unknown type of message"),
            },
            message: field("auth_message").unwrap_or_default().to_string(),
        }),
        _ => Err("an unknown type of reply"),
    }
}

/// Enough of a JSON parser for one object whose members that matter are
/// strings. Members of other kinds are read past, so a newer greetd that
/// adds one does not break the login screen.
struct Parser<'a> {
    rest: &'a [u8],
    /// How deep in arrays and objects, so that nesting cannot run away.
    depth: u32,
}

impl<'a> Parser<'a> {
    fn new(json: &'a str) -> Parser<'a> {
        Parser {
            rest: json.as_bytes(),
            depth: 0,
        }
    }

    fn space(&mut self) {
        while let [b' ' | b'\t' | b'\n' | b'\r', rest @ ..] = self.rest {
            self.rest = rest;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.space();
        match self.rest {
            [first, rest @ ..] if *first == byte => {
                self.rest = rest;
                true
            }
            _ => false,
        }
    }

    /// The whole input as one object: its members that are strings.
    fn object(mut self) -> Result<Vec<(String, String)>, &'static str> {
        let mut fields = Vec::new();
        if !self.eat(b'{') {
            return Err("not an object");
        }
        if !self.eat(b'}') {
            loop {
                self.space();
                let key = self.string()?;
                if !self.eat(b':') {
                    return Err("a member with no value");
                }
                self.space();
                if self.rest.first() == Some(&b'"') {
                    fields.push((key, self.string()?));
                } else {
                    self.skip()?;
                }
                if self.eat(b'}') {
                    break;
                }
                if !self.eat(b',') {
                    return Err("an object that does not end");
                }
            }
        }
        self.space();
        if self.rest.is_empty() {
            Ok(fields)
        } else {
            Err("something after the object")
        }
    }

    fn string(&mut self) -> Result<String, &'static str> {
        let [b'"', rest @ ..] = self.rest else {
            return Err("not a string");
        };
        self.rest = rest;
        let mut bytes = Vec::new();
        loop {
            let [byte, rest @ ..] = self.rest else {
                return Err("a string that does not end");
            };
            self.rest = rest;
            match byte {
                b'"' => break,
                b'\\' => {
                    let [escape, rest @ ..] = self.rest else {
                        return Err("a string that does not end");
                    };
                    self.rest = rest;
                    let character = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.escaped()?,
                        _ => return Err("an unknown escape"),
                    };
                    bytes.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
                }
                0..=0x1f => return Err("a control character in a string"),
                _ => bytes.push(*byte),
            }
        }
        // The input was UTF-8 and was cut only at quotes and backslashes.
        String::from_utf8(bytes).map_err(|_| "not UTF-8")
    }

    fn hex4(&mut self) -> Result<u32, &'static str> {
        if self.rest.len() < 4 {
            return Err("a short escape");
        }
        let (digits, rest) = self.rest.split_at(4);
        self.rest = rest;
        digits.iter().try_fold(0u32, |value, digit| {
            (*digit as char)
                .to_digit(16)
                .map(|digit| value * 16 + digit)
                .ok_or("a bad escape")
        })
    }

    /// What follows `\u`: one character, from one escape or from the pair
    /// that a character outside the first plane is written as.
    fn escaped(&mut self) -> Result<char, &'static str> {
        let first = self.hex4()?;
        let code = if (0xd800..0xdc00).contains(&first) {
            let [b'\\', b'u', rest @ ..] = self.rest else {
                return Err("half a surrogate pair");
            };
            self.rest = rest;
            let second = self.hex4()?;
            if !(0xdc00..0xe000).contains(&second) {
                return Err("half a surrogate pair");
            }
            0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
        } else {
            first
        };
        char::from_u32(code).ok_or("half a surrogate pair")
    }

    /// Reads past one value of any kind.
    fn skip(&mut self) -> Result<(), &'static str> {
        self.space();
        match self.rest.first() {
            Some(b'"') => self.string().map(drop),
            Some(&open @ (b'{' | b'[')) => {
                let members = open == b'{';
                let close = if members { b'}' } else { b']' };
                self.rest = &self.rest[1..];
                self.depth += 1;
                if self.depth > 16 {
                    return Err("nested too deep");
                }
                if !self.eat(close) {
                    loop {
                        if members {
                            self.space();
                            self.string()?;
                            if !self.eat(b':') {
                                return Err("a member with no value");
                            }
                        }
                        self.skip()?;
                        if self.eat(close) {
                            break;
                        }
                        if !self.eat(b',') {
                            return Err("a list that does not end");
                        }
                    }
                }
                self.depth -= 1;
                Ok(())
            }
            Some(_) => {
                // A number, true, false or null: up to whatever ends it.
                let end = self
                    .rest
                    .iter()
                    .position(|byte| {
                        matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
                    })
                    .unwrap_or(self.rest.len());
                let word = &self.rest[..end];
                let plain = !word.is_empty()
                    && word
                        .iter()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(byte));
                if !plain {
                    return Err("not a value");
                }
                self.rest = &self.rest[end..];
                Ok(())
            }
            None => Err("a value that is missing"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(request: &Request) -> &str {
        let bytes = request.bytes();
        let length = u32::from_ne_bytes(bytes[..4].try_into().unwrap()) as usize;
        assert_eq!(length, bytes.len() - 4, "the length is the payload's");
        std::str::from_utf8(&bytes[4..]).unwrap()
    }

    #[test]
    fn requests_are_what_the_manual_says() {
        assert_eq!(
            json(&Request::create_session("me")),
            r#"{"type":"create_session","username":"me"}"#
        );
        assert_eq!(
            json(&Request::post_auth_message_response(Some("live"))),
            r#"{"type":"post_auth_message_response","response":"live"}"#
        );
        assert_eq!(
            json(&Request::post_auth_message_response(None)),
            r#"{"type":"post_auth_message_response"}"#
        );
        assert_eq!(
            json(&Request::start_session(
                &["/usr/bin/sengi-session".to_string()],
                &[]
            )),
            r#"{"type":"start_session","cmd":["/usr/bin/sengi-session"],"env":[]}"#
        );
        assert_eq!(
            json(&Request::start_session(
                &["a".to_string(), "b c".to_string()],
                &["X=1".to_string(), "Y=2".to_string()]
            )),
            r#"{"type":"start_session","cmd":["a","b c"],"env":["X=1","Y=2"]}"#
        );
        assert_eq!(
            json(&Request::cancel_session()),
            r#"{"type":"cancel_session"}"#
        );
    }

    #[test]
    fn a_password_of_any_characters_arrives_as_it_was_typed() {
        let password = "p\"a\\s s\u{e9}\u{20ac}\u{1f511}\t\u{1}/";
        let request = Request::post_auth_message_response(Some(password));
        assert_eq!(
            json(&request),
            "{\"type\":\"post_auth_message_response\",\"response\":\"p\\\"a\\\\s s\u{e9}\u{20ac}\u{1f511}\\u0009\\u0001/\"}"
        );
        // And our own reader, which reads what greetd's would, gets it back.
        let fields = Parser::new(json(&request)).object().unwrap();
        assert_eq!(fields[1], ("response".to_string(), password.to_string()));
    }

    #[test]
    fn the_buffer_with_the_password_in_it_is_never_grown() {
        // Writing past the room set aside panics, so getting through the
        // worst cases is the proof. Every byte here needs escaping.
        for password in ["", "x", &"\u{1}".repeat(511), &"\"".repeat(511)] {
            let request = Request::post_auth_message_response(Some(password));
            assert!(request.bytes().len() > password.len());
        }
        let words = vec!["\u{1}\"".repeat(40); 5];
        let _ = Request::start_session(&words, &words);
        let _ = Request::create_session(&"\\".repeat(300));
        let request = Request::post_auth_message_response(Some("secret"));
        assert_eq!(format!("{request:?}"), "Request(..)");
    }

    #[test]
    fn replies_are_read() {
        assert_eq!(parse_reply(r#"{"type":"success"}"#), Ok(Reply::Success));
        assert_eq!(
            parse_reply(
                r#"{"type":"error","error_type":"auth_error","description":"pam_authenticate: AUTH_ERR"}"#
            ),
            Ok(Reply::Error {
                auth: true,
                description: "pam_authenticate: AUTH_ERR".to_string()
            })
        );
        assert_eq!(
            parse_reply(
                r#" { "type" : "error" , "error_type" : "error", "description": "busy" } "#
            ),
            Ok(Reply::Error {
                auth: false,
                description: "busy".to_string()
            })
        );
        for (name, asks) in [
            ("visible", Asks::Visible),
            ("secret", Asks::Secret),
            ("info", Asks::Info),
            ("error", Asks::Error),
        ] {
            let json = format!(
                r#"{{"type":"auth_message","auth_message_type":"{name}","auth_message":"Password: "}}"#
            );
            assert_eq!(
                parse_reply(&json),
                Ok(Reply::AuthMessage {
                    asks,
                    message: "Password: ".to_string()
                })
            );
        }
    }

    #[test]
    fn escapes_in_a_reply_are_undone() {
        let reply = parse_reply(
            r#"{"type":"auth_message","auth_message_type":"info","auth_message":"a\"b\\c\/d\né🔑"}"#,
        );
        assert_eq!(
            reply,
            Ok(Reply::AuthMessage {
                asks: Asks::Info,
                message: "a\"b\\c/d\n\u{e9}\u{1f511}".to_string()
            })
        );
    }

    #[test]
    fn members_that_are_not_strings_are_read_past() {
        assert_eq!(
            parse_reply(
                r#"{"version":2,"extra":{"a":[1,2,{"b":null}],"c":"}"},"flag":true,"type":"success","n":-1.5e3}"#
            ),
            Ok(Reply::Success)
        );
    }

    #[test]
    fn what_is_not_a_reply_is_refused() {
        for bad in [
            "",
            "null",
            "[]",
            "{",
            r#"{"type":"success""#,
            r#"{"type":"success"} x"#,
            r#"{"type":"succes"}"#,
            r#"{"type":"error"}"#,
            r#"{"type":"error","error_type":"other"}"#,
            r#"{"type":"auth_message","auth_message":"x"}"#,
            r#"{"type":"auth_message","auth_message_type":"loud","auth_message":"x"}"#,
            r#"{"type":"success","x":"\ud83d"}"#,
            r#"{"type":"success","x":"\q"}"#,
            r#"{"type":"success","x":}"#,
            r#"{"type" "success"}"#,
            r#"{"description":"no type"}"#,
        ] {
            assert!(parse_reply(bad).is_err(), "{bad:?}");
        }
        let deep = format!(
            r#"{{"type":"success","x":{}1{}}}"#,
            "[".repeat(40),
            "]".repeat(40)
        );
        assert!(parse_reply(&deep).is_err());
    }

    #[test]
    fn a_reply_comes_off_the_wire_whole_or_not_at_all() {
        let payload = br#"{"type":"success"}"#;
        let mut wire = (payload.len() as u32).to_ne_bytes().to_vec();
        wire.extend_from_slice(payload);
        assert!(matches!(read_reply(&mut &wire[..]), Ok(Reply::Success)));
        // Cut short: greetd went away in the middle.
        assert!(matches!(
            read_reply(&mut &wire[..wire.len() - 1]),
            Err(ReadError::Io(_))
        ));
        assert!(matches!(read_reply(&mut &wire[..2]), Err(ReadError::Io(_))));
        // A length no reply has is not waited for.
        let huge = u32::MAX.to_ne_bytes();
        assert!(matches!(
            read_reply(&mut &huge[..]),
            Err(ReadError::Protocol(_))
        ));
    }
}
