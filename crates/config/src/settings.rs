use crate::validate::Issue;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
};

pub const SCHEMA: &str = include_str!("../../../PatchulaSettings.pkl");
/// The name the settings schema is evaluated under.
pub const SCHEMA_NAME: &str = "PatchulaSettings.pkl";

/// The largest `github.writeBurst`: the GitHub client's own bound, which
/// keeps writes under GitHub's content-request rate.
pub const MAX_WRITE_BURST: usize = 19;

/// Whether `id` is an installation id the server accepts: positive and
/// within `i64`.
fn valid_installation(id: u64) -> bool {
    id > 0 && i64::try_from(id).is_ok()
}

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
    pub github: Github,
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
#[serde(try_from = "RegistryFields", into = "RegistryFields")]
pub struct Registry {
    /// How requests authenticate; `None` sends no credentials, for an open
    /// mirror.
    pub auth: Option<RegistryAuth>,
    pub mirror_of: Option<String>,
    pub allowed_installations: Vec<u64>,
    /// Whether lockfile tools get the entry's credentials.
    pub tools: bool,
}
/// How an entry authenticates, with where its secret is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryAuth {
    /// `username` and the secret as a password.
    Basic { username: String, secret: Secret },
    /// The secret as a bearer token.
    Bearer { secret: Secret },
}
/// A registry entry as `PatchulaSettings.pkl` writes it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegistryFields {
    auth: Option<AuthKind>,
    username: Option<String>,
    secret: Option<Secret>,
    mirror_of: Option<String>,
    allowed_installations: Vec<u64>,
    #[serde(default)]
    tools: bool,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AuthKind {
    Basic,
    Bearer,
}
impl TryFrom<RegistryFields> for Registry {
    type Error = &'static str;
    fn try_from(fields: RegistryFields) -> Result<Self, Self::Error> {
        let username = match (fields.auth, fields.username.as_deref()) {
            (Some(AuthKind::Basic), Some(name)) => !name.is_empty() && !name.contains(':'),
            (Some(AuthKind::Bearer) | None, None) => true,
            _ => false,
        };
        if !username {
            return Err(
                "username is required for basic registries, without a colon, and refused otherwise",
            );
        }
        let auth = match (fields.auth, fields.username, fields.secret) {
            (Some(AuthKind::Basic), Some(username), Some(secret)) => {
                Some(RegistryAuth::Basic { username, secret })
            }
            (Some(AuthKind::Bearer), _, Some(secret)) => Some(RegistryAuth::Bearer { secret }),
            (None, _, None) if fields.mirror_of.is_some() => None,
            (None, _, None) => return Err("a registry needs auth or mirrorOf"),
            _ => return Err("secret is required with auth, and refused without it"),
        };
        Ok(Self {
            auth,
            mirror_of: fields.mirror_of,
            allowed_installations: fields.allowed_installations,
            tools: fields.tools,
        })
    }
}
impl From<Registry> for RegistryFields {
    fn from(registry: Registry) -> Self {
        let (auth, username, secret) = match registry.auth {
            Some(RegistryAuth::Basic { username, secret }) => {
                (Some(AuthKind::Basic), Some(username), Some(secret))
            }
            Some(RegistryAuth::Bearer { secret }) => (Some(AuthKind::Bearer), None, Some(secret)),
            None => (None, None, None),
        };
        Self {
            auth,
            username,
            secret,
            mirror_of: registry.mirror_of,
            allowed_installations: registry.allowed_installations,
            tools: registry.tools,
        }
    }
}
/// Where a secret is read from, never the secret itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SecretFields", into = "SecretFields")]
pub enum Secret {
    /// An environment variable holding the secret.
    Env(String),
    /// A file holding the secret.
    File(PathBuf),
}
/// A secret as `PatchulaSettings.pkl` writes it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SecretFields {
    env: Option<String>,
    file: Option<PathBuf>,
}
impl TryFrom<SecretFields> for Secret {
    type Error = &'static str;
    fn try_from(fields: SecretFields) -> Result<Self, Self::Error> {
        match (fields.env, fields.file) {
            (Some(name), None) if !name.is_empty() && !name.contains('=') => Ok(Self::Env(name)),
            (None, Some(path)) if !path.as_os_str().is_empty() => Ok(Self::File(path)),
            _ => Err("secret needs exactly one of env and file"),
        }
    }
}
impl From<Secret> for SecretFields {
    fn from(secret: Secret) -> Self {
        match secret {
            Secret::Env(name) => Self {
                env: Some(name),
                file: None,
            },
            Secret::File(path) => Self {
                env: None,
                file: Some(path),
            },
        }
    }
}
impl Secret {
    /// Where the secret is read from, for errors: never the value.
    fn source(&self) -> String {
        match self {
            Self::Env(name) => format!("environment variable {name}"),
            Self::File(path) => format!("file {}", path.display()),
        }
    }
    /// The one rule every secret source shares, on bytes: strip trailing
    /// CR/LF, reject empty. Values need not be UTF-8 — a webhook secret or a
    /// key file is bytes. Errors name where the secret should be, never what
    /// it is.
    pub fn read_bytes(&self) -> Result<Vec<u8>, String> {
        self.read_bytes_with(|name| std::env::var_os(name))
    }
    /// `read_bytes` with the environment lookup injected, so tests never
    /// touch the process environment.
    fn read_bytes_with(
        &self,
        var: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Vec<u8>, String> {
        let value = match self {
            Self::Env(name) => {
                let value =
                    var(name).ok_or_else(|| format!("environment variable {name} is not set"))?;
                os_bytes(&value)
                    .ok_or_else(|| format!("environment variable {name} is not valid Unicode"))?
            }
            Self::File(path) => std::fs::read(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?,
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
/// a variable is text, and one that is not valid Unicode is refused rather
/// than silently altered into a different secret.
#[cfg(unix)]
fn os_bytes(value: &std::ffi::OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Some(value.as_bytes().to_vec())
}
#[cfg(not(unix))]
fn os_bytes(value: &std::ffi::OsStr) -> Option<Vec<u8>> {
    value.to_str().map(|text| text.as_bytes().to_vec())
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Http {
    pub default_host_concurrency: usize,
    pub host_concurrency: BTreeMap<String, usize>,
    pub registry_freshness_secs: u64,
    pub go_freshness_secs: u64,
    pub advisory_freshness_secs: u64,
}
/// GitHub client settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Github {
    /// Writes that go out unpaced before pacing to one a second.
    pub write_burst: usize,
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
    /// The schema's defaults, for a CLI run without a settings file: the
    /// bundled `PatchulaSettings.pkl` amended with nothing.
    fn default() -> Self {
        let entry = "/settings.pkl";
        let value = crate::virtual_files::evaluate(
            entry,
            BTreeMap::from([
                (entry.to_owned(), format!("amends \"{SCHEMA_NAME}\"\n")),
                (format!("/{SCHEMA_NAME}"), SCHEMA.to_owned()),
            ]),
        )
        .expect("the bundled settings schema evaluates");
        serde_json::from_value(value).expect("the bundled settings schema's defaults decode")
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
        let value = crate::evaluate_local(path, SCHEMA_NAME, SCHEMA)?;
        let settings: Self = serde_path_to_error::deserialize(value).map_err(|error| {
            crate::problem(path, &source, &error.path().to_string(), &error.to_string())
        })?;
        settings
            .validate()
            .map_err(|issue| crate::problem(path, &source, &issue.field, &issue.message))?;
        Ok(settings)
    }
    /// The first setting out of bounds, naming its field.
    pub(crate) fn validate(&self) -> Result<(), Issue> {
        /// `field` and what is wrong with it, read as one sentence.
        fn issue(field: impl Into<String>, predicate: &str) -> Issue {
            let field = field.into();
            let message = format!("{field} {predicate}");
            Issue { field, message }
        }
        if !(1..=100).contains(&self.concurrency) {
            return Err(issue("concurrency", "must be 1..100"));
        }
        // Registry prefixes, mirrors and host-concurrency rules need the HTTP
        // client's types, so patchula-engine's `settings::check` runs them;
        // `Settings::load` callers that build an engine get both.
        for (key, registry) in &self.registries {
            let field = |name: &str| format!("registries[{key:?}]{name}");
            if registry
                .allowed_installations
                .iter()
                .any(|id| !valid_installation(*id))
            {
                return Err(issue(
                    field(".allowedInstallations"),
                    "IDs must be positive",
                ));
            }
            // Lockfile tools read the public registry, never a mirror, and an
            // entry without credentials has nothing to give them.
            if registry.tools && (registry.auth.is_none() || registry.mirror_of.is_some()) {
                return Err(issue(
                    field(".tools"),
                    "needs auth, and does not apply to a mirror",
                ));
            }
        }
        if !(1..=100).contains(&self.http.default_host_concurrency) {
            return Err(issue("http.defaultHostConcurrency", "must be 1..100"));
        }
        if !(1..=MAX_WRITE_BURST).contains(&self.github.write_burst) {
            return Err(issue(
                "github.writeBurst",
                &format!("must be 1..{MAX_WRITE_BURST}"),
            ));
        }
        if !(1..=1_000_000).contains(&self.lockfiles.cache_gib) {
            return Err(issue("lockfiles.cacheGiB", "must be 1..1000000"));
        }
        let server = &self.server;
        if !(1..=1000).contains(&server.batch_size) {
            return Err(issue("server.batchSize", "must be 1..1000"));
        }
        if server.coalescing_ms > 60_000 {
            return Err(issue("server.coalescingMs", "must be 0..60000"));
        }
        if !(1..=64).contains(&server.workers) {
            return Err(issue("server.workers", "must be 1..64"));
        }
        if let Some(cron) = &server.sweep_cron {
            crate::schedule(cron, true)
                .map_err(|_| issue("server.sweepCron", "must be valid (six or seven fields)"))?;
        }
        chrono_tz::Tz::from_str(&server.sweep_timezone)
            .map_err(|_| issue("server.sweepTimezone", "must be valid"))?;
        if server
            .report_directory
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(issue("server.reportDirectory", "must be nonempty"));
        }
        if !(1..=100_000).contains(&server.reports_kept) {
            return Err(issue("server.reportsKept", "must be 1..100000"));
        }
        if server
            .allowed_installations
            .iter()
            .any(|id| !valid_installation(*id))
        {
            return Err(issue("server.allowedInstallations", "IDs must be positive"));
        }
        if server
            .allowed_repositories
            .iter()
            .any(|repo| !valid_repo(repo))
        {
            return Err(issue(
                "server.allowedRepositories",
                "entries must be owner/repo",
            ));
        }
        if server
            .public_url
            .as_deref()
            .is_some_and(|url| public_url(url).is_none())
        {
            return Err(issue(
                "server.publicUrl",
                "must be an https URL without a query, fragment or user",
            ));
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
        valid_installation(id) && (allowed.is_empty() || allowed.contains(&id))
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
    /// The file's path comes back with the settings so a later check can
    /// point into it; it is `None` for the defaults.
    pub fn for_cli(
        flag: Option<&Path>,
        user: Option<PathBuf>,
    ) -> miette::Result<(Option<PathBuf>, Self)> {
        match named(flag).or_else(|| user.filter(|path| path.is_file())) {
            Some(path) => Self::load(&path).map(|settings| (Some(path), settings)),
            None => Ok((None, Self::default())),
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
    /// Load settings that amend the schema with `body`, written to `path`.
    fn load_body(path: &Path, body: &str) -> miette::Result<Settings> {
        std::fs::write(path, format!("amends \"{SCHEMA_NAME}\"\n{body}\n")).unwrap();
        Settings::load(path)
    }
    /// A registry's shape is checked as it is read: the error names the
    /// registry and what is wrong with it.
    #[test]
    fn a_registry_shape_problem_names_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        for (body, problem) in [
            (
                r#"auth = "basic"; secret { env = "T" }"#,
                "username is required",
            ),
            (
                r#"auth = "bearer"; username = "ci"; secret { env = "T" }"#,
                "username is required",
            ),
            (r#"auth = "bearer""#, "secret is required with auth"),
            (r#"secret { env = "T" }"#, "secret is required with auth"),
            ("", "needs auth or mirrorOf"),
            (
                r#"auth = "bearer"; secret { env = "T"; file = "/t" }"#,
                "secret needs exactly one of env and file",
            ),
            (
                r#"auth = "bearer"; secret { }"#,
                "secret needs exactly one of env and file",
            ),
        ] {
            let body = body.replace("; ", "\n");
            let error = load_body(
                &path,
                &format!("registries {{ [\"acme\"] {{\n{body}\n}} }}"),
            )
            .unwrap_err()
            .to_string();
            // The diagnostic masks quoted values, the registry's key among
            // them, so only the unquoted parts are checked.
            assert!(
                error.contains("registries.") && error.contains(problem),
                "{body}: {error}"
            );
        }
        let read = load_body(
            &path,
            "registries { [\"acme\"] {\nauth = \"basic\"\nusername = \"ci\"\nsecret { file = \"/t\" }\n} }",
        )
        .unwrap();
        assert_eq!(
            read.registries["acme"].auth,
            Some(RegistryAuth::Basic {
                username: "ci".into(),
                secret: Secret::File("/t".into()),
            })
        );
    }
    /// An unreadable secret file keeps the io error: not found and permission
    /// denied read differently.
    #[test]
    fn an_unreadable_secret_file_keeps_its_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let error = Secret::File(missing.clone()).read_bytes().unwrap_err();
        let io = std::fs::read(&missing).unwrap_err();
        assert_eq!(error, format!("cannot read {}: {io}", missing.display()));
    }
    #[test]
    fn reports_kept_is_read_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        let load = |body: &str| load_body(&path, body);
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
    fn local_imports_bundled_schema_and_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.pkl");
        // Absolute imports are refused: only modules inside the settings'
        // own tree load, so an absolute `amends` fails where a relative one
        // of the same file works.
        let parent = dir.path().join("parent.pkl");
        std::fs::write(
            &parent,
            "amends \"PatchulaSettings.pkl\"\nconcurrency = 3\n",
        )
        .unwrap();
        std::fs::write(&path, format!("amends \"{}\"\n", parent.display())).unwrap();
        assert!(Settings::load(&path).is_err());
        std::fs::write(&path, "amends \"parent.pkl\"\nconcurrency = 4\n").unwrap();
        assert_eq!(Settings::load(&path).unwrap().concurrency, 4);
        // The schema resolves by name, vendored or bundled.
        let schema = dir.path().join("PatchulaSettings.pkl");
        for vendored in [false, true] {
            if vendored {
                std::fs::write(&schema, SCHEMA).unwrap();
            }
            std::fs::write(&path, "amends \"PatchulaSettings.pkl\"\nconcurrency = 4\n").unwrap();
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
        let load = |body: &str| load_body(&path, body);
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
        let load = |body: &str| load_body(&path, body);
        let defaults = Settings::default();
        assert!(defaults.validate().is_ok());
        assert_eq!(load("").unwrap(), defaults);
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
    #[test]
    fn secrets_strip_line_endings_reject_empty_and_keep_non_utf8_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret");
        let file_secret = |bytes: &[u8]| {
            std::fs::write(&file, bytes).unwrap();
            Secret::File(file.clone())
        };
        for bytes in [&b"abc\r\n"[..], &b"abc\r\n\r\n"[..], &b"abc"[..]] {
            let secret = file_secret(bytes);
            assert_eq!(secret.read_bytes().unwrap(), b"abc", "{bytes:?}");
            assert_eq!(secret.read().unwrap(), "abc", "{bytes:?}");
        }
        for bytes in [&b""[..], &b"\n"[..], &b"\r\n\r\n"[..]] {
            assert!(file_secret(bytes).read_bytes().is_err(), "{bytes:?}");
        }
        // Non-UTF-8 is kept as bytes; the UTF-8 view refuses it.
        let secret = file_secret(&[0xff, b'\n']);
        assert_eq!(secret.read_bytes().unwrap(), [0xff]);
        assert!(secret.read().is_err());
        // Env values follow the same rule, with the lookup injected.
        let env_secret = Secret::Env("PATCHULA_TEST_SECRET".into());
        let read =
            |value: Option<std::ffi::OsString>| env_secret.read_bytes_with(|_| value.clone());
        assert_eq!(read(Some("abc\r\n".into())).unwrap(), b"abc");
        assert_eq!(read(Some("abc".into())).unwrap(), b"abc");
        assert!(read(Some("\n".into())).is_err());
        assert!(read(None).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let raw = std::ffi::OsString::from_vec(vec![0xff, b'\n']);
            assert_eq!(read(Some(raw)).unwrap(), [0xff]);
        }
    }
    #[test]
    fn a_secret_problem_names_where_it_is_read_from_never_the_value() {
        assert_eq!(
            Secret::Env("TOKEN".into()).source(),
            "environment variable TOKEN"
        );
        assert_eq!(
            Secret::File("/run/secrets/token".into()).source(),
            "file /run/secrets/token"
        );
    }
}
