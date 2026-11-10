//! Reader for `tokens/sengi.toml`.
//!
//! The tokens file only uses flat `[section]` tables with string and integer
//! values, so this parses that subset and nothing more. Keys are exposed as
//! `section.key`, the same names the config templates use.

use std::collections::BTreeMap;
use std::fmt;

/// The tokens shipped with this build.
pub const SOURCE: &str = include_str!("../../../tokens/sengi.toml");

#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tokens line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Debug, Default)]
pub struct Tokens {
    values: BTreeMap<String, String>,
}

impl Tokens {
    pub fn parse(source: &str) -> Result<Self, ParseError> {
        let mut values = BTreeMap::new();
        let mut section = String::new();

        for (index, raw) in source.lines().enumerate() {
            let line = raw.trim();
            let err = |message| ParseError {
                line: index + 1,
                message,
            };

            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                section = rest
                    .strip_suffix(']')
                    .ok_or_else(|| err("unterminated section header"))?
                    .to_string();
                continue;
            }

            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| err("expected key = value"))?;
            let value = value.trim();
            let value = match value.strip_prefix('"') {
                Some(rest) => {
                    rest.split_once('"')
                        .ok_or_else(|| err("unterminated string"))?
                        .0
                }
                None => value.split('#').next().unwrap_or("").trim(),
            };
            if section.is_empty() {
                return Err(err("value outside of a section"));
            }
            values.insert(format!("{section}.{}", key.trim()), value.to_string());
        }

        Ok(Self { values })
    }

    /// The tokens compiled into this crate.
    pub fn builtin() -> Self {
        Self::parse(SOURCE).expect("the shipped tokens file is valid")
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn px(&self, name: &str) -> Option<u32> {
        self.get(name)?.parse().ok()
    }

    pub fn color(&self, name: &str) -> Option<Rgba> {
        let hex = self.get(name)?.strip_prefix('#')?;
        if !hex.is_ascii() || (hex.len() != 6 && hex.len() != 8) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Rgba {
            r: byte(0)?,
            g: byte(2)?,
            b: byte(4)?,
            a: if hex.len() == 8 { byte(6)? } else { 0xff },
        })
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_tokens_parse() {
        let tokens = Tokens::builtin();
        assert!(tokens.names().count() > 40);
    }

    // These two are fixed project decisions, not free design choices.
    #[test]
    fn radius_is_six_everywhere() {
        assert_eq!(Tokens::builtin().px("radius.base"), Some(6));
    }

    #[test]
    fn typeface_is_inter() {
        assert_eq!(Tokens::builtin().get("type.family"), Some("Inter"));
    }

    #[test]
    fn every_colour_is_valid() {
        let tokens = Tokens::builtin();
        for name in tokens.names().filter(|n| n.starts_with("color.")) {
            assert!(tokens.color(name).is_some(), "{name} is not a colour");
        }
    }

    #[test]
    fn colours_carry_alpha() {
        let tokens = Tokens::parse("[color]\na = \"#10203040\"\nb = \"#102030\"\n").unwrap();
        assert_eq!(
            tokens.color("color.a"),
            Some(Rgba {
                r: 0x10,
                g: 0x20,
                b: 0x30,
                a: 0x40
            })
        );
        assert_eq!(tokens.color("color.b").unwrap().a, 0xff);
    }

    #[test]
    fn comments_and_bare_values() {
        let tokens = Tokens::parse("# top\n[space]\nsm = 8 # px\n").unwrap();
        assert_eq!(tokens.px("space.sm"), Some(8));
    }

    #[test]
    fn errors_name_the_line() {
        let error = Tokens::parse("[color]\nnope\n").unwrap_err();
        assert_eq!(error.line, 2);
    }
}
