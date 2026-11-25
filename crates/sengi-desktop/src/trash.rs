//! Moving to the Trash. The moving itself, and what to say when it fails,
//! are `sengi_system::trash`, which Files shares; what is here is only
//! where the desktop finds the Trash.

use std::path::PathBuf;

use sengi_apps::Env;

pub use sengi_system::trash::{explain, now, trash};

/// `$XDG_DATA_HOME/Trash`, which is `~/.local/share/Trash` unless set.
pub fn trash_dir(env: &Env) -> PathBuf {
    env.data_home.join("Trash")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_trash_is_under_the_data_home() {
        let env = Env::from_vars(|name| match name {
            "HOME" => Some("/home/ada".into()),
            _ => None,
        });
        assert_eq!(trash_dir(&env), Path::new("/home/ada/.local/share/Trash"));
    }
}
