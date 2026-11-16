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
/// - The AWS SDK mise's s3 backend uses finds no credentials: no `AWS_*`
///   variable is inherited, its config and credential files point into the
///   scratch directory, and `AWS_EC2_METADATA_DISABLED` stops it asking the
///   instance-metadata endpoint for the machine's own.
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
        ("AWS_EC2_METADATA_DISABLED", "true".into()),
        ("AWS_CONFIG_FILE", home.join("aws-config").into_os_string()),
        (
            "AWS_SHARED_CREDENTIALS_FILE",
            home.join("aws-credentials").into_os_string(),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect()
}

/// Turns off mise's plugin backends, `asdf` and `vfox`, which clone a plugin
/// and run its scripts, for a mise that handles tool names a repository
/// chose: a lookup, or `mise lock` on its configuration. A registry
/// shorthand that lists one falls through to its next backend, or fails
/// when there is none; a name with one is refused before mise starts
/// (`admit`). The Count's own lockfile tools are named by the Count and
/// install as mise's registry has them.
pub fn without_plugins(env: &mut BTreeMap<OsString, OsString>) {
    env.insert(
        "MISE_DISABLE_BACKENDS".into(),
        crate::policy::PLUGIN_BACKENDS.join(",").into(),
    );
}
