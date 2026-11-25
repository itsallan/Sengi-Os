//! The parts of taking a screenshot that need no compositor: the command
//! line, the name of the file, and turning the compositor's pixels into a
//! picture.

use std::path::{Path, PathBuf};

pub const USAGE: &str = "\
Usage: sengi-shot [--select] [--delay SECONDS] [--clipboard]
                  [--output NAME] [--region X,Y,WIDTH,HEIGHT]

Saves a PNG of the screen to ~/Pictures/Screenshot-DATE-TIME.png and says
where in a notification. With several outputs each gets a file, named after
the output.

  --select         dim the screen and drag a rectangle to keep; Enter or
                   letting go takes it, Escape cancels (needs sengi-snip)
  --delay N        wait N seconds first (1 to 60), counting down on screen
  --clipboard      also copy the picture to the clipboard (needs wl-copy)
  --output NAME    one output only
  --region X,Y,W,H a part of the output, in its logical coordinates; this
                   is what the compositor passes for Alt+Print

  -h, --help       show this help
  -V, --version    show the version
";

/// The longest wait before a shot.
pub const MAX_DELAY: u32 = 60;

/// A part of an output, in its logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// What was asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub output: Option<String>,
    pub region: Option<Region>,
    /// Pick the region on screen, with the mouse.
    pub select: bool,
    /// Seconds to wait first; 0 for none.
    pub delay: u32,
    pub clipboard: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Invocation {
    Take(Options),
    Help,
    Version,
}

pub fn parse<S: AsRef<str>>(arguments: &[S]) -> Result<Invocation, String> {
    let mut options = Options::default();
    let mut words = arguments.iter().map(AsRef::as_ref);
    while let Some(word) = words.next() {
        match word {
            "-h" | "--help" => return Ok(Invocation::Help),
            "-V" | "--version" => return Ok(Invocation::Version),
            "--output" => {
                let name = words.next().ok_or("--output needs a name")?;
                options.output = Some(name.to_string());
            }
            "--region" => {
                let text = words.next().ok_or("--region needs X,Y,WIDTH,HEIGHT")?;
                options.region = Some(parse_region(text)?);
            }
            "--select" => options.select = true,
            "--clipboard" => options.clipboard = true,
            "--delay" => {
                let text = words.next().ok_or("--delay needs a number of seconds")?;
                options.delay = match text.parse::<u32>() {
                    Ok(n) if (1..=MAX_DELAY).contains(&n) => n,
                    _ => {
                        return Err(format!(
                            "'{text}' is not a number of seconds from 1 to {MAX_DELAY}"
                        ))
                    }
                };
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    if options.select && options.region.is_some() {
        return Err("--select and --region both say which part; use one".into());
    }
    if options.select && options.output.is_some() {
        return Err("--select picks the output itself; leave --output out".into());
    }
    Ok(Invocation::Take(options))
}

/// What sengi-snip prints for a choice: `NAME X,Y,WIDTH,HEIGHT`, with `-`
/// for a name it does not know.
pub fn parse_choice(line: &str) -> Option<(Option<String>, Region)> {
    let (name, region) = line.trim().split_once(' ')?;
    let region = parse_region(region).ok()?;
    Some(((name != "-").then(|| name.to_string()), region))
}

/// Where a screenshot is copied to the clipboard from: the program that
/// does it, and how it is told the kind of picture.
pub const CLIPBOARD_COMMAND: (&str, &[&str]) = ("wl-copy", &["--type", "image/png"]);

fn parse_region(text: &str) -> Result<Region, String> {
    let bad = || format!("'{text}' is not X,Y,WIDTH,HEIGHT");
    let numbers: Vec<i32> = text
        .split(',')
        .map(|part| part.trim().parse().map_err(|_| bad()))
        .collect::<Result<_, _>>()?;
    match numbers[..] {
        [x, y, width, height] if width > 0 && height > 0 => Ok(Region {
            x,
            y,
            width,
            height,
        }),
        _ => Err(bad()),
    }
}

/// A date and a time of day, as the local clock shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Stamp {
    /// Now, in the time zone of the session.
    pub fn now() -> Stamp {
        // SAFETY: both calls only fill in the structs given to them.
        let mut moment: libc::tm = unsafe { std::mem::zeroed() };
        unsafe {
            let seconds = libc::time(std::ptr::null_mut());
            libc::localtime_r(&seconds, &mut moment);
        }
        Stamp {
            year: moment.tm_year + 1900,
            month: (moment.tm_mon + 1) as u32,
            day: moment.tm_mday as u32,
            hour: moment.tm_hour as u32,
            minute: moment.tm_min as u32,
            second: moment.tm_sec as u32,
        }
    }
}

/// `Screenshot-2026-11-25-143007`, with the output's name after it when
/// the screenshot is one of several. Only plain characters of the name are
/// kept, so it cannot say where the file goes.
pub fn file_stem(stamp: &Stamp, output: Option<&str>) -> String {
    let mut stem = format!(
        "Screenshot-{:04}-{:02}-{:02}-{:02}{:02}{:02}",
        stamp.year, stamp.month, stamp.day, stamp.hour, stamp.minute, stamp.second
    );
    if let Some(name) = output {
        let plain: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .take(32)
            .collect();
        if !plain.is_empty() {
            stem.push('-');
            stem.push_str(&plain);
        }
    }
    stem
}

/// A path in `dir` for `stem` that nothing is at yet: `stem.png`, or
/// `stem-2.png` and on when a second shot is taken in the same second.
pub fn free_path(dir: &Path, stem: &str, exists: impl Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(format!("{stem}.png"));
    if !exists(&first) {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.png")))
        .find(|path| !exists(path))
        .expect("there are not endlessly many files")
}

/// Where the pictures go: `~/Pictures`.
pub fn pictures_dir(home: Option<&Path>) -> Option<PathBuf> {
    home.filter(|home| home.is_absolute())
        .map(|home| home.join("Pictures"))
}

/// The compositor's XRGB8888 rows as red, green and blue bytes without the
/// padding at the end of each row. The bytes are stored blue first.
pub fn xrgb_to_rgb(xrgb: &[u8], width: usize, height: usize, stride: usize) -> Option<Vec<u8>> {
    if stride < width * 4 || xrgb.len() < stride.checked_mul(height)? {
        return None;
    }
    let mut rgb = Vec::with_capacity(width * height * 3);
    for row in xrgb.chunks(stride).take(height) {
        for pixel in row[..width * 4].as_chunks::<4>().0 {
            rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
        }
    }
    Some(rgb)
}

/// Encodes red, green and blue bytes as a PNG.
pub fn encode_png(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        // A screenshot is taken to be done with at once.
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgb)?;
        writer.finish()?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(options: Options) -> Result<Invocation, String> {
        Ok(Invocation::Take(options))
    }

    #[test]
    fn the_command_line() {
        assert_eq!(parse::<&str>(&[]), take(Options::default()));
        assert_eq!(
            parse(&["--output", "eDP-1", "--region", "10,20,300,200"]),
            take(Options {
                output: Some("eDP-1".into()),
                region: Some(Region {
                    x: 10,
                    y: 20,
                    width: 300,
                    height: 200
                }),
                ..Options::default()
            })
        );
        assert_eq!(parse(&["-h"]), Ok(Invocation::Help));
        assert!(parse(&["--region", "1,2,3"]).is_err());
        assert!(parse(&["--region", "1,2,0,5"]).is_err());
        assert!(parse(&["--region", "a,b,c,d"]).is_err());
        assert!(parse(&["--output"]).is_err());
        assert!(parse(&["--wild"]).is_err());
    }

    #[test]
    fn select_delay_and_clipboard() {
        assert_eq!(
            parse(&["--select", "--delay", "3", "--clipboard"]),
            take(Options {
                select: true,
                delay: 3,
                clipboard: true,
                ..Options::default()
            })
        );
        assert_eq!(
            parse(&["--delay", "60"]),
            take(Options {
                delay: 60,
                ..Options::default()
            })
        );
        for bad in [
            &["--delay", "0"][..],
            &["--delay", "61"],
            &["--delay", "-1"],
            &["--delay", "soon"],
            &["--delay"],
            &["--select", "--region", "1,2,3,4"],
            &["--select", "--output", "eDP-1"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_choice_from_the_selection_screen() {
        // The same line sengi-snip's tests expect to write.
        assert_eq!(
            parse_choice("eDP-1 10,20,640,480\n"),
            Some((
                Some("eDP-1".into()),
                Region {
                    x: 10,
                    y: 20,
                    width: 640,
                    height: 480
                }
            ))
        );
        assert_eq!(parse_choice("- 0,0,5,5").unwrap().0, None);
        assert_eq!(parse_choice(""), None);
        assert_eq!(parse_choice("eDP-1"), None);
        assert_eq!(parse_choice("eDP-1 1,2,0,4"), None);
    }

    fn stamp() -> Stamp {
        Stamp {
            year: 2026,
            month: 11,
            day: 5,
            hour: 9,
            minute: 3,
            second: 7,
        }
    }

    #[test]
    fn the_name_has_the_date_and_time() {
        assert_eq!(file_stem(&stamp(), None), "Screenshot-2026-11-05-090307");
        assert_eq!(
            file_stem(&stamp(), Some("HDMI-A-1")),
            "Screenshot-2026-11-05-090307-HDMI-A-1"
        );
        // Nothing in an output's name can lead out of the folder.
        assert_eq!(
            file_stem(&stamp(), Some("../../x/y")),
            "Screenshot-2026-11-05-090307-xy"
        );
        assert_eq!(
            file_stem(&stamp(), Some("/")),
            "Screenshot-2026-11-05-090307"
        );
    }

    #[test]
    fn a_second_shot_in_a_second_gets_a_number() {
        let dir = Path::new("/p");
        let taken = ["/p/s.png", "/p/s-2.png"];
        let exists = |path: &Path| taken.iter().any(|t| Path::new(t) == path);
        assert_eq!(free_path(dir, "s", |_| false), Path::new("/p/s.png"));
        assert_eq!(free_path(dir, "s", exists), Path::new("/p/s-3.png"));
    }

    #[test]
    fn pictures_live_in_the_home_folder() {
        assert_eq!(
            pictures_dir(Some(Path::new("/home/ann"))),
            Some(PathBuf::from("/home/ann/Pictures"))
        );
        assert_eq!(pictures_dir(Some(Path::new("home"))), None);
        assert_eq!(pictures_dir(None), None);
    }

    #[test]
    fn pixels_come_out_as_rgb_without_the_row_padding() {
        // Two pixels a row, eight bytes of padding after them.
        let mut xrgb = Vec::new();
        for row in 0..2u8 {
            xrgb.extend_from_slice(&[30 + row, 20, 10, 255, 3, 2, 1, 255]);
            xrgb.extend_from_slice(&[0; 8]);
        }
        let rgb = xrgb_to_rgb(&xrgb, 2, 2, 16).unwrap();
        assert_eq!(rgb, [10, 20, 30, 1, 2, 3, 10, 20, 31, 1, 2, 3]);
        assert_eq!(xrgb_to_rgb(&xrgb, 2, 2, 4), None);
        assert_eq!(xrgb_to_rgb(&xrgb[..20], 2, 2, 16), None);
    }

    #[test]
    fn the_png_reads_back_as_it_went_in() {
        let rgb: Vec<u8> = (0..4 * 3 * 3).map(|n| n as u8).collect();
        let bytes = encode_png(&rgb, 4, 3).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut back = vec![0; rgb.len()];
        let info = reader.next_frame(&mut back).unwrap();
        assert_eq!((info.width, info.height), (4, 3));
        assert_eq!(back[..rgb.len()], rgb[..]);
    }
}
