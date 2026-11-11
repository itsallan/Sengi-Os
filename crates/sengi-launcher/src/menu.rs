//! Menu mode: choose one of the lines given on standard input.

use std::io::Read;

use crate::matcher::Candidate;

/// A menu is something a person reads; more than this is a mistake upstream.
const MAX_BYTES: u64 = 1 << 20;
const MAX_LINES: usize = 10_000;

/// Read the menu lines. Blank lines are dropped and bytes that are not UTF-8
/// are replaced rather than refused. Input beyond the limits is not read.
pub fn read_items(input: impl Read) -> std::io::Result<Vec<String>> {
    read_items_limited(input, MAX_BYTES, MAX_LINES)
}

fn read_items_limited(
    input: impl Read,
    max_bytes: u64,
    max_lines: usize,
) -> std::io::Result<Vec<String>> {
    let mut bytes = Vec::new();
    input.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        // Cut at the last whole line, so no entry is shown half.
        let whole = bytes[..max_bytes as usize]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |at| at + 1);
        bytes.truncate(whole);
    }
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
        .map(str::to_owned)
        .collect())
}

pub fn candidates(items: &[String]) -> Vec<Candidate> {
    items.iter().map(Candidate::new).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lines_and_drops_blank_ones() {
        let items = read_items(&b"Lock\nLog out\r\n\n   \nShut down"[..]).unwrap();
        assert_eq!(items, vec!["Lock", "Log out", "Shut down"]);
        assert!(read_items(&b""[..]).unwrap().is_empty());
        assert!(read_items(&b"\n\n"[..]).unwrap().is_empty());
    }

    #[test]
    fn keeps_inner_spacing_and_survives_bad_bytes() {
        let items = read_items(&b"  indented\ttab\nbad \xff byte\n"[..]).unwrap();
        assert_eq!(items, vec!["  indented\ttab", "bad \u{fffd} byte"]);
    }

    #[test]
    fn candidates_keep_the_line_as_the_name() {
        let items = vec!["Restart".to_string(), "Shut down".to_string()];
        let names: Vec<String> = candidates(&items).into_iter().map(|c| c.name).collect();
        assert_eq!(names, items);
    }

    #[test]
    fn input_beyond_the_limits_is_cut_at_a_whole_line() {
        let input = &b"one\ntwo\nthree\nfour\n"[..];
        assert_eq!(
            read_items_limited(input, 100, 2).unwrap(),
            vec!["one", "two"]
        );
        // Ten bytes reach into "three", which is then left out.
        assert_eq!(
            read_items_limited(input, 10, 100).unwrap(),
            vec!["one", "two"]
        );
        assert_eq!(
            read_items_limited(input, 8, 100).unwrap(),
            vec!["one", "two"]
        );
        assert!(read_items_limited(&b"no newline at all"[..], 5, 100)
            .unwrap()
            .is_empty());
        assert_eq!(read_items_limited(input, 19, 100).unwrap().len(), 4);
    }

    #[test]
    fn an_endless_input_stops_at_the_limit() {
        let items = read_items(std::io::repeat(b'x')).unwrap();
        assert!(items.is_empty());
        let lines = b"line\n".repeat(50_000);
        assert_eq!(read_items(&lines[..]).unwrap().len(), MAX_LINES);
    }
}
