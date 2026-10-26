//! Where gh-prs keeps its files: `$XDG_CONFIG_HOME/gh-prs/`, defaulting to
//! `~/.config/gh-prs/`.

use std::ffi::OsString;
use std::path::PathBuf;

/// `$XDG_CONFIG_HOME/gh-prs/<name>`, or `~/.config/gh-prs/<name>`.
#[must_use]
pub fn store_path(name: &str) -> PathBuf {
    path_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::home_dir(),
        name,
    )
}

/// [`store_path`] with the environment passed in, so tests don't have to
/// mutate the process environment. An empty `XDG_CONFIG_HOME` counts as
/// unset, as the XDG spec says.
fn path_from(xdg_config_home: Option<OsString>, home: Option<PathBuf>, name: &str) -> PathBuf {
    let config_home = xdg_config_home
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.unwrap_or_default().join(".config"));
    config_home.join("gh-prs").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_xdg_config_home() {
        let path = path_from(Some("/xdg".into()), Some("/home/me".into()), "config.json");
        assert_eq!(path, PathBuf::from("/xdg/gh-prs/config.json"));
    }

    #[test]
    fn falls_back_to_home_config() {
        for xdg in [None, Some(OsString::new())] {
            let path = path_from(xdg, Some("/home/me".into()), "snooze.json");
            assert_eq!(path, PathBuf::from("/home/me/.config/gh-prs/snooze.json"));
        }
    }
}
