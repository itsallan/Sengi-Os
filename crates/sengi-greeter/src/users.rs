//! Who can log in here.
//!
//! The accounts offered are the people's: those in the passwd database
//! with a user number from 1000 to 59999, which is where Debian puts
//! them, and a shell that is a shell. The database is read through the C
//! library, so accounts that come from somewhere other than /etc/passwd
//! are found too.

use std::ffi::CStr;
use std::fs;

use sengi_lock::user::{shown_name, User};

/// Debian's range for the accounts of people (`UID_MIN` and `UID_MAX` in
/// /etc/login.defs and in adduser's configuration).
const FIRST_UID: u32 = 1000;
const LAST_UID: u32 = 59999;

/// The shells the system has, one path a line.
const SHELLS: &str = "/etc/shells";

/// More accounts than this are not listed. A machine with so many wants
/// the name typed, not picked.
const MOST: usize = 64;

/// One line of the passwd database, as far as it matters here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub login: String,
    pub uid: u32,
    pub gecos: String,
    pub shell: String,
}

/// The shells listed in `text`, which has the form of /etc/shells.
pub fn shells(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or_default().trim())
        .filter(|line| line.starts_with('/'))
        .map(str::to_string)
        .collect()
}

/// Whether `shell` lets anyone in. With a list of the system's shells it
/// has to be on it. In any case it must not be one of the programs that
/// exist to turn a login away.
fn is_login_shell(shell: &str, known: Option<&[String]>) -> bool {
    let name = shell.rsplit('/').next().unwrap_or_default();
    if shell.is_empty() || matches!(name, "nologin" | "false" | "") {
        return false;
    }
    known.is_none_or(|known| known.iter().any(|listed| listed == shell))
}

/// The accounts out of `accounts` that are offered, in the order they
/// came, each login name once.
pub fn offered(accounts: &[Account], known_shells: Option<&[String]>) -> Vec<User> {
    let mut users: Vec<User> = Vec::new();
    for account in accounts {
        if !(FIRST_UID..=LAST_UID).contains(&account.uid)
            || account.login.is_empty()
            || !is_login_shell(&account.shell, known_shells)
            || users.iter().any(|user| user.login == account.login)
        {
            continue;
        }
        if users.len() == MOST {
            break;
        }
        users.push(User {
            shown: shown_name(&account.login, &account.gecos),
            login: account.login.clone(),
        });
    }
    // Two people with one full name are told apart by their login names.
    let shown: Vec<String> = users.iter().map(|user| user.shown.clone()).collect();
    for user in &mut users {
        if shown.iter().filter(|name| **name == user.shown).count() > 1 {
            user.shown = sengi_text::line(&format!("{} ({})", user.shown, user.login), 64);
        }
    }
    users
}

/// Every entry of the passwd database. Not to be called while another
/// thread may be reading the database: the C library keeps its place in
/// it in one shared spot.
fn database() -> Vec<Account> {
    let mut accounts = Vec::new();
    // SAFETY: called once, from `main`, before any thread is started. The
    // entry and its strings are valid until the next call, and are copied
    // before it.
    unsafe {
        libc::setpwent();
        loop {
            let entry = libc::getpwent();
            if entry.is_null() || accounts.len() >= 65536 {
                break;
            }
            let text = |pointer: *const libc::c_char| {
                if pointer.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(pointer).to_string_lossy().into_owned()
                }
            };
            accounts.push(Account {
                login: text((*entry).pw_name),
                uid: (*entry).pw_uid,
                gecos: text((*entry).pw_gecos),
                shell: text((*entry).pw_shell),
            });
        }
        libc::endpwent();
    }
    accounts
}

/// The accounts this machine offers at the login screen.
pub fn logins() -> Vec<User> {
    let known = fs::read_to_string(SHELLS).ok().map(|text| shells(&text));
    offered(&database(), known.as_deref())
}

/// A name typed at the login screen, as it is sent to greetd: one line,
/// no spaces around it, not endless. Empty if nothing usable was typed.
pub fn typed_name(text: &str) -> String {
    sengi_text::line(text, MAX_NAME)
}

/// The longest login name taken, in characters. Linux's own limit is 32.
pub const MAX_NAME: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    fn account(login: &str, uid: u32, gecos: &str, shell: &str) -> Account {
        Account {
            login: login.to_string(),
            uid,
            gecos: gecos.to_string(),
            shell: shell.to_string(),
        }
    }

    fn names(users: &[User]) -> Vec<(&str, &str)> {
        users
            .iter()
            .map(|user| (user.login.as_str(), user.shown.as_str()))
            .collect()
    }

    #[test]
    fn people_are_offered_and_the_system_s_own_accounts_are_not() {
        let known = shells("# /etc/shells: valid login shells\n/bin/sh\n/bin/bash\n/usr/bin/bash\n");
        let accounts = [
            account("root", 0, "root", "/bin/bash"),
            account("daemon", 1, "daemon", "/usr/sbin/nologin"),
            account("_greetd", 103, "", "/usr/sbin/nologin"),
            account("almost", 999, "", "/bin/bash"),
            account("live", 1000, "Sengi OS", "/bin/bash"),
            account("allan", 1001, "Allan Example,,,", "/usr/bin/bash"),
            account("guest", 1002, "", "/bin/sh"),
            account("shut", 1003, "Shut Out", "/usr/sbin/nologin"),
            account("barred", 1004, "", "/bin/false"),
            account("odd", 1005, "", "/opt/not-a-shell"),
            account("none", 1006, "", ""),
            account("last", 59999, "", "/bin/bash"),
            account("nobody", 65534, "nobody", "/usr/sbin/nologin"),
            account("beyond", 60000, "", "/bin/bash"),
        ];
        assert_eq!(
            names(&offered(&accounts, Some(&known))),
            [
                ("live", "Sengi OS"),
                ("allan", "Allan Example"),
                ("guest", "guest"),
                ("last", "last")
            ]
        );
    }

    #[test]
    fn with_no_list_of_shells_any_shell_but_a_refusal_will_do() {
        let accounts = [
            account("a", 1000, "", "/opt/fish"),
            account("b", 1001, "", "/usr/sbin/nologin"),
            account("c", 1002, "", "/bin/false"),
        ];
        assert_eq!(names(&offered(&accounts, None)), [("a", "a")]);
        // A list that has nologin on it does not make it a way in.
        let known = shells("/usr/sbin/nologin\n/opt/fish\n");
        assert_eq!(names(&offered(&accounts, Some(&known))), [("a", "a")]);
    }

    #[test]
    fn a_name_is_listed_once_and_like_names_are_told_apart() {
        let accounts = [
            account("sam", 1000, "Sam Smith", "/bin/sh"),
            account("sam", 1000, "Sam Smith", "/bin/sh"),
            account("sam2", 1001, "Sam Smith", "/bin/sh"),
            account("kim", 1002, "Kim", "/bin/sh"),
        ];
        assert_eq!(
            names(&offered(&accounts, None)),
            [
                ("sam", "Sam Smith (sam)"),
                ("sam2", "Sam Smith (sam2)"),
                ("kim", "Kim")
            ]
        );
    }

    #[test]
    fn the_list_has_an_end() {
        let accounts: Vec<Account> = (0..200)
            .map(|n| account(&format!("u{n}"), 1000 + n, "", "/bin/sh"))
            .collect();
        assert_eq!(offered(&accounts, None).len(), MOST);
    }

    #[test]
    fn the_file_of_shells_is_read_as_the_system_reads_it() {
        assert_eq!(
            shells("# comment\n\n/bin/sh\n  /bin/bash  # the usual\nrelative\n"),
            ["/bin/sh", "/bin/bash"]
        );
    }

    #[test]
    fn a_typed_name_is_one_clean_line() {
        assert_eq!(typed_name("  allan "), "allan");
        assert_eq!(typed_name("al\nlan\u{202e}"), "al lan");
        assert_eq!(typed_name("   "), "");
        assert!(typed_name(&"n".repeat(500)).chars().count() <= MAX_NAME);
    }

    #[test]
    fn the_database_can_be_read() {
        // Whatever is in it, reading it ends and filters cleanly.
        let all = database();
        let _ = offered(&all, None);
        assert!(all.iter().any(|account| account.uid == 0) || all.is_empty());
    }
}
