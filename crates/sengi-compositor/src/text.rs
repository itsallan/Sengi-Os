//! Finding the UI typeface and turning a line of text into pixels.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use ab_glyph::{point, Font as _, FontVec, PxScale, ScaleFont as _};

use crate::decoration::TextBitmap;

/// Titles longer than this many pixels are cut off; no title bar is wider.
const MAX_WIDTH: f32 = 4096.0;
/// The most characters `Font::fit` measures.
const FITTED_CHARS: usize = 300;

/// How deep font directories are searched.
const MAX_DEPTH: usize = 4;

pub struct Font {
    font: FontVec,
    scale: PxScale,
}

/// The style name font files use for a CSS-like weight.
pub fn style_name(weight: u32) -> &'static str {
    match weight {
        0..=349 => "Light",
        350..=449 => "Regular",
        450..=549 => "Medium",
        550..=649 => "SemiBold",
        _ => "Bold",
    }
}

/// How well a font file matches, lower is better. `None` if it is not a
/// usable file of this family at all.
pub fn score(file_name: &str, family: &str, weight: u32) -> Option<u32> {
    let (stem, extension) = file_name.rsplit_once('.')?;
    if !["otf", "ttf"].contains(&extension.to_ascii_lowercase().as_str()) {
        return None;
    }
    let stem = stem.to_ascii_lowercase();
    let family = family.to_ascii_lowercase().replace(' ', "");
    let style = style_name(weight).to_ascii_lowercase();

    if stem == format!("{family}-{style}") {
        Some(0)
    } else if stem == format!("{family}-regular") {
        Some(1)
    } else if stem == family || stem == format!("{family}variable") {
        // A variable font renders at its default weight.
        Some(2)
    } else {
        None
    }
}

fn font_dirs() -> Vec<PathBuf> {
    let user = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    user.into_iter()
        .map(|dir| dir.join("fonts"))
        .chain(["/usr/local/share/fonts".into(), "/usr/share/fonts".into()])
        .collect()
}

fn search(dir: &Path, depth: usize, family: &str, weight: u32, best: &mut Option<(u32, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < MAX_DEPTH {
                search(&path, depth + 1, family, weight, best);
            }
            continue;
        }
        let name = entry.file_name();
        let Some(found) = score(&name.to_string_lossy(), family, weight) else {
            continue;
        };
        if best.as_ref().is_none_or(|(known, _)| found < *known) {
            *best = Some((found, path));
        }
    }
}

/// The best installed file for `family` at `weight`.
pub fn find(family: &str, weight: u32) -> Option<PathBuf> {
    let mut best = None;
    for dir in font_dirs() {
        search(&dir, 0, family, weight, &mut best);
        if matches!(best, Some((0, _))) {
            break;
        }
    }
    best.map(|(_, path)| path)
}

impl Font {
    /// Loads `family` for text `px` pixels tall (the em size).
    pub fn load(family: &str, weight: u32, px: u32) -> Option<Self> {
        let path = find(family, weight)?;
        let font = FontVec::try_from_vec(fs::read(path).ok()?).ok()?;
        // ab_glyph scales by line height, the tokens give the em size.
        let scale = font.pt_to_px_scale(px as f32 * 72.0 / 96.0)?;
        Some(Self { font, scale })
    }

    /// `text` as one line that is at most `max_width` pixels wide. What does
    /// not fit is cut short and ends with an ellipsis, so that a long title
    /// reads as cut rather than as stopping at a letter. Nothing past
    /// `FITTED_CHARS` characters is measured: a title is never that long,
    /// and a client should not be able to make this slow.
    pub fn fit(&self, text: &str, max_width: usize) -> TextBitmap {
        let characters: Vec<char> = text.chars().take(FITTED_CHARS + 1).collect();
        let cut = characters.len() > FITTED_CHARS;
        let whole: String = characters.iter().take(FITTED_CHARS).collect();
        let bitmap = self.render(&whole);
        if !cut && bitmap.width <= max_width {
            return bitmap;
        }
        // The longest kept part that fits with its ellipsis: the width only
        // grows with the kept part, so a binary search finds it.
        let ellipsised = |keep: usize| {
            let mut shorter: String = characters[..keep].iter().collect();
            shorter.push('\u{2026}');
            shorter
        };
        let (mut low, mut high) = (0, characters.len().min(FITTED_CHARS).saturating_sub(1));
        let mut best = None;
        while low < high {
            let keep = (low + high).div_ceil(2);
            let candidate = self.render(&ellipsised(keep));
            if candidate.width <= max_width {
                low = keep;
                best = Some(candidate);
            } else {
                high = keep - 1;
            }
        }
        best.unwrap_or_else(|| self.render("\u{2026}"))
    }

    /// One line of text, as tall as the font's ascent plus descent.
    pub fn render(&self, text: &str) -> TextBitmap {
        let scaled = self.font.as_scaled(self.scale);
        let ascent = scaled.ascent();
        let height = (ascent - scaled.descent()).ceil().max(1.0) as usize;

        let mut glyphs = Vec::new();
        let mut pen = 0.0f32;
        let mut previous = None;
        for character in text.chars().filter(|c| !c.is_control()) {
            let id = self.font.glyph_id(character);
            if let Some(previous) = previous {
                pen += scaled.kern(previous, id);
            }
            if pen > MAX_WIDTH {
                break;
            }
            glyphs.push(id.with_scale_and_position(self.scale, point(pen, ascent)));
            pen += scaled.h_advance(id);
            previous = Some(id);
        }

        let width = pen.ceil().max(1.0) as usize;
        let mut coverage = vec![0u8; width * height];
        for glyph in glyphs {
            let Some(outline) = self.font.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outline.px_bounds();
            outline.draw(|x, y, amount| {
                let column = bounds.min.x as i32 + x as i32;
                let row = bounds.min.y as i32 + y as i32;
                if column < 0 || row < 0 || column as usize >= width || row as usize >= height {
                    return;
                }
                let cell = &mut coverage[row as usize * width + column as usize];
                *cell = (*cell).max((amount.clamp(0.0, 1.0) * 255.0).round() as u8);
            });
        }
        TextBitmap {
            width,
            height,
            coverage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_map_to_style_names() {
        assert_eq!(style_name(400), "Regular");
        assert_eq!(style_name(500), "Medium");
        assert_eq!(style_name(600), "SemiBold");
        assert_eq!(style_name(700), "Bold");
    }

    #[test]
    fn exact_style_beats_regular_beats_variable() {
        assert_eq!(score("Inter-Medium.otf", "Inter", 500), Some(0));
        assert_eq!(score("inter-medium.TTF", "Inter", 500), Some(0));
        assert_eq!(score("Inter-Regular.otf", "Inter", 500), Some(1));
        assert_eq!(score("InterVariable.ttf", "Inter", 500), Some(2));
    }

    #[test]
    fn other_families_styles_and_files_are_ignored() {
        assert_eq!(score("InterDisplay-Medium.otf", "Inter", 500), None);
        assert_eq!(score("Inter-MediumItalic.otf", "Inter", 500), None);
        assert_eq!(score("Inter-Bold.otf", "Inter", 500), None);
        assert_eq!(score("Inter-Medium.woff2", "Inter", 500), None);
        assert_eq!(score("Inter-Medium", "Inter", 500), None);
        assert_eq!(score("DejaVuSans.ttf", "Inter", 500), None);
    }

    #[test]
    fn family_names_with_spaces_match_file_names() {
        assert_eq!(
            score("DejaVuSansMono-Regular.ttf", "DejaVu Sans Mono", 400),
            Some(0)
        );
    }

    // Needs the typeface installed. A machine without it has to say so by
    // setting SENGI_TEST_WITHOUT_FONTS, so the gap is a choice and not a
    // silent pass.
    #[test]
    fn renders_text_with_the_installed_font() {
        let Some(font) = Font::load("Inter", 500, 13) else {
            assert!(
                std::env::var_os("SENGI_TEST_WITHOUT_FONTS").is_some(),
                "Inter is not installed (fonts-inter); set SENGI_TEST_WITHOUT_FONTS=1 to skip"
            );
            return;
        };
        let short = font.render("foot");
        let long = font.render("foot terminal");
        assert!(short.height >= 13 && short.height < 26);
        assert!(long.width > short.width);
        assert_eq!(short.coverage.len(), short.width * short.height);
        assert!(short.coverage.contains(&255));
        assert_eq!(font.render("").width, 1);
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    // Needs the typeface installed, like the test above: fonts-inter is in
    // the image and the CI runner installs it. Without it the test fails
    // unless SENGI_TEST_WITHOUT_FONTS is set.
    #[test]
    fn a_long_title_is_cut_with_an_ellipsis_and_a_short_one_is_whole() {
        let Some(font) = Font::load("Inter", 500, 13) else {
            assert!(
                std::env::var_os("SENGI_TEST_WITHOUT_FONTS").is_some(),
                "Inter is not installed (fonts-inter); set SENGI_TEST_WITHOUT_FONTS=1 to skip"
            );
            return;
        };
        let long = "a title that is far too long for the room a window gives it";
        let fitted = font.fit(long, 120);
        assert!(fitted.width <= 120, "{} wide", fitted.width);
        assert!(fitted.width < font.render(long).width);
        let short = font.fit("Terminal", 400);
        assert_eq!(short.width, font.render("Terminal").width);
    }

    #[test]
    fn a_title_of_any_length_is_cut_to_the_room_quickly() {
        let Some(font) = Font::load("Inter", 500, 13) else {
            assert!(
                std::env::var_os("SENGI_TEST_WITHOUT_FONTS").is_some(),
                "Inter is not installed (fonts-inter); set SENGI_TEST_WITHOUT_FONTS=1 to skip"
            );
            return;
        };
        let endless = "word ".repeat(10_000);
        let fitted = font.fit(&endless, 120);
        assert!(
            fitted.width <= 120 && fitted.width > 0,
            "{} wide",
            fitted.width
        );
        // No room at all: still an answer, not a panic.
        let _ = font.fit(&endless, 0);
    }
}
