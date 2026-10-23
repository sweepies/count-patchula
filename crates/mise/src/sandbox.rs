//! The environment every mise process of the Count runs in.
use std::{collections::BTreeMap, ffi::OsString, path::Path};

/// A clean environment for one mise process: nothing is inherited from the
/// operator, so no user, global or repository configuration, credential file,
/// hook or tool-specific setting applies. `home` is a scratch directory the
/// caller owns and `data` is the Count's own tool store.
///
/// Settings, from mise's settings and CLI reference:
/// - `MISE_NO_CONFIG` loads no config file at all: no global or system
///   config, no project `mise.toml`, so there is nothing to trust and no
///   `[env]`, `[hooks]` or `[settings]` to apply (`--no-config`).
/// - `MISE_NO_ENV` and `MISE_NO_HOOKS` repeat that for env templates and hooks
///   (`--no-env`, `--no-hooks`).
/// - `MISE_YES` answers any prompt, and `CI` keeps mise non-interactive.
/// - `MISE_COLOR=0` keeps output plain.
/// - `MISE_DATA_DIR` is the Count's tool store, apart from the operator's own.
/// - `HOME` and the `XDG_*` directories point into the scratch directory, so
///   caches, state, `gh`, netrc and Git credential files cannot be found.
pub fn sandbox_env(home: &Path, data: &Path) -> BTreeMap<OsString, OsString> {
    [
        ("PATH", OsString::from("/usr/local/bin:/usr/bin:/bin")),
        ("HOME", home.as_os_str().into()),
        ("XDG_CONFIG_HOME", home.join("config").into_os_string()),
        ("XDG_CACHE_HOME", home.join("cache").into_os_string()),
        ("LANG", "C.UTF-8".into()),
        ("CI", "true".into()),
        ("MISE_DATA_DIR", data.as_os_str().into()),
        ("MISE_NO_CONFIG", "1".into()),
        ("MISE_NO_ENV", "1".into()),
        ("MISE_NO_HOOKS", "1".into()),
        ("MISE_YES", "1".into()),
        ("MISE_COLOR", "0".into()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
        ("GIT_ALLOW_PROTOCOL", "https".into()),
        (
            "NPM_CONFIG_USERCONFIG",
            home.join("npm-user.cfg").into_os_string(),
        ),
        (
            "NPM_CONFIG_GLOBALCONFIG",
            home.join("npm-global.cfg").into_os_string(),
        ),
        ("NPM_CONFIG_IGNORE_SCRIPTS", "true".into()),
        (
            "BUN_INSTALL_CACHE_DIR",
            home.join("bun-cache").into_os_string(),
        ),
        ("CARGO_HOME", home.join("cargo").into_os_string()),
        ("GOPATH", home.join("go").into_os_string()),
        ("GOWORK", "off".into()),
        ("GOTOOLCHAIN", "local".into()),
        ("GOENV", "off".into()),
        ("TF_IN_AUTOMATION", "1".into()),
        ("TF_INPUT", "0".into()),
        ("TF_CLI_CONFIG_FILE", "/dev/null".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect()
}
