//! Menu mode: choose one of the lines given on standard input.

use std::io::Read;

use crate::matcher::Candidate;

/// Read the menu lines. Blank lines are dropped and bytes that are not UTF-8
/// are replaced rather than refused.
pub fn read_items(mut input: impl Read) -> std::io::Result<Vec<String>> {
    let mut bytes = Vec::new();
    input.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.trim().is_empty())
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
}
