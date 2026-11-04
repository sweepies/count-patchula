use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, RwLock},
};

pub const SCHEMA: &str = include_str!("../../../PatchulaSettings.pkl");

/// Operator settings shared by the CLI and the server; `server` is read only by
/// `patchula serve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub concurrency: usize,
    pub recheck: Option<String>,
    pub http: Http,
    pub registries: BTreeMap<String, Registry>,
    pub lockfiles: Lockfiles,
    pub server: Server,
}
/// Lockfile tooling settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lockfiles {
    /// GiB of persistent package caches lockfile tools share between runs; 0
    /// turns them off.
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
    /// The secret's value. Errors name where it should be, never what it is.
    fn read(&self) -> Result<String, String> {
        let value = match (&self.env, &self.file) {
            (Some(name), None) => std::env::var(name)
                .map_err(|_| format!("environment variable {name} is not set or not UTF-8"))?,
            (None, Some(path)) => std::fs::read_to_string(path)
                .map_err(|_| format!("cannot read {}", path.display()))?
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
            _ => return Err("secret needs exactly one of env and file".into()),
        };
        if value.is_empty() {
            return Err("secret is empty".into());
        }
        Ok(value)
    }
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
        let value = patchula_config::evaluate_local(path, "PatchulaSettings.pkl", SCHEMA)?;
        let settings: Self = serde_path_to_error::deserialize(value).map_err(|error| {
            patchula_config::problem(path, &source, &error.path().to_string(), &error.to_string())
        })?;
        settings.validate().map_err(|message| {
            let field = message.split_whitespace().next().unwrap_or("");
            patchula_config::problem(path, &source, field, message)
        })?;
        Ok(settings)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=100).contains(&self.concurrency) {
            return Err("concurrency must be 1..100");
        }
        if self
            .recheck
            .as_deref()
            .is_some_and(|r| !matches!(r, "now" | "next-run"))
        {
            return Err("recheck must be now or next-run");
        }
        let mut prefixes = std::collections::BTreeSet::new();
        let mut mirrored = std::collections::BTreeSet::new();
        let mut entries = Vec::new();
        for (prefix, registry) in &self.registries {
            let Some(normalised) =
                patchula_http::Credential::new(prefix, patchula_http::Auth::Withheld)
            else {
                return Err(
                    "registries keys must be https URL prefixes without a user, query or fragment",
                );
            };
            if !prefixes.insert(normalised.prefix().to_owned()) {
                return Err("registries keys must differ by more than a trailing slash");
            }
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
            if let Some(public) = &registry.mirror_of {
                let Some(mirror) = normalised.clone().mirroring(public) else {
                    return Err(
                        "mirrorOf must be an https URL prefix without a user, query or fragment, other than the entry's own",
                    );
                };
                if !mirrored.insert(mirror.mirror_of().unwrap_or_default().to_owned()) {
                    return Err("mirrorOf must differ between registries");
                }
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
            // An `.npmrc` key ends at `=` and a comment starts at `;`.
            if registry.tools && normalised.prefix().contains(['=', ';']) {
                return Err("tools entries need a prefix without = or ;");
            }
            entries.push((normalised, registry.tools));
        }
        // npm, aube and Bun give a URL the credentials of the nearest
        // enclosing prefix they are told about, and are told only about tools
        // entries. An entry within a tools entry, which lookups would prefer
        // or keep from an installation, would get its credentials there.
        if entries.iter().any(|(outer, tools)| {
            *tools
                && entries
                    .iter()
                    .any(|(inner, _)| inner.prefix() != outer.prefix() && outer.encloses(inner))
        }) {
            return Err("tools entries cannot hold other registries under their prefix");
        }
        if !(1..=100).contains(&self.http.default_host_concurrency) {
            return Err("defaultHostConcurrency must be 1..100");
        }
        self.http_options()
            .validate()
            .map_err(|_| "hostConcurrency needs lowercase hostnames and limits 1..100")?;
        if self.lockfiles.cache_gib > 1_000_000 {
            return Err("lockfiles.cacheGiB must be 0..1000000");
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
            patchula_config::schedule(cron, true)
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
    /// The HTTP client settings.
    pub fn http_options(&self) -> patchula_http::Options {
        patchula_http::Options {
            default_host_concurrency: self.http.default_host_concurrency,
            host_concurrency: self.http.host_concurrency.clone(),
            registry_freshness_secs: self.http.registry_freshness_secs,
            go_freshness_secs: self.http.go_freshness_secs,
            ..Default::default()
        }
    }
    /// The registry credentials `installation` may use, or every entry's for
    /// the CLI, with their secrets read now. An entry the installation may not
    /// use, or whose secret cannot be read, is withheld: its prefix still
    /// claims its requests, which carry nothing, so a broader entry never
    /// stands in. Unreadable secrets also come back by prefix and reason,
    /// never the value.
    pub fn credentials(
        &self,
        installation: Option<u64>,
    ) -> (Vec<patchula_http::Credential>, Vec<(String, String)>) {
        let mut credentials = Vec::new();
        let mut failed = Vec::new();
        for (prefix, registry) in &self.registries {
            // Settings built without `validate` can hold a bad prefix: it is
            // reported, never a panic.
            let Some(withheld) =
                patchula_http::Credential::new(prefix, patchula_http::Auth::Withheld)
            else {
                failed.push((prefix.clone(), "not an https URL prefix".to_owned()));
                continue;
            };
            if let Some(id) = installation
                && !registry.allowed_installations.is_empty()
                && !registry.allowed_installations.contains(&id)
            {
                credentials.push(withheld);
                continue;
            }
            // A mirror whose secret cannot be read still stands in for its
            // public registry, without credentials: there is no fallback.
            let mirror = |entry: patchula_http::Credential| match &registry.mirror_of {
                Some(public) => entry.mirroring(public),
                None => Some(entry),
            };
            let auth = match (registry.auth, &registry.secret) {
                (None, _) => Ok(patchula_http::Auth::Anonymous),
                (Some(_), None) => Err("secret is missing".to_owned()),
                (Some(kind), Some(secret)) => secret.read().map(|secret| match kind {
                    RegistryAuth::Basic => patchula_http::Auth::Basic {
                        username: registry.username.clone().unwrap_or_default(),
                        password: secret,
                    },
                    RegistryAuth::Bearer => patchula_http::Auth::Bearer { token: secret },
                }),
            };
            match auth.and_then(|auth| {
                patchula_http::Credential::new(prefix, auth)
                    .map(|entry| {
                        if registry.tools {
                            entry.for_tools()
                        } else {
                            entry
                        }
                    })
                    .and_then(mirror)
                    .ok_or_else(|| "secret cannot be sent in a header".to_owned())
            }) {
                Ok(credential) => credentials.push(credential),
                Err(reason) => {
                    credentials.extend(mirror(withheld));
                    failed.push((prefix.clone(), reason));
                }
            }
        }
        (credentials, failed)
    }
    /// The installation GitHub client settings. It keeps the client's own
    /// lower default unless `http.hostConcurrency` sets api.github.com.
    pub fn github_options(&self) -> patchula_github::Options {
        let defaults = patchula_github::Options::default();
        patchula_github::Options {
            concurrency: self
                .http
                .host_concurrency
                .get("api.github.com")
                .copied()
                .unwrap_or(defaults.concurrency),
            ..defaults
        }
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
    pub fn recheck(&self, override_wording: Option<&str>) -> patchula_engine::Recheck {
        match override_wording.or(self.recheck.as_deref()) {
            Some("next-run") => patchula_engine::Recheck::NextRun,
            _ => patchula_engine::Recheck::Now,
        }
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

/// Invalid edits leave the active snapshot usable but readiness reports the error.
#[derive(Clone)]
pub struct Reload(Arc<RwLock<Snapshot>>);
struct Snapshot {
    settings: Settings,
    valid: bool,
}
impl Reload {
    pub fn new(settings: Settings) -> Self {
        Self(Arc::new(RwLock::new(Snapshot {
            settings,
            valid: true,
        })))
    }
    pub fn current(&self) -> Settings {
        self.0.read().expect("settings lock").settings.clone()
    }
    pub fn valid(&self) -> bool {
        self.0.read().expect("settings lock").valid
    }
    pub fn refresh(&self, path: &Path) {
        let result = Settings::load(path);
        let mut state = self.0.write().expect("settings lock");
        match result {
            Ok(settings) => {
                if !state.valid || settings != state.settings {
                    tracing::info!(target: "patchula_server", "settings loaded");
                }
                state.settings = settings;
                state.valid = true;
            }
            Err(_) => {
                if state.valid {
                    tracing::error!(target: "patchula_server", "settings rejected; keeping last good settings; run config check-settings for diagnostics");
                }
                state.valid = false;
            }
        }
    }
}

/// `url` normalised for advertising, or None if the CLI would refuse it.
pub fn public_url(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
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
    let mut url = reqwest::Url::parse(url).ok()?;
    let base = url.path().strip_suffix("/webhook")?.to_owned();
    url.set_path(&base);
    url.set_query(None);
    public_url(url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn reload_keeps_last_good_settings_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let source = "amends \"PatchulaSettings.pkl\"\nconcurrency = 3\n";
        std::fs::write(&path, source).unwrap();
        let settings = Settings::load(&path).unwrap();
        let reload = Reload::new(settings);
        std::fs::write(&path, "amends \"PatchulaSettings.pkl\"\nconcurrency = 0\n").unwrap();
        reload.refresh(&path);
        assert!(!reload.valid());
        assert_eq!(reload.current().concurrency, 3);
        std::fs::write(&path, source.replace("3", "5")).unwrap();
        reload.refresh(&path);
        assert!(reload.valid());
        assert_eq!(reload.current().concurrency, 5);
        std::fs::write(
            &path,
            "amends \"PatchulaSettings.pkl\"\nwebhookSecret = \"forbidden\"\n",
        )
        .unwrap();
        assert!(Settings::load(&path).is_err());
    }
    #[test]
    fn defaults_match_the_schema_and_http_settings_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let load = |body: &str| {
            std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
            Settings::load(&path)
        };
        assert_eq!(load("").unwrap(), Settings::default());
        assert_eq!(
            load("lockfiles { cacheGiB = 0 }")
                .unwrap()
                .lockfiles
                .cache_gib,
            0
        );
        assert!(load("lockfiles { cacheGiB = -1 }").is_err());
        let http = load(
            "http {\n  hostConcurrency { [\"api.github.com\"] = 8 }\n  goFreshnessSecs = 0\n}",
        )
        .unwrap()
        .http_options();
        assert_eq!(http.host_limit("api.github.com"), 8);
        assert_eq!(http.host_limit("registry.npmjs.org"), 16);
        assert_eq!(http.go_freshness_secs, 0);
        for bad in [
            "http { hostConcurrency { [\"API.github.com\"] = 8 } }",
            "http { hostConcurrency { [\"api.github.com\"] = 0 } }",
            "http { defaultHostConcurrency = 101 }",
        ] {
            assert!(load(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn registry_entries_are_validated_and_their_secrets_read_per_installation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let secret = dir.path().join("token");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let load = |body: &str| {
            std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
            Settings::load(&path)
        };
        let settings = load(&format!(
            r#"registries {{
  ["https://acme.jfrog.io/artifactory/api/helm/charts"] {{
    auth = "basic"
    username = "ci"
    secret {{ file = "{}" }}
    allowedInstallations {{ 7 }}
  }}
  ["https://ghcr.io"] {{
    auth = "bearer"
    secret {{ env = "PATCHULA_TEST_UNSET_REGISTRY_SECRET" }}
  }}
}}"#,
            secret.display()
        ))
        .unwrap();
        let (cli, failed) = settings.credentials(None);
        assert_eq!(cli.len(), 2);
        assert_eq!(cli[1].auth, patchula_http::Auth::Withheld);
        assert_eq!(
            cli[0].prefix(),
            "https://acme.jfrog.io/artifactory/api/helm/charts"
        );
        assert_eq!(
            cli[0].auth,
            patchula_http::Auth::Basic {
                username: "ci".into(),
                password: "s3cret".into()
            }
        );
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "https://ghcr.io");
        assert!(failed[0].1.contains("PATCHULA_TEST_UNSET_REGISTRY_SECRET"));
        assert!(matches!(
            settings.credentials(Some(7)).0[0].auth,
            patchula_http::Auth::Basic { .. }
        ));
        // Another installation gets the prefix withheld, not a broader entry.
        assert!(
            settings
                .credentials(Some(8))
                .0
                .iter()
                .all(|entry| entry.auth == patchula_http::Auth::Withheld)
        );
        for bad in [
            r#"registries { ["http://acme.jfrog.io"] { auth = "bearer"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io/?a=b"] { auth = "bearer"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "basic"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; username = "ci"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; secret { } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; secret { env = "T"; file = "/t" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "token"; secret { env = "T" } } }"#,
            r#"registries { ["https://acme.jfrog.io"] { auth = "bearer"; secret { env = "T" } } ["https://acme.jfrog.io/"] { auth = "bearer"; secret { env = "U" } } }"#,
        ] {
            assert!(load(&bad.replace("; ", "\n")).is_err(), "{bad}");
        }
    }
    #[test]
    fn mirrors_need_no_credentials_and_keep_standing_in_when_a_secret_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let load = |body: &str| {
            std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
            Settings::load(&path)
        };
        let settings = load(
            r#"registries {
  ["https://nexus.acme.dev/repository/npm-proxy"] {
    mirrorOf = "https://registry.npmjs.org"
  }
  ["https://acme.jfrog.io/api/pypi/pypi-remote/simple"] {
    auth = "bearer"
    secret { env = "PATCHULA_TEST_UNSET_MIRROR_SECRET" }
    mirrorOf = "https://pypi.org/simple"
  }
}"#,
        )
        .unwrap();
        let (entries, failed) = settings.credentials(None);
        assert_eq!(failed.len(), 1);
        let [pypi, npm] = &entries[..] else {
            panic!("two entries")
        };
        assert_eq!(npm.auth, patchula_http::Auth::Anonymous);
        assert_eq!(npm.mirror_of(), Some("https://registry.npmjs.org"));
        assert_eq!(pypi.auth, patchula_http::Auth::Withheld);
        assert_eq!(pypi.mirror_of(), Some("https://pypi.org/simple"));
        for bad in [
            r#"registries { ["https://nexus.acme.dev"] { } }"#,
            r#"registries { ["https://nexus.acme.dev"] { auth = "bearer" } }"#,
            r#"registries { ["https://nexus.acme.dev"] { secret { env = "T" }; mirrorOf = "https://registry.npmjs.org" } }"#,
            r#"registries { ["https://nexus.acme.dev"] { mirrorOf = "https://nexus.acme.dev/" } }"#,
            r#"registries { ["https://nexus.acme.dev"] { mirrorOf = "http://registry.npmjs.org" } }"#,
            r#"registries { ["https://nexus.acme.dev/a"] { mirrorOf = "https://registry.npmjs.org" } ["https://nexus.acme.dev/b"] { mirrorOf = "https://registry.npmjs.org/" } }"#,
        ] {
            assert!(load(&bad.replace("; ", "\n")).is_err(), "{bad}");
        }
    }
    #[test]
    fn tools_take_an_entrys_credentials_only_when_it_allows_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let token = dir.path().join("token");
        std::fs::write(&token, "t0ken\n").unwrap();
        let load = |body: &str| {
            std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{body}\n")).unwrap();
            Settings::load(&path)
        };
        let settings = load(
            &r#"registries {
  ["https://npm.acme.dev"] {
    auth = "bearer"
    secret { file = "TOKEN" }
    tools = true
  }
  ["https://npm.other.dev"] {
    auth = "bearer"
    secret { file = "TOKEN" }
  }
}"#
            .replace("TOKEN", &token.to_string_lossy()),
        )
        .unwrap();
        let (entries, _) = settings.credentials(None);
        let tools: Vec<_> = entries
            .iter()
            .filter(|e| e.tools())
            .map(|e| e.prefix())
            .collect();
        assert_eq!(tools, ["https://npm.acme.dev"]);
        for bad in [
            r#"registries { ["https://nexus.acme.dev"] { mirrorOf = "https://registry.npmjs.org"; tools = true } }"#,
            r#"registries { ["https://nexus.acme.dev"] { auth = "bearer"; secret { env = "T" }; mirrorOf = "https://registry.npmjs.org"; tools = true } }"#,
            // An entry under a tools entry, for another installation or not.
            r#"registries { ["https://nexus.acme.dev"] { auth = "bearer"; secret { env = "T" }; tools = true }; ["https://nexus.acme.dev/repository/b"] { auth = "bearer"; secret { env = "U" }; allowedInstallations { 2 } } }"#,
            r#"registries { ["https://nexus.acme.dev"] { auth = "bearer"; secret { env = "T" }; tools = true }; ["https://nexus.acme.dev/npm-proxy"] { mirrorOf = "https://registry.npmjs.org" } }"#,
            r#"registries { ["https://nexus.acme.dev/a=b"] { auth = "bearer"; secret { env = "T" }; tools = true } }"#,
        ] {
            assert!(load(&bad.replace("; ", "\n")).is_err(), "{bad}");
        }
    }
}
