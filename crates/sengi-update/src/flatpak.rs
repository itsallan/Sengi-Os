//! Reading what Flatpak says: the list of apps with an update waiting
//! (`flatpak --user remote-ls --updates`). How far an update has got is
//! read by `sengi_text::progress`, which the Store uses too.

/// An app (or runtime) that has an update waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUpdate {
    pub id: String,
    pub name: String,
    pub version: String,
    /// What is to be downloaded, as Flatpak wrote it: `12.3 MB`.
    pub size: String,
}

/// The columns asked of `remote-ls`, in this order, tab separated.
pub const COLUMNS: &str = "--columns=name,application,version,download-size";

fn app_id(text: &str) -> bool {
    text.contains('.')
        && text.len() <= 255
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The updates out of `remote-ls`'s output. A header line, or a line
/// without an application id in the second column, is skipped.
pub fn parse_remote_ls(output: &str) -> Vec<AppUpdate> {
    let mut found: Vec<AppUpdate> = Vec::new();
    for line in output.lines() {
        let columns: Vec<&str> = line.split('\t').map(str::trim).collect();
        if columns.len() < 2 || !app_id(columns[1]) {
            continue;
        }
        let id = columns[1].to_string();
        // The same app comes once for each remote and branch it has an
        // update on: it is one update to the person.
        if found.iter().any(|app| app.id == id) {
            continue;
        }
        let name = sengi_text::line(columns[0], 80);
        found.push(AppUpdate {
            name: if name.is_empty() { id.clone() } else { name },
            id,
            version: sengi_text::line(columns.get(2).copied().unwrap_or(""), 48),
            size: sengi_text::line(columns.get(3).copied().unwrap_or(""), 24),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_of_updates() {
        let output = "Name\tApplication ID\tVersion\tDownload size\n\
                      GIMP\torg.gimp.GIMP\t2.10.38\t112.4 MB\n\
                      Firefox\torg.mozilla.firefox\t128.0\t60.1 MB\n\
                      Firefox\torg.mozilla.firefox\t128.0\t60.1 MB\n\
                      \torg.freedesktop.Platform\t23.08\t5 MB\n\
                      \n\
                      no tabs here\n";
        let found = parse_remote_ls(output);
        let ids: Vec<&str> = found.iter().map(|app| app.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "org.gimp.GIMP",
                "org.mozilla.firefox",
                "org.freedesktop.Platform"
            ]
        );
        assert_eq!(found[0].name, "GIMP");
        assert_eq!(found[0].version, "2.10.38");
        assert_eq!(found[0].size, "112.4 MB");
        // No name falls back to the id.
        assert_eq!(found[2].name, "org.freedesktop.Platform");
        assert!(parse_remote_ls("").is_empty());
    }
}
