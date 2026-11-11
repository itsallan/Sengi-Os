//! Reading files other programs wrote, without trusting their size.

use std::io::Read;
use std::path::Path;

/// Read a whole file, or nothing if it is larger than `limit` bytes. The
/// size is enforced while reading, so a file that grows, or one that only
/// claims to be small, cannot make the launcher allocate without bound.
pub fn read_limited(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn reads_up_to_the_limit_and_nothing_beyond() {
        let tmp = TempDir::new();
        let path = tmp.write("file", "12345");
        assert_eq!(read_limited(&path, 5).as_deref(), Some(&b"12345"[..]));
        assert_eq!(read_limited(&path, 100).as_deref(), Some(&b"12345"[..]));
        assert_eq!(read_limited(&path, 4), None);
        assert_eq!(read_limited(&tmp.path().join("missing"), 100), None);
        assert_eq!(read_limited(tmp.path(), 100), None);
    }

    #[test]
    fn a_huge_sparse_file_is_refused_without_reading_it_all() {
        let tmp = TempDir::new();
        let path = tmp.path().join("sparse");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(8 << 30).unwrap();
        assert_eq!(read_limited(&path, 1 << 20), None);
    }
}
