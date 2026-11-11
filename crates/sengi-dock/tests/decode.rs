//! The icon decoder as the dock really runs it: the sengi-dock binary
//! started as a child for one file.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sengi_apps::icons::{self, Contexts, Decoder, IconDirs, DECODE_TIMEOUT};

fn decoder() -> Decoder {
    Decoder::new(env!("CARGO_BIN_EXE_sengi-dock").into())
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sengi-dock-decode-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_png(path: &Path, side: u32, rgba: [u8; 4]) {
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

#[test]
fn a_png_comes_back_at_the_size_asked_for() {
    let dir = temp_dir("png");
    let path = dir.join("icon.png");
    write_png(&path, 256, [200, 100, 50, 255]);
    let icon = decoder().decode_file(&path, 40, DECODE_TIMEOUT).unwrap();
    assert_eq!(icon.size, 40);
    assert_eq!(icon.data.len(), 40 * 40 * 4);
    assert_eq!(&icon.data[..4], [200, 100, 50, 255]);
}

#[cfg(feature = "svg")]
#[test]
fn an_svg_comes_back_too() {
    let dir = temp_dir("svg");
    let path = dir.join("icon.svg");
    fs::write(
        &path,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 10 10\">\
         <rect width=\"10\" height=\"10\" fill=\"#00f\"/></svg>",
    )
    .unwrap();
    let icon = decoder().decode_file(&path, 80, DECODE_TIMEOUT).unwrap();
    assert_eq!(&icon.data[..4], [0, 0, 255, 255]);
}

#[test]
fn hostile_files_cost_no_more_than_the_timeout() {
    let dir = temp_dir("hostile");
    let decoder = decoder();
    let started = Instant::now();

    // Garbage, a device that never ends, and a pipe nobody writes to.
    fs::write(dir.join("garbage.png"), b"\x89PNG\r\n\x1a\nnot really").unwrap();
    assert!(decoder
        .decode_file(&dir.join("garbage.png"), 40, DECODE_TIMEOUT)
        .is_none());

    std::os::unix::fs::symlink("/dev/zero", dir.join("zero.png")).unwrap();
    assert!(decoder
        .decode_file(&dir.join("zero.png"), 40, DECODE_TIMEOUT)
        .is_none());

    let fifo = dir.join("fifo.svg");
    let status = std::process::Command::new("mkfifo").arg(&fifo).status();
    if status.is_ok_and(|s| s.success()) {
        assert!(decoder.decode_file(&fifo, 40, DECODE_TIMEOUT).is_none());
    }

    // An SVG that points at a file it should not read.
    fs::write(
        dir.join("reader.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\" \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 8 8\">\
         <image width=\"8\" height=\"8\" xlink:href=\"/dev/zero\"/></svg>",
    )
    .unwrap();
    let _ = decoder.decode_file(&dir.join("reader.svg"), 40, DECODE_TIMEOUT);

    // Deeply nested groups: either it draws or it is given up on.
    let depth = 20_000;
    let nested = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 8 8\">{}<rect width=\"8\" \
         height=\"8\"/>{}</svg>",
        "<g>".repeat(depth),
        "</g>".repeat(depth)
    );
    fs::write(dir.join("nested.svg"), nested).unwrap();
    let _ = decoder.decode_file(&dir.join("nested.svg"), 40, DECODE_TIMEOUT);

    assert!(started.elapsed() < DECODE_TIMEOUT * 4 + Duration::from_secs(2));
}

#[test]
fn a_missing_file_gives_nothing() {
    assert!(decoder()
        .decode_file(Path::new("/nonexistent/icon.png"), 40, DECODE_TIMEOUT)
        .is_none());
}

#[test]
fn an_icon_that_links_to_a_file_with_another_name_is_decoded() {
    // What the dock does for `Icon=/path/app.png`: find it here, then have
    // the child decode what was found.
    let dir = temp_dir("linked");
    let target = dir.join("store/3f2a.bin");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    write_png(&target, 64, [10, 20, 30, 255]);
    let link = dir.join("app.png");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let dirs = IconDirs::default();
    let found = icons::find(&dirs, None, link.to_str().unwrap(), 40, Contexts::Apps).unwrap();
    let icon = decoder()
        .decode_file(&found, 40, DECODE_TIMEOUT)
        .expect("the linked icon is decoded");
    assert_eq!(icon.size, 40);
    assert_eq!(&icon.data[..4], [10, 20, 30, 255]);

    // The child's own refusals still go by where a link leads.
    let zero = dir.join("zero.png");
    std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
    assert!(icons::find(&dirs, None, zero.to_str().unwrap(), 40, Contexts::Apps).is_none());
    assert!(decoder().decode_file(&zero, 40, DECODE_TIMEOUT).is_none());
}
