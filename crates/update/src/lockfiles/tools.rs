//! The runner's tool store: one mise data directory for every lockfile tool,
//! separate from the operator's own mise installs and kept across runs.
//! Usage is recorded per `tool@version`, so `prune` can remove versions no
//! run has needed recently, and `install` can prewarm the pinned defaults.
use super::*;

const USAGE: &str = "rustovate-usage.json";

/// `RUSTOVATE_TOOLS_DIR`, or `<cache>.tools` beside the SQLite cache.
pub fn tools_dir(workspace: &Path) -> PathBuf {
    std::env::var_os("RUSTOVATE_TOOLS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.with_extension("tools"))
}

fn read_usage(dir: &Path) -> BTreeMap<String, i64> {
    std::fs::read(dir.join(USAGE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
fn write_usage(dir: &Path, usage: &BTreeMap<String, i64>) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!("{USAGE}.tmp"));
    std::fs::write(&temp, serde_json::to_vec(usage).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(temp, dir.join(USAGE)).map_err(|e| e.to_string())
}

/// Every pinned default except Rust, which runs through rustup.
pub(super) fn defaults() -> Result<Vec<(String, String)>, String> {
    let doc = tool_defaults()?;
    let mut tools = Vec::new();
    if let Some(table) = doc.get("tools").and_then(toml_edit::Item::as_table_like) {
        for (tool, version) in table.iter() {
            if tool != "rust"
                && let Some(version) = version.as_str()
            {
                tools.push((tool.to_string(), version.to_string()));
            }
        }
    }
    if let Some(berry) = doc["yarn"]["berry"].as_str() {
        tools.push(("yarn".into(), berry.into()));
    }
    Ok(tools)
}

impl Runner {
    pub(super) fn record_use(&self, tools: &[(String, String)]) {
        let _guard = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut usage = read_usage(&self.mise_data);
        let now = chrono::Utc::now().timestamp();
        for (tool, version) in tools {
            usage.insert(format!("{tool}@{version}"), now);
        }
        if let Err(error) = write_usage(&self.mise_data, &usage) {
            tracing::warn!(%error, "tool usage was not recorded");
        }
    }
    /// The directory this runner installs tools into.
    pub fn tools(&self) -> &Path {
        &self.mise_data
    }
    async fn mise(&self, args: Vec<String>) -> Result<(), String> {
        let driver = tempfile::tempdir().map_err(|e| e.to_string())?;
        let mut spec = self.command(driver.path());
        spec.args.extend(args);
        self.executor
            .run(spec)
            .await
            .map(|_| ())
            .map_err(|e| format!("mise: {e}"))
    }
    /// Install the pinned default toolchains, e.g. while building an image.
    pub async fn prewarm(&self) -> Result<Vec<String>, String> {
        let tools = defaults()?;
        let specs: Vec<String> = tools.iter().map(|(t, v)| format!("{t}@{v}")).collect();
        let mut args = vec!["install".to_string()];
        args.extend(specs.iter().cloned());
        self.mise(args).await?;
        self.record_use(&tools);
        Ok(specs)
    }
    /// Uninstall tool versions no run has used for `days`; pinned defaults stay.
    pub async fn prune(&self, days: i64) -> Result<Vec<String>, String> {
        let cutoff = chrono::Utc::now().timestamp() - days * 86_400;
        let keep: BTreeSet<String> = defaults()?
            .iter()
            .map(|(t, v)| format!("{t}@{v}"))
            .collect();
        let stale: Vec<String> = read_usage(&self.mise_data)
            .into_iter()
            .filter(|(spec, used)| *used < cutoff && !keep.contains(spec))
            .map(|(spec, _)| spec)
            .collect();
        for spec in &stale {
            self.mise(vec!["uninstall".into(), spec.clone()]).await?;
        }
        let _guard = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut usage = read_usage(&self.mise_data);
        for spec in &stale {
            usage.remove(spec);
        }
        write_usage(&self.mise_data, &usage)?;
        Ok(stale)
    }
}
