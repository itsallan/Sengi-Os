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

/// Which set of colours is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    Dark,
    Light,
    HighContrast,
}

/// The sizes that follow the text size setting: the type, and the heights
/// of what holds a line of it. The Slint `Tokens` does the same sums for
/// the same names, see ui/tokens.slint.in.
pub const SCALED: [&str; 13] = [
    "type.size-sm",
    "type.size-md",
    "type.size-lg",
    "type.size-xl",
    "type.size-display",
    "size.titlebar-height",
    "size.list-row-height",
    "size.bar-height",
    "size.button",
    "menu.row",
    "size.menu-width",
    "size.snip-countdown-width",
    "size.snip-countdown-height",
];

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
        Self::palette(if light { Palette::Light } else { Palette::Dark })
    }

    /// The tokens with `palette` in place of `color.*`. The high contrast
    /// palette also brings its thicker lines.
    pub fn palette(palette: Palette) -> Self {
        let mut tokens = Self::builtin();
        match palette {
            Palette::Dark => {}
            Palette::Light => tokens.use_light(),
            Palette::HighContrast => {
                tokens.use_prefix("color-hc.", "color.");
                tokens.use_prefix("border.width-hc", "border.width");
                tokens.use_prefix("border.focus-ring-hc", "border.focus-ring");
            }
        }
        tokens
    }

    /// The tokens as a program with these settings shows them: a palette
    /// and the text scale in percent, see `scaled`.
    pub fn for_appearance(palette: Palette, text_percent: u32) -> Self {
        let mut tokens = Self::palette(palette);
        tokens.scaled(text_percent);
        tokens
    }

    /// Replaces each `color.X` with `color-light.X`, so everything that
    /// reads `color.*` shows the light theme.
    pub fn use_light(&mut self) {
        self.use_prefix("color-light.", "color.");
    }

    /// Every token named `from...` also under `to...`, replacing what is
    /// there. A `from` that is a whole name moves just that token.
    fn use_prefix(&mut self, from: &str, to: &str) {
        let moved: Vec<(String, String)> = self
            .values
            .iter()
            .filter_map(|(name, value)| {
                let rest = name.strip_prefix(from)?;
                Some((format!("{to}{rest}"), value.clone()))
            })
            .collect();
        self.values.extend(moved);
    }

    /// Multiplies the type sizes and the heights that hold a line of text
    /// by `percent` / 100, rounded to whole pixels. See `SCALED`.
    pub fn scaled(&mut self, percent: u32) {
        for name in SCALED {
            if let Some(px) = self.px(name) {
                let value = (px * percent + 50) / 100;
                self.values.insert((*name).to_string(), value.to_string());
            }
        }
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

    fn both() -> [(&'static str, Tokens); 3] {
        [
            ("dark", Tokens::themed(false)),
            ("light", Tokens::themed(true)),
            ("high-contrast", Tokens::palette(Palette::HighContrast)),
        ]
    }

    /// The lowest contrast of text on its surface: high contrast asks for 7.
    fn text_min(theme: &str) -> f64 {
        if theme == "high-contrast" {
            7.0
        } else {
            4.5
        }
    }

    /// The lowest contrast of the parts of a control.
    fn part_min(theme: &str) -> f64 {
        if theme == "high-contrast" {
            4.5
        } else {
            3.0
        }
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
        assert_eq!(names("color."), names("color-hc."));
        for name in tokens.names().filter(|n| n.starts_with("color-hc.")) {
            assert!(tokens.color(name).is_some(), "{name} is not a colour");
        }
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
                    assert!(
                        r >= text_min(theme),
                        "{theme}: {text} on {surface} is {r:.2}"
                    );
                }
            }
            // The words on the bar and the dock, over what shows through.
            for panel in ["bar", "dock"] {
                let under = over(colour(&tokens, panel), colour(&tokens, "desktop"));
                for text in ["text", "text-muted"] {
                    let r = ratio(colour(&tokens, text), under);
                    assert!(
                        r >= text_min(theme),
                        "{theme}: {text} on the {panel} is {r:.2}"
                    );
                }
            }
            // Coloured words: the accent as text, and the status colours.
            for text in ["accent-strong", "danger", "warning", "success"] {
                for surface in ["bg", "surface"] {
                    let r = ratio(colour(&tokens, text), colour(&tokens, surface));
                    assert!(
                        r >= text_min(theme),
                        "{theme}: {text} on {surface} is {r:.2}"
                    );
                }
            }
            // The label on a filled accent button, at rest and hovered.
            for fill in ["accent", "accent-hover"] {
                let r = ratio(colour(&tokens, "accent-text"), colour(&tokens, fill));
                assert!(
                    r >= text_min(theme),
                    "{theme}: accent-text on {fill} is {r:.2}"
                );
            }
        }
    }

    #[test]
    fn parts_of_controls_have_enough_contrast() {
        for (theme, tokens) in both() {
            for part in ["accent", "focus-ring", "text-muted"] {
                for surface in ["bg", "surface"] {
                    let r = ratio(colour(&tokens, part), colour(&tokens, surface));
                    assert!(
                        r >= part_min(theme),
                        "{theme}: {part} on {surface} is {r:.2}"
                    );
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

    #[test]
    fn high_contrast_is_white_on_black_with_thick_lines() {
        let hc = Tokens::palette(Palette::HighContrast);
        let white = Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let black = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        assert_eq!(colour(&hc, "text"), white);
        assert_eq!(colour(&hc, "bg"), black);
        assert_eq!(colour(&hc, "desktop"), black);
        assert_eq!(hc.px("border.width"), Some(2));
        assert_eq!(hc.px("border.focus-ring"), Some(3));
        // The other two keep their one pixel lines.
        assert_eq!(Tokens::themed(false).px("border.width"), Some(1));
        assert_eq!(Tokens::themed(true).px("border.focus-ring"), Some(2));
        // Everything but colours and lines is the same.
        assert_eq!(hc.px("radius.base"), Some(6));
        assert_eq!(hc.get("type.family"), Some("Inter"));
    }

    #[test]
    fn high_contrast_lines_and_state_colours_show_on_every_surface() {
        let hc = Tokens::palette(Palette::HighContrast);
        for surface in ["bg", "surface", "surface-raised", "surface-hover"] {
            for part in ["border", "focus-ring", "accent"] {
                let r = ratio(colour(&hc, part), colour(&hc, surface));
                assert!(r >= 4.5, "{part} on {surface} is {r:.2}");
            }
            // Disabled things are dimmer but not gone.
            let r = ratio(colour(&hc, "outline-disabled"), colour(&hc, surface));
            assert!(r >= 3.0, "outline-disabled on {surface} is {r:.2}");
            let r = ratio(colour(&hc, "text-disabled"), colour(&hc, surface));
            assert!(r >= 3.0, "text-disabled on {surface} is {r:.2}");
        }
        // The bar and the dock over the desktop.
        for panel in ["bar", "dock"] {
            let under = over(colour(&hc, panel), colour(&hc, "desktop"));
            let r = ratio(colour(&hc, "text"), under);
            assert!(r >= 7.0, "text on the {panel} is {r:.2}");
        }
        for text in ["accent-strong", "danger", "warning", "success"] {
            let r = ratio(colour(&hc, text), colour(&hc, "surface-hover"));
            assert!(r >= 4.5, "{text} on surface-hover is {r:.2}");
        }
    }

    #[test]
    fn the_text_scale_steps_are_the_ones_in_the_tokens() {
        let tokens = Tokens::builtin();
        let steps: Vec<u32> = ["small", "default", "large", "extra-large"]
            .iter()
            .map(|name| tokens.px(&format!("text-scale.{name}")).unwrap())
            .collect();
        assert_eq!(steps, [90, 100, 120, 140]);
    }

    #[test]
    fn scaling_changes_the_type_and_the_text_heights_only() {
        let plain = Tokens::for_appearance(Palette::Dark, 100);
        assert_eq!(plain.px("type.size-md"), Some(13));
        let big = Tokens::for_appearance(Palette::Dark, 140);
        assert_eq!(big.px("type.size-md"), Some(18));
        assert_eq!(big.px("type.size-sm"), Some(15));
        assert_eq!(big.px("type.size-lg"), Some(22));
        assert_eq!(big.px("type.size-xl"), Some(31));
        assert_eq!(big.px("size.titlebar-height"), Some(42));
        assert_eq!(big.px("size.list-row-height"), Some(59));
        let small = Tokens::for_appearance(Palette::Dark, 90);
        assert_eq!(small.px("type.size-md"), Some(12));
        assert_eq!(small.px("type.size-sm"), Some(10));
        // Not text: spacing, radius, icons stay.
        for name in ["radius.base", "space.md", "size.bar-icon", "dock.icon"] {
            assert_eq!(big.px(name), plain.px(name), "{name}");
        }
        // The pt sizes of programs that do not take px are not scaled.
        assert_eq!(big.px("type.pt-md"), plain.px("type.pt-md"));
    }

    #[test]
    fn slint_scales_the_same_tokens_as_this_crate() {
        let template = include_str!("../../../ui/tokens.slint.in");
        let mut in_slint: Vec<&str> = template
            .lines()
            .filter(|line| line.trim_end().ends_with("* ThemeSwitch.text-scale;"))
            .filter_map(|line| {
                let start = line.find("@{")? + 2;
                Some(&line[start..start + line[start..].find('}')?])
            })
            .collect();
        in_slint.sort_unstable();
        let mut here = SCALED.to_vec();
        here.sort_unstable();
        assert_eq!(in_slint, here);
    }

    #[test]
    fn reduced_motion_zeroes_the_motion_durations_and_only_those() {
        let template = include_str!("../../../ui/tokens.slint.in");
        let mut motion = 0;
        for line in template.lines().filter(|l| l.contains("property <duration>")) {
            let name = line.split("property <duration>").nth(1).unwrap();
            if name.trim_start().starts_with("motion-") {
                motion += 1;
                assert!(
                    line.contains("ThemeSwitch.reduce-motion ? 0ms :"),
                    "{line}"
                );
            } else {
                // Timeouts and delays are not animations.
                assert!(!line.contains("reduce-motion"), "{line}");
            }
        }
        assert!(motion >= 4, "{motion} motion durations");
    }

    #[test]
    fn every_animation_in_the_interface_takes_its_time_from_the_tokens() {
        fn slint_files(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if !path.ends_with("target") {
                        slint_files(&path, found);
                    }
                } else if path.extension().is_some_and(|e| e == "slint") {
                    found.push(path);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        slint_files(&root.join("crates"), &mut files);
        slint_files(&root.join("ui"), &mut files);
        assert!(files.len() > 10);
        let mut animations = 0;
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for line in text.lines().map(str::trim) {
                if line.starts_with("//") {
                    continue;
                }
                if let Some((_, rest)) = line.split_once("duration:") {
                    animations += 1;
                    assert!(
                        rest.trim_start().starts_with("Tokens.motion-"),
                        "{}: {line}",
                        file.display()
                    );
                }
            }
        }
        assert!(animations > 10, "{animations} animations");
    }

    #[test]
    fn a_scale_of_one_hundred_changes_nothing() {
        let a = Tokens::themed(true);
        let b = Tokens::for_appearance(Palette::Light, 100);
        assert!(a.names().all(|n| a.get(n) == b.get(n)));
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
