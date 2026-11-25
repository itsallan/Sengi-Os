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

    /// The tokens with one of the two palettes in place of `color.*`: the
    /// light one, from `color-light.*`, or the dark one as it is.
    pub fn themed(light: bool) -> Self {
        let mut tokens = Self::builtin();
        if light {
            tokens.use_light();
        }
        tokens
    }

    /// Replaces each `color.X` with `color-light.X`, so everything that
    /// reads `color.*` shows the light theme.
    pub fn use_light(&mut self) {
        let light: Vec<(String, String)> = self
            .values
            .iter()
            .filter_map(|(name, value)| {
                let rest = name.strip_prefix("color-light.")?;
                Some((format!("color.{rest}"), value.clone()))
            })
            .collect();
        self.values.extend(light);
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
    fn the_dock_is_spaced_as_designed() {
        let tokens = Tokens::builtin();
        let px = |name: &str| tokens.px(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(px("dock.icon"), 48);
        assert_eq!(px("dock.gap"), 14);
        assert_eq!(px("dock.end-padding"), 10);
        assert_eq!(px("size.dock-padding"), 6);
        assert_eq!(px("dock.margin"), 6);
        assert_eq!(px("dock.dot"), 4);
        assert_eq!(px("dock.dot-gap"), 3);
        assert_eq!(px("dock.bounce"), 8);
        assert_eq!(px("motion.bounce-duration"), 300);
        assert_eq!(tokens.get("motion.bounce-easing"), Some("ease-out"));
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

#[cfg(test)]
mod themes {
    use super::*;

    /// Relative luminance as WCAG 2.x defines it.
    fn luminance(c: Rgba) -> f64 {
        let lin = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    fn ratio(a: Rgba, b: Rgba) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// `top` laid over an opaque `under`.
    fn over(top: Rgba, under: Rgba) -> Rgba {
        let a = f64::from(top.a) / 255.0;
        let mix = |t: u8, u: u8| (f64::from(t) * a + f64::from(u) * (1.0 - a)).round() as u8;
        Rgba {
            r: mix(top.r, under.r),
            g: mix(top.g, under.g),
            b: mix(top.b, under.b),
            a: 255,
        }
    }

    fn both() -> [(&'static str, Tokens); 2] {
        [
            ("dark", Tokens::themed(false)),
            ("light", Tokens::themed(true)),
        ]
    }

    fn colour(tokens: &Tokens, name: &str) -> Rgba {
        tokens
            .color(&format!("color.{name}"))
            .unwrap_or_else(|| panic!("color.{name}"))
    }

    #[test]
    fn both_palettes_have_the_same_names_and_are_colours() {
        let tokens = Tokens::builtin();
        let names = |prefix: &str| -> Vec<String> {
            tokens
                .names()
                .filter_map(|n| n.strip_prefix(prefix).map(str::to_string))
                .collect()
        };
        assert_eq!(names("color."), names("color-light."));
        for name in tokens.names().filter(|n| n.starts_with("color-light.")) {
            assert!(tokens.color(name).is_some(), "{name} is not a colour");
        }
    }

    #[test]
    fn the_light_palette_replaces_the_dark_one_and_nothing_else() {
        let (dark, light) = (Tokens::themed(false), Tokens::themed(true));
        assert_ne!(colour(&dark, "bg"), colour(&light, "bg"));
        assert_eq!(
            colour(&light, "surface"),
            Tokens::builtin().color("color-light.surface").unwrap()
        );
        assert_eq!(dark.px("radius.base"), light.px("radius.base"));
        assert_eq!(light.get("type.family"), Some("Inter"));
        // The accent is the same green in both.
        assert_eq!(colour(&dark, "accent"), colour(&light, "accent"));
    }

    #[test]
    fn text_has_enough_contrast_on_every_surface_it_sits_on() {
        for (theme, tokens) in both() {
            let surfaces = [
                "desktop",
                "bg",
                "surface",
                "surface-raised",
                "surface-hover",
            ];
            for text in ["text", "text-muted"] {
                for surface in surfaces {
                    let r = ratio(colour(&tokens, text), colour(&tokens, surface));
                    assert!(r >= 4.5, "{theme}: {text} on {surface} is {r:.2}");
                }
            }
            // The words on the bar and the dock, over what shows through.
            for panel in ["bar", "dock"] {
                let under = over(colour(&tokens, panel), colour(&tokens, "desktop"));
                for text in ["text", "text-muted"] {
                    let r = ratio(colour(&tokens, text), under);
                    assert!(r >= 4.5, "{theme}: {text} on the {panel} is {r:.2}");
                }
            }
            // Coloured words: the accent as text, and the status colours.
            for text in ["accent-strong", "danger", "warning", "success"] {
                for surface in ["bg", "surface"] {
                    let r = ratio(colour(&tokens, text), colour(&tokens, surface));
                    assert!(r >= 4.5, "{theme}: {text} on {surface} is {r:.2}");
                }
            }
            // The label on a filled accent button, at rest and hovered.
            for fill in ["accent", "accent-hover"] {
                let r = ratio(colour(&tokens, "accent-text"), colour(&tokens, fill));
                assert!(r >= 4.5, "{theme}: accent-text on {fill} is {r:.2}");
            }
        }
    }

    #[test]
    fn parts_of_controls_have_enough_contrast() {
        for (theme, tokens) in both() {
            for part in ["accent", "focus-ring", "text-muted"] {
                for surface in ["bg", "surface"] {
                    let r = ratio(colour(&tokens, part), colour(&tokens, surface));
                    assert!(r >= 3.0, "{theme}: {part} on {surface} is {r:.2}");
                }
            }
        }
    }

    #[test]
    fn disabled_things_stay_apart_from_their_surface() {
        // Disabled text is exempt from the 4.5 rule but must not vanish.
        for (theme, tokens) in both() {
            for surface in ["bg", "surface"] {
                let r = ratio(colour(&tokens, "text-disabled"), colour(&tokens, surface));
                assert!(r >= 2.0, "{theme}: text-disabled on {surface} is {r:.2}");
                let r = ratio(
                    colour(&tokens, "outline-disabled"),
                    colour(&tokens, surface),
                );
                assert!(r >= 1.3, "{theme}: outline-disabled on {surface} is {r:.2}");
            }
        }
    }

    /// Prints the numbers quoted in tokens/sengi.toml:
    /// `cargo test -p sengi-tokens contrast_table -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn contrast_table() {
        for (theme, tokens) in both() {
            for (text, surface) in [
                ("text", "surface"),
                ("text-muted", "surface"),
                ("text-muted", "surface-hover"),
                ("accent-strong", "surface"),
                ("accent", "surface"),
                ("accent-text", "accent"),
                ("danger", "surface"),
                ("warning", "surface"),
                ("success", "surface"),
            ] {
                let r = ratio(colour(&tokens, text), colour(&tokens, surface));
                println!("{theme}: {text} on {surface} {r:.2}");
            }
        }
    }
}
