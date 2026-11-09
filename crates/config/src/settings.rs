use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
};

pub const SCHEMA: &str = include_str!("../../../PatchulaSettings.pkl");

/// The environment variable naming the settings file, read by both the CLI's
/// scan commands and `patchula serve`.
pub const ENV_VAR: &str = "PATCHULA_SETTINGS";

/// Operator settings shared by the CLI and the server; `server` is read only by
/// `patchula serve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub concurrency: usize,
    pub recheck: Option<patchula_words::Recheck>,
    pub http: Http,
    pub registries: BTreeMap<String, Registry>,
    pub lockfiles: Lockfiles,
    pub server: Server,
}
/// Lockfile tooling settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lockfiles {
    /// GiB of persistent package caches lockfile tools share between runs.
    #[serde(rename = "cacheGiB")]
    pub cache_gib: u64,
}
/// A private registry entry. It names where its secret lives, never the secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Registry {
    pub auth: Option<RegistryAuth>,
    pub username: Option<String>,
    pub secret: Option<Secret>,
    pub mirror_of: Option<String>,
    pub allowed_installations: Vec<u64>,
    /// Whether lockfile tools get the entry's credentials.
    #[serde(default)]
    pub tools: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegistryAuth {
    Basic,
    Bearer,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Secret {
    pub env: Option<String>,
    pub file: Option<PathBuf>,
}
impl Secret {
    /// Where the secret is read from, for errors: never the value.
    fn source(&self) -> String {
        match (&self.env, &self.file) {
            (Some(name), None) => format!("environment variable {name}"),
            (None, Some(path)) => format!("file {}", path.display()),
            _ => "secret".to_owned(),
        }
    }
    /// The one rule every secret source shares, on bytes: strip trailing
    /// CR/LF, reject empty. Values need not be UTF-8 — a webhook secret or a
    /// key file is bytes. Errors name where the secret should be, never what
    /// it is.
    pub fn read_bytes(&self) -> Result<Vec<u8>, String> {
        let value = match (&self.env, &self.file) {
            (Some(name), None) => std::env::var_os(name)
                .map(|value| os_bytes(&value))
                .ok_or_else(|| format!("environment variable {name} is not set"))?,
            (None, Some(path)) => {
                std::fs::read(path).map_err(|_| format!("cannot read {}", path.display()))?
            }
            _ => return Err("secret needs exactly one of env and file".into()),
        };
        let end = value
            .iter()
            .rposition(|byte| !matches!(byte, b'\r' | b'\n'))
            .map_or(0, |at| at + 1);
        if end == 0 {
            return Err("secret is empty".into());
        }
        Ok(value[..end].to_vec())
    }
    /// The secret's value as UTF-8 text, for credentials sent as text.
    pub fn read(&self) -> Result<String, String> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes).map_err(|_| format!("the {} is not UTF-8", self.source()))
    }
}
/// An environment variable's bytes. Unix keeps them as they are; elsewhere
/// they arrive as text already.
#[cfg(unix)]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}
#[cfg(not(unix))]
fn os_bytes(value: &std::ffi::OsStr) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Http {
    pub default_host_concurrency: usize,
    pub host_concurrency: BTreeMap<String, usize>,
    pub registry_freshness_secs: u64,
    pub go_freshness_secs: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Server {
    pub triggers: Triggers,
    pub sweep_cron: Option<String>,
    pub sweep_timezone: String,
    pub batch_size: usize,
    pub coalescing_ms: u64,
    /// Installations whose batches run at once, each one batch at a time.
    pub workers: usize,
    pub report_directory: Option<PathBuf>,
    /// Batch reports `report_directory` keeps; older ones are deleted.
    pub reports_kept: usize,
    pub allowed_installations: Vec<u64>,
    pub allowed_repositories: Vec<String>,
    pub advertise: bool,
    pub public_url: Option<String>,
    pub public_api: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Triggers {
    pub push: bool,
    pub dashboard: bool,
    pub pull_request_edited: bool,
    pub pull_request_closed: bool,
    pub installation: bool,
    pub checks: bool,
}
impl Default for Settings {
    /// The schema's defaults, for a CLI run without a settings file. A test
    /// checks them against `PatchulaSettings.pkl`.
    fn default() -> Self {
        Self {
            concurrency: 8,
            recheck: None,
            http: Http {
                default_host_concurrency: 16,
                host_concurrency: BTreeMap::new(),
                registry_freshness_secs: 3600,
                go_freshness_secs: 3600,
            },
            registries: BTreeMap::new(),
            lockfiles: Lockfiles { cache_gib: 20 },
            server: Server {
                triggers: Triggers {
                    push: true,
                    dashboard: true,
                    pull_request_edited: true,
                    pull_request_closed: true,
                    installation: true,
                    checks: true,
                },
                sweep_cron: Some("0 0 * * * *".into()),
                sweep_timezone: "UTC".into(),
                batch_size: 32,
                coalescing_ms: 1000,
                workers: 4,
                report_directory: None,
                reports_kept: 200,
                allowed_installations: Vec::new(),
                allowed_repositories: Vec::new(),
                advertise: true,
                public_url: None,
                public_api: true,
            },
        }
    }
}
impl Settings {
    pub fn load(path: &Path) -> miette::Result<Self> {
        Self::load_diagnostic(path).map_err(sanitize_diagnostic)
    }
    fn load_diagnostic(path: &Path) -> miette::Result<Self> {
        use miette::{IntoDiagnostic, WrapErr};
        let source = std::fs::read_to_string(path)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot read settings {}", path.display()))?;
        let value = crate::evaluate_local(path, "PatchulaSettings.pkl", SCHEMA)?;
        let settings: Self = serde_path_to_error::deserialize(value).map_err(|error| {
            crate::problem(path, &source, &error.path().to_string(), &error.to_string())
        })?;
        settings.validate().map_err(|message| {
            let field = message.split_whitespace().next().unwrap_or("");
            crate::problem(path, &source, field, message)
        })?;
        Ok(settings)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=100).contains(&self.concurrency) {
            return Err("concurrency must be 1..100");
        }
        // Registry prefixes, mirrors and host-concurrency rules need the HTTP
        // client's types, so patchula-engine's `settings::check` runs them;
        // `Settings::load` callers that build an engine get both.
        for registry in self.registries.values() {
            match (registry.auth, registry.username.as_deref()) {
                (Some(RegistryAuth::Basic), Some(name))
                    if !name.is_empty() && !name.contains(':') => {}
                (Some(RegistryAuth::Bearer) | None, None) => {}
                _ => {
                    return Err(
                        "username is required for basic registries, without a colon, and refused otherwise",
                    );
                }
            }
            match (registry.auth, &registry.secret) {
                (Some(_), Some(secret)) => match (&secret.env, &secret.file) {
                    (Some(name), None) if !name.is_empty() && !name.contains('=') => {}
                    (None, Some(path)) if !path.as_os_str().is_empty() => {}
                    _ => return Err("secret needs exactly one of env and file"),
                },
                (None, None) if registry.mirror_of.is_some() => {}
                (None, None) => return Err("auth or mirrorOf is required for each registry"),
                _ => return Err("secret is required with auth, and refused without it"),
            }
            if registry
                .allowed_installations
                .iter()
                .any(|id| *id == 0 || *id > i64::MAX as u64)
            {
                return Err("allowedInstallations IDs must be positive");
            }
            // Lockfile tools read the public registry, never a mirror, and an
            // entry without credentials has nothing to give them.
            if registry.tools && (registry.auth.is_none() || registry.mirror_of.is_some()) {
                return Err("tools needs auth, and does not apply to a mirror");
            }
        }
        if !(1..=100).contains(&self.http.default_host_concurrency) {
            return Err("defaultHostConcurrency must be 1..100");
        }
        if !(1..=1_000_000).contains(&self.lockfiles.cache_gib) {
            return Err("lockfiles.cacheGiB must be 1..1000000");
        }
        let server = &self.server;
        if !(1..=1000).contains(&server.batch_size) {
            return Err("batchSize must be 1..1000");
        }
        if server.coalescing_ms > 60_000 {
            return Err("coalescingMs must be 0..60000");
        }
        if !(1..=64).contains(&server.workers) {
            return Err("workers must be 1..64");
        }
        if let Some(cron) = &server.sweep_cron {
            crate::schedule(cron, true)
                .map_err(|_| "sweepCron must be valid (six or seven fields)")?;
        }
        chrono_tz::Tz::from_str(&server.sweep_timezone)
            .map_err(|_| "sweepTimezone must be valid")?;
        if server
            .report_directory
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err("reportDirectory must be nonempty");
        }
        if !(1..=100_000).contains(&server.reports_kept) {
            return Err("reportsKept must be 1..100000");
        }
        if server
            .allowed_installations
            .iter()
            .any(|id| *id == 0 || *id > i64::MAX as u64)
        {
            return Err("allowedInstallations IDs must be positive");
        }
        if server
            .allowed_repositories
            .iter()
            .any(|repo| !valid_repo(repo))
        {
            return Err("allowedRepositories entries must be owner/repo");
        }
        if server
            .public_url
            .as_deref()
            .is_some_and(|url| public_url(url).is_none())
        {
            return Err("publicUrl must be an https URL without a query, fragment or user");
        }
        Ok(())
    }
    /// The URL to hide in what the Count writes: `publicUrl`, else the one
    /// derived from the App's webhook URL.
    pub fn advertised(&self, derived: Option<&str>) -> Option<String> {
        if !self.server.advertise {
            return None;
        }
        match &self.server.public_url {
            Some(url) => public_url(url),
            None => derived.map(str::to_owned),
        }
    }
    pub fn installation_allowed(&self, id: u64) -> bool {
        let allowed = &self.server.allowed_installations;
        id > 0 && id <= i64::MAX as u64 && (allowed.is_empty() || allowed.contains(&id))
    }
    pub fn allows(&self, id: u64, repo: &str) -> bool {
        let allowed = &self.server.allowed_repositories;
        self.installation_allowed(id)
            && valid_repo(repo)
            && (allowed.is_empty() || allowed.iter().any(|a| a.eq_ignore_ascii_case(repo)))
    }
    pub fn recheck(
        &self,
        override_recheck: Option<patchula_words::Recheck>,
    ) -> patchula_words::Recheck {
        override_recheck
            .or(self.recheck)
            .unwrap_or(patchula_words::Recheck::Now)
    }
}

/// The settings file `flag` names, else the one `PATCHULA_SETTINGS` names.
fn named(flag: Option<&Path>) -> Option<PathBuf> {
    flag.map(Path::to_path_buf)
        .or_else(|| std::env::var_os(ENV_VAR).map(PathBuf::from))
}

impl Settings {
    /// The settings for `lookup`, `run`, `apply` and `tui`: the file `flag` or
    /// `PATCHULA_SETTINGS` names, else `user` — the user's
    /// `patchula/settings.pkl` — when it exists, else the schema's defaults.
    pub fn for_cli(flag: Option<&Path>, user: Option<PathBuf>) -> miette::Result<Self> {
        match named(flag).or_else(|| user.filter(|path| path.is_file())) {
            Some(path) => Self::load(&path),
            None => Ok(Self::default()),
        }
    }
    /// The settings file `patchula serve` uses: the one `flag` or
    /// `PATCHULA_SETTINGS` names, required — a server never picks up a
    /// developer's personal settings file. The path, not the settings, since
    /// serve reloads the file as it changes.
    pub fn for_server(flag: Option<&Path>) -> miette::Result<PathBuf> {
        named(flag).ok_or_else(|| miette::miette!("serve requires --settings or {ENV_VAR}"))
    }
}

/// Operator source can accidentally contain secrets, even in rejected properties.
/// Retain the evaluator's file and labels, but mask string contents in excerpts.
fn sanitize_diagnostic(error: miette::Report) -> miette::Report {
    let Some(contents) = error
        .source_code()
        .and_then(|source| source.read_span(&(0, 0).into(), 0, usize::MAX).ok())
    else {
        return error;
    };
    let source = String::from_utf8_lossy(contents.data());
    let mut message = format!("{error:#}");
    let mut labels = error
        .labels()
        .map(|labels| labels.collect::<Vec<_>>())
        .unwrap_or_default();
    let mut masked = source.as_bytes().to_vec();
    let mut start = None;
    let mut escaped = false;
    for (index, byte) in source.bytes().enumerate() {
        if let Some(begin) = start {
            if byte == b'"' && !escaped {
                let value = &source[begin..index];
                if !value.is_empty() {
                    message = message.replace(value, "[redacted]");
                    for label in &mut labels {
                        label
                            .set_label(label.label().map(|text| text.replace(value, "[redacted]")));
                    }
                }
                start = None;
            } else if byte != b'\n' && byte != b'\r' {
                masked[index] = b'*';
            }
            escaped = byte == b'\\' && !escaped;
        } else if byte == b'"' {
            start = Some(index + 1);
            escaped = false;
        }
    }
    let src = miette::NamedSource::new(
        contents.name().unwrap_or("settings"),
        String::from_utf8(masked).expect("masked UTF-8"),
    );
    miette::Report::new(SettingsDiagnostic {
        message,
        src,
        labels,
    })
}
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{message}")]
struct SettingsDiagnostic {
    message: String,
    #[source_code]
    src: miette::NamedSource<String>,
    #[label(collection)]
    labels: Vec<miette::LabeledSpan>,
}
pub fn valid_repo(repo: &str) -> bool {
    let parts: Vec<_> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}

/// `url` normalised for advertising, or None if the CLI would refuse it.
pub fn public_url(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    (url.scheme() == "https"
        && url.host().is_some()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.as_str().trim_end_matches('/').to_owned())
}
/// The server's base URL from its webhook URL, if that URL ends in /webhook.
/// A relay such as smee.io does not, and needs `publicUrl`.
pub fn from_webhook(url: &str) -> Option<String> {
    let mut url = url::Url::parse(url).ok()?;
    let base = url.path().strip_suffix("/webhook")?.to_owned();
    url.set_path(&base);
    url.set_query(None);
    public_url(url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_kept_is_read_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let load = |body: &str| {
            std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
            Settings::load(&path)
        };
        assert_eq!(load("").unwrap().server.reports_kept, 200);
        assert_eq!(
            load("server { reportsKept = 5 }")
                .unwrap()
                .server
                .reports_kept,
            5
        );
        assert_eq!(
            load("server { reportsKept = 100000 }")
                .unwrap()
                .server
                .reports_kept,
            100_000
        );
        for kept in [0, 100_001] {
            assert!(load(&format!("server {{ reportsKept = {kept} }}")).is_err());
        }
    }
    #[test]
    fn local_absolute_imports_bundled_schema_and_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let parent = dir.path().join("parent.pkl");
        std::fs::write(
            &parent,
            "amends \"PatchulaSettings.pkl\"\nconcurrency = 3\n",
        )
        .unwrap();
        std::fs::write(&path, format!("amends \"{}\"\n", parent.display())).unwrap();
        assert_eq!(Settings::load(&path).unwrap().concurrency, 3);
        // An absolute schema path works both with a real file and bundled fallback.
        let schema = dir.path().join("PatchulaSettings.pkl");
        for vendored in [false, true] {
            if vendored {
                std::fs::write(&schema, SCHEMA).unwrap();
            }
            std::fs::write(
                &path,
                format!("amends \"{}\"\nconcurrency = 4\n", schema.display()),
            )
            .unwrap();
            assert_eq!(Settings::load(&path).unwrap().concurrency, 4);
        }
        for body in [
            "concurrency = 0",
            "concurrency =",
            "unknownSetting = true",
            "server { sweepCron = \"invalid\" }",
        ] {
            std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
            let diagnostic = format!("{:?}", Settings::load(&path).unwrap_err());
            assert!(diagnostic.contains("settings.pkl"), "{diagnostic}");
            assert!(
                diagnostic.contains('╭'),
                "missing source span: {diagnostic}"
            );
            assert!(!diagnostic.contains("server settings Pkl evaluation failed"));
        }
    }
    #[test]
    fn the_advertised_url_comes_from_public_url_or_the_webhook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let load = |body: &str| {
            std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
            Settings::load(&path)
        };
        let derived = from_webhook("https://patchula.acme.dev/count/webhook?x=1");
        assert_eq!(derived.as_deref(), Some("https://patchula.acme.dev/count"));
        assert_eq!(from_webhook("https://smee.io/abc"), None);
        assert_eq!(from_webhook("http://patchula.acme.dev/webhook"), None);
        let defaults = load("").unwrap();
        assert_eq!(defaults.advertised(derived.as_deref()), derived);
        assert_eq!(defaults.advertised(None), None);
        let explicit = load("server { publicUrl = \"https://count.acme.dev/\" }").unwrap();
        assert_eq!(
            explicit.advertised(derived.as_deref()).as_deref(),
            Some("https://count.acme.dev")
        );
        let off =
            load("server {\n  advertise = false\n  publicUrl = \"https://count.acme.dev\"\n}")
                .unwrap();
        assert_eq!(off.advertised(derived.as_deref()), None);
        for bad in [
            "http://count.acme.dev",
            "https://count.acme.dev/?a=b",
            "count",
        ] {
            assert!(
                load(&format!("server {{ publicUrl = \"{bad}\" }}")).is_err(),
                "{bad}"
            );
        }
    }
    #[test]
    fn defaults_match_the_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let load = |body: &str| {
            std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
            Settings::load(&path)
        };
        assert_eq!(load("").unwrap(), Settings::default());
        assert_eq!(
            load("lockfiles { cacheGiB = 1 }")
                .unwrap()
                .lockfiles
                .cache_gib,
            1
        );
        assert!(load("lockfiles { cacheGiB = 0 }").is_err());
        assert!(load("lockfiles { cacheGiB = -1 }").is_err());
        assert!(load("http { defaultHostConcurrency = 101 }").is_err());
        // Host-concurrency keys and limits are the HTTP client's rules;
        // patchula-engine's settings tests cover them.
        assert!(load("http { hostConcurrency { [\"API.github.com\"] = 8 } }").is_ok());
    }
}
