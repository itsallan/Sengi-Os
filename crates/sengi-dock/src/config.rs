//! The dock's configuration: which apps are pinned. Reading and writing
//! `sengi-os/dock.toml` lives in sengi-apps, which the launcher shares, so
//! that "Keep in Dock" there writes the same file.

pub use sengi_apps::pins::{
    parse, parse_with_warnings, paths, paths_from, pinned, render, save, user_path, user_path_from,
    DEFAULT_PINNED,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_default_parses() {
        let text = include_str!("../dock.toml");
        assert_eq!(parse(text).unwrap(), DEFAULT_PINNED);
    }
}
