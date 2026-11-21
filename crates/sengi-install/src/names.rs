//! What may be typed for a user name, a full name, a computer name and a
//! password. The window checks with these as it is typed into, and the
//! program that installs checks again: it trusts no plan.

/// The longest user name useradd takes.
pub const MAX_USER_NAME: usize = 32;
pub const MAX_FULL_NAME: usize = 64;
/// One label of a host name.
pub const MAX_HOST_NAME: usize = 63;
/// PAM's limit, less the byte that ends the string. The lock screen and
/// the login screen take no more than this either.
pub const MAX_PASSWORD_BYTES: usize = 511;

/// Names no person's account may have, whatever the passwd file says:
/// the account of the live image, which the installer removes, and ones
/// that mean something to the system.
const RESERVED: [&str; 6] = ["root", "live", "admin", "sudo", "nobody", "sengi"];

/// Why `name` cannot be a user name, in words for the person typing it.
/// The rule is Debian's: a small letter, then small letters, digits,
/// hyphens and underscores.
pub fn user_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err("Choose a user name".to_string());
    };
    if !first.is_ascii_lowercase() {
        return Err("A user name starts with a small letter".to_string());
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err("Use only small letters, digits, hyphens and underscores".to_string());
    }
    if name.len() > MAX_USER_NAME {
        return Err(format!(
            "A user name has at most {MAX_USER_NAME} characters"
        ));
    }
    if RESERVED.contains(&name) {
        return Err(format!("\u{201c}{name}\u{201d} is kept for the system"));
    }
    Ok(())
}

/// Whether `name` is already the name of an account or a group in files
/// of the form of /etc/passwd and /etc/group.
pub fn taken(name: &str, passwd: &str, group: &str) -> bool {
    passwd
        .lines()
        .chain(group.lines())
        .any(|line| line.split(':').next() == Some(name))
}

/// Why `name` cannot be a person's full name. It may be empty. It goes
/// into the passwd file, where a colon ends a field and a comma ends the
/// name.
pub fn full_name(name: &str) -> Result<(), String> {
    if name.chars().count() > MAX_FULL_NAME {
        return Err(format!("A name has at most {MAX_FULL_NAME} characters"));
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, ':' | ',' | '='))
    {
        return Err("A name cannot have a colon, a comma or an equals sign in it".to_string());
    }
    if name != name.trim() {
        return Err("A name cannot start or end with a space".to_string());
    }
    Ok(())
}

/// Why `name` cannot be the computer's name: one label of a host name,
/// in small letters.
pub fn host_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Choose a name for the computer".to_string());
    }
    if name.len() > MAX_HOST_NAME {
        return Err(format!(
            "A computer name has at most {MAX_HOST_NAME} characters"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err("Use only small letters, digits and hyphens".to_string());
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err("A computer name cannot start or end with a hyphen".to_string());
    }
    Ok(())
}

/// Why `password` cannot be a password. Nothing is said about how good
/// it is: that is its owner's business.
pub fn password(password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("Choose a password".to_string());
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err("That password is too long".to_string());
    }
    if password.chars().any(char::is_control) {
        return Err("A password cannot have control characters in it".to_string());
    }
    Ok(())
}

/// A user name to offer for a full name: its first word, in small
/// letters, without what a user name cannot hold. Empty if nothing is
/// left.
pub fn suggest_user_name(full: &str) -> String {
    let word: String = full
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .skip_while(char::is_ascii_digit)
        .take(MAX_USER_NAME)
        .collect();
    if user_name(&word).is_ok() {
        word
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_names_follow_debians_rule() {
        for good in ["allan", "a", "a1", "ada-l", "x_y", &"a".repeat(32)] {
            assert_eq!(user_name(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            "Allan",
            "1allan",
            "-allan",
            "_allan",
            "al lan",
            "allan!",
            "all\u{e4}n",
            "a:b",
            "a/b",
            "a\n",
            &"a".repeat(33),
        ] {
            assert!(user_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_systems_names_are_not_for_people() {
        for name in ["root", "live", "sudo", "nobody"] {
            assert!(user_name(name).is_err(), "{name}");
        }
        let passwd = "root:x:0:0:root:/root:/bin/bash\n_greetd:x:999:999::/:/usr/sbin/nologin\n";
        let group = "audio:x:29:live\nvideo:x:44:\n";
        assert!(taken("_greetd", passwd, group));
        assert!(taken("audio", passwd, group));
        assert!(!taken("allan", passwd, group));
        // A member of a group is not a name that is taken.
        assert!(!taken("x", passwd, group));
    }

    #[test]
    fn full_names_fit_the_passwd_file() {
        for good in [
            "",
            "Allan",
            "Ada Lovelace",
            "Zo\u{eb} Nakato",
            "\u{41e}\u{43b}\u{435}\u{43d}\u{430} \u{428}\u{435}\u{432}\u{447}\u{435}\u{43d}\u{43a}\u{43e}",
            "\u{7530}\u{4e2d} \u{82b1}\u{5b50}",
            "Jos\u{e9} \u{c5}ngstr\u{f6}m",
            "O'Neil",
        ] {
            assert_eq!(full_name(good), Ok(()), "{good}");
        }
        for bad in [
            "a:b",
            "Last, First",
            "a=b",
            "tab\there",
            " Allan",
            "Allan ",
            "line\nbreak",
        ] {
            assert!(full_name(bad).is_err(), "{bad:?}");
        }
        assert!(full_name(&"n".repeat(65)).is_err());
    }

    #[test]
    fn host_names_are_one_plain_label() {
        for good in ["zenbook", "a", "pc-1", "9lives", &"a".repeat(63)] {
            assert_eq!(host_name(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            "ZenBook",
            "-pc",
            "pc-",
            "my pc",
            "pc.local",
            "pc_1",
            "caf\u{e9}",
            &"a".repeat(64),
        ] {
            assert!(host_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn passwords_are_one_line_that_pam_can_take() {
        assert_eq!(password("correct horse: battery"), Ok(()));
        assert_eq!(password("p\u{e4}ss"), Ok(()));
        assert!(password("").is_err());
        assert!(password("two\nlines").is_err());
        assert!(password(&"p".repeat(512)).is_err());
        assert_eq!(password(&"p".repeat(511)), Ok(()));
    }

    #[test]
    fn a_user_name_is_offered_for_a_full_name() {
        assert_eq!(suggest_user_name("Allan Smith"), "allan");
        assert_eq!(suggest_user_name("  Ada"), "ada");
        assert_eq!(suggest_user_name("Jos\u{e9} M"), "jos");
        assert_eq!(suggest_user_name("4chan user"), "chan");
        assert_eq!(suggest_user_name("Root"), "");
        assert_eq!(suggest_user_name("\u{674e}"), "");
        assert_eq!(suggest_user_name(""), "");
    }
}
