//! Finding and decoding the icon files tray items name.
//!
//! An item says which icon it wants with `IconName` and, optionally, a
//! directory of its own in `IconThemePath`. Both come from an arbitrary
//! app, so the directory is only used if it really is one, only image
//! files are picked out of it, and no file is decoded in the bar itself:
//! [`Decoder`] runs this program again as a short-lived child that decodes
//! one file under tight limits and writes the pixels to its standard
//! output.
//!
//! The lookup and the decoder follow sengi-dock's `icons.rs`, cut down to
//! what the tray needs. They belong in a shared crate.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use super::pixmap::{self, Pixels, MAX_ICON};

/// Largest source image we are willing to decode, in pixels per side.
const MAX_SOURCE: u32 = 2048;

/// Largest icon files we are willing to read.
const MAX_PNG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SVG_BYTES: u64 = 1024 * 1024;

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

/// Where icons are looked for when the item's own directory has none.
#[derive(Clone, Debug, Default)]
pub struct IconDirs {
    /// Directories that hold themes, e.g. `/usr/share/icons`.
    pub themes: Vec<PathBuf>,
    /// Flat directories of loose icons, e.g. `/usr/share/pixmaps`.
    pub pixmaps: Vec<PathBuf>,
}

impl IconDirs {
    /// The standard places, from the environment.
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").ok().filter(|home| !home.is_empty());
        let mut data = Vec::new();
        match std::env::var("XDG_DATA_HOME") {
            Ok(dir) if dir.starts_with('/') => data.push(PathBuf::from(dir)),
            _ => data.extend(
                home.as_ref()
                    .map(|home| Path::new(home).join(".local/share")),
            ),
        }
        let system = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
        data.extend(
            system
                .split(':')
                .filter(|dir| dir.starts_with('/'))
                .map(PathBuf::from),
        );

        let mut themes: Vec<PathBuf> = home
            .iter()
            .map(|home| Path::new(home).join(".icons"))
            .collect();
        themes.extend(data.iter().map(|dir| dir.join("icons")));
        Self {
            themes,
            pixmaps: vec![PathBuf::from("/usr/share/pixmaps")],
        }
    }
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

/// The item's own icon directory, if it names a real one. It is resolved
/// first, so a link cannot lead somewhere other than where it appears to,
/// and the kernel's file systems are never searched.
pub fn theme_path(path: &str) -> Option<PathBuf> {
    if !path.starts_with('/') || path.len() > MAX_PATH {
        return None;
    }
    let real = fs::canonicalize(path).ok()?;
    let special = ["/proc", "/sys", "/dev"]
        .iter()
        .any(|root| real.starts_with(root));
    (real != Path::new("/") && !special && real.is_dir()).then_some(real)
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

/// The best-sized file for `name` in a theme laid out as
/// `<size>/<context>/<name>.<ext>`. Tray icons turn up under `apps`,
/// `status` and others, so every context is looked in.
fn themed(theme: &Path, name: &str, pixels: u32) -> Option<PathBuf> {
    let mut best: Option<((u8, u32), PathBuf)> = None;
    for sub in entries(theme) {
        let Some(size) = sub.file_name().to_str().and_then(dir_size) else {
            continue;
        };
        let score = rank(size, pixels);
        if best.as_ref().is_some_and(|(held, _)| *held <= score) {
            continue;
        }
        let found = entries(&sub.path()).find_map(|context| {
            let path = loose(&context.path(), name)?;
            // A scalable directory is for vectors only.
            (size != DirSize::Scalable || path.extension().is_some_and(|e| e == "svg"))
                .then_some(path)
        });
        if let Some(path) = found {
            best = Some((score, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Finds the file for an icon name, to be drawn `pixels` wide: in the
/// item's own directory, then the `hicolor` theme, then the loose pixmaps.
pub fn find(dirs: &IconDirs, own: Option<&Path>, icon: &str, pixels: u32) -> Option<PathBuf> {
    // Some apps give a whole file name instead of an icon name.
    if icon.starts_with('/') {
        let path = PathBuf::from(icon);
        return (icon.len() <= MAX_PATH && supported(&path) && path.is_file()).then_some(path);
    }
    let name = [".png", ".svg", ".xpm"]
        .iter()
        .find_map(|ext| icon.strip_suffix(ext))
        .unwrap_or(icon);
    if !plain_name(name) {
        return None;
    }

    if let Some(own) = own {
        let found = loose(own, name).or_else(|| themed(&own.join("hicolor"), name, pixels));
        if found.is_some() {
            return found;
        }
    }
    dirs.themes
        .iter()
        .find_map(|themes| themed(&themes.join("hicolor"), name, pixels))
        .or_else(|| dirs.pixmaps.iter().find_map(|dir| loose(dir, name)))
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
/// process. The bar goes through [`Decoder`] instead.
pub fn load(path: &Path, pixels: u32) -> Option<Pixels> {
    if pixels == 0 || pixels > MAX_ICON {
        return None;
    }
    let bytes = read_icon(path)?;
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => {
            let (width, height, rgba) = decode_png(&bytes)?;
            Some(pixmap::fit(&rgba, width, height, pixels))
        }
        #[cfg(feature = "svg")]
        Some("svg") => render_svg(&bytes, pixels),
        _ => None,
    }
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
    /// so it is called from the tray's worker, never the UI thread. None
    /// means the file is not a usable icon, whatever the reason.
    pub fn decode(&self, path: &Path, pixels: u32) -> Option<Pixels> {
        if pixels == 0 || pixels > MAX_ICON {
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
            "sengi-bar-{tag}-{}-{}",
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

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

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
        assert_eq!(super::find(&dirs(&root), None, "only-there", 16), None);
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
        assert_eq!(
            find(&dirs, None, own.join("zero.png").to_str().unwrap(), 16),
            None
        );
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
        assert_eq!(load(&small, MAX_ICON + 1), None);
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
        assert!(run_decoder(&args(&[DECODE_FLAG, "only-a-path"])).is_none());
    }

    #[test]
    fn a_decoder_that_cannot_run_or_misbehaves_gives_nothing() {
        let root = temp_dir("icons");
        let icon = root.join("icon.png");
        write_png(&icon, 4, [1, 2, 3, 255]);
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
        let short = Decoder::new(script("short", "printf abc"), quick);
        assert_eq!(short.decode(&icon, 4), None);
        let long = Decoder::new(script("long", "head -c 4096 /dev/zero"), quick);
        assert_eq!(long.decode(&icon, 4), None);
        let stuck = Decoder::new(script("stuck", "exec sleep 30"), quick);
        let started = Instant::now();
        assert_eq!(stuck.decode(&icon, 4), None);
        assert!(started.elapsed() < Duration::from_secs(5));

        let good = Decoder::new(script("good", "head -c 64 /dev/zero"), quick);
        assert_eq!(good.decode(&icon, 4).unwrap().data, [0u8; 64]);
    }
}
