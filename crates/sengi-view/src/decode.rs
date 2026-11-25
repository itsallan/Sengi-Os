//! Decoding a PNG or a JPEG in this program: the PNG crate and zune-jpeg,
//! both pure Rust. The size is read first and a picture over the limit is
//! refused before its pixels are touched; a PNG is shrunk row by row as it
//! is read, so what is held is the shrunk copy and one row.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::scale::Downscaler;

/// The most pixels a picture may have: 100 megapixels.
pub const MAX_PIXELS: u64 = 100_000_000;

/// The longer side of the copy that is drawn. A bigger picture is shrunk to
/// this, and at 100% is drawn from the shrunk copy; that keeps one picture
/// within about 38 MB of memory whatever its size.
pub const DISPLAY_SIDE: usize = 3072;

/// A picture ready to draw.
#[derive(Debug)]
pub struct Decoded {
    /// The size of the original, in pixels.
    pub width: u32,
    pub height: u32,
    /// The size of the copy that is drawn.
    pub shown_width: u32,
    pub shown_height: u32,
    /// The copy: RGBA, straight alpha.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
}

/// Which format the first bytes of a file are.
pub fn sniff(head: &[u8]) -> Option<Format> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Format::Jpeg)
    } else {
        None
    }
}

fn too_big(width: u64, height: u64) -> Option<String> {
    (width * height > MAX_PIXELS).then(|| {
        format!(
            "It is {width} x {height} pixels, {} megapixels, and Image Viewer opens up to {}.",
            (width * height).div_ceil(1_000_000),
            MAX_PIXELS / 1_000_000
        )
    })
}

/// The picture in the file, or what to tell the user.
pub fn decode(path: &Path) -> Result<Decoded, String> {
    let mut file = File::open(path).map_err(|e| format!("It cannot be read: {e}."))?;
    let mut head = [0u8; 24];
    let got = file
        .read(&mut head)
        .map_err(|e| format!("It cannot be read: {e}."))?;
    match sniff(&head[..got]) {
        Some(Format::Png) => {
            drop(file);
            // The size is in the first chunk, right after the signature.
            if got == head.len() {
                let word =
                    |at: usize| u64::from(u32::from_be_bytes(head[at..at + 4].try_into().unwrap()));
                if let Some(refusal) = too_big(word(16), word(20)) {
                    return Err(refusal);
                }
            }
            png(path)
        }
        Some(Format::Jpeg) => {
            let mut data = head[..got].to_vec();
            file.read_to_end(&mut data)
                .map_err(|e| format!("It cannot be read: {e}."))?;
            jpeg(&data)
        }
        None => Err("It is not a PNG or a JPEG picture.".to_string()),
    }
}

fn png(path: &Path) -> Result<Decoded, String> {
    let file = File::open(path).map_err(|e| format!("It cannot be read: {e}."))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    // Palettes, small bit depths and transparency colours become plain
    // 8-bit samples.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("It is damaged or not a PNG: {e}."))?;
    let (width, height, interlaced) = {
        let info = reader.info();
        (info.width, info.height, info.interlaced)
    };
    if let Some(refusal) = too_big(u64::from(width), u64::from(height)) {
        return Err(refusal);
    }
    let (color, _) = reader.output_color_type();
    let (w, h) = (width as usize, height as usize);
    let mut scaler = Downscaler::new(w, h, DISPLAY_SIDE);
    let mut line = vec![0u8; w * 4];
    let damaged = |e: png::DecodingError| format!("It is damaged: {e}.");
    if interlaced {
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| "It is too big to open.".to_string())?;
        let mut buffer = vec![0u8; size];
        let frame = reader.next_frame(&mut buffer).map_err(damaged)?;
        for row in buffer.chunks_exact(frame.line_size).take(h) {
            expand(color, row, &mut line);
            scaler.push_row(&line);
        }
    } else {
        while let Some(row) = reader.next_row().map_err(damaged)? {
            expand(color, row.data(), &mut line);
            scaler.push_row(&line);
        }
    }
    let (shown_width, shown_height) = scaler.size();
    Ok(Decoded {
        width,
        height,
        shown_width: shown_width as u32,
        shown_height: shown_height as u32,
        rgba: scaler.finish(),
    })
}

/// One row of 8-bit samples of any colour type, as RGBA.
fn expand(color: png::ColorType, row: &[u8], out: &mut [u8]) {
    match color {
        png::ColorType::Grayscale => {
            for (pixel, g) in out.as_chunks_mut::<4>().0.iter_mut().zip(row) {
                *pixel = [*g, *g, *g, 255];
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for (pixel, ga) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(row.as_chunks::<2>().0.iter())
            {
                *pixel = [ga[0], ga[0], ga[0], ga[1]];
            }
        }
        png::ColorType::Rgb => {
            for (pixel, rgb) in out
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(row.as_chunks::<3>().0.iter())
            {
                *pixel = [rgb[0], rgb[1], rgb[2], 255];
            }
        }
        png::ColorType::Rgba => out.copy_from_slice(&row[..out.len()]),
        // EXPAND turns a palette into RGB or RGBA before it gets here.
        png::ColorType::Indexed => out.fill(0),
    }
}

fn jpeg(data: &[u8]) -> Result<Decoded, String> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    use zune_jpeg::JpegDecoder;

    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    decoder
        .decode_headers()
        .map_err(|e| format!("It is damaged or not a JPEG: {e:?}."))?;
    let (w, h) = decoder
        .dimensions()
        .ok_or_else(|| "It is damaged: it has no size.".to_string())?;
    if let Some(refusal) = too_big(w as u64, h as u64) {
        return Err(refusal);
    }
    let pixels = decoder
        .decode()
        .map_err(|e| format!("It is damaged or not a kind of JPEG that is opened: {e:?}."))?;
    if w == 0 || h == 0 || pixels.len() % (w * h) != 0 {
        return Err("It is damaged: the pixels do not fit its size.".to_string());
    }
    let channels = pixels.len() / (w * h);
    let mut scaler = Downscaler::new(w, h, DISPLAY_SIDE);
    let mut line = vec![255u8; w * 4];
    for row in pixels.chunks_exact(w * channels) {
        match channels {
            1 => {
                for (pixel, g) in line.as_chunks_mut::<4>().0.iter_mut().zip(row) {
                    pixel[..3].fill(*g);
                }
            }
            3 => {
                for (pixel, rgb) in line
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(row.as_chunks::<3>().0.iter())
                {
                    pixel[..3].copy_from_slice(rgb);
                }
            }
            4 => line.copy_from_slice(row),
            _ => return Err("It uses colours that are not opened.".to_string()),
        }
        scaler.push_row(&line);
    }
    drop(pixels);
    let (shown_width, shown_height) = scaler.size();
    Ok(Decoded {
        width: w as u32,
        height: h as u32,
        shown_width: shown_width as u32,
        shown_height: shown_height as u32,
        rgba: scaler.finish(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUADRANTS: &[u8] = include_bytes!("../tests/fixtures/quadrants.jpg");
    const GRAY: &[u8] = include_bytes!("../tests/fixtures/gray.jpg");

    struct Dir(std::path::PathBuf);

    impl Dir {
        fn new(tag: &str) -> Dir {
            let path =
                std::env::temp_dir().join(format!("sengi-view-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Dir(path)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A PNG of the given colour type with a pixel that depends on x and y.
    pub fn make_png(width: u32, height: u32, color: png::ColorType) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let samples = color.samples();
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                for s in 0..samples {
                    data.push((x * 3 + y * 5 + s as u32 * 40) as u8);
                }
            }
        }
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&data).unwrap();
        writer.finish().unwrap();
        out
    }

    fn at(d: &Decoded, x: usize, y: usize) -> [u8; 4] {
        let i = (y * d.shown_width as usize + x) * 4;
        d.rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn rgba_and_rgb_pngs_keep_their_pixels() {
        let dir = Dir::new("png");
        let rgba = dir.file("a.png", &make_png(10, 6, png::ColorType::Rgba));
        let d = decode(&rgba).unwrap();
        assert_eq!(
            (d.width, d.height, d.shown_width, d.shown_height),
            (10, 6, 10, 6)
        );
        assert_eq!(at(&d, 0, 0), [0, 40, 80, 120]);
        assert_eq!(at(&d, 2, 1), [11, 51, 91, 131]);
        let rgb = dir.file("b.png", &make_png(10, 6, png::ColorType::Rgb));
        let d = decode(&rgb).unwrap();
        assert_eq!(at(&d, 2, 1), [11, 51, 91, 255]);
    }

    #[test]
    fn grey_pngs_become_grey_pixels() {
        let dir = Dir::new("grey");
        let g = dir.file("g.png", &make_png(4, 4, png::ColorType::Grayscale));
        assert_eq!(at(&decode(&g).unwrap(), 1, 1), [8, 8, 8, 255]);
        let ga = dir.file("ga.png", &make_png(4, 4, png::ColorType::GrayscaleAlpha));
        assert_eq!(at(&decode(&ga).unwrap(), 1, 1), [8, 8, 8, 48]);
    }

    #[test]
    fn a_big_png_is_shrunk_to_the_display_side() {
        let dir = Dir::new("shrink");
        let path = dir.file("wide.png", &make_png(6144, 8, png::ColorType::Rgb));
        let d = decode(&path).unwrap();
        assert_eq!((d.width, d.height), (6144, 8));
        assert_eq!((d.shown_width, d.shown_height), (3072, 4));
        assert_eq!(d.rgba.len(), 3072 * 4 * 4);
    }

    #[test]
    fn jpegs_decode_to_their_colours() {
        let dir = Dir::new("jpeg");
        let path = dir.file("q.jpg", QUADRANTS);
        let d = decode(&path).unwrap();
        assert_eq!((d.width, d.height), (64, 48));
        let near = |got: [u8; 4], want: [u8; 3]| {
            got[3] == 255 && got[..3].iter().zip(want).all(|(g, w)| g.abs_diff(w) < 24)
        };
        assert!(near(at(&d, 8, 6), [255, 0, 0]), "{:?}", at(&d, 8, 6));
        assert!(near(at(&d, 48, 6), [0, 255, 0]), "{:?}", at(&d, 48, 6));
        assert!(near(at(&d, 8, 36), [0, 0, 255]), "{:?}", at(&d, 8, 36));
        assert!(
            near(at(&d, 48, 36), [255, 255, 255]),
            "{:?}",
            at(&d, 48, 36)
        );
        let gray = decode(&dir.file("g.jpg", GRAY)).unwrap();
        let (top, bottom) = (at(&gray, 10, 1), at(&gray, 10, 46));
        assert!(top[0] < 40 && bottom[0] > 215 && top[0] == top[1] && top[1] == top[2]);
    }

    #[test]
    fn other_files_and_damaged_ones_say_so() {
        let dir = Dir::new("bad");
        assert!(decode(&dir.file("a.png", b"not a picture"))
            .err()
            .unwrap()
            .contains("not a PNG or a JPEG"));
        let mut cut = make_png(20, 20, png::ColorType::Rgb);
        cut.truncate(cut.len() / 2);
        assert!(decode(&dir.file("cut.png", &cut)).is_err());
        assert!(decode(&dir.file("cut.jpg", &QUADRANTS[..120])).is_err());
        assert!(decode(&dir.0.join("missing.png")).is_err());
    }

    #[test]
    fn over_a_hundred_megapixels_is_refused_before_decoding() {
        let dir = Dir::new("huge");
        // A header that says 10001 x 10000 and nothing after it: the size is
        // read first, so the refusal comes before any pixel is wanted.
        let mut header = Vec::new();
        {
            let mut e = png::Encoder::new(&mut header, 10_001, 10_000);
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::One);
            // No data is written: the writer is dropped unfinished.
            drop(e.write_header().unwrap());
        }
        let path = dir.file("huge.png", &header);
        let why = decode(&path).unwrap_err();
        assert!(why.contains("100"), "{why}");
        assert!(why.contains("10001 x 10000"), "{why}");
    }
}
