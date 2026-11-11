//! Remembering the icons tray items named.
//!
//! An item says which icon it wants with `IconName` and, optionally, a
//! directory of its own in `IconThemePath`. Finding and decoding the file
//! is sengi-apps' business and happens in a child process; this is the
//! short memory in front of it.

use std::time::{Duration, Instant};

use super::pixmap::Pixels;

/// How long a found (or not found) icon is remembered. Items flip between
/// a few icons; this is long enough to make that free and short enough
/// that an icon installed or replaced meanwhile shows up soon.
const CACHE_TIME: Duration = Duration::from_secs(30);

/// Answers that are remembered at once.
const CACHE_SIZE: usize = 24;

struct Cached {
    name: String,
    theme_path: String,
    pixels: u32,
    icon: Option<Pixels>,
    at: Instant,
}

/// What the decoder said lately, so that asking again costs no child
/// process and no walk through the theme directories.
#[derive(Default)]
pub struct Cache {
    entries: Vec<Cached>,
}

impl Cache {
    /// The remembered answer: `Some(None)` is "there is no such icon".
    pub fn get(
        &self,
        name: &str,
        theme_path: &str,
        pixels: u32,
        now: Instant,
    ) -> Option<Option<Pixels>> {
        self.entries
            .iter()
            .find(|held| {
                held.pixels == pixels
                    && held.name == name
                    && held.theme_path == theme_path
                    && now.saturating_duration_since(held.at) < CACHE_TIME
            })
            .map(|held| held.icon.clone())
    }

    pub fn put(
        &mut self,
        name: &str,
        theme_path: &str,
        pixels: u32,
        icon: Option<Pixels>,
        now: Instant,
    ) {
        self.entries.retain(|held| {
            now.saturating_duration_since(held.at) < CACHE_TIME
                && !(held.pixels == pixels && held.name == name && held.theme_path == theme_path)
        });
        if self.entries.len() >= CACHE_SIZE {
            self.entries.remove(0);
        }
        self.entries.push(Cached {
            name: name.to_string(),
            theme_path: theme_path.to_string(),
            pixels,
            icon,
            at: now,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_remembered_for_a_while() {
        let start = Instant::now();
        let icon = Pixels {
            size: 16,
            data: vec![1; 16 * 16 * 4],
        };
        let mut cache = Cache::default();
        assert_eq!(cache.get("chat", "", 16, start), None);
        cache.put("chat", "", 16, Some(icon.clone()), start);
        cache.put("missing", "", 16, None, start);
        let soon = start + Duration::from_secs(5);
        assert_eq!(cache.get("chat", "", 16, soon), Some(Some(icon.clone())));
        // Not finding one is an answer too, and saves looking again.
        assert_eq!(cache.get("missing", "", 16, soon), Some(None));
        // Another size, or another app's directory, is another question.
        assert_eq!(cache.get("chat", "", 32, soon), None);
        assert_eq!(cache.get("chat", "/opt/chat", 16, soon), None);
        // After a while the file may have changed.
        assert_eq!(cache.get("chat", "", 16, start + CACHE_TIME), None);
        // A newer answer replaces the old one.
        cache.put("chat", "", 16, None, soon);
        assert_eq!(cache.get("chat", "", 16, soon), Some(None));
    }

    #[test]
    fn the_cache_does_not_grow_without_bound() {
        let start = Instant::now();
        let mut cache = Cache::default();
        for n in 0..CACHE_SIZE * 4 {
            cache.put(&format!("icon-{n}"), "", 16, None, start);
        }
        assert_eq!(cache.entries.len(), CACHE_SIZE);
        assert_eq!(cache.get("icon-0", "", 16, start), None);
        let last = format!("icon-{}", CACHE_SIZE * 4 - 1);
        assert_eq!(cache.get(&last, "", 16, start), Some(None));
    }
}
