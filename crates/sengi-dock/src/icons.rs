//! Finding and decoding application icons.
//!
//! Lookup follows the icon theme rules for the `hicolor` theme, which is
//! where apps install their own icons, then `/usr/share/pixmaps`. Icons are
//! decoded straight to the size they are drawn at, so a 256 px PNG costs the
//! dock a 40 px bitmap and nothing more.
//!
//! Icon files come from whatever the user installs, and the dock must
//! survive any of them. So the dock itself never decodes one: [`Decoder`]
//! starts this same program again as a short-lived child that decodes one
//! file under tight limits and writes the pixels to its standard output. A
//! file that crashes or stalls the decoder costs that child, and the app
//! keeps its letter tile.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// Largest source image we are willing to decode, in pixels per side.
const MAX_SOURCE: u32 = 2048;

/// Largest icon files we are willing to read. Real icons are far smaller.
const MAX_PNG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SVG_BYTES: u64 = 1024 * 1024;

/// Largest icon we are willing to produce, in pixels per side.
const MAX_PIXELS: u32 = 512;

/// How long a decode may take before it is given up on.
pub const DECODE_TIMEOUT: Duration = Duration::from_secs(1);

/// First argument of the child that decodes one icon.
pub const DECODE_FLAG: &str = "--decode-icon";

/// Address space the decoding child may use.
const DECODE_MEMORY: u64 = 512 * 1024 * 1024;

/// Where icons are looked for.
#[derive(Clone, Debug, Default)]
pub struct IconDirs {
    /// Directories that hold themes, e.g. `/usr/share/icons`.
    pub themes: Vec<PathBuf>,
    /// Flat directories of loose icons, e.g. `/usr/share/pixmaps`.
    pub pixmaps: Vec<PathBuf>,
}

impl IconDirs {
    /// The standard places, given the XDG data directories.
    pub fn from_data_dirs(home: Option<&str>, data_dirs: &[PathBuf]) -> Self {
        let mut themes = Vec::new();
        if let Some(home) = home {
            themes.push(Path::new(home).join(".icons"));
        }
        themes.extend(data_dirs.iter().map(|dir| dir.join("icons")));
        Self {
            themes,
            pixmaps: vec![PathBuf::from("/usr/share/pixmaps")],
        }
    }
}

fn supported(path: &Path) -> bool {
    let extension = path.extension().and_then(|e| e.to_str());
    extension.is_some_and(|e| extensions().contains(&e))
}

fn extensions() -> &'static [&'static str] {
    if cfg!(feature = "svg") {
        &["png", "svg"]
    } else {
        &["png"]
    }
}

/// What a theme subdirectory such as `48x48`, `64x64@2` or `scalable` holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirSize {
    /// Bitmaps of this many pixels per side.
    Fixed(u32),
    Scalable,
}

fn dir_size(name: &str) -> Option<DirSize> {
    if name == "scalable" {
        return Some(DirSize::Scalable);
    }
    let (size, scale) = match name.split_once('@') {
        Some((size, scale)) => (size, scale.parse::<u32>().ok()?),
        None => (name, 1),
    };
    let (width, height) = size.split_once('x')?;
    let width: u32 = width.parse().ok()?;
    (width.to_string() == height && width > 0 && scale > 0)
        .then(|| DirSize::Fixed(width.saturating_mul(scale)))
}

/// Lower is better. An exact bitmap wins, then a vector, then the smallest
/// bitmap that is too big, and last the largest one that is too small.
fn rank(size: DirSize, wanted: u32) -> (u8, u32) {
    match size {
        DirSize::Fixed(px) if px == wanted => (0, 0),
        DirSize::Scalable => (1, 0),
        DirSize::Fixed(px) if px > wanted => (2, px - wanted),
        DirSize::Fixed(px) => (3, wanted - px),
    }
}

/// Finds the file for an `Icon=` value, to be drawn `pixels` wide.
pub fn find(dirs: &IconDirs, icon: &str, pixels: u32) -> Option<PathBuf> {
    if icon.is_empty() {
        return None;
    }
    if icon.starts_with('/') {
        let path = PathBuf::from(icon);
        return (supported(&path) && path.is_file()).then_some(path);
    }
    // Some entries wrongly include the extension in the name.
    let name = [".png", ".svg", ".xpm"]
        .iter()
        .find_map(|ext| icon.strip_suffix(ext))
        .unwrap_or(icon);

    let mut best: Option<((u8, u32), PathBuf)> = None;
    for theme_dir in &dirs.themes {
        let hicolor = theme_dir.join("hicolor");
        let Ok(read) = fs::read_dir(&hicolor) else {
            continue;
        };
        for sub in read.flatten() {
            let Some(size) = sub.file_name().to_str().and_then(dir_size) else {
                continue;
            };
            let score = rank(size, pixels);
            if best.as_ref().is_some_and(|(held, _)| *held <= score) {
                continue;
            }
            for ext in extensions() {
                if size == DirSize::Scalable && *ext != "svg" {
                    continue;
                }
                let path = sub.path().join("apps").join(format!("{name}.{ext}"));
                if path.is_file() {
                    best = Some((score, path));
                    break;
                }
            }
        }
        // An icon in the user's directory beats a better-sized system one.
        if best.is_some() {
            break;
        }
    }
    if let Some((_, path)) = best {
        return Some(path);
    }

    for dir in &dirs.pixmaps {
        for ext in extensions() {
            let path = dir.join(format!("{name}.{ext}"));
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// A decoded icon: `size` by `size` pixels of premultiplied RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    pub size: u32,
    pub data: Vec<u8>,
}

/// Reads an icon file, refusing anything that is not a regular file of a
/// believable size. A FIFO or `/dev/zero` under an icon's name would
/// otherwise be read for ever.
fn read_icon(path: &Path) -> Option<Vec<u8>> {
    let limit = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => MAX_PNG_BYTES,
        Some("svg") => MAX_SVG_BYTES,
        _ => return None,
    };
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    // The size is checked again while reading, in case the file grows.
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

/// Decodes an icon file into a square of `pixels` per side, in this
/// process. The dock goes through [`Decoder`] instead.
pub fn load(path: &Path, pixels: u32) -> Option<Pixels> {
    if pixels == 0 || pixels > MAX_PIXELS {
        return None;
    }
    let bytes = read_icon(path)?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => {
            let (width, height, rgba) = decode_png(&bytes)?;
            Some(fit(&rgba, width, height, pixels))
        }
        #[cfg(feature = "svg")]
        Some("svg") => render_svg(&bytes, pixels),
        _ => None,
    }
}

/// Finds and decodes in one step.
pub fn load_named(dirs: &IconDirs, icon: &str, pixels: u32) -> Option<Pixels> {
    load(&find(dirs, icon, pixels)?, pixels)
}

/// Decodes to straight (not premultiplied) RGBA.
fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width, reader.info().height);
    if width == 0 || height == 0 || width > MAX_SOURCE || height > MAX_SOURCE {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let data = &buffer[..frame.buffer_size()];

    let count = (width * height) as usize;
    let mut rgba = Vec::with_capacity(count * 4);
    match frame.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(data),
        png::ColorType::Rgb => {
            for p in data.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            for &g in data {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for p in data.as_chunks::<2>().0 {
                rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        png::ColorType::Indexed => return None,
    }
    (rgba.len() == count * 4).then_some((width, height, rgba))
}

/// How much of source cell `cell` falls inside the span `[start, end)`.
fn overlap(cell: usize, start: f32, end: f32) -> f32 {
    let lo = (cell as f32).max(start);
    let hi = ((cell + 1) as f32).min(end);
    (hi - lo).max(0.0)
}

/// Scales straight RGBA to fit a `size` square, centred, keeping its shape,
/// and premultiplies. Each output pixel is the area average of the source
/// pixels it covers, which is what keeps a shrunken icon from looking jagged.
fn fit(rgba: &[u8], width: u32, height: u32, size: u32) -> Pixels {
    let scale = size as f32 / width.max(height) as f32;
    let out_w = ((width as f32 * scale).round() as u32).clamp(1, size);
    let out_h = ((height as f32 * scale).round() as u32).clamp(1, size);
    let left = (size - out_w) / 2;
    let top = (size - out_h) / 2;
    let step_x = width as f32 / out_w as f32;
    let step_y = height as f32 / out_h as f32;

    let mut data = vec![0u8; (size * size * 4) as usize];
    for oy in 0..out_h {
        let (y0, y1) = (oy as f32 * step_y, (oy + 1) as f32 * step_y);
        for ox in 0..out_w {
            let (x0, x1) = (ox as f32 * step_x, (ox + 1) as f32 * step_x);
            let mut sum = [0f32; 4];
            let mut area = 0f32;
            for sy in (y0 as usize)..(y1.ceil() as usize).min(height as usize) {
                let wy = overlap(sy, y0, y1);
                for sx in (x0 as usize)..(x1.ceil() as usize).min(width as usize) {
                    let weight = wy * overlap(sx, x0, x1);
                    let p = &rgba[(sy * width as usize + sx) * 4..][..4];
                    let alpha = p[3] as f32 / 255.0;
                    sum[0] += p[0] as f32 * alpha * weight;
                    sum[1] += p[1] as f32 * alpha * weight;
                    sum[2] += p[2] as f32 * alpha * weight;
                    sum[3] += p[3] as f32 * weight;
                    area += weight;
                }
            }
            if area > 0.0 {
                let at = (((top + oy) * size + left + ox) * 4) as usize;
                for (out, total) in data[at..at + 4].iter_mut().zip(sum) {
                    *out = (total / area).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    Pixels { size, data }
}

#[cfg(feature = "svg")]
fn render_svg(bytes: &[u8], size: u32) -> Option<Pixels> {
    use resvg::{tiny_skia, usvg};

    // By default an <image> element may name any local file and usvg reads
    // it whole. An icon has no business doing that, so nothing is resolved.
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let (width, height) = (tree.size().width(), tree.size().height());
    let scale = size as f32 / width.max(height);
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(
        (size as f32 - width * scale) / 2.0,
        (size as f32 - height * scale) / 2.0,
    );
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(Pixels {
        size,
        data: pixmap.take(),
    })
}

/// The child's side: decodes one icon and writes its pixels to standard
/// output. Returns None when the arguments are not a decode request, so
/// `main` can call this first and carry on as usual otherwise.
pub fn run_decoder(args: &[String]) -> Option<ExitCode> {
    let [flag, path, pixels] = args else {
        return None;
    };
    if flag != DECODE_FLAG {
        return None;
    }
    // A decoder gone wrong is stopped by the kernel rather than left to
    // eat the machine. The parent has its own clock as well.
    for (resource, limit) in [
        (libc::RLIMIT_AS, DECODE_MEMORY),
        (libc::RLIMIT_CPU, DECODE_TIMEOUT.as_secs() + 1),
    ] {
        let limit = libc::rlimit {
            rlim_cur: limit,
            rlim_max: limit,
        };
        // SAFETY: setrlimit only reads the struct it is handed.
        unsafe { libc::setrlimit(resource, &limit) };
    }
    let decoded = pixels
        .parse()
        .ok()
        .and_then(|pixels| load(Path::new(path), pixels));
    let written = decoded.is_some_and(|icon| {
        let mut out = std::io::stdout().lock();
        out.write_all(&icon.data).and_then(|()| out.flush()).is_ok()
    });
    Some(if written {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Decodes icons in a child process, so that no icon file can crash or
/// stall the program that asked.
#[derive(Clone, Debug)]
pub struct Decoder {
    program: PathBuf,
    timeout: Duration,
}

impl Decoder {
    /// A decoder that runs `program`, which must answer [`DECODE_FLAG`] by
    /// calling [`run_decoder`].
    pub fn new(program: PathBuf, timeout: Duration) -> Self {
        Self { program, timeout }
    }

    /// A decoder that runs the program we are part of.
    pub fn for_this_program() -> Option<Self> {
        let program = std::env::current_exe().ok()?;
        Some(Self::new(program, DECODE_TIMEOUT))
    }

    /// Decodes `path` to `pixels` per side. Blocks for at most the timeout,
    /// so call it away from the UI thread. None means the file is not a
    /// usable icon, whatever the reason.
    pub fn decode(&self, path: &Path, pixels: u32) -> Option<Pixels> {
        if pixels == 0 || pixels > MAX_PIXELS {
            return None;
        }
        let expected = (pixels * pixels * 4) as usize;
        let mut child = Command::new(&self.program)
            .arg(DECODE_FLAG)
            .arg(path)
            .arg(pixels.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut output = child.stdout.take()?;

        // Someone has to watch the clock while the pipe is read.
        let timeout = self.timeout;
        let watchdog = std::thread::spawn(move || {
            let started = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => return status.success(),
                    Ok(None) if started.elapsed() < timeout => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return false;
                    }
                }
            }
        });

        let mut data = Vec::with_capacity(expected);
        let read = (&mut output)
            .take(expected as u64 + 1)
            .read_to_end(&mut data);
        // Closing our end unblocks a child that has more to say.
        drop(output);
        let finished = watchdog.join().unwrap_or(false);
        (finished && read.is_ok() && data.len() == expected)
            .then_some(Pixels { size: pixels, data })
    }

    /// Finds and decodes in one step.
    pub fn decode_named(&self, dirs: &IconDirs, icon: &str, pixels: u32) -> Option<Pixels> {
        self.decode(&find(dirs, icon, pixels)?, pixels)
    }
}

/// The letter shown on the fallback tile for an app with no usable icon.
pub fn initial(name: &str) -> String {
    name.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_else(|| "?".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::temp_dir;

    fn write_png(path: &Path, width: u32, height: u32, color: png::ColorType, data: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(data).unwrap();
    }

    fn solid(path: &Path, side: u32, rgba: [u8; 4]) {
        let data: Vec<u8> = std::iter::repeat_n(rgba, (side * side) as usize)
            .flatten()
            .collect();
        write_png(path, side, side, png::ColorType::Rgba, &data);
    }

    fn dirs(root: &Path) -> IconDirs {
        IconDirs {
            themes: vec![root.join("home/icons"), root.join("usr/icons")],
            pixmaps: vec![root.join("pixmaps")],
        }
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"").unwrap();
    }

    #[test]
    fn theme_directory_names_give_sizes() {
        assert_eq!(dir_size("48x48"), Some(DirSize::Fixed(48)));
        assert_eq!(dir_size("64x64@2"), Some(DirSize::Fixed(128)));
        assert_eq!(dir_size("scalable"), Some(DirSize::Scalable));
        assert_eq!(dir_size("symbolic"), None);
        assert_eq!(dir_size("48x32"), None);
        assert_eq!(dir_size("0x0"), None);
        assert_eq!(dir_size("x"), None);
    }

    #[test]
    fn the_exact_size_is_preferred() {
        let root = temp_dir("icons");
        let base = root.join("usr/icons/hicolor");
        for size in ["16x16", "40x40", "48x48", "256x256"] {
            touch(&base.join(size).join("apps/web.png"));
        }
        assert_eq!(
            find(&dirs(&root), "web", 40),
            Some(base.join("40x40/apps/web.png"))
        );
        // Twice the scale: the next size up, not a blurry enlargement.
        assert_eq!(
            find(&dirs(&root), "web", 80),
            Some(base.join("256x256/apps/web.png"))
        );
    }

    #[test]
    fn the_nearest_larger_bitmap_is_chosen_over_a_smaller_one() {
        let root = temp_dir("icons");
        let base = root.join("usr/icons/hicolor");
        for size in ["16x16", "32x32", "48x48", "128x128"] {
            touch(&base.join(size).join("apps/web.png"));
        }
        assert_eq!(
            find(&dirs(&root), "web", 40),
            Some(base.join("48x48/apps/web.png"))
        );
        assert_eq!(
            find(&dirs(&root), "web", 500),
            Some(base.join("128x128/apps/web.png"))
        );
    }

    #[test]
    fn hidpi_directories_count_at_their_real_size() {
        let root = temp_dir("icons");
        let base = root.join("usr/icons/hicolor");
        touch(&base.join("32x32/apps/web.png"));
        touch(&base.join("20x20@2/apps/web.png"));
        assert_eq!(
            find(&dirs(&root), "web", 40),
            Some(base.join("20x20@2/apps/web.png"))
        );
    }

    #[cfg(feature = "svg")]
    #[test]
    fn a_vector_beats_a_bitmap_of_the_wrong_size() {
        let root = temp_dir("icons");
        let base = root.join("usr/icons/hicolor");
        touch(&base.join("48x48/apps/web.png"));
        touch(&base.join("scalable/apps/web.svg"));
        assert_eq!(
            find(&dirs(&root), "web", 40),
            Some(base.join("scalable/apps/web.svg"))
        );
        assert_eq!(
            find(&dirs(&root), "web", 48),
            Some(base.join("48x48/apps/web.png"))
        );
    }

    #[test]
    fn only_the_apps_context_is_searched() {
        let root = temp_dir("icons");
        touch(&root.join("usr/icons/hicolor/48x48/actions/web.png"));
        touch(&root.join("usr/icons/Other/48x48/apps/web.png"));
        assert_eq!(find(&dirs(&root), "web", 40), None);
    }

    #[test]
    fn the_user_theme_directory_wins() {
        let root = temp_dir("icons");
        touch(&root.join("usr/icons/hicolor/40x40/apps/web.png"));
        touch(&root.join("home/icons/hicolor/128x128/apps/web.png"));
        assert_eq!(
            find(&dirs(&root), "web", 40),
            Some(root.join("home/icons/hicolor/128x128/apps/web.png"))
        );
    }

    #[test]
    fn pixmaps_are_the_last_resort() {
        let root = temp_dir("icons");
        touch(&root.join("pixmaps/old.png"));
        touch(&root.join("pixmaps/older.xpm"));
        assert_eq!(
            find(&dirs(&root), "old", 40),
            Some(root.join("pixmaps/old.png"))
        );
        // An extension in the name is tolerated.
        assert_eq!(
            find(&dirs(&root), "old.png", 40),
            Some(root.join("pixmaps/old.png"))
        );
        // XPM is not decoded, so it is not found either.
        assert_eq!(find(&dirs(&root), "older", 40), None);
        assert_eq!(find(&dirs(&root), "missing", 40), None);
        assert_eq!(find(&dirs(&root), "", 40), None);
    }

    #[test]
    fn absolute_paths_are_used_as_they_are() {
        let root = temp_dir("icons");
        let path = root.join("opt/app/icon.png");
        touch(&path);
        touch(&root.join("opt/app/icon.xpm"));
        let name = path.to_str().unwrap();
        assert_eq!(find(&dirs(&root), name, 40), Some(path.clone()));
        assert_eq!(
            find(
                &dirs(&root),
                root.join("opt/app/icon.xpm").to_str().unwrap(),
                40
            ),
            None
        );
        assert_eq!(find(&dirs(&root), "/nonexistent/icon.png", 40), None);
    }

    #[test]
    fn the_standard_directories_are_derived_from_the_data_dirs() {
        let dirs = IconDirs::from_data_dirs(
            Some("/home/a"),
            &[
                PathBuf::from("/home/a/.local/share"),
                PathBuf::from("/usr/share"),
            ],
        );
        assert_eq!(
            dirs.themes,
            [
                PathBuf::from("/home/a/.icons"),
                PathBuf::from("/home/a/.local/share/icons"),
                PathBuf::from("/usr/share/icons"),
            ]
        );
        assert_eq!(dirs.pixmaps, [PathBuf::from("/usr/share/pixmaps")]);
    }

    #[test]
    fn a_large_png_is_shrunk_to_the_drawn_size() {
        let root = temp_dir("icons");
        let path = root.join("big.png");
        solid(&path, 256, [200, 100, 50, 255]);
        let pixels = load(&path, 40).unwrap();
        assert_eq!(pixels.size, 40);
        assert_eq!(pixels.data.len(), 40 * 40 * 4);
        assert_eq!(&pixels.data[..4], [200, 100, 50, 255]);
        assert_eq!(&pixels.data[pixels.data.len() - 4..], [200, 100, 50, 255]);
    }

    #[test]
    fn translucent_pixels_are_premultiplied() {
        let root = temp_dir("icons");
        let path = root.join("glass.png");
        solid(&path, 8, [255, 255, 255, 128]);
        let pixels = load(&path, 8).unwrap();
        assert_eq!(&pixels.data[..4], [128, 128, 128, 128]);
    }

    #[test]
    fn shrinking_averages_neighbouring_pixels() {
        let root = temp_dir("icons");
        let path = root.join("checks.png");
        // Black and white columns, two pixels shrunk into one give grey.
        let data: Vec<u8> = (0..4 * 4)
            .flat_map(|i| {
                if i % 2 == 0 {
                    [0, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                }
            })
            .collect();
        write_png(&path, 4, 4, png::ColorType::Rgba, &data);
        let pixels = load(&path, 2).unwrap();
        for pixel in pixels.data.as_chunks::<4>().0 {
            assert_eq!(*pixel, [128, 128, 128, 255]);
        }
    }

    #[test]
    fn a_wide_image_is_centred_with_clear_bands() {
        let root = temp_dir("icons");
        let path = root.join("wide.png");
        let data: Vec<u8> = std::iter::repeat_n([10u8, 20, 30], 8 * 4)
            .flatten()
            .collect();
        write_png(&path, 8, 4, png::ColorType::Rgb, &data);
        let pixels = load(&path, 8).unwrap();
        let row = |y: usize| &pixels.data[y * 8 * 4..][..4];
        assert_eq!(row(0), [0, 0, 0, 0]);
        assert_eq!(row(1), [0, 0, 0, 0]);
        assert_eq!(row(2), [10, 20, 30, 255]);
        assert_eq!(row(5), [10, 20, 30, 255]);
        assert_eq!(row(6), [0, 0, 0, 0]);
    }

    #[test]
    fn grey_and_small_images_are_handled() {
        let root = temp_dir("icons");
        let grey = root.join("grey.png");
        write_png(&grey, 2, 2, png::ColorType::Grayscale, &[90; 4]);
        assert_eq!(&load(&grey, 4).unwrap().data[..4], [90, 90, 90, 255]);

        let grey_alpha = root.join("ga.png");
        write_png(&grey_alpha, 1, 1, png::ColorType::GrayscaleAlpha, &[255, 0]);
        assert_eq!(load(&grey_alpha, 2).unwrap().data, [0; 16]);
    }

    #[test]
    fn oversized_and_irregular_files_are_refused_unread() {
        let root = temp_dir("icons");
        // Far too big to be an icon, though each would decode.
        let big_svg = root.join("big.svg");
        let padding = " ".repeat(MAX_SVG_BYTES as usize);
        fs::write(
            &big_svg,
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\">{padding}</svg>"
            ),
        )
        .unwrap();
        assert_eq!(load(&big_svg, 40), None);
        let big_png = root.join("big.png");
        fs::write(&big_png, vec![0u8; MAX_PNG_BYTES as usize + 1]).unwrap();
        assert_eq!(read_icon(&big_png), None);

        // Not regular files. Reading /dev/zero would never end.
        std::os::unix::fs::symlink("/dev/zero", root.join("zero.png")).unwrap();
        assert_eq!(load(&root.join("zero.png"), 40), None);
        fs::create_dir(root.join("dir.png")).unwrap();
        assert_eq!(load(&root.join("dir.png"), 40), None);

        // An absurd output size is refused as well.
        let small = root.join("small.png");
        solid(&small, 4, [1, 2, 3, 255]);
        assert_eq!(load(&small, MAX_PIXELS + 1), None);
        assert!(load(&small, 40).is_some());
    }

    #[cfg(feature = "svg")]
    #[test]
    fn an_svg_cannot_pull_in_other_files() {
        let root = temp_dir("icons");
        let inner = root.join("inner.png");
        solid(&inner, 8, [255, 0, 0, 255]);
        for href in [inner.to_str().unwrap(), "/dev/zero"] {
            let path = root.join("outer.svg");
            fs::write(
                &path,
                format!(
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" \
                     xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 8 8\">\
                     <image width=\"8\" height=\"8\" xlink:href=\"{href}\"/></svg>"
                ),
            )
            .unwrap();
            // It renders, promptly, with nothing where the image would be.
            let pixels = load(&path, 8).unwrap();
            assert!(pixels.data.iter().all(|&byte| byte == 0));
        }
    }

    #[test]
    fn requests_that_are_not_for_the_decoder_are_passed_over() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(run_decoder(&args(&[])).is_none());
        assert!(run_decoder(&args(&["--dump"])).is_none());
        assert!(run_decoder(&args(&[DECODE_FLAG, "only-a-path"])).is_none());
    }

    #[test]
    fn a_decoder_that_cannot_run_or_misbehaves_gives_nothing() {
        let root = temp_dir("icons");
        let icon = root.join("icon.png");
        solid(&icon, 4, [1, 2, 3, 255]);
        let quick = Duration::from_millis(300);

        let missing = Decoder::new(root.join("no-such-program"), quick);
        assert_eq!(missing.decode(&icon, 4), None);

        let script = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = root.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        // Too little, too much, a failure, and one that never answers.
        let short = Decoder::new(script("short", "printf abc"), quick);
        assert_eq!(short.decode(&icon, 4), None);
        let long = Decoder::new(script("long", "head -c 4096 /dev/zero"), quick);
        assert_eq!(long.decode(&icon, 4), None);
        let failing = Decoder::new(script("failing", "head -c 64 /dev/zero; exit 1"), quick);
        assert_eq!(failing.decode(&icon, 4), None);
        let stuck = Decoder::new(script("stuck", "exec sleep 30"), quick);
        let started = Instant::now();
        assert_eq!(stuck.decode(&icon, 4), None);
        assert!(started.elapsed() < Duration::from_secs(5));

        // And one that behaves.
        let good = Decoder::new(script("good", "head -c 64 /dev/zero"), quick);
        assert_eq!(good.decode(&icon, 4).unwrap().data, [0u8; 64]);
        assert_eq!(good.decode(&icon, 0), None);
    }

    #[test]
    fn broken_files_give_nothing() {
        let root = temp_dir("icons");
        let path = root.join("broken.png");
        fs::write(&path, b"not a png").unwrap();
        assert_eq!(load(&path, 40), None);
        assert_eq!(load(&root.join("absent.png"), 40), None);
        let text = root.join("readme.txt");
        fs::write(&text, b"hello").unwrap();
        assert_eq!(load(&text, 40), None);
    }

    #[cfg(feature = "svg")]
    #[test]
    fn an_svg_is_drawn_at_the_requested_size() {
        let root = temp_dir("icons");
        let path = root.join("hicolor/scalable/apps/dot.svg");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\">\
             <rect width=\"10\" height=\"10\" fill=\"red\"/></svg>",
        )
        .unwrap();
        let dirs = IconDirs {
            themes: vec![root.clone()],
            pixmaps: Vec::new(),
        };
        let pixels = load_named(&dirs, "dot", 40).unwrap();
        assert_eq!(pixels.size, 40);
        let middle = (20 * 40 + 20) * 4;
        assert_eq!(&pixels.data[middle..middle + 4], [255, 0, 0, 255]);
    }

    #[test]
    fn the_fallback_letter_is_the_first_letter_or_digit() {
        assert_eq!(initial("foot"), "F");
        assert_eq!(initial("  (beta) editor"), "B");
        assert_eq!(initial("7zip"), "7");
        assert_eq!(initial("élan"), "É");
        assert_eq!(initial("---"), "?");
        assert_eq!(initial(""), "?");
    }
}
