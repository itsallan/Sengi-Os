//! Finding and decoding icons: the ones applications install and the ones
//! tray items name.
//!
//! Lookup follows the icon theme rules for the `hicolor` theme, which is
//! where apps install their own icons, then `/usr/share/pixmaps`. Icons are
//! decoded straight to the size they are drawn at, so a 256 px PNG costs
//! its user a 40 px bitmap and nothing more.
//!
//! Icon files come from whatever the user installs, and a file may be built
//! to break a decoder. So the program that draws the icon never decodes
//! one. [`Decoder`] starts that same program again as a short-lived child
//! that decodes one icon under tight limits and a timeout, and writes the
//! pixels to its standard output. A file that crashes or stalls the
//! decoder costs that child, and the icon is simply missing.
//!
//! Who looks the file up differs. A tray item can name any path it likes,
//! and one may lead to a network or FUSE mount that never answers, so for
//! [`Decoder::decode`] the child does the lookup as well and the caller
//! never touches the path. The dock calls [`find`] itself, on a worker
//! thread, for names out of desktop files, and hands the child the file
//! with [`Decoder::decode_file`]: a path that never answers there stalls
//! that worker, not the dock's window.
//!
//! The child is the program itself rather than a helper of its own, so
//! nothing new has to be installed: a program that uses [`Decoder`] passes
//! its arguments to [`run_decoder`] first thing in `main`.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use crate::env::Env;

/// Largest source image we are willing to decode, in pixels per side.
const MAX_SOURCE: u32 = 2048;

/// Largest icon files we are willing to read. Real icons are far smaller.
const MAX_PNG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SVG_BYTES: u64 = 1024 * 1024;

/// Largest icon that will be produced, in pixels per side.
pub const MAX_PIXELS: u32 = 256;

/// How long a decode may take before it is given up on.
pub const DECODE_TIMEOUT: Duration = Duration::from_secs(1);

/// First argument of the child that decodes one icon.
pub const DECODE_FLAG: &str = "--decode-icon";

/// Address space the decoding child may use.
const DECODE_MEMORY: u64 = 512 * 1024 * 1024;

/// Longest icon name or theme path we will look up.
const MAX_NAME: usize = 128;
const MAX_PATH: usize = 1024;

/// Directory entries looked at in any one directory. Theme directories
/// hold a few dozen sizes and contexts.
const MAX_ENTRIES: usize = 128;

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

    /// The standard places for `env`: `~/.icons`, then `icons` under each
    /// of [`Env::data_roots`], Flatpak's export directories included.
    pub fn for_env(env: &Env) -> Self {
        Self::from_data_dirs(env.home.to_str(), &env.data_roots())
    }

    /// The standard places, from this process's environment.
    pub fn from_env() -> Self {
        Self::for_env(&Env::from_process())
    }
}

/// Which of a theme's contexts (`apps`, `status`, `devices` and so on) an
/// icon is looked for in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contexts {
    /// `apps` only: where an application's own icon is installed.
    Apps,
    /// Every context. Tray icons turn up under `apps`, `status` and others.
    Any,
}

fn extensions() -> &'static [&'static str] {
    if cfg!(feature = "svg") {
        &["png", "svg"]
    } else {
        &["png"]
    }
}

fn supported(path: &Path) -> bool {
    let extension = path.extension().and_then(|e| e.to_str());
    extension.is_some_and(|e| extensions().contains(&e))
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

/// A plain icon name: no directories, nothing hidden, not too long.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0'])
}

/// Where an item-supplied path really leads, unless that is into one of
/// the kernel's file systems. Resolving first means a link cannot lead
/// somewhere other than where it appears to.
fn resolve(path: &str) -> Option<PathBuf> {
    if !path.starts_with('/') || path.len() > MAX_PATH {
        return None;
    }
    let real = fs::canonicalize(path).ok()?;
    let special = ["/proc", "/sys", "/dev"]
        .iter()
        .any(|root| real.starts_with(root));
    (!special).then_some(real)
}

/// The item's own icon directory, if it names a real one.
pub fn theme_path(path: &str) -> Option<PathBuf> {
    resolve(path).filter(|real| real != Path::new("/") && real.is_dir())
}

fn entries(dir: &Path) -> impl Iterator<Item = fs::DirEntry> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .take(MAX_ENTRIES)
}

/// An image file by that name, straight in `dir`.
fn loose(dir: &Path, name: &str) -> Option<PathBuf> {
    extensions()
        .iter()
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .find(|path| path.is_file())
}

/// An image file by that name in one context directory of a theme. A
/// scalable directory is for vectors only.
fn in_context(dir: &Path, name: &str, size: DirSize) -> Option<PathBuf> {
    extensions()
        .iter()
        .filter(|ext| size != DirSize::Scalable || **ext == "svg")
        .map(|ext| dir.join(format!("{name}.{ext}")))
        .find(|path| path.is_file())
}

/// The best-sized file for `name` in a theme laid out as
/// `<size>/<context>/<name>.<ext>`.
fn themed(theme: &Path, name: &str, pixels: u32, contexts: Contexts) -> Option<PathBuf> {
    let mut best: Option<((u8, u32), PathBuf)> = None;
    for sub in entries(theme) {
        let Some(size) = sub.file_name().to_str().and_then(dir_size) else {
            continue;
        };
        let score = rank(size, pixels);
        if best.as_ref().is_some_and(|(held, _)| *held <= score) {
            continue;
        }
        let found = match contexts {
            Contexts::Apps => in_context(&sub.path().join("apps"), name, size),
            Contexts::Any => {
                entries(&sub.path()).find_map(|context| in_context(&context.path(), name, size))
            }
        };
        if let Some(path) = found {
            best = Some((score, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Finds the file for an icon name, to be drawn `pixels` wide: in `own`
/// (a tray item's own icon directory, already through [`theme_path`]),
/// then the `hicolor` theme of each theme directory, then the loose
/// pixmaps. An icon in the user's theme directory beats a better-sized
/// system one.
///
/// `icon` may also be an absolute path, which some apps give instead of a
/// name. Anything else with a directory in it, hidden names and names
/// over 128 bytes are refused.
pub fn find(
    dirs: &IconDirs,
    own: Option<&Path>,
    icon: &str,
    pixels: u32,
    contexts: Contexts,
) -> Option<PathBuf> {
    if icon.starts_with('/') {
        // The extension is judged on the name as given: an icon may be a
        // link to a file called something else. What the file really is
        // is settled from its bytes when it is read.
        if !supported(Path::new(icon)) {
            return None;
        }
        // The path is returned as it was given. Whoever reads it resolves
        // it again, and has the name to judge the extension by.
        return resolve(icon)
            .filter(|path| path.is_file())
            .map(|_| PathBuf::from(icon));
    }
    // Some entries wrongly include the extension in the name.
    let name = [".png", ".svg", ".xpm"]
        .iter()
        .find_map(|ext| icon.strip_suffix(ext))
        .unwrap_or(icon);
    if !plain_name(name) {
        return None;
    }

    if let Some(own) = own {
        let found =
            loose(own, name).or_else(|| themed(&own.join("hicolor"), name, pixels, contexts));
        if found.is_some() {
            return found;
        }
    }
    dirs.themes
        .iter()
        .find_map(|themes| themed(&themes.join("hicolor"), name, pixels, contexts))
        .or_else(|| dirs.pixmaps.iter().find_map(|dir| loose(dir, name)))
}

/// A decoded icon: `size` by `size` pixels of premultiplied RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    /// Pixels per side.
    pub size: u32,
    /// `size * size * 4` bytes, row by row.
    pub data: Vec<u8>,
}

/// How every PNG file begins.
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Reads an icon file, refusing anything that is not a regular file of a
/// believable size. A FIFO or `/dev/zero` under an icon's name would
/// otherwise be read for ever.
fn read_icon(path: &Path) -> Option<Vec<u8>> {
    let limit = MAX_PNG_BYTES.max(MAX_SVG_BYTES);
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
/// process. Programs go through [`Decoder`] instead; this is what its
/// child runs.
///
/// What kind of image it is comes from the file's first bytes, not its
/// name: a PNG if it starts like one, otherwise (with the `svg` feature)
/// an SVG if it parses as one.
pub fn load(path: &Path, pixels: u32) -> Option<Pixels> {
    if pixels == 0 || pixels > MAX_PIXELS {
        return None;
    }
    let bytes = read_icon(path)?;
    if bytes.starts_with(PNG_MAGIC) {
        let (width, height, rgba) = decode_png(&bytes)?;
        return Some(fit(&rgba, width, height, pixels));
    }
    #[cfg(feature = "svg")]
    if bytes.len() as u64 <= MAX_SVG_BYTES {
        return render_svg(&bytes, pixels);
    }
    None
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
/// `rgba` must hold `width * height` pixels.
pub fn fit(rgba: &[u8], width: u32, height: u32, size: u32) -> Pixels {
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

/// The child's side: finds and decodes one icon and writes its pixels to
/// standard output. The arguments are [`DECODE_FLAG`], the size, a tray
/// item's theme path (which may be empty) and the icon name or path.
/// Returns None when the arguments are not a decode request, so `main` can
/// call this first and carry on as usual otherwise.
pub fn run_decoder(args: &[String]) -> Option<ExitCode> {
    let [flag, pixels, theme, name] = args else {
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
    let decoded = pixels.parse().ok().and_then(|pixels| {
        let own = theme_path(theme);
        let dirs = IconDirs::from_env();
        let path = find(&dirs, own.as_deref(), name, pixels, Contexts::Any)?;
        load(&path, pixels)
    });
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

/// The running program, to start again as a helper. Not `current_exe`:
/// after a package upgrade that names a file that is gone, or worse, the
/// new version, which may not speak to this one. This is the binary we
/// are actually running, whatever has happened to its name.
pub const THIS_PROGRAM: &str = "/proc/self/exe";

/// Finds and decodes icons in a child process, so that no path and no
/// icon file can crash or stall the program that asked.
#[derive(Clone, Debug)]
pub struct Decoder {
    program: PathBuf,
}

impl Default for Decoder {
    /// A decoder that runs the program we are part of.
    fn default() -> Self {
        Self::new(PathBuf::from(THIS_PROGRAM))
    }
}

impl Decoder {
    /// A decoder that runs `program`, which must answer [`DECODE_FLAG`] by
    /// calling [`run_decoder`].
    pub fn new(program: PathBuf) -> Self {
        Self { program }
    }

    /// Finds the icon `name` and decodes it to `pixels` per side. The
    /// lookup happens in the child too, in the standard places of the
    /// environment and every context, after `theme_path` if that is not
    /// empty. Blocks for at most `timeout`, so call it away from the UI
    /// thread. None means there is no usable icon by that name, whatever
    /// the reason; a `pixels` of zero or above [`MAX_PIXELS`] is one such
    /// reason.
    pub fn decode(
        &self,
        name: &str,
        theme_path: &str,
        pixels: u32,
        timeout: Duration,
    ) -> Option<Pixels> {
        // An argument cannot hold a NUL, and nothing real is this long.
        let sane = |text: &str, limit: usize| text.len() <= limit && !text.contains('\0');
        if pixels == 0 || pixels > MAX_PIXELS || name.is_empty() {
            return None;
        }
        if !sane(name, MAX_PATH) || !sane(theme_path, MAX_PATH) {
            return None;
        }
        let expected = (pixels * pixels * 4) as usize;
        let mut child = Command::new(&self.program)
            .arg(DECODE_FLAG)
            .arg(pixels.to_string())
            .arg(theme_path)
            .arg(name)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut output = child.stdout.take()?;

        // Someone has to watch the clock while the pipe is read.
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

    /// Decodes the file at `path`, which the caller found itself with
    /// [`find`], to `pixels` per side. Otherwise as [`Decoder::decode`].
    pub fn decode_file(&self, path: &Path, pixels: u32, timeout: Duration) -> Option<Pixels> {
        self.decode(path.to_str()?, "", pixels, timeout)
    }
}

#[cfg(test)]
pub mod testing {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A fresh directory under the system's temporary one.
    pub fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sengi-apps-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn write_png(path: &Path, side: u32, rgba: [u8; 4]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let data: Vec<u8> = std::iter::repeat_n(rgba, (side * side) as usize)
            .flatten()
            .collect();
        let mut encoder = png::Encoder::new(fs::File::create(path).unwrap(), side, side);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&data)
            .unwrap();
    }

    pub fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"").unwrap();
    }
}

/// The tests that came with the bar's tray, which looks in every context
/// and has the child do the lookup.
#[cfg(test)]
mod tray_tests {
    use super::testing::*;
    use super::*;

    fn find(dirs: &IconDirs, own: Option<&Path>, icon: &str, pixels: u32) -> Option<PathBuf> {
        super::find(dirs, own, icon, pixels, Contexts::Any)
    }

    fn dirs(root: &Path) -> IconDirs {
        IconDirs {
            themes: vec![root.join("home/icons"), root.join("usr/icons")],
            pixmaps: vec![root.join("pixmaps")],
        }
    }

    #[test]
    fn theme_directory_names_give_sizes() {
        assert_eq!(dir_size("48x48"), Some(DirSize::Fixed(48)));
        assert_eq!(dir_size("64x64@2"), Some(DirSize::Fixed(128)));
        assert_eq!(dir_size("scalable"), Some(DirSize::Scalable));
        assert_eq!(dir_size("symbolic"), None);
        assert_eq!(dir_size("48x32"), None);
        assert_eq!(dir_size("0x0"), None);
    }

    #[test]
    fn hicolor_is_searched_in_every_context_for_the_best_size() {
        let root = temp_dir("icons");
        let base = root.join("usr/icons/hicolor");
        touch(&base.join("16x16/status/nm-signal.png"));
        touch(&base.join("22x22/apps/nm-signal.png"));
        touch(&base.join("48x48/apps/nm-signal.png"));
        let find = |pixels| find(&dirs(&root), None, "nm-signal", pixels);
        assert_eq!(find(16), Some(base.join("16x16/status/nm-signal.png")));
        assert_eq!(find(32), Some(base.join("48x48/apps/nm-signal.png")));
        assert_eq!(find(20), Some(base.join("22x22/apps/nm-signal.png")));
        assert_eq!(find(96), Some(base.join("48x48/apps/nm-signal.png")));
        // Other themes are not consulted.
        touch(&root.join("usr/icons/Other/16x16/apps/only-there.png"));
        assert_eq!(
            super::find(&dirs(&root), None, "only-there", 16, Contexts::Any),
            None
        );
    }

    #[test]
    fn the_items_own_directory_comes_first() {
        let root = temp_dir("icons");
        touch(&root.join("usr/icons/hicolor/16x16/apps/chat.png"));
        touch(&root.join("pixmaps/chat.png"));
        let own = root.join("app/icons");
        touch(&own.join("chat.png"));
        touch(&own.join("hicolor/32x32/status/chat-unread.png"));
        let own = theme_path(own.to_str().unwrap()).unwrap();

        let dirs = dirs(&root);
        assert_eq!(
            find(&dirs, Some(&own), "chat", 16),
            Some(own.join("chat.png"))
        );
        assert_eq!(
            find(&dirs, Some(&own), "chat-unread", 16),
            Some(own.join("hicolor/32x32/status/chat-unread.png"))
        );
        // Without it, the system theme, then the loose pixmaps.
        assert_eq!(
            find(&dirs, None, "chat", 16),
            Some(root.join("usr/icons/hicolor/16x16/apps/chat.png"))
        );
        touch(&root.join("pixmaps/old.png"));
        assert_eq!(
            find(&dirs, Some(&own), "old", 16),
            Some(root.join("pixmaps/old.png"))
        );
        assert_eq!(find(&dirs, Some(&own), "missing", 16), None);
    }

    #[test]
    fn only_real_directories_are_accepted_as_a_theme_path() {
        let root = temp_dir("icons");
        let real = root.join("real");
        fs::create_dir_all(&real).unwrap();
        assert!(theme_path(real.to_str().unwrap()).is_some());

        assert_eq!(theme_path(""), None);
        assert_eq!(theme_path("relative/dir"), None);
        assert_eq!(theme_path("/"), None);
        assert_eq!(theme_path("/nonexistent/sengi-bar"), None);
        assert_eq!(theme_path("/proc/self"), None);
        assert_eq!(theme_path("/sys/class"), None);
        assert_eq!(theme_path("/dev"), None);
        assert_eq!(theme_path(&format!("/{}", "a".repeat(MAX_PATH))), None);

        // A file is not a directory, and a link is judged by its target.
        let file = root.join("file");
        fs::write(&file, b"").unwrap();
        assert_eq!(theme_path(file.to_str().unwrap()), None);
        let link = root.join("link");
        std::os::unix::fs::symlink("/proc/self", &link).unwrap();
        assert_eq!(theme_path(link.to_str().unwrap()), None);
        let honest = root.join("honest");
        std::os::unix::fs::symlink(&real, &honest).unwrap();
        assert_eq!(
            theme_path(honest.to_str().unwrap()),
            Some(fs::canonicalize(&real).unwrap())
        );
    }

    #[test]
    fn icon_names_cannot_climb_out_of_the_directory() {
        let root = temp_dir("icons");
        let own = root.join("app");
        touch(&own.join("icon.png"));
        touch(&root.join("secret.png"));
        touch(&own.join(".hidden.png"));
        let dirs = dirs(&root);
        for name in ["../secret", "sub/../../secret", ".hidden", "..", "a\\b", ""] {
            assert_eq!(find(&dirs, Some(&own), name, 16), None, "{name:?}");
        }
        let long = "n".repeat(MAX_NAME + 1);
        assert_eq!(find(&dirs, Some(&own), &long, 16), None);
        assert!(find(&dirs, Some(&own), "icon", 16).is_some());
        // An extension in the name is tolerated.
        assert!(find(&dirs, Some(&own), "icon.png", 16).is_some());
    }

    #[test]
    fn only_image_files_are_found() {
        let root = temp_dir("icons");
        let own = root.join("app");
        touch(&own.join("notes.txt"));
        touch(&own.join("script"));
        fs::create_dir_all(own.join("folder.png")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", own.join("zero.png")).unwrap();
        let dirs = dirs(&root);
        for name in ["notes", "notes.txt", "script", "folder", "zero"] {
            assert_eq!(find(&dirs, Some(&own), name, 16), None, "{name:?}");
        }

        // The same goes for a whole path given as the name.
        let image = root.join("opt/app/icon.png");
        touch(&image);
        assert_eq!(
            find(&dirs, None, image.to_str().unwrap(), 16),
            Some(image.clone())
        );
        assert_eq!(find(&dirs, None, "/etc/passwd", 16), None);
        assert_eq!(find(&dirs, None, "/dev/zero", 16), None);
        // A whole path gets the same exclusions as a theme directory, and
        // is judged by where it really leads.
        assert_eq!(find(&dirs, None, "/proc/self/exe.png", 16), None);
        assert_eq!(find(&dirs, None, "/sys/class/x.png", 16), None);
        // A link through /proc to a real file leads to that file, which
        // is read and found to be no image.
        let link = root.join("via-link.png");
        std::os::unix::fs::symlink("/proc/self/exe", &link).unwrap();
        let found = find(&dirs, None, link.to_str().unwrap(), 16);
        assert_eq!(found.and_then(|path| load(&path, 16)), None);
        assert_eq!(
            find(&dirs, None, own.join("zero.png").to_str().unwrap(), 16),
            None
        );
    }

    #[test]
    fn an_icon_may_be_a_link_to_a_file_called_something_else() {
        let root = temp_dir("linked");
        let dirs = dirs(&root);
        let target = root.join("store/3f2a.bin");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        write_png(&target, 32, [10, 20, 30, 255]);
        // Named by path.
        let link = root.join("app.png");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let found = find(&dirs, None, link.to_str().unwrap(), 16).unwrap();
        assert_eq!(found, link);
        // Found again from what was found, as the decoder child does.
        assert_eq!(
            find(&dirs, None, found.to_str().unwrap(), 16),
            Some(link.clone())
        );
        assert_eq!(&load(&found, 16).unwrap().data[..4], [10, 20, 30, 255]);
        // Named by icon name, in a theme.
        let themed = root.join("home/icons/hicolor/32x32/apps/linked.png");
        fs::create_dir_all(themed.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &themed).unwrap();
        let found = find(&dirs, None, "linked", 16).unwrap();
        assert!(load(&found, 16).is_some());
        // The name still has to be an image's: the target alone is not
        // offered under any name at all.
        assert_eq!(find(&dirs, None, target.to_str().unwrap(), 16), None);
        // And a link does not make something else an image.
        let fake = root.join("fake.png");
        std::os::unix::fs::symlink("/etc/passwd", &fake).unwrap();
        let found = find(&dirs, None, fake.to_str().unwrap(), 16);
        assert_eq!(found.and_then(|path| load(&path, 16)), None);
    }

    #[test]
    fn the_standard_places_include_flatpak_exports() {
        let env =
            Env::from_vars(|name| (name == "HOME").then(|| std::ffi::OsString::from("/home/a")));
        let dirs = IconDirs::for_env(&env);
        assert_eq!(dirs.themes[0], Path::new("/home/a/.icons"));
        assert!(dirs
            .themes
            .contains(&PathBuf::from("/var/lib/flatpak/exports/share/icons")));
        assert!(dirs.themes.contains(&PathBuf::from(
            "/home/a/.local/share/flatpak/exports/share/icons"
        )));
    }

    #[test]
    fn a_png_is_decoded_to_the_drawn_size() {
        let root = temp_dir("icons");
        let path = root.join("big.png");
        write_png(&path, 256, [200, 100, 50, 255]);
        let pixels = load(&path, 16).unwrap();
        assert_eq!(pixels.size, 16);
        assert_eq!(&pixels.data[..4], [200, 100, 50, 255]);

        let glass = root.join("glass.png");
        write_png(&glass, 8, [255, 255, 255, 128]);
        assert_eq!(&load(&glass, 8).unwrap().data[..4], [128, 128, 128, 128]);
    }

    #[test]
    fn oversized_irregular_and_broken_files_give_nothing() {
        let root = temp_dir("icons");
        let big = root.join("big.png");
        fs::write(&big, vec![0u8; MAX_PNG_BYTES as usize + 1]).unwrap();
        assert_eq!(read_icon(&big), None);
        std::os::unix::fs::symlink("/dev/zero", root.join("zero.png")).unwrap();
        assert_eq!(load(&root.join("zero.png"), 16), None);
        let broken = root.join("broken.png");
        fs::write(&broken, b"not a png").unwrap();
        assert_eq!(load(&broken, 16), None);
        let text = root.join("readme.txt");
        fs::write(&text, b"hello").unwrap();
        assert_eq!(load(&text, 16), None);

        let small = root.join("small.png");
        write_png(&small, 4, [1, 2, 3, 255]);
        assert_eq!(load(&small, MAX_PIXELS + 1), None);
        assert_eq!(load(&small, 0), None);
        assert!(load(&small, 16).is_some());
    }

    #[cfg(feature = "svg")]
    #[test]
    fn an_svg_is_drawn_at_the_requested_size_and_pulls_in_nothing() {
        let root = temp_dir("icons");
        let path = root.join("dot.svg");
        fs::write(
            &path,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\">\
             <rect width=\"10\" height=\"10\" fill=\"red\"/></svg>",
        )
        .unwrap();
        let pixels = load(&path, 32).unwrap();
        let middle = (16 * 32 + 16) * 4;
        assert_eq!(&pixels.data[middle..middle + 4], [255, 0, 0, 255]);

        let outer = root.join("outer.svg");
        fs::write(
            &outer,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" \
             xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 8 8\">\
             <image width=\"8\" height=\"8\" xlink:href=\"/dev/zero\"/></svg>",
        )
        .unwrap();
        assert!(load(&outer, 8).unwrap().data.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn requests_that_are_not_for_the_decoder_are_passed_over() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(run_decoder(&args(&[])).is_none());
        assert!(run_decoder(&args(&["--menu", "eDP-1", "100"])).is_none());
        assert!(run_decoder(&args(&[DECODE_FLAG, "16", "only-two"])).is_none());
    }

    #[test]
    fn a_decoder_that_cannot_run_or_misbehaves_gives_nothing() {
        let root = temp_dir("icons");
        let quick = Duration::from_millis(300);
        let decode = |decoder: &Decoder| decoder.decode("icon", "", 4, quick);

        assert_eq!(decode(&Decoder::new(root.join("no-such-program"))), None);

        let script = |name: &str, body: &str| {
            use std::os::unix::fs::PermissionsExt;
            let path = root.join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            Decoder::new(path)
        };
        assert_eq!(decode(&script("short", "printf abc")), None);
        assert_eq!(decode(&script("long", "head -c 4096 /dev/zero")), None);
        // A lookup that hangs, as on a dead network mount.
        let started = Instant::now();
        assert_eq!(decode(&script("stuck", "exec sleep 30")), None);
        assert!(started.elapsed() < Duration::from_secs(5));

        let good = script("good", "head -c 64 /dev/zero");
        assert_eq!(decode(&good).unwrap().data, [0u8; 64]);
        // Requests that make no sense never start the child.
        assert_eq!(good.decode("", "", 4, quick), None);
        assert_eq!(good.decode("icon", "", 0, quick), None);
        assert_eq!(good.decode("icon", "", MAX_PIXELS + 1, quick), None);
        assert_eq!(good.decode("ic\0on", "", 4, quick), None);
    }

    #[test]
    fn the_child_is_told_the_size_the_theme_path_and_the_name() {
        let root = temp_dir("icons");
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("echo");
        let seen = root.join("seen");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s|' \"$@\" >{}\nhead -c 64 /dev/zero\n",
                seen.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let decoder = Decoder::new(path);
        assert!(decoder
            .decode("chat-unread", "/opt/chat/icons", 4, Duration::from_secs(2))
            .is_some());
        assert_eq!(
            fs::read_to_string(seen).unwrap(),
            format!("{DECODE_FLAG}|4|/opt/chat/icons|chat-unread|")
        );
    }
}

/// The tests that came with the dock, which looks in the `apps` context
/// only, finds the file itself and has the child decode it.
#[cfg(test)]
mod app_tests {
    use super::testing::temp_dir;
    use super::*;

    fn find(dirs: &IconDirs, icon: &str, pixels: u32) -> Option<PathBuf> {
        super::find(dirs, None, icon, pixels, Contexts::Apps)
    }

    #[cfg(feature = "svg")]
    fn load_named(dirs: &IconDirs, icon: &str, pixels: u32) -> Option<Pixels> {
        load(&find(dirs, icon, pixels)?, pixels)
    }

    struct Decoder(super::Decoder, Duration);

    impl Decoder {
        fn new(program: PathBuf, timeout: Duration) -> Self {
            Decoder(super::Decoder::new(program), timeout)
        }

        fn decode(&self, path: &Path, pixels: u32) -> Option<Pixels> {
            self.0.decode_file(path, pixels, self.1)
        }
    }

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
}
