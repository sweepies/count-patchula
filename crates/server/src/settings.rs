use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, RwLock},
};

pub const SCHEMA: &str = include_str!("../../../RustovateServer.pkl");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub triggers: Triggers,
    pub sweep_cron: Option<String>,
    pub sweep_timezone: String,
    pub concurrency: usize,
    pub batch_size: usize,
    pub coalescing_ms: u64,
    pub recheck: Option<String>,
    pub report_directory: Option<PathBuf>,
    pub allowed_installations: Vec<u64>,
    pub allowed_repositories: Vec<String>,
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
impl Settings {
    pub fn load(path: &Path) -> miette::Result<Self> {
        Self::load_diagnostic(path).map_err(sanitize_diagnostic)
    }
    fn load_diagnostic(path: &Path) -> miette::Result<Self> {
        use miette::{IntoDiagnostic, WrapErr};
        let source = std::fs::read_to_string(path)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot read server settings {}", path.display()))?;
        let value = rustovate_config::evaluate_local(path, "RustovateServer.pkl", SCHEMA)?;
        let settings: Self = serde_path_to_error::deserialize(value).map_err(|error| {
            rustovate_config::problem(path, &source, &error.path().to_string(), &error.to_string())
        })?;
        settings.validate().map_err(|message| {
            let field = message.split_whitespace().next().unwrap_or("");
            rustovate_config::problem(path, &source, field, message)
        })?;
        Ok(settings)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=100).contains(&self.concurrency) {
            return Err("concurrency must be 1..100");
        }
        if !(1..=1000).contains(&self.batch_size) {
            return Err("batchSize must be 1..1000");
        }
        if self.coalescing_ms > 60_000 {
            return Err("coalescingMs must be 0..60000");
        }
        if let Some(cron) = &self.sweep_cron {
            cron::Schedule::from_str(cron)
                .map_err(|_| "sweepCron must be valid (six or seven fields)")?;
        }
        chrono_tz::Tz::from_str(&self.sweep_timezone).map_err(|_| "sweepTimezone must be valid")?;
        if self
            .recheck
            .as_deref()
            .is_some_and(|r| !matches!(r, "now" | "next-run"))
        {
            return Err("recheck must be now or next-run");
        }
        if self
            .report_directory
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err("reportDirectory must be nonempty");
        }
        if self
            .allowed_installations
            .iter()
            .any(|id| *id == 0 || *id > i64::MAX as u64)
        {
            return Err("allowedInstallations IDs must be positive");
        }
        if self
            .allowed_repositories
            .iter()
            .any(|repo| !valid_repo(repo))
        {
            return Err("allowedRepositories entries must be owner/repo");
        }
        Ok(())
    }
    pub fn installation_allowed(&self, id: u64) -> bool {
        id > 0
            && id <= i64::MAX as u64
            && (self.allowed_installations.is_empty() || self.allowed_installations.contains(&id))
    }
    pub fn allows(&self, id: u64, repo: &str) -> bool {
        self.installation_allowed(id)
            && valid_repo(repo)
            && (self.allowed_repositories.is_empty()
                || self
                    .allowed_repositories
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(repo)))
    }
    pub fn recheck(&self, override_wording: Option<&str>) -> rustovate_engine::Recheck {
        match override_wording.or(self.recheck.as_deref()) {
            Some("next-run") => rustovate_engine::Recheck::NextRun,
            _ => rustovate_engine::Recheck::Now,
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
        contents.name().unwrap_or("server settings"),
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
                    tracing::info!(target: "rustovate_server", "server settings loaded");
                }
                state.settings = settings;
                state.valid = true;
            }
            Err(_) => {
                if state.valid {
                    tracing::error!(target: "rustovate_server", "server settings rejected; keeping last good settings; run config check-server for diagnostics");
                }
                state.valid = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_absolute_imports_bundled_schema_and_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.pkl");
        let parent = dir.path().join("parent.pkl");
        std::fs::write(&parent, "amends \"RustovateServer.pkl\"\nconcurrency = 3\n").unwrap();
        std::fs::write(&path, format!("amends \"{}\"\n", parent.display())).unwrap();
        assert_eq!(Settings::load(&path).unwrap().concurrency, 3);
        // An absolute schema path works both with a real file and bundled fallback.
        let schema = dir.path().join("RustovateServer.pkl");
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
            "sweepCron = \"invalid\"",
        ] {
            std::fs::write(&path, format!("amends \"RustovateServer.pkl\"\n{body}\n")).unwrap();
            let diagnostic = format!("{:?}", Settings::load(&path).unwrap_err());
            assert!(diagnostic.contains("server.pkl"), "{diagnostic}");
            assert!(
                diagnostic.contains('╭'),
                "missing source span: {diagnostic}"
            );
            assert!(!diagnostic.contains("server settings Pkl evaluation failed"));
        }
    }
    #[test]
    fn reload_keeps_last_good_settings_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.pkl");
        let source = "amends \"RustovateServer.pkl\"\nconcurrency = 3\n";
        std::fs::write(&path, source).unwrap();
        let settings = Settings::load(&path).unwrap();
        let reload = Reload::new(settings);
        std::fs::write(&path, "amends \"RustovateServer.pkl\"\nconcurrency = 0\n").unwrap();
        reload.refresh(&path);
        assert!(!reload.valid());
        assert_eq!(reload.current().concurrency, 3);
        std::fs::write(&path, source.replace("3", "5")).unwrap();
        reload.refresh(&path);
        assert!(reload.valid());
        assert_eq!(reload.current().concurrency, 5);
        std::fs::write(
            &path,
            "amends \"RustovateServer.pkl\"\nwebhookSecret = \"forbidden\"\n",
        )
        .unwrap();
        assert!(Settings::load(&path).is_err());
    }
}
