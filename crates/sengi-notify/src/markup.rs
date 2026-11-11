//! Turns what a client sent into plain text that is safe to draw.
//!
//! The notification spec lets a body carry a little HTML: `<b>`, `<i>`,
//! `<u>`, `<a>` and `<img>`. Nothing here is rendered as markup. Tags are
//! dropped, entities are decoded once, and whatever is left is text.

/// A tag longer than this is not a tag, it is text with a `<` in it.
const TAG_MAX: usize = 256;

/// The longest entity worth looking for, `&#x10FFFF;`, with room to spare.
const ENTITY_MAX: usize = 12;

/// The longest prefix of `text` that is at most `max` bytes and ends on a
/// character boundary.
pub fn clip_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Strips tags and decodes entities. The result still needs [`tidy`].
pub fn strip(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(at) = rest.find(['<', '&']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with('<') {
            match tag(rest) {
                Some((length, replacement)) => {
                    out.push_str(&replacement);
                    rest = &rest[length..];
                }
                None => {
                    out.push('<');
                    rest = &rest[1..];
                }
            }
        } else {
            match entity(rest) {
                Some((length, decoded)) => {
                    out.push(decoded);
                    rest = &rest[length..];
                }
                None => {
                    out.push('&');
                    rest = &rest[1..];
                }
            }
        }
    }
    out.push_str(rest);
    out
}

/// If `text` starts with something shaped like a tag, its length in bytes
/// and what to put in its place.
fn tag(text: &str) -> Option<(usize, String)> {
    let window = clip_bytes(text, TAG_MAX);
    let end = window[1..].find(['<', '>']).map(|at| at + 1)?;
    if window.as_bytes()[end] != b'>' {
        return None;
    }
    let inside = &window[1..end];
    let closing = inside.starts_with('/');
    let inside = inside.strip_prefix('/').unwrap_or(inside);
    let name_end = inside
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(inside.len());
    let name = &inside[..name_end];
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    // After the name comes a space, a slash or the end; `<a@b.c>` is an
    // address, not a tag.
    let after = &inside[name_end..];
    if !(after.is_empty() || after.starts_with([' ', '\t', '\n', '/'])) {
        return None;
    }

    let replacement = if closing {
        String::new()
    } else if name.eq_ignore_ascii_case("br") {
        "\n".to_string()
    } else if name.eq_ignore_ascii_case("img") {
        attribute(after, "alt").unwrap_or_default()
    } else {
        String::new()
    };
    Some((end + 1, replacement))
}

/// The value of a quoted attribute, entities decoded.
fn attribute(attributes: &str, wanted: &str) -> Option<String> {
    let mut rest = attributes;
    loop {
        rest = rest.trim_start_matches([' ', '\t', '\n', '/']);
        if rest.is_empty() {
            return None;
        }
        let name_end = rest.find(['=', ' ', '\t', '\n']).unwrap_or(rest.len());
        let name = &rest[..name_end];
        rest = &rest[name_end..];
        let Some(value) = rest.strip_prefix('=') else {
            continue;
        };
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let value = &value[1..];
        let close = value.find(quote)?;
        if name.eq_ignore_ascii_case(wanted) {
            return Some(decode_entities(&value[..close]));
        }
        rest = &value[close + 1..];
    }
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        match entity(rest) {
            Some((length, decoded)) => {
                out.push(decoded);
                rest = &rest[length..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// If `text` starts with an entity, its length in bytes and its character.
fn entity(text: &str) -> Option<(usize, char)> {
    let window = clip_bytes(text, ENTITY_MAX);
    let end = window.find(';')?;
    let name = &window[1..end];
    let decoded = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) if !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    u32::from_str_radix(hex, 16).ok()?
                }
                None if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) => {
                    number.parse().ok()?
                }
                _ => return None,
            };
            char::from_u32(code)?
        }
    };
    Some((end + 1, decoded))
}

pub use sengi_text::tidy;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_allowed_tags_are_dropped_and_their_text_kept() {
        assert_eq!(strip("<b>Bold</b> and <i>italic</i>"), "Bold and italic");
        assert_eq!(strip("<u>under</u>"), "under");
        assert_eq!(
            strip("See <a href=\"https://example.org/?a=1&amp;b=2\">the page</a>."),
            "See the page."
        );
        assert_eq!(strip("one<br>two<br/>three"), "one\ntwo\nthree");
        assert_eq!(strip("<B>Shout</B>"), "Shout");
    }

    #[test]
    fn an_image_leaves_its_alt_text() {
        assert_eq!(
            strip("<img src=\"file:///x.png\" alt=\"A &amp; B\"/> done"),
            "A & B done"
        );
        assert_eq!(strip("<img src='x.png'>"), "");
        assert_eq!(strip("<img alt='cat' src=x>"), "cat");
        // An attribute that never closes its quote is not searched further.
        assert_eq!(strip("<img alt=\"cat>"), "");
    }

    #[test]
    fn other_tags_are_dropped_and_never_interpreted() {
        assert_eq!(strip("<script>alert(1)</script>"), "alert(1)");
        assert_eq!(
            strip("<span foreground=\"red\" size=\"99999\">big</span>"),
            "big"
        );
        assert_eq!(strip("<style>*{}</style><iframe src=x></iframe>"), "*{}");
    }

    #[test]
    fn text_that_only_looks_a_little_like_markup_survives() {
        assert_eq!(strip("a < b and c > d"), "a < b and c > d");
        assert_eq!(strip("1 <3 you"), "1 <3 you");
        assert_eq!(strip("<<<>>>"), "<<<>>>");
        assert_eq!(
            strip("mail <me@example.org> now"),
            "mail <me@example.org> now"
        );
        assert_eq!(strip("<"), "<");
        assert_eq!(strip("<b"), "<b");
        assert_eq!(strip("<>"), "<>");
        assert_eq!(strip("</>"), "</>");
        assert_eq!(strip("AT&T & co"), "AT&T & co");
        assert_eq!(strip("&"), "&");
        assert_eq!(strip("&;"), "&;");
    }

    #[test]
    fn entities_are_decoded_exactly_once() {
        assert_eq!(
            strip("&lt;b&gt; &amp; &quot;q&quot; &apos;a&apos;"),
            "<b> & \"q\" 'a'"
        );
        assert_eq!(strip("&#65;&#x42;&#X43;"), "ABC");
        assert_eq!(strip("&amp;lt;b&amp;gt;"), "&lt;b&gt;");
        // A decoded tag is text. It is not looked at again.
        assert_eq!(
            strip("&lt;script&gt;x&lt;/script&gt;"),
            "<script>x</script>"
        );
    }

    #[test]
    fn bad_entities_are_left_as_they_are() {
        assert_eq!(strip("&bogus;"), "&bogus;");
        assert_eq!(strip("&#;"), "&#;");
        assert_eq!(strip("&#x;"), "&#x;");
        assert_eq!(strip("&#xZZ;"), "&#xZZ;");
        assert_eq!(strip("&#-5;"), "&#-5;");
        assert_eq!(strip("&#+5;"), "&#+5;");
        // Not a character at all: a surrogate and a value past the end.
        assert_eq!(strip("&#xD800;"), "&#xD800;");
        assert_eq!(strip("&#x110000;"), "&#x110000;");
        assert_eq!(strip("&#99999999999999999999;"), "&#99999999999999999999;");
        assert_eq!(strip("&amp"), "&amp");
    }

    #[test]
    fn a_tag_that_never_ends_is_text() {
        let long = format!("<a href=\"{}\">x", "y".repeat(TAG_MAX * 2));
        assert!(strip(&long).starts_with("<a href="));
        // A flood of openers costs one bounded look-ahead each.
        let flood = "<".repeat(100_000);
        assert_eq!(strip(&flood).len(), 100_000);
        let flood = "&".repeat(100_000);
        assert_eq!(strip(&flood).len(), 100_000);
    }

    #[test]
    fn multibyte_text_around_tags_does_not_panic() {
        assert_eq!(strip("<b>žluťoučký</b> kůň"), "žluťoučký kůň");
        assert_eq!(strip("<é>"), "<é>");
        assert_eq!(strip("&é;"), "&é;");
        let edge = format!("<{}", "é".repeat(TAG_MAX));
        assert_eq!(strip(&edge), edge);
        let edge = format!("&{}", "日".repeat(ENTITY_MAX));
        assert_eq!(strip(&edge), edge);
    }

    #[test]
    fn decoded_control_characters_are_tidied_away() {
        assert_eq!(tidy(&strip("a&#0;b&#27;c&#x202e;d"), 100, false), "abcd");
    }

    #[test]
    fn clip_bytes_never_splits_a_character() {
        assert_eq!(clip_bytes("héllo", 2), "h");
        assert_eq!(clip_bytes("héllo", 3), "hé");
        assert_eq!(clip_bytes("abc", 10), "abc");
        assert_eq!(clip_bytes("日本", 0), "");
    }
}
