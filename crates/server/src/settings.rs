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
    pub fn load(path: &Path) -> Result<Self, &'static str> {
        // Pkl diagnostics can echo source. Never log source text from operator files.
        let value = rustovate_config::evaluate_local(path, "RustovateServer.pkl", SCHEMA)
            .map_err(|_| "server settings Pkl evaluation failed")?;
        let settings: Self =
            serde_json::from_value(value).map_err(|_| "unknown or invalid server setting")?;
        settings.validate()?;
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
                .map_err(|_| "invalid sweepCron (six or seven fields)")?;
        }
        chrono_tz::Tz::from_str(&self.sweep_timezone).map_err(|_| "invalid sweepTimezone")?;
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
            return Err("installation IDs must be positive");
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
            Err(error) => {
                if state.valid {
                    tracing::error!(target: "rustovate_server", error, "server settings rejected; keeping last good settings");
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
