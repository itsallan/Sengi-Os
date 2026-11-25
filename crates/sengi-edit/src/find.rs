//! Searching the text. Case is ignored for the letters A to Z, which keeps
//! every byte offset the same in the lowered copy and in the text.

/// The first match at or after byte `from` (wrapping round to the start),
/// as byte offsets. None when the text has no match at all.
pub fn next(text: &str, query: &str, from: usize) -> Option<(usize, usize)> {
    if query.is_empty() {
        return None;
    }
    let hay = text.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let from = floor_boundary(&hay, from);
    hay[from..]
        .find(&needle)
        .map(|at| at + from)
        .or_else(|| hay.find(&needle))
        .map(|at| (at, at + needle.len()))
}

/// The last match that starts before byte `before` (wrapping round to the
/// end).
pub fn previous(text: &str, query: &str, before: usize) -> Option<(usize, usize)> {
    if query.is_empty() {
        return None;
    }
    let hay = text.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let before = floor_boundary(&hay, before);
    hay[..before]
        .rfind(&needle)
        .or_else(|| hay.rfind(&needle))
        .map(|at| (at, at + needle.len()))
}

/// How many matches there are, and which one starts at `at`, counting from 1.
pub fn count(text: &str, query: &str, at: usize) -> (usize, usize) {
    if query.is_empty() {
        return (0, 0);
    }
    let hay = text.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let mut total = 0;
    let mut which = 0;
    let mut start = 0;
    while let Some(found) = hay[start..].find(&needle) {
        let found = found + start;
        total += 1;
        if found == at {
            which = total;
        }
        start = found + needle.len().max(1);
        while !hay.is_char_boundary(start) {
            start += 1;
        }
    }
    (which, total)
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Where the caret is, as a line and a column (both from 1, the column in
/// characters).
pub fn line_and_column(text: &str, offset: usize) -> (usize, usize) {
    let offset = floor_boundary(text, offset);
    let before = &text[..offset];
    let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, before[start..].chars().count() + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_forward_and_wraps() {
        let text = "Alpha beta ALPHA gamma alpha";
        assert_eq!(next(text, "alpha", 0), Some((0, 5)));
        assert_eq!(next(text, "alpha", 1), Some((11, 16)));
        assert_eq!(next(text, "alpha", 12), Some((23, 28)));
        assert_eq!(next(text, "alpha", 24), Some((0, 5)));
        assert_eq!(next(text, "delta", 0), None);
        assert_eq!(next(text, "", 0), None);
    }

    #[test]
    fn finds_backward_and_wraps() {
        let text = "ab ab ab";
        assert_eq!(previous(text, "ab", 8), Some((6, 8)));
        assert_eq!(previous(text, "ab", 6), Some((3, 5)));
        assert_eq!(previous(text, "ab", 3), Some((0, 2)));
        assert_eq!(previous(text, "ab", 0), Some((6, 8)));
    }

    #[test]
    fn offsets_stay_valid_with_other_scripts() {
        let text = "caf\u{e9} \u{4e2d}\u{6587} Caf\u{e9}";
        let (start, end) = next(text, "caf\u{e9}", 1).unwrap();
        assert_eq!(&text[start..end], "Caf\u{e9}");
        // A start in the middle of a character is moved back to its start.
        assert!(next(text, "x", 4).is_none());
        assert_eq!(
            next(text, "\u{6587}", 7).map(|m| &text[m.0..m.1]),
            Some("\u{6587}")
        );
    }

    #[test]
    fn counts_and_numbers_matches() {
        let text = "a b a b a";
        assert_eq!(count(text, "a", 4), (2, 3));
        assert_eq!(count(text, "a", 1), (0, 3));
        assert_eq!(count(text, "zz", 0), (0, 0));
    }

    #[test]
    fn line_and_column_count_characters() {
        let text = "one\ntw\u{e9}o\n\nx";
        assert_eq!(line_and_column(text, 0), (1, 1));
        assert_eq!(line_and_column(text, 3), (1, 4));
        assert_eq!(line_and_column(text, 4), (2, 1));
        assert_eq!(line_and_column(text, 8), (2, 4));
        assert_eq!(line_and_column(text, 10), (3, 1));
        assert_eq!(line_and_column(text, 11), (4, 1));
        assert_eq!(line_and_column(text, 999), (4, 2));
    }
}
