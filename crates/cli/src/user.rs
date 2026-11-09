//! The invoking user's config, cache and data directories: the XDG variables
//! first, then the conventional home-directory fallbacks. One definition for
//! the settings file, the local cache, the tool store's mise data and the
//! `--remote` credential stores.
use std::{ffi::OsString, path::PathBuf};

const HOME: &str = "HOME";
const XDG_CONFIG: &str = "XDG_CONFIG_HOME";
const XDG_CACHE: &str = "XDG_CACHE_HOME";
const XDG_DATA: &str = "XDG_DATA_HOME";
const MISE_DATA: &str = "MISE_DATA_DIR";

fn var(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

fn home(var: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    var(HOME).filter(|home| !home.is_empty()).map(PathBuf::from)
}

/// `patchula/settings.pkl` in the user config directory, when there is one.
pub fn settings_file() -> Option<PathBuf> {
    file("settings.pkl")
}

/// A `patchula/<name>` file in the user config directory, when there is one.
pub fn file(name: &str) -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("patchula").join(name))
}

/// The user config directory: an absolute `XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_with(var)
}

fn config_dir_with(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    var(XDG_CONFIG)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home(&var).map(|home| home.join(".config")))
}

/// The platform's user cache directory: `~/Library/Caches` on macOS, else an
/// absolute `XDG_CACHE_HOME` or `~/.cache`.
pub fn cache_dir() -> Option<PathBuf> {
    cache_dir_with(var)
}

fn cache_dir_with(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let home = home(&var);
    if cfg!(target_os = "macos") {
        return home.map(|home| home.join("Library/Caches"));
    }
    var(XDG_CACHE)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| home.map(|home| home.join(".cache")))
}

/// The invoking user's mise data directory, when it has installs. Local runs
/// borrow matching tool versions from it instead of downloading them.
pub fn mise_data() -> Option<PathBuf> {
    mise_data_with(var)
}

fn mise_data_with(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    var(MISE_DATA)
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            var(XDG_DATA)
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join("mise"))
        })
        .or_else(|| home(&var).map(|home| home.join(".local/share/mise")))
        .filter(|dir| dir.join("installs").is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars<'a>(list: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        let vars: HashMap<String, OsString> = list
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn cache_dir_follows_xdg_then_home() {
        let lookup = |list: &[(&str, &str)]| cache_dir_with(vars(list));
        assert_eq!(
            lookup(&[("XDG_CACHE_HOME", "/xdg"), ("HOME", "/home/count")]),
            Some(PathBuf::from("/xdg"))
        );
        assert_eq!(
            lookup(&[("XDG_CACHE_HOME", "relative"), ("HOME", "/home/count")]),
            Some(PathBuf::from("/home/count/.cache"))
        );
        assert_eq!(lookup(&[("HOME", "")]), None);
        assert_eq!(lookup(&[]), None);
    }

    #[test]
    fn config_dir_needs_an_absolute_xdg_or_a_home() {
        assert_eq!(
            config_dir_with(vars(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/h")])),
            Some(PathBuf::from("/xdg"))
        );
        assert_eq!(
            config_dir_with(vars(&[("XDG_CONFIG_HOME", "rel"), ("HOME", "/h")])),
            Some(PathBuf::from("/h/.config"))
        );
        assert_eq!(config_dir_with(vars(&[])), None);
        assert_eq!(
            settings_file_with(vars(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.config/patchula/settings.pkl"))
        );
    }

    fn settings_file_with(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
        config_dir_with(var).map(|dir| dir.join("patchula").join("settings.pkl"))
    }

    #[test]
    fn mise_data_needs_absolute_dirs_with_installs() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let mise = dir.path().join("mise-data");
        let xdg = dir.path().join("xdg");
        for base in [
            home.join(".local/share/mise"),
            mise.clone(),
            xdg.join("mise"),
        ] {
            std::fs::create_dir_all(base.join("installs")).unwrap();
        }
        let lookup = |list: &[(&str, &str)]| mise_data_with(vars(list));
        assert_eq!(
            lookup(&[
                (MISE_DATA, mise.to_str().unwrap()),
                (XDG_DATA, xdg.to_str().unwrap()),
                (HOME, home.to_str().unwrap()),
            ]),
            Some(mise)
        );
        assert_eq!(
            lookup(&[
                (XDG_DATA, xdg.to_str().unwrap()),
                (HOME, home.to_str().unwrap()),
            ]),
            Some(xdg.join("mise"))
        );
        // An empty MISE_DATA_DIR or a relative XDG_DATA_HOME is ignored, like
        // the config and cache lookups.
        assert_eq!(
            lookup(&[
                (MISE_DATA, ""),
                (XDG_DATA, "relative"),
                (HOME, home.to_str().unwrap()),
            ]),
            Some(home.join(".local/share/mise"))
        );
        assert_eq!(
            lookup(&[(MISE_DATA, "relative"), (HOME, home.to_str().unwrap())]),
            Some(home.join(".local/share/mise"))
        );
    }
}
