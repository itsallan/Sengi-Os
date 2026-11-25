//! sengi-open: which program opens a file. The decision is made from the
//! name and the first bytes of the file and is a pure function, so it can be
//! tested as a table.

use std::path::{Path, PathBuf};

/// How much of a file is looked at to tell text from anything else.
pub const SNIFF_BYTES: usize = 8192;

/// What opens it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// sengi-edit.
    Edit,
    /// sengi-view.
    View,
    /// sengi-files, for folders.
    Files,
    /// Nothing we ship: the user is told.
    Nothing,
}

impl Target {
    /// The program that does it.
    pub fn program(self) -> Option<&'static str> {
        match self {
            Target::Edit => Some("sengi-edit"),
            Target::View => Some("sengi-view"),
            Target::Files => Some("sengi-files"),
            Target::Nothing => None,
        }
    }
}

const TEXT: &[&str] = &[
    "txt",
    "text",
    "md",
    "markdown",
    "log",
    "rst",
    "adoc",
    "org",
    "rs",
    "toml",
    "json",
    "yaml",
    "yml",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "js",
    "mjs",
    "ts",
    "tsx",
    "jsx",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "go",
    "java",
    "kt",
    "py",
    "rb",
    "pl",
    "lua",
    "php",
    "sh",
    "bash",
    "zsh",
    "fish",
    "conf",
    "cfg",
    "ini",
    "csv",
    "tsv",
    "tex",
    "sql",
    "diff",
    "patch",
    "svg",
    "desktop",
    "service",
    "timer",
    "socket",
    "rules",
    "list",
    "lock",
    "slint",
    "mk",
    "env",
    "gitignore",
    "properties",
];

/// Kinds that are never text, so their first bytes are not even looked at.
const BINARY: &[&str] = &[
    "pdf", "zip", "gz", "xz", "bz2", "zst", "7z", "rar", "tar", "iso", "img", "deb", "rpm", "exe",
    "so", "o", "a", "bin", "mp3", "mp4", "mkv", "webm", "ogg", "wav", "flac", "m4a", "avi", "mov",
    "odt", "ods", "odp", "docx", "xlsx", "pptx", "epub", "ttf", "otf", "woff", "woff2", "webp",
    "bmp", "tif", "tiff", "ico", "heic", "avif", "flatpak", "squashfs", "qcow2", "sqlite", "db",
];

const IMAGES: &[&str] = &["png", "jpg", "jpeg"];

fn extension(path: &Path) -> String {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Whether the bytes begin a PNG or a JPEG file.
pub fn is_image(head: &[u8]) -> bool {
    head.starts_with(b"\x89PNG\r\n\x1a\n") || head.starts_with(&[0xff, 0xd8, 0xff])
}

/// Whether the bytes look like text: UTF-8 with no NUL. A character cut off
/// at the end of the sample is not held against the file.
pub fn looks_like_text(head: &[u8]) -> bool {
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none() && head.len() - error.valid_up_to() < 4,
    }
}

/// What opens `path`, given whether it is a folder and its first bytes.
pub fn decide(path: &Path, is_dir: bool, head: &[u8]) -> Target {
    if is_dir {
        return Target::Files;
    }
    let ext = extension(path);
    if is_image(head) || IMAGES.contains(&ext.as_str()) {
        return Target::View;
    }
    if TEXT.contains(&ext.as_str()) {
        return Target::Edit;
    }
    if BINARY.contains(&ext.as_str()) {
        return Target::Nothing;
    }
    if looks_like_text(head) {
        Target::Edit
    } else {
        Target::Nothing
    }
}

/// The path an argument stands for: a path, or a `file://` address. Any
/// other address is not something this program opens.
pub fn resolve(arg: &str, cwd: &Path) -> Result<PathBuf, String> {
    if let Some(rest) = arg.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        return Ok(sengi_system::trash::decode(rest));
    }
    if let Some((scheme, _)) = arg.split_once(':') {
        let is_scheme = scheme.len() > 1
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if is_scheme {
            return Err(format!("Links ({scheme}:) are not opened by Sengi OS yet"));
        }
    }
    let path = PathBuf::from(arg);
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

/// Reads what `decide` needs from the disk.
pub fn decide_path(path: &Path) -> Result<Target, String> {
    use std::io::Read;
    let meta =
        std::fs::metadata(path).map_err(|_| format!("\"{}\" does not exist.", path.display()))?;
    if meta.is_dir() {
        return Ok(Target::Files);
    }
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    std::fs::File::open(path)
        .and_then(|file| file.take(SNIFF_BYTES as u64).read_to_end(&mut head))
        .map_err(|error| format!("\"{}\" cannot be read: {error}", path.display()))?;
    Ok(decide(path, false, &head))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";
    const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 0x10];

    fn target(name: &str, head: &[u8]) -> Target {
        decide(Path::new(name), false, head)
    }

    #[test]
    fn folders_go_to_files() {
        assert_eq!(decide(Path::new("/h/a.txt"), true, b""), Target::Files);
    }

    #[test]
    fn text_by_extension_in_any_case() {
        for name in [
            "a.txt", "A.TXT", "x.md", "y.log", "z.rs", "c.Toml", "m.json",
        ] {
            assert_eq!(target(name, b"hello"), Target::Edit, "{name}");
        }
    }

    #[test]
    fn images_by_bytes_or_by_name() {
        assert_eq!(target("a.png", PNG), Target::View);
        assert_eq!(target("a.JPG", JPEG), Target::View);
        assert_eq!(target("a.jpeg", b"broken"), Target::View);
        assert_eq!(target("no-extension", PNG), Target::View);
        assert_eq!(target("wrong.txt", JPEG), Target::View);
    }

    #[test]
    fn unknown_names_are_sniffed() {
        assert_eq!(target("README", b"plain words\n"), Target::Edit);
        assert_eq!(target(".bashrc", "naïve\n".as_bytes()), Target::Edit);
        assert_eq!(target("empty", b""), Target::Edit);
        assert_eq!(target("data.dat", &[1, 2, 0, 3]), Target::Nothing);
        assert_eq!(target("data", &[0xff, 0xfe, 0xfd]), Target::Nothing);
    }

    #[test]
    fn a_character_cut_at_the_end_of_the_sample_is_text() {
        let cut = &"abc\u{e9}".as_bytes()[..4];
        assert_eq!(target("notes", cut), Target::Edit);
    }

    #[test]
    fn known_binary_kinds_are_not_sniffed() {
        assert_eq!(target("a.pdf", b"%PDF-1.7 plain looking"), Target::Nothing);
        assert_eq!(target("a.zip", b"PK"), Target::Nothing);
        assert_eq!(target("a.webp", b"RIFF"), Target::Nothing);
    }

    #[test]
    fn addresses_and_paths_resolve() {
        let cwd = Path::new("/home/a");
        assert_eq!(resolve("/x/y.txt", cwd), Ok(PathBuf::from("/x/y.txt")));
        assert_eq!(resolve("y.txt", cwd), Ok(PathBuf::from("/home/a/y.txt")));
        assert_eq!(
            resolve("file:///x/a%20b.txt", cwd),
            Ok(PathBuf::from("/x/a b.txt"))
        );
        assert_eq!(
            resolve("file://localhost/x/y", cwd),
            Ok(PathBuf::from("/x/y"))
        );
        assert!(resolve("https://example.org/", cwd)
            .unwrap_err()
            .contains("https:"));
        assert!(resolve("mailto:a@b.c", cwd).is_err());
        // A name with a colon in it is still a name.
        assert_eq!(resolve("a:b", cwd), Ok(PathBuf::from("/home/a/a:b")));
    }

    #[test]
    fn the_disk_is_read_for_the_decision() {
        let dir = std::env::temp_dir().join(format!("sengi-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let note = dir.join("note");
        std::fs::write(&note, "words").unwrap();
        let pic = dir.join("pic");
        std::fs::write(&pic, PNG).unwrap();
        assert_eq!(decide_path(&note), Ok(Target::Edit));
        assert_eq!(decide_path(&pic), Ok(Target::View));
        assert_eq!(decide_path(&dir), Ok(Target::Files));
        assert!(decide_path(&dir.join("nope")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
