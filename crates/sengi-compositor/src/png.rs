//! Just enough PNG to save a screenshot: 8-bit RGB, no compression.
//!
//! Screenshots are a test and debugging aid, so a dependency-free writer with
//! large files is a better deal here than pulling in an image library.

use std::io::{self, Write};

/// Deflate "stored" blocks carry at most this many bytes each.
const STORED_MAX: usize = 0xffff;

pub fn write_rgb(out: &mut impl Write, width: u32, height: u32, rgb: &[u8]) -> io::Result<()> {
    let row = width as usize * 3;
    if row == 0 || rgb.len() != row * height as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pixel data does not match the image size",
        ));
    }

    // Every scanline is prefixed with its filter type; 0 means unfiltered.
    let mut raw = Vec::with_capacity(rgb.len() + height as usize);
    for line in rgb.chunks_exact(row) {
        raw.push(0);
        raw.extend_from_slice(line);
    }

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // 8 bits per channel, colour type 2 (RGB), default compression, filter
    // and interlace methods.
    header.extend_from_slice(&[8, 2, 0, 0, 0]);

    out.write_all(b"\x89PNG\r\n\x1a\n")?;
    chunk(out, b"IHDR", &header)?;
    chunk(out, b"IDAT", &zlib_stored(&raw))?;
    chunk(out, b"IEND", &[])
}

fn chunk(out: &mut impl Write, kind: &[u8; 4], data: &[u8]) -> io::Result<()> {
    out.write_all(&(data.len() as u32).to_be_bytes())?;
    out.write_all(kind)?;
    out.write_all(data)?;
    let crc = crc32(crc32(0, kind), data);
    out.write_all(&crc.to_be_bytes())
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut blocks = data.chunks(STORED_MAX).peekable();
    while let Some(block) = blocks.next() {
        let len = block.len() as u16;
        out.push(u8::from(blocks.peek().is_none()));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(seed: u32, data: &[u8]) -> u32 {
    let mut crc = !seed;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(crc32(0, b"1234"), b"56789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn one_pixel_image_has_the_expected_layout() {
        let mut png = Vec::new();
        write_rgb(&mut png, 1, 1, &[0x12, 0x13, 0x16]).unwrap();

        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..24], &[0, 0, 0, 1, 0, 0, 0, 1]);
        // IDAT payload: zlib header, one final stored block of four bytes
        // (filter byte plus one pixel), adler32.
        let idat = png.windows(4).position(|w| w == b"IDAT").unwrap() + 4;
        assert_eq!(
            &png[idat..idat + 11],
            &[0x78, 0x01, 1, 4, 0, 0xfb, 0xff, 0, 0x12, 0x13, 0x16]
        );
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn large_images_are_split_into_stored_blocks() {
        let data = vec![7u8; STORED_MAX + 10];
        let z = zlib_stored(&data);
        // Two block headers of five bytes each, zlib header and checksum.
        assert_eq!(z.len(), data.len() + 2 + 5 + 5 + 4);
        assert_eq!(z[2], 0);
        assert_eq!(z[2 + 5 + STORED_MAX], 1);
    }

    #[test]
    fn wrong_buffer_size_is_rejected() {
        assert!(write_rgb(&mut Vec::new(), 2, 2, &[0; 3]).is_err());
    }
}
