//! Reading what apt says. Two things are read: the plan that
//! `apt-get -s upgrade` prints (what would be installed, what would be
//! removed, how much would be downloaded), and the machine-readable
//! status lines that `-o APT::Status-Fd=N` writes while apt works.
//!
//! The formats are the documented ones: apt-get(8) for the simulation,
//! and apt's doc/progress-reporting.md for the status lines
//! (`dlstatus:ID:PERCENT:MESSAGE` and `pmstatus:PACKAGE:PERCENT:MESSAGE`).
//! Nothing here is guessed at: a line that does not fit is left out.

/// One package that an upgrade would install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upgrade {
    pub name: String,
    /// The version now installed, or none for a package that is new
    /// (a dependency the upgrade needs).
    pub from: Option<String>,
    pub to: String,
    /// Comes from a security suite.
    pub security: bool,
}

/// What `apt-get -s upgrade` says it would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub upgrades: Vec<Upgrade>,
    /// Packages it would remove. An update must never do that.
    pub removals: Vec<String>,
    /// Bytes still to download, when apt says.
    pub download: Option<u64>,
}

fn clean(text: &str, max: usize) -> String {
    sengi_text::line(text, max)
}

/// One `Inst` line:
///
/// ```text
/// Inst libssl3t64 [3.0.15-1] (3.0.16-1 Debian-Security:13/stable-security [amd64])
/// Inst libnew1 (1.0-1 Debian:13/stable [amd64])
/// ```
fn parse_inst(rest: &str) -> Option<Upgrade> {
    let rest = rest.trim();
    let (name, mut rest) = rest.split_once(' ')?;
    if name.is_empty() {
        return None;
    }
    rest = rest.trim_start();
    let mut from = None;
    if let Some(after) = rest.strip_prefix('[') {
        let (old, tail) = after.split_once(']')?;
        from = Some(clean(old, 64));
        rest = tail.trim_start();
    }
    let inside = rest.strip_prefix('(')?;
    let inside = &inside[..inside.rfind(')')?];
    let to = inside.split_whitespace().next()?;
    let security = inside.contains("-security") || inside.contains("Security");
    Some(Upgrade {
        name: clean(name, 80),
        from,
        to: clean(to, 64),
        security,
    })
}

/// `36.2 MB`, `512 kB`, `0 B`: apt's sizes, which are powers of 1000.
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim().replace(',', "");
    let (number, unit) = match text.split_once(' ') {
        Some((number, unit)) => (number, unit.trim()),
        None => {
            let at = text.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
            (&text[..at], &text[at..])
        }
    };
    let number: f64 = number.parse().ok()?;
    let scale = match unit {
        "B" => 1.0,
        "kB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    Some((number * scale) as u64)
}

/// A size the way people read it: `36 MB`, `1.2 GB`, `512 kB`.
pub fn format_size(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", b / 1e9)
    } else if bytes >= 10_000_000 {
        format!("{:.0} MB", b / 1e6)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", b / 1e6)
    } else if bytes >= 1_000 {
        format!("{:.0} kB", b / 1e3)
    } else {
        format!("{bytes} B")
    }
}

/// The plan out of `apt-get -s upgrade`'s output.
pub fn parse_plan(output: &str) -> Plan {
    let mut plan = Plan::default();
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Inst ") {
            if let Some(upgrade) = parse_inst(rest) {
                plan.upgrades.push(upgrade);
            }
        } else if let Some(rest) = line.strip_prefix("Remv ") {
            if let Some(name) = rest.split_whitespace().next() {
                plan.removals.push(clean(name, 80));
            }
        } else if let Some(rest) = line.strip_prefix("Need to get ") {
            // "Need to get 0 B/36.2 MB of archives." (some of it is in
            // the cache already) or "Need to get 36.2 MB of archives."
            if let Some((sizes, _)) = rest.split_once(" of archives") {
                let total = sizes.rsplit('/').next().unwrap_or(sizes);
                plan.download = parse_size(total);
            }
        }
    }
    plan
}

/// One line of what apt writes to its status descriptor.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// Fetching files: `Retrieving file 2 of 6`.
    Download {
        percent: f32,
        file: Option<(u32, u32)>,
        message: String,
    },
    /// Unpacking and configuring.
    Install {
        package: String,
        percent: f32,
        message: String,
    },
    /// dpkg reported an error for a package.
    Error { package: String, message: String },
}

fn percent_of(text: &str) -> Option<f32> {
    let value: f32 = text.trim().parse().ok()?;
    value.is_finite().then(|| value.clamp(0.0, 100.0))
}

/// "Retrieving file 2 of 6 (3s remaining)" gives (2, 6).
fn file_of(message: &str) -> Option<(u32, u32)> {
    let after = &message[message.find("file ")? + 5..];
    let mut words = after.split_whitespace();
    let n: u32 = words.next()?.parse().ok()?;
    if words.next()? != "of" {
        return None;
    }
    let m: u32 = words
        .next()?
        .trim_end_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .ok()?;
    (n >= 1 && m >= n).then_some((n, m))
}

pub fn parse_status(line: &str) -> Option<Status> {
    let mut parts = line.trim().splitn(4, ':');
    let kind = parts.next()?;
    let id = parts.next()?;
    let percent = parts.next()?;
    let message = parts.next().unwrap_or("");
    match kind {
        "dlstatus" => Some(Status::Download {
            percent: percent_of(percent)?,
            file: file_of(message),
            message: clean(message, 100),
        }),
        "pmstatus" => Some(Status::Install {
            package: clean(id, 80),
            percent: percent_of(percent)?,
            message: clean(message, 100),
        }),
        "pmerror" => Some(Status::Error {
            package: clean(id, 80),
            message: clean(message, 160),
        }),
        _ => None,
    }
}

/// Which package of an install is being worked on, out of how many. The
/// status lines name each package as it is reached; a package named for
/// the first time is the next one.
pub struct Steps {
    total: u32,
    seen: Vec<String>,
}

impl Steps {
    pub fn new(total: u32) -> Steps {
        Steps {
            total,
            seen: Vec::new(),
        }
    }

    /// Notes a package and says "n of m".
    pub fn reach(&mut self, package: &str) -> Option<(u32, u32)> {
        if package == "dpkg-exec" || package.is_empty() || self.total == 0 {
            return self.current();
        }
        if !self.seen.iter().any(|seen| seen == package) {
            self.seen.push(package.to_string());
        }
        self.current()
    }

    fn current(&self) -> Option<(u32, u32)> {
        let n = self.seen.len() as u32;
        (n >= 1 && self.total >= 1).then(|| (n.min(self.total), self.total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // What apt-get -s --with-new-pkgs upgrade prints, as apt-get(8) and
    // the Debian archive give it, shortened.
    pub const SIMULATION: &str = "\
NOTE: This is only a simulation!
      apt-get needs root privileges for real execution.
Reading package lists...
Building dependency tree...
Calculating upgrade...
The following NEW packages will be installed:
  libnew1
The following packages will be upgraded:
  libssl3t64 openssl tzdata
3 upgraded, 1 newly installed, 0 to remove and 0 not upgraded.
Need to get 2,049 kB/36.2 MB of archives.
After this operation, 1,024 kB of additional disk space will be used.
Inst libssl3t64 [3.0.15-1~deb13u1] (3.0.16-1~deb13u1 Debian-Security:13/stable-security [amd64])
Inst openssl [3.0.15-1~deb13u1] (3.0.16-1~deb13u1 Debian-Security:13/stable-security [amd64]) []
Inst libnew1 (1.0-1 Debian:13.1/stable [amd64])
Inst tzdata [2024a-4] (2025b-1 Debian:13.1/stable [all])
Conf libssl3t64 (3.0.16-1~deb13u1 Debian-Security:13/stable-security [amd64])
";

    #[test]
    fn the_plan_is_read_from_a_simulation() {
        let plan = parse_plan(SIMULATION);
        assert_eq!(plan.upgrades.len(), 4);
        let ssl = &plan.upgrades[0];
        assert_eq!(ssl.name, "libssl3t64");
        assert_eq!(ssl.from.as_deref(), Some("3.0.15-1~deb13u1"));
        assert_eq!(ssl.to, "3.0.16-1~deb13u1");
        assert!(ssl.security);
        // Trailing brackets after the closing parenthesis do not matter.
        assert_eq!(plan.upgrades[1].name, "openssl");
        assert!(plan.upgrades[1].security);
        let new = &plan.upgrades[2];
        assert_eq!(new.name, "libnew1");
        assert_eq!(new.from, None);
        assert_eq!(new.to, "1.0-1");
        assert!(!new.security);
        assert!(!plan.upgrades[3].security);
        assert!(plan.removals.is_empty());
        assert_eq!(plan.download, Some(36_200_000));
    }

    #[test]
    fn removals_are_seen() {
        let plan = parse_plan("Remv oldlib [1.0]\nInst a [1] (2 Debian:13/stable [amd64])\n");
        assert_eq!(plan.removals, ["oldlib"]);
        assert_eq!(plan.upgrades.len(), 1);
    }

    #[test]
    fn nothing_to_do_is_an_empty_plan() {
        let plan = parse_plan("Reading package lists...\n0 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.\n");
        assert_eq!(plan, Plan::default());
    }

    #[test]
    fn sizes_are_apts() {
        assert_eq!(parse_size("36.2 MB"), Some(36_200_000));
        assert_eq!(parse_size("512 kB"), Some(512_000));
        assert_eq!(parse_size("1,234 kB"), Some(1_234_000));
        assert_eq!(parse_size("0 B"), Some(0));
        assert_eq!(parse_size("2GB"), Some(2_000_000_000));
        assert_eq!(parse_size("many"), None);
        assert_eq!(parse_size("3 parsecs"), None);
        assert_eq!(
            parse_plan("Need to get 36.2 MB of archives.\n").download,
            Some(36_200_000)
        );
        assert_eq!(format_size(36_200_000), "36 MB");
        assert_eq!(format_size(2_500_000), "2.5 MB");
        assert_eq!(format_size(1_200_000_000), "1.2 GB");
        assert_eq!(format_size(512_000), "512 kB");
        assert_eq!(format_size(12), "12 B");
    }

    #[test]
    fn hostile_names_are_cleaned() {
        let plan = parse_plan("Inst evil\u{202e}\u{1b}[31m [1] (2 Debian:13/stable [amd64])\n");
        assert_eq!(plan.upgrades.len(), 1);
        assert!(!plan.upgrades[0].name.contains('\u{202e}'));
        assert!(!plan.upgrades[0].name.contains('\u{1b}'));
        assert!(parse_plan("Inst\nInst  \nInst x\nInst x [1\n")
            .upgrades
            .is_empty());
    }

    #[test]
    fn download_lines_give_percent_and_file() {
        assert_eq!(
            parse_status("dlstatus:2:33.3333:Retrieving file 2 of 6 (3s remaining)"),
            Some(Status::Download {
                percent: 33.3333,
                file: Some((2, 6)),
                message: "Retrieving file 2 of 6 (3s remaining)".to_string(),
            })
        );
        let Some(Status::Download { file, percent, .. }) =
            parse_status("dlstatus:1:0.0000:Retrieving file 1 of 3")
        else {
            panic!("a download line");
        };
        assert_eq!(file, Some((1, 3)));
        assert_eq!(percent, 0.0);
    }

    #[test]
    fn install_lines_give_package_and_percent() {
        assert_eq!(
            parse_status(
                "pmstatus:libssl3t64:41.6667:Unpacking libssl3t64 (3.0.16-1) over (3.0.15-1) ..."
            ),
            Some(Status::Install {
                package: "libssl3t64".to_string(),
                percent: 41.6667,
                message: "Unpacking libssl3t64 (3.0.16-1) over (3.0.15-1) ...".to_string(),
            })
        );
        assert_eq!(
            parse_status("pmstatus:dpkg-exec:0.0000:Running dpkg"),
            Some(Status::Install {
                package: "dpkg-exec".to_string(),
                percent: 0.0,
                message: "Running dpkg".to_string(),
            })
        );
        assert_eq!(
            parse_status("pmerror:openssl:50.0000:trouble: disk full"),
            Some(Status::Error {
                package: "openssl".to_string(),
                message: "trouble: disk full".to_string(),
            })
        );
    }

    #[test]
    fn lines_that_do_not_fit_are_left_out() {
        assert_eq!(
            parse_status("Get:1 http://deb.debian.org/debian trixie InRelease"),
            None
        );
        assert_eq!(parse_status("dlstatus:1:lots:Retrieving"), None);
        assert_eq!(parse_status("pmconffile:'/etc/x'"), None);
        assert_eq!(parse_status(""), None);
        // A percentage outside 0 to 100 is held to it.
        let Some(Status::Install { percent, .. }) = parse_status("pmstatus:a:250:x") else {
            panic!("an install line");
        };
        assert_eq!(percent, 100.0);
        assert_eq!(file_of("Retrieving file 3 of 2"), None);
    }

    #[test]
    fn steps_count_packages_as_they_are_reached() {
        let mut steps = Steps::new(3);
        assert_eq!(steps.reach("dpkg-exec"), None);
        assert_eq!(steps.reach("a"), Some((1, 3)));
        assert_eq!(steps.reach("a"), Some((1, 3)));
        assert_eq!(steps.reach("b"), Some((2, 3)));
        assert_eq!(steps.reach("c"), Some((3, 3)));
        // More names than were planned stay at the end.
        assert_eq!(steps.reach("d"), Some((3, 3)));
        assert_eq!(Steps::new(0).reach("a"), None);
    }
}
