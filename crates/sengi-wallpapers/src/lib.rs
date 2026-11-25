//! The wallpapers Sengi OS ships: flat shapes in the colours of the design
//! tokens, no gradients and nothing taken from anyone else. Each is drawn
//! at full size and as a thumbnail for Settings.
//!
//! A pixel is one token colour. Where two colours meet, the edge is
//! smoothed by averaging four samples, so the only other colours in a
//! picture are the few steps between two neighbours at an edge.

use std::fs;
use std::io;
use std::path::Path;

use sengi_tokens::Tokens;

/// Full size: the target laptop's screen. swaybg scales to fill whatever
/// the output is.
pub const WIDTH: u32 = 1920;
pub const HEIGHT: u32 = 1080;
/// The thumbnail Settings shows.
pub const THUMB_WIDTH: u32 = 192;
pub const THUMB_HEIGHT: u32 = 108;

/// The colours a design may use, by token name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Desktop,
    Bg,
    Surface,
    Raised,
    Accent,
}

impl Ink {
    pub const ALL: [Ink; 5] = [
        Ink::Desktop,
        Ink::Bg,
        Ink::Surface,
        Ink::Raised,
        Ink::Accent,
    ];

    fn token(self) -> &'static str {
        match self {
            Ink::Desktop => "color.desktop",
            Ink::Bg => "color.bg",
            Ink::Surface => "color.surface",
            Ink::Raised => "color.surface-raised",
            Ink::Accent => "color.accent",
        }
    }
}

/// The token colour of `ink`.
pub fn rgb(tokens: &Tokens, ink: Ink) -> [u8; 3] {
    let c = tokens.color(ink.token()).expect("the colour is a token");
    [c.r, c.g, c.b]
}

/// What a design paints at a point, in units of the picture's height, so
/// that it looks the same at every size: x runs 0 to 16/9 and y 0 to 1.
type Paint = fn(f32, f32) -> Ink;

pub struct Design {
    pub file: &'static str,
    pub paint: Paint,
}

fn inside(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> bool {
    (x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r
}

fn graphite(_: f32, _: f32) -> Ink {
    Ink::Raised
}

fn horizon(_: f32, y: f32) -> Ink {
    if (0.60..0.606).contains(&y) {
        Ink::Accent
    } else if y > 0.606 {
        Ink::Surface
    } else {
        Ink::Bg
    }
}

fn hills(x: f32, y: f32) -> Ink {
    if inside(x, y, 1.15, 1.42, 0.62) {
        Ink::Accent
    } else if inside(x, y, 1.62, 1.35, 0.78) {
        Ink::Raised
    } else if inside(x, y, 0.45, 1.5, 0.82) {
        Ink::Surface
    } else {
        Ink::Bg
    }
}

fn tiles(x: f32, y: f32) -> Ink {
    // Squares a ninth of the height, a gap between them.
    let cell = 1.0 / 9.0;
    let (col, row) = ((x / cell) as i32, (y / cell) as i32);
    let (fx, fy) = (x / cell - col as f32, y / cell - row as f32);
    if !(0.08..0.92).contains(&fx) || !(0.08..0.92).contains(&fy) {
        return Ink::Desktop;
    }
    match (col * 7 + row * 13) % 11 {
        0 => Ink::Accent,
        1..=3 => Ink::Raised,
        4..=7 => Ink::Surface,
        _ => Ink::Bg,
    }
}

fn sunrise(x: f32, y: f32) -> Ink {
    let middle = 16.0 / 18.0;
    if y > 0.68 {
        Ink::Bg
    } else if inside(x, y, middle, 0.68, 0.30) {
        Ink::Accent
    } else if inside(x, y, middle, 0.68, 0.46) {
        Ink::Surface
    } else {
        Ink::Desktop
    }
}

fn stripes(x: f32, y: f32) -> Ink {
    // Bands at 45 degrees, a fifth of the height wide.
    let band = (x + y) / 0.2;
    let (index, along) = (band.floor() as i32, band.fract());
    if index % 4 == 0 && along < 0.12 {
        Ink::Accent
    } else if index % 2 == 0 {
        Ink::Surface
    } else {
        Ink::Bg
    }
}

/// The wallpapers, in the order Settings lists them. The number in the
/// file name is that order; the rest is the name shown.
pub const DESIGNS: [Design; 6] = [
    Design {
        file: "01-graphite.png",
        paint: graphite,
    },
    Design {
        file: "02-horizon.png",
        paint: horizon,
    },
    Design {
        file: "03-hills.png",
        paint: hills,
    },
    Design {
        file: "04-tiles.png",
        paint: tiles,
    },
    Design {
        file: "05-sunrise.png",
        paint: sunrise,
    },
    Design {
        file: "06-stripes.png",
        paint: stripes,
    },
];

/// The design drawn `width` by `height`, as RGB bytes.
pub fn render(tokens: &Tokens, design: &Design, width: u32, height: u32) -> Vec<u8> {
    let colours = Ink::ALL.map(|ink| rgb(tokens, ink));
    let at = |ink: Ink| colours[Ink::ALL.iter().position(|&i| i == ink).unwrap()];
    let unit = height as f32;
    let mut out = Vec::with_capacity((width * height * 3) as usize);
    for py in 0..height {
        for px in 0..width {
            let samples = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]
                .map(|(dx, dy)| (design.paint)((px as f32 + dx) / unit, (py as f32 + dy) / unit));
            if samples.iter().all(|&s| s == samples[0]) {
                out.extend_from_slice(&at(samples[0]));
            } else {
                let mut sum = [0u32; 3];
                for s in samples {
                    for (total, channel) in sum.iter_mut().zip(at(s)) {
                        *total += u32::from(channel);
                    }
                }
                out.extend(sum.iter().map(|total| ((total + 2) / 4) as u8));
            }
        }
    }
    out
}

/// A PNG of RGB bytes.
pub fn encode(rgb: &[u8], width: u32, height: u32) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::High);
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    writer.write_image_data(rgb).map_err(io::Error::other)?;
    writer.finish().map_err(io::Error::other)?;
    Ok(bytes)
}

/// The file of a wallpaper and that of its thumbnail, as written.
pub fn files(tokens: &Tokens, design: &Design) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let full = encode(&render(tokens, design, WIDTH, HEIGHT), WIDTH, HEIGHT)?;
    let thumb = encode(
        &render(tokens, design, THUMB_WIDTH, THUMB_HEIGHT),
        THUMB_WIDTH,
        THUMB_HEIGHT,
    )?;
    Ok((full, thumb))
}

/// Write every wallpaper to `dir` and its thumbnail to `dir/thumbs`.
pub fn write_all(dir: &Path) -> io::Result<usize> {
    let tokens = Tokens::builtin();
    fs::create_dir_all(dir.join("thumbs"))?;
    for design in &DESIGNS {
        let (full, thumb) = files(&tokens, design)?;
        fs::write(dir.join(design.file), full)?;
        fs::write(dir.join("thumbs").join(design.file), thumb)?;
    }
    Ok(DESIGNS.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn decode(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut reader = png::Decoder::new(io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut data).unwrap();
        assert_eq!(frame.color_type, png::ColorType::Rgb);
        data.truncate(frame.buffer_size());
        (frame.width, frame.height, data)
    }

    fn colours(data: &[u8]) -> HashMap<[u8; 3], usize> {
        let mut counts = HashMap::new();
        for pixel in data.as_chunks::<3>().0 {
            *counts.entry([pixel[0], pixel[1], pixel[2]]).or_insert(0) += 1;
        }
        counts
    }

    #[test]
    fn every_wallpaper_has_the_size_it_says_and_stays_small() {
        let tokens = Tokens::builtin();
        for design in &DESIGNS {
            let (full, thumb) = files(&tokens, design).unwrap();
            let (width, height, data) = decode(&full);
            assert_eq!((width, height), (WIDTH, HEIGHT), "{}", design.file);
            assert_eq!(data.len(), (WIDTH * HEIGHT * 3) as usize);
            let (width, height, _) = decode(&thumb);
            assert_eq!((width, height), (THUMB_WIDTH, THUMB_HEIGHT));
            assert!(
                full.len() < 100_000,
                "{}: {} bytes",
                design.file,
                full.len()
            );
            assert!(
                thumb.len() < 10_000,
                "{}: {} bytes",
                design.file,
                thumb.len()
            );
        }
    }

    #[test]
    fn the_wallpapers_are_flat_fills_in_token_colours() {
        let tokens = Tokens::builtin();
        let palette = Ink::ALL.map(|ink| rgb(&tokens, ink));
        for design in &DESIGNS {
            let (full, _) = files(&tokens, design).unwrap();
            let (_, _, data) = decode(&full);
            let counts = colours(&data);
            // Few colours: a gradient would use hundreds.
            assert!(
                counts.len() <= 40,
                "{}: {} colours",
                design.file,
                counts.len()
            );
            // Nearly every pixel is exactly a token colour; the rest is
            // the smoothing of edges.
            let exact: usize = palette.iter().filter_map(|c| counts.get(c)).sum();
            let total = (WIDTH * HEIGHT) as usize;
            assert!(
                exact * 100 >= total * 97,
                "{}: {exact} of {total}",
                design.file
            );
        }
    }

    #[test]
    fn the_designs_differ_and_their_names_are_in_order() {
        let tokens = Tokens::builtin();
        let mut seen = Vec::new();
        for design in &DESIGNS {
            let data = render(&tokens, design, 96, 54);
            assert!(!seen.contains(&data), "{} repeats another", design.file);
            seen.push(data);
        }
        for (index, design) in DESIGNS.iter().enumerate() {
            let name = design.file.strip_suffix(".png").unwrap();
            let (number, word) = name.split_once('-').unwrap();
            assert_eq!(number, format!("{:02}", index + 1));
            assert!(word.chars().all(|c| c.is_ascii_lowercase()));
        }
    }

    #[test]
    fn the_files_in_the_image_are_what_the_generator_writes() {
        let tokens = Tokens::builtin();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../image/rootfs/usr/share/backgrounds/sengi");
        for design in &DESIGNS {
            let (full, thumb) = files(&tokens, design).unwrap();
            let stale = format!(
                "run scripts/gen-wallpapers.sh: {} is missing or stale",
                design.file
            );
            assert!(
                fs::read(dir.join(design.file)).is_ok_and(|c| c == full),
                "{stale}"
            );
            assert!(
                fs::read(dir.join("thumbs").join(design.file)).is_ok_and(|c| c == thumb),
                "{stale}"
            );
        }
    }
}
