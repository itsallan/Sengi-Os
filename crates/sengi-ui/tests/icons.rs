//! Every icon the interface names has a file, and the generated table of
//! paths knows it.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files_under(dir: &Path, extension: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                files_under(&path, extension, found);
            }
        } else if path.extension().is_some_and(|e| e == extension) {
            found.push(path);
        }
    }
}

/// The names of the icon files, without the weight at the end.
fn icon_files() -> BTreeSet<String> {
    let mut files = Vec::new();
    files_under(&root().join("ui/icons/phosphor/bold"), "svg", &mut files);
    files
        .iter()
        .filter_map(|path| path.file_name()?.to_str()?.strip_suffix("-bold.svg"))
        .map(str::to_string)
        .collect()
}

fn without_comments(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The string literals in a `name:` binding, which is how a Slint source
/// names an icon. A literal that follows `==` is something compared with,
/// not an icon.
fn icon_names(source: &str) -> Vec<String> {
    let text = without_comments(source);
    let mut names = Vec::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find("name:") {
        let before = rest[..at].chars().next_back();
        rest = &rest[at + "name:".len()..];
        if before.is_some_and(|c| c.is_alphanumeric() || c == '-') {
            continue;
        }
        // `in property <string> name: "..."` declares a property, it does
        // not name an icon.
        if text[..text.len() - rest.len() - "name:".len()]
            .trim_end()
            .ends_with('>')
        {
            continue;
        }
        let end = rest.find([';', ',', '}']).unwrap_or(rest.len());
        let statement = &rest[..end];
        let mut parts = statement.split('"');
        let mut previous = parts.next().unwrap_or("");
        while let (Some(literal), Some(after)) = (parts.next(), parts.next()) {
            if !previous.trim_end().ends_with("==") {
                names.push(literal.to_string());
            }
            previous = after;
        }
    }
    names
}

fn slint_sources() -> Vec<PathBuf> {
    let mut found = Vec::new();
    files_under(&root().join("ui"), "slint", &mut found);
    files_under(&root().join("ui"), "in", &mut found);
    files_under(&root().join("crates"), "slint", &mut found);
    found.retain(|path| {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        name != "icon-paths.slint" && name != "tokens.slint" && name != "tokens.slint.in"
    });
    found
}

#[test]
fn every_icon_the_interface_names_has_a_file() {
    let files = icon_files();
    assert!(
        files.len() > 10,
        "the icon files are in ui/icons/phosphor/bold"
    );
    // Settings draws some names itself, in NamedIcon, see the next tests.
    let named = fs::read_to_string(root().join("crates/sengi-settings/ui/icons.slint")).unwrap();
    let mut used = 0;
    for path in slint_sources() {
        let source = fs::read_to_string(&path).unwrap();
        for name in icon_names(&source) {
            used += 1;
            assert!(
                files.contains(&name) || named.contains(&format!("root.name == \"{name}\"")),
                "{} names the icon {name:?}, which has no file",
                path.display()
            );
        }
    }
    assert!(used > 10, "found the icon names in the sources ({used})");
}

#[test]
fn the_generated_table_has_every_file() {
    let table = fs::read_to_string(root().join("ui/icon-paths.slint")).unwrap();
    for name in icon_files() {
        assert!(
            table.contains(&format!("name == \"{name}\" ?")),
            "{name} is missing from ui/icon-paths.slint; run scripts/render-icons.sh"
        );
    }
}

/// The names Settings hands to its icons as data are either a file or one
/// of the names `NamedIcon` maps.
#[test]
fn the_icons_settings_asks_for_are_drawn() {
    let files = icon_files();
    let icons = fs::read_to_string(root().join("crates/sengi-settings/ui/icons.slint")).unwrap();
    let mapped =
        |name: &str| files.contains(name) || icons.contains(&format!("root.name == \"{name}\""));
    let mut sources = Vec::new();
    files_under(
        &root().join("crates/sengi-settings/src"),
        "rs",
        &mut sources,
    );
    let mut asked = 0;
    for path in sources {
        let text = fs::read_to_string(&path).unwrap();
        let mut rest = text.as_str();
        while let Some(at) = rest.find("icon: \"") {
            rest = &rest[at + "icon: \"".len()..];
            let name = rest.split('"').next().unwrap_or("");
            asked += 1;
            assert!(mapped(name), "{} asks for {name:?}", path.display());
        }
        // The sidebar's pages, in `fn icon`.
        if let Some(at) = text.find("fn icon(page: Page)") {
            for line in text[at..].lines().skip(1).take(14) {
                if let Some((_, name)) = line.split_once("=> \"") {
                    let name = name.split('"').next().unwrap_or("");
                    asked += 1;
                    assert!(mapped(name), "the page icon {name:?} is not drawn");
                }
            }
        }
    }
    assert!(asked > 10, "found the names Settings asks for ({asked})");
}
