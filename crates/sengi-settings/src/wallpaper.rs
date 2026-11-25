//! The Wallpaper page: which picture is behind the desktop. The setting is
//! `appearance.wallpaper`: `solid`, the plain desktop colour, or the full
//! path of a PNG or JPEG.
//!
//! The picture is drawn by swaybg, which the session starts from
//! `/usr/lib/sengi-os/wallpaper.sh`; that script asks `sengi-settings
//! --wallpaper` what to show. So there is one judge of whether a file is
//! usable, here, and the page and the session agree. swaybg keeps the
//! decoded picture for as long as it runs, so what it is given is limited
//! to [`MAX_PIXELS`]; a bigger one is refused with the reason.
//!
//! Previews are decoded here, and only PNG: the ones Sengi OS ships come
//! with a small thumbnail of their own, and a picture of the user's is
//! shrunk as it is read. A JPEG is shown by name and size, not drawn.

use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use slint::{ComponentHandle, Image, Rgb8Pixel, SharedPixelBuffer};

use crate::app::{self, model, App, Controller};
use crate::settings::{Settings, WALLPAPER_SOLID};
use crate::store::Value;
use crate::ui::{PictureRow, WallpaperData, WallpaperTile};

/// Where the pictures Sengi OS ships are, and their thumbnails under
/// `thumbs/`.
pub const BUILTIN_DIR: &str = "/usr/share/backgrounds/sengi";
/// The script that has the session show the setting.
pub const SCRIPT: &str = "/usr/lib/sengi-os/wallpaper.sh";
/// The most pixels swaybg is given. It holds the picture decoded, four
/// bytes a pixel, so this is 40 MB at the most.
pub const MAX_PIXELS: u64 = 10_000_000;
/// Files larger than this are not looked at.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Above this many pixels the page says what the picture costs.
pub const HEAVY_PIXELS: u64 = 2_500_000;
/// How many files of the Pictures folder are listed.
pub const MAX_LISTED: usize = 12;

pub const THUMB_WIDTH: u32 = 192;
pub const THUMB_HEIGHT: u32 = 108;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Png => "PNG",
            Kind::Jpeg => "JPEG",
        }
    }
}

/// A file that is a picture swaybg can be given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub path: PathBuf,
    pub kind: Kind,
    pub width: u32,
    pub height: u32,
}

impl Picture {
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// What it costs while it is the wallpaper, in megabytes.
    pub fn megabytes(&self) -> u64 {
        (self.pixels() * 4).div_ceil(1_000_000)
    }
}

/// Look at the file at `path` and say whether it can be a wallpaper: a
/// readable regular file, a PNG or JPEG by its first bytes and not by its
/// name, of a size that is not absurd.
pub fn inspect(path: &Path) -> Result<Picture, String> {
    let shown = path.display();
    let metadata = fs::metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => format!("{shown} is not there"),
        std::io::ErrorKind::PermissionDenied => format!("{shown} cannot be read"),
        _ => format!("{shown} cannot be opened"),
    })?;
    if !metadata.is_file() {
        return Err(format!("{shown} is not a file"));
    }
    if metadata.len() > MAX_BYTES {
        return Err(format!("{shown} is too big, over {} MB", MAX_BYTES >> 20));
    }
    let file = File::open(path).map_err(|_| format!("{shown} cannot be read"))?;
    let (kind, width, height) = sniff(BufReader::new(file))
        .ok_or_else(|| format!("{shown} is not a PNG or JPEG picture"))?;
    if width == 0 || height == 0 {
        return Err(format!("{shown} has no size"));
    }
    let picture = Picture {
        path: path.to_path_buf(),
        kind,
        width,
        height,
    };
    if picture.pixels() > MAX_PIXELS {
        return Err(format!(
            "{} is {width} by {height}, more than the {} megapixels the desktop can hold",
            path.display(),
            MAX_PIXELS / 1_000_000
        ));
    }
    Ok(picture)
}

/// The kind and size of a picture from its header.
fn sniff<R: Read + Seek>(mut reader: R) -> Option<(Kind, u32, u32)> {
    let mut head = [0u8; 8];
    reader.read_exact(&mut head).ok()?;
    if head == *b"\x89PNG\r\n\x1a\n" {
        // The first chunk is IHDR: length, name, width, height.
        let mut ihdr = [0u8; 16];
        reader.read_exact(&mut ihdr).ok()?;
        if &ihdr[4..8] != b"IHDR" {
            return None;
        }
        let word =
            |at: usize| u32::from_be_bytes([ihdr[at], ihdr[at + 1], ihdr[at + 2], ihdr[at + 3]]);
        return Some((Kind::Png, word(8), word(12)));
    }
    if head[..3] == [0xff, 0xd8, 0xff] {
        reader.seek(SeekFrom::Start(2)).ok()?;
        return jpeg_size(&mut reader).map(|(w, h)| (Kind::Jpeg, w, h));
    }
    None
}

/// Walk the segments of a JPEG to the first start-of-frame marker.
fn jpeg_size<R: Read + Seek>(reader: &mut R) -> Option<(u32, u32)> {
    // A header with more segments than this is not one we read on.
    for _ in 0..256 {
        let mut byte = [0u8; 1];
        // Skip to the next marker: 0xff, then a byte that is not 0xff or 0.
        loop {
            reader.read_exact(&mut byte).ok()?;
            if byte[0] == 0xff {
                break;
            }
        }
        let marker = loop {
            reader.read_exact(&mut byte).ok()?;
            if byte[0] != 0xff {
                break byte[0];
            }
        };
        match marker {
            0x00 | 0x01 | 0xd0..=0xd8 => continue,
            // End of image, or the data itself: no frame header came.
            0xd9 | 0xda => return None,
            _ => {}
        }
        let mut length = [0u8; 2];
        reader.read_exact(&mut length).ok()?;
        let length = u64::from(u16::from_be_bytes(length));
        if length < 2 {
            return None;
        }
        // SOF0 to SOF15, except DHT (c4), JPG (c8) and DAC (cc).
        if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let mut frame = [0u8; 5];
            reader.read_exact(&mut frame).ok()?;
            let height = u32::from(u16::from_be_bytes([frame[1], frame[2]]));
            let width = u32::from(u16::from_be_bytes([frame[3], frame[4]]));
            return Some((width, height));
        }
        reader.seek(SeekFrom::Current(length as i64 - 2)).ok()?;
    }
    None
}

/// What `appearance.wallpaper` comes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// The desktop colour, as chosen.
    Solid,
    Picture(Picture),
    /// A picture was chosen and cannot be shown: the desktop colour is,
    /// and this is why.
    Unusable {
        path: String,
        reason: String,
    },
}

pub fn resolve(setting: &str) -> Resolved {
    if setting == WALLPAPER_SOLID || setting.is_empty() {
        return Resolved::Solid;
    }
    match inspect(Path::new(setting)) {
        Ok(picture) => Resolved::Picture(picture),
        Err(reason) => Resolved::Unusable {
            path: setting.to_string(),
            reason,
        },
    }
}

/// The path to give swaybg, or None for the plain colour. For
/// `sengi-settings --wallpaper`; a reason goes to standard error.
pub fn for_session(settings: &Settings) -> Option<PathBuf> {
    match resolve(&settings.text("appearance", "wallpaper")) {
        Resolved::Picture(picture) => Some(picture.path),
        Resolved::Solid => None,
        Resolved::Unusable { reason, .. } => {
            eprintln!("sengi-settings: wallpaper: {reason}; using the desktop colour");
            None
        }
    }
}

/// Have the session show the setting as it is now. Blocks until the
/// script has run; call it off the UI thread.
pub fn refresh() -> bool {
    Command::new("sh")
        .arg(SCRIPT)
        .arg("apply")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// One of the pictures Sengi OS ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Builtin {
    pub path: PathBuf,
    pub thumb: PathBuf,
    pub title: String,
}

/// The name shown for `01-hills.png`: `Hills`.
pub fn title_of(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let name = match stem.split_once('-') {
        Some((number, rest)) if number.chars().all(|c| c.is_ascii_digit()) => rest,
        _ => stem,
    };
    let mut chars = name.replace(['-', '_'], " ").chars().collect::<Vec<_>>();
    if let Some(first) = chars.first_mut() {
        *first = first.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}

/// The shipped pictures in `dir`, in the order of their names.
pub fn builtins(dir: &Path) -> Vec<Builtin> {
    let mut files: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".png"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| Builtin {
            path: dir.join(&file),
            thumb: dir.join("thumbs").join(&file),
            title: title_of(&file),
        })
        .collect()
}

/// A picture in the Pictures folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub path: PathBuf,
    pub name: String,
    pub detail: String,
    pub usable: bool,
}

/// PNG and JPEG files directly in `dir`, by name, as far as
/// [`MAX_LISTED`]. Whether each is usable is told in the detail, which is
/// what the page shows beside it.
pub fn pictures(dir: &Path) -> Vec<Listed> {
    let mut files: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let lower = name.to_lowercase();
            let wanted = [".png", ".jpg", ".jpeg"]
                .iter()
                .any(|ext| lower.ends_with(ext));
            (wanted && !name.starts_with('.') && entry.path().is_file())
                .then(|| (name, entry.path()))
        })
        .collect();
    files.sort_by_key(|(name, _)| name.to_lowercase());
    files
        .into_iter()
        .take(MAX_LISTED)
        .map(|(name, path)| {
            let (detail, usable) = match inspect(&path) {
                Ok(picture) => (describe(&picture), true),
                Err(reason) => (
                    reason.replacen(&path.display().to_string(), "This file", 1),
                    false,
                ),
            };
            Listed {
                path,
                name,
                detail,
                usable,
            }
        })
        .collect()
}

/// `PNG, 1920 by 1080`.
pub fn describe(picture: &Picture) -> String {
    format!(
        "{}, {} by {}",
        picture.kind.name(),
        picture.width,
        picture.height
    )
}

/// A preview: RGB, `THUMB_WIDTH` by `THUMB_HEIGHT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

/// The PNG at `path` shrunk to cover a preview, each preview pixel the
/// mean of the box of the picture it stands for. None if it is not a PNG
/// this can read.
pub fn thumb_of(path: &Path) -> Option<Thumb> {
    let file = File::open(path).ok()?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = (reader.info().width as usize, reader.info().height as usize);
    if width == 0 || height == 0 || (width * height) as u64 > MAX_PIXELS {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    let channels = match frame.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Indexed => return None,
    };
    let (out_w, out_h) = (THUMB_WIDTH as usize, THUMB_HEIGHT as usize);
    // The part of the picture that fills the preview with nothing left
    // over: as swaybg's fill mode does.
    let scale = (width as f64 / out_w as f64).min(height as f64 / out_h as f64);
    let (box_w, box_h) = (out_w as f64 * scale, out_h as f64 * scale);
    let (left, top) = ((width as f64 - box_w) / 2.0, (height as f64 - box_h) / 2.0);
    let mut rgb = Vec::with_capacity(out_w * out_h * 3);
    for oy in 0..out_h {
        let y0 = (top + oy as f64 * scale) as usize;
        let y1 = ((top + (oy + 1) as f64 * scale).ceil() as usize).clamp(y0 + 1, height);
        for ox in 0..out_w {
            let x0 = (left + ox as f64 * scale) as usize;
            let x1 = ((left + (ox + 1) as f64 * scale).ceil() as usize).clamp(x0 + 1, width);
            let mut sum = [0u64; 3];
            for y in y0..y1.min(height) {
                for x in x0..x1.min(width) {
                    let at = (y * width + x) * channels;
                    let pixel = &buffer[at..at + channels];
                    let colour = if channels >= 3 {
                        [pixel[0], pixel[1], pixel[2]]
                    } else {
                        [pixel[0]; 3]
                    };
                    for (total, channel) in sum.iter_mut().zip(colour) {
                        *total += u64::from(channel);
                    }
                }
            }
            let count = ((y1.min(height) - y0) * (x1.min(width) - x0)).max(1) as u64;
            rgb.extend(sum.iter().map(|total| (total / count) as u8));
        }
    }
    Some(Thumb {
        width: THUMB_WIDTH,
        height: THUMB_HEIGHT,
        rgb,
    })
}

/// Everything the page shows, found off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    pub builtin: Vec<(Builtin, Option<Thumb>)>,
    pub listed: Vec<Listed>,
    pub current: Resolved,
    pub current_thumb: Option<Thumb>,
    /// Which of `builtin` is the current one.
    pub current_builtin: Option<usize>,
}

pub fn scan(setting: &str, builtin_dir: &Path, pictures_dir: Option<&Path>) -> Scan {
    let builtin: Vec<(Builtin, Option<Thumb>)> = builtins(builtin_dir)
        .into_iter()
        .map(|entry| {
            let thumb = thumb_of(&entry.thumb);
            (entry, thumb)
        })
        .collect();
    let current = resolve(setting);
    let current_builtin = match &current {
        Resolved::Picture(picture) => builtin.iter().position(|(b, _)| b.path == picture.path),
        _ => None,
    };
    let current_thumb = match (&current, current_builtin) {
        (_, Some(at)) => builtin[at].1.clone(),
        (Resolved::Picture(picture), None) if picture.kind == Kind::Png => thumb_of(&picture.path),
        _ => None,
    };
    Scan {
        builtin,
        listed: pictures_dir.map(pictures).unwrap_or_default(),
        current,
        current_thumb,
        current_builtin,
    }
}

fn image_of(thumb: &Thumb) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(thumb.width, thumb.height);
    buffer.make_mut_bytes().copy_from_slice(&thumb.rgb);
    Image::from_rgb8(buffer)
}

/// What the page says about the current setting.
pub struct Words {
    pub title: String,
    pub detail: String,
    pub notice: String,
}

pub fn words(scan: &Scan) -> Words {
    match &scan.current {
        Resolved::Solid => Words {
            title: "Solid colour".to_string(),
            detail: "The plain desktop colour. It uses the least memory.".to_string(),
            notice: String::new(),
        },
        Resolved::Picture(picture) => {
            let name = match scan.current_builtin {
                Some(at) => scan.builtin[at].0.title.clone(),
                None => picture
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            };
            let mut detail = describe(picture);
            if picture.pixels() > HEAVY_PIXELS {
                detail.push_str(&format!(
                    ". It takes about {} MB of memory while it is the wallpaper.",
                    picture.megabytes()
                ));
            }
            Words {
                title: name,
                detail,
                notice: String::new(),
            }
        }
        Resolved::Unusable { reason, .. } => Words {
            title: "Solid colour".to_string(),
            detail: "The plain desktop colour, until a picture is chosen.".to_string(),
            notice: format!("The chosen picture cannot be shown: {reason}."),
        },
    }
}

fn pictures_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| home.join("Pictures"))
}

pub struct WallpaperPage {
    builtin_dir: PathBuf,
    scan: std::cell::RefCell<Option<Scan>>,
    path_error: std::cell::RefCell<String>,
}

impl WallpaperPage {
    pub fn attach(app: &Rc<App>) -> Rc<WallpaperPage> {
        Self::attach_in(app, Path::new(BUILTIN_DIR))
    }

    pub fn attach_in(app: &Rc<App>, builtin_dir: &Path) -> Rc<WallpaperPage> {
        let page = Rc::new(WallpaperPage {
            builtin_dir: builtin_dir.to_path_buf(),
            scan: Default::default(),
            path_error: Default::default(),
        });
        let data = app.window.global::<WallpaperData>();
        let (weak_app, weak_page) = (Rc::downgrade(app), Rc::downgrade(&page));
        data.on_choose(move |path| {
            if let (Some(app), Some(page)) = (weak_app.upgrade(), weak_page.upgrade()) {
                page.choose(&app, path.as_str());
            }
        });
        page
    }

    /// Set the wallpaper to `value`, which is `solid` or a path. A path is
    /// looked at first, off the UI thread, and refused with a reason in
    /// the page if it will not do.
    fn choose(self: &Rc<Self>, app: &Rc<App>, value: &str) {
        if value == WALLPAPER_SOLID {
            self.commit(app, value);
            return;
        }
        let typed = value.trim().to_string();
        let checked = if let Some(rest) = typed.strip_prefix("~/") {
            std::env::var("HOME")
                .map(|home| format!("{home}/{rest}"))
                .unwrap_or(typed)
        } else {
            typed
        };
        std::thread::spawn(move || {
            let outcome = if !crate::settings::wallpaper_is_plain(&checked) {
                Err("Type the whole path of the picture, starting with /".to_string())
            } else {
                inspect(Path::new(&checked)).map(|picture| picture.path.display().to_string())
            };
            app::post(move |app| {
                if let Some(page) = app.page::<WallpaperPage>() {
                    match outcome {
                        Ok(path) => page.commit(app, &path),
                        Err(reason) => {
                            *page.path_error.borrow_mut() = reason;
                            page.render(app);
                        }
                    }
                }
            });
        });
    }

    fn commit(self: &Rc<Self>, app: &Rc<App>, value: &str) {
        self.path_error.borrow_mut().clear();
        if app.set("appearance", "wallpaper", Value::text(value)) {
            app.after_saved(|| {
                refresh();
            });
        }
        self.rescan(app);
    }

    /// Look again at everything, off the UI thread, then draw it.
    fn rescan(&self, app: &Rc<App>) {
        let setting = app.settings.borrow().text("appearance", "wallpaper");
        let (builtin_dir, pictures) = (self.builtin_dir.clone(), pictures_dir());
        // Show what is stored at once; the pictures follow.
        self.render(app);
        std::thread::spawn(move || {
            let found = scan(&setting, &builtin_dir, pictures.as_deref());
            app::post(move |app| {
                if let Some(page) = app.page::<WallpaperPage>() {
                    *page.scan.borrow_mut() = Some(found);
                    page.render(app);
                }
            });
        });
    }

    fn render(&self, app: &Rc<App>) {
        let data = app.window.global::<WallpaperData>();
        let setting = app.settings.borrow().text("appearance", "wallpaper");
        let scan = self.scan.borrow();
        let Some(scan) = scan.as_ref() else {
            data.set_current_title(if setting == WALLPAPER_SOLID {
                "Solid colour".into()
            } else {
                "".into()
            });
            data.set_path_error(self.path_error.borrow().as_str().into());
            return;
        };
        // The scan may be of the setting before the last change.
        let stale = match (&scan.current, setting.as_str()) {
            (Resolved::Solid, WALLPAPER_SOLID) => false,
            (Resolved::Picture(p), s) => p.path != Path::new(s),
            (Resolved::Unusable { path, .. }, s) => path != s,
            _ => true,
        };
        let said = words(scan);
        data.set_current_title(said.title.into());
        data.set_current_detail(said.detail.into());
        data.set_notice(said.notice.into());
        data.set_has_current_image(scan.current_thumb.is_some() && !stale);
        if let Some(thumb) = &scan.current_thumb {
            data.set_current_image(image_of(thumb));
        }
        let picked_solid = matches!(scan.current, Resolved::Solid | Resolved::Unusable { .. });
        let mut tiles = vec![WallpaperTile {
            path: WALLPAPER_SOLID.into(),
            title: "Solid colour".into(),
            image: Image::default(),
            has_image: false,
            chosen: picked_solid && !stale,
        }];
        for (index, (entry, thumb)) in scan.builtin.iter().enumerate() {
            tiles.push(WallpaperTile {
                path: entry.path.display().to_string().into(),
                title: entry.title.as_str().into(),
                image: thumb.as_ref().map(image_of).unwrap_or_default(),
                has_image: thumb.is_some(),
                chosen: scan.current_builtin == Some(index) && !stale,
            });
        }
        data.set_tiles(model(tiles));
        data.set_pictures(model(
            scan.listed
                .iter()
                .map(|listed| PictureRow {
                    path: listed.path.display().to_string().into(),
                    title: listed.name.as_str().into(),
                    detail: listed.detail.as_str().into(),
                    usable: listed.usable,
                    chosen: matches!(&scan.current, Resolved::Picture(p) if p.path == listed.path),
                })
                .collect(),
        ));
        data.set_pictures_note(
            if scan.listed.is_empty() {
                "There are no PNG or JPEG pictures in the Pictures folder."
            } else {
                ""
            }
            .into(),
        );
        data.set_path_error(self.path_error.borrow().as_str().into());
    }
}

impl Controller for WallpaperPage {
    fn show(&self, app: &Rc<App>) {
        self.path_error.borrow_mut().clear();
        self.rescan(app);
    }

    fn as_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Dirs;
    use crate::testutil::TempDir;

    fn png_bytes(width: u32, height: u32, colour: [u8; 3]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let data: Vec<u8> = std::iter::repeat_n(colour, (width * height) as usize)
            .flatten()
            .collect();
        writer.write_image_data(&data).unwrap();
        writer.finish().unwrap();
        bytes
    }

    /// The segments of a JPEG up to its frame header, and not a picture.
    fn jpeg_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        // An APP1 segment of 10 bytes of payload, as an EXIF block would be.
        bytes.extend([0xff, 0xe1, 0x00, 0x0c]);
        bytes.extend([b'E', b'x', b'i', b'f', 0, 0, 1, 2, 3, 4]);
        // A quantisation table is skipped too.
        bytes.extend([0xff, 0xdb, 0x00, 0x04, 0, 0]);
        bytes.extend([0xff, 0xc0, 0x00, 0x11, 8]);
        bytes.extend(height.to_be_bytes());
        bytes.extend(width.to_be_bytes());
        bytes.extend([3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        bytes
    }

    fn write(tmp: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = tmp.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_png_and_a_jpeg_are_known_by_their_first_bytes_and_size() {
        let tmp = TempDir::new();
        let png = write(&tmp, "a.png", &png_bytes(64, 36, [1, 2, 3]));
        let found = inspect(&png).unwrap();
        assert_eq!((found.kind, found.width, found.height), (Kind::Png, 64, 36));

        // The name does not matter.
        let jpeg = write(&tmp, "photo.png", &jpeg_bytes(4000, 2250));
        let found = inspect(&jpeg).unwrap();
        assert_eq!(
            (found.kind, found.width, found.height),
            (Kind::Jpeg, 4000, 2250)
        );
        assert_eq!(describe(&found), "JPEG, 4000 by 2250");
        assert_eq!(found.megabytes(), 36);
    }

    #[test]
    fn what_is_not_a_usable_picture_is_refused_with_a_reason() {
        let tmp = TempDir::new();
        let reason = |path: &Path| inspect(path).unwrap_err();

        assert!(reason(&tmp.path().join("nothing.png")).contains("not there"));
        assert!(reason(tmp.path()).contains("not a file"));
        let text = write(&tmp, "x.png", b"this is text, not a picture at all");
        assert!(reason(&text).contains("not a PNG or JPEG"));
        // Cut off after the signature, and a JPEG that stops in its headers.
        let short = write(&tmp, "short.png", &png_bytes(8, 8, [0; 3])[..12]);
        assert!(reason(&short).contains("not a PNG or JPEG"));
        let cut = write(&tmp, "cut.jpg", &jpeg_bytes(10, 10)[..14]);
        assert!(reason(&cut).contains("not a PNG or JPEG"));
        let empty = write(&tmp, "empty.png", b"");
        assert!(reason(&empty).contains("not a PNG or JPEG"));
        // Too many pixels for swaybg to hold.
        let huge = write(&tmp, "huge.jpg", &jpeg_bytes(12000, 8000));
        assert!(reason(&huge).contains("megapixels"));
        // A header that says no size.
        let flat = write(&tmp, "zero.jpg", &jpeg_bytes(0, 0));
        assert!(reason(&flat).contains("no size"));
        // A scan that starts before any frame header.
        let scan_first = write(&tmp, "scan.jpg", &[0xff, 0xd8, 0xff, 0xda, 0, 2, 0, 0]);
        assert!(reason(&scan_first).contains("not a PNG or JPEG"));
    }

    #[test]
    fn the_setting_resolves_to_what_the_session_shows() {
        let tmp = TempDir::new();
        let good = write(&tmp, "good.png", &png_bytes(32, 18, [9, 9, 9]));
        let path = good.display().to_string();
        assert_eq!(resolve("solid"), Resolved::Solid);
        assert_eq!(resolve(""), Resolved::Solid);
        assert!(matches!(resolve(&path), Resolved::Picture(p) if p.path == good));

        // Gone, or never a picture: the plain colour, with the reason.
        fs::remove_file(&good).unwrap();
        let Resolved::Unusable {
            path: shown,
            reason,
        } = resolve(&path)
        else {
            panic!("a missing file is unusable");
        };
        assert_eq!(shown, path);
        assert!(reason.contains("not there"));
        let bad = write(&tmp, "bad.png", b"corrupt");
        assert!(matches!(
            resolve(&bad.display().to_string()),
            Resolved::Unusable { .. }
        ));
    }

    #[test]
    fn for_the_session_a_bad_picture_is_no_picture() {
        let tmp = TempDir::new();
        let good = write(&tmp, "good.png", &png_bytes(32, 18, [9, 9, 9]));
        let bad = write(&tmp, "bad.png", b"corrupt");
        let settings = |value: &str| {
            Settings::from_documents(
                Default::default(),
                crate::store::Document::parse(&format!("[appearance]\nwallpaper = \"{value}\"\n"))
                    .0,
                None,
            )
        };
        assert_eq!(
            for_session(&settings(&good.display().to_string())),
            Some(good)
        );
        assert_eq!(for_session(&settings(&bad.display().to_string())), None);
        assert_eq!(for_session(&settings("solid")), None);
        assert_eq!(for_session(&Settings::default()), None);
    }

    #[test]
    fn the_shipped_pictures_are_listed_in_order_with_their_names() {
        let tmp = TempDir::new();
        for name in ["02-horizon.png", "01-graphite.png", "03-big-sun.png"] {
            write(&tmp, name, &png_bytes(8, 8, [0; 3]));
        }
        write(&tmp, "readme.txt", b"x");
        write(&tmp, "thumbs/01-graphite.png", &png_bytes(8, 8, [0; 3]));
        let found = builtins(tmp.path());
        let titles: Vec<_> = found.iter().map(|b| b.title.as_str()).collect();
        assert_eq!(titles, ["Graphite", "Horizon", "Big sun"]);
        assert_eq!(found[0].thumb, tmp.path().join("thumbs/01-graphite.png"));
        assert_eq!(title_of("plain.png"), "Plain");
        assert!(builtins(&tmp.path().join("nowhere")).is_empty());
    }

    #[test]
    fn the_pictures_folder_lists_pictures_and_says_which_will_not_do() {
        let tmp = TempDir::new();
        write(&tmp, "Zebra.PNG", &png_bytes(16, 9, [0; 3]));
        write(&tmp, "apple.jpg", &jpeg_bytes(1920, 1080));
        write(&tmp, "broken.png", b"nope");
        write(&tmp, "notes.txt", b"x");
        write(&tmp, ".hidden.png", &png_bytes(8, 8, [0; 3]));
        write(&tmp, "sub/inner.png", &png_bytes(8, 8, [0; 3]));
        let listed = pictures(tmp.path());
        let names: Vec<_> = listed.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["apple.jpg", "broken.png", "Zebra.PNG"]);
        assert_eq!(listed[0].detail, "JPEG, 1920 by 1080");
        assert!(listed[1]
            .detail
            .starts_with("This file is not a PNG or JPEG"));
        assert_eq!(listed[2].detail, "PNG, 16 by 9");

        for n in 0..30 {
            write(&tmp, &format!("more{n:02}.png"), &png_bytes(8, 8, [0; 3]));
        }
        assert_eq!(pictures(tmp.path()).len(), MAX_LISTED);
        assert!(pictures(&tmp.path().join("nowhere")).is_empty());
    }

    #[test]
    fn a_thumbnail_covers_the_preview_and_keeps_the_colours() {
        let tmp = TempDir::new();
        // Wide and tall pictures are cropped to the preview's shape, and a
        // flat picture stays flat.
        for (w, h) in [(1920, 1080), (400, 400), (100, 300), (192, 108)] {
            let path = write(&tmp, "p.png", &png_bytes(w, h, [47, 158, 107]));
            let thumb = thumb_of(&path).unwrap();
            assert_eq!((thumb.width, thumb.height), (THUMB_WIDTH, THUMB_HEIGHT));
            assert_eq!(thumb.rgb.len(), (THUMB_WIDTH * THUMB_HEIGHT * 3) as usize);
            assert!(thumb.rgb.chunks(3).all(|p| p == [47, 158, 107]), "{w}x{h}");
        }
        assert_eq!(thumb_of(&write(&tmp, "j.png", &jpeg_bytes(10, 10))), None);
        assert_eq!(thumb_of(&tmp.path().join("nowhere.png")), None);
    }

    #[test]
    fn a_scan_finds_the_current_picture_among_the_shipped_ones() {
        let tmp = TempDir::new();
        let hills = write(&tmp, "share/02-hills.png", &png_bytes(64, 36, [1, 1, 1]));
        write(&tmp, "share/01-graphite.png", &png_bytes(64, 36, [2, 2, 2]));
        write(
            &tmp,
            "share/thumbs/02-hills.png",
            &png_bytes(192, 108, [1, 1, 1]),
        );
        let mine = write(
            &tmp,
            "home/Pictures/mine.png",
            &png_bytes(300, 200, [5, 5, 5]),
        );

        let share = tmp.path().join("share");
        let pictures = tmp.path().join("home/Pictures");
        let found = scan(&hills.display().to_string(), &share, Some(&pictures));
        assert_eq!(found.builtin.len(), 2);
        assert_eq!(found.current_builtin, Some(1));
        assert!(found.current_thumb.is_some());
        assert_eq!(found.listed.len(), 1);
        let said = words(&found);
        assert_eq!(said.title, "Hills");
        assert_eq!(said.detail, "PNG, 64 by 36");
        assert!(said.notice.is_empty());
        // The first has no thumbnail file: it is drawn without one.
        assert!(found.builtin[0].1.is_none());

        let found = scan(&mine.display().to_string(), &share, Some(&pictures));
        assert_eq!(found.current_builtin, None);
        assert!(found.current_thumb.is_some());
        assert_eq!(words(&found).title, "mine.png");

        let found = scan("solid", &share, None);
        assert_eq!(found.current, Resolved::Solid);
        assert_eq!(words(&found).title, "Solid colour");
        assert!(found.listed.is_empty());
    }

    #[test]
    fn a_picture_that_went_missing_is_a_notice_and_the_plain_colour() {
        let tmp = TempDir::new();
        let gone = tmp.path().join("gone.png").display().to_string();
        let found = scan(&gone, tmp.path(), None);
        let said = words(&found);
        assert_eq!(said.title, "Solid colour");
        assert!(said.notice.contains("cannot be shown"), "{}", said.notice);
        assert!(said.notice.contains("not there"));
    }

    #[test]
    fn a_heavy_picture_says_what_it_costs() {
        let tmp = TempDir::new();
        let big = write(&tmp, "big.jpg", &jpeg_bytes(3840, 2160));
        let found = scan(&big.display().to_string(), tmp.path(), None);
        assert!(
            words(&found).detail.contains("about 34 MB"),
            "{}",
            words(&found).detail
        );
        let small = write(&tmp, "small.jpg", &jpeg_bytes(1920, 1080));
        let found = scan(&small.display().to_string(), tmp.path(), None);
        assert!(!words(&found).detail.contains("MB"));
    }

    #[test]
    fn the_setting_round_trips_through_the_file() {
        let tmp = TempDir::new();
        let dirs = Dirs::from(
            Some(tmp.path().to_str().unwrap()),
            Some(tmp.path().join("config").to_str().unwrap()),
            None,
        );
        let (mut settings, _) = Settings::load(&dirs);
        assert_eq!(settings.text("appearance", "wallpaper"), "solid");
        let path = "/home/me/My Pictures/sea \"front\".png";
        settings
            .change(&[(
                "appearance".to_string(),
                "wallpaper".to_string(),
                Value::text(path),
            )])
            .unwrap();
        let (again, warnings) = Settings::load(&dirs);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(again.text("appearance", "wallpaper"), path);
        // A value that is not a path is refused before it is written.
        assert!(settings
            .change(&[(
                "appearance".to_string(),
                "wallpaper".to_string(),
                Value::text("relative.png"),
            )])
            .is_err());
        assert_eq!(
            Settings::load(&dirs).0.text("appearance", "wallpaper"),
            path
        );
    }
}
