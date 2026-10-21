//! The runner's tool store: one mise data directory for every lockfile tool,
//! separate from the operator's own mise installs and kept across runs.
//! Usage is recorded per `tool@version`, so `prune` can remove versions no
//! run has needed recently, and `install` can prewarm the pinned defaults.
use super::*;

const USAGE: &str = "patchula-usage.json";
/// `tool@version`s already seeded from the user's mise installs. Each is seeded
/// at most once, so a copy that fails falls back to a normal install.
const SEEDED: &str = "patchula-seeded.json";
/// Tools whose mise installs are relocatable, so a hard-linked copy works from
/// the Count's store. Python, Ruby and PHP builds embed their install path,
/// .NET versions share one SDK root, and Rust goes through rustup.
const SEEDABLE: &[&str] = &["node", "go", "bun", "aube", "yarn", "uv", "terraform"];
/// mise itself, which is not in mise's registry under its own name.
pub(super) const MISE: &str = "aqua:jdx/mise";
/// Time allowed for installing tools. Some hosts are slow: static-php.dev
/// served about 44 KB/s when measured.
pub(super) const INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

/// `PATCHULA_TOOLS_DIR`, or `<cache>.tools` beside the SQLite cache.
pub fn tools_dir(workspace: &Path) -> PathBuf {
    std::env::var_os("PATCHULA_TOOLS_DIR")
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
            if !matches!(tool, "rust" | "php" | "composer")
                && let Some(version) = version.as_str()
            {
                let tool = if tool == "mise" { MISE } else { tool };
                tools.push((tool.to_string(), version.to_string()));
            }
        }
    }
    if let Some(berry) = doc["yarn"]["berry"].as_str() {
        tools.push(("yarn".into(), berry.into()));
    }
    // The static build is the default; compiled PHP is built on first use.
    if let (Some(php), Some(composer)) = (
        doc["tools"]["php"].as_str(),
        doc["tools"]["composer"].as_str(),
    ) {
        tools.extend(super::php::tools(
            patchula_config::PhpBuild::Static,
            php,
            composer,
        ));
    }
    Ok(tools)
}

/// Recreate `from` at `to` with hard links, keeping symlinks as symlinks. The
/// copy shares the user's files but is the Count's own directory: removing it
/// never touches the original. Symlinks must resolve inside `root`, the version
/// directory being copied: relative ones are kept, absolute ones (mise's own
/// `.mise-bins`) are pointed at the same file under `published`, where the copy
/// will live. Any other link refuses the copy.
#[cfg(unix)]
fn link_tree(root: &Path, published: &Path, from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let link = std::fs::read_link(&source)?;
            let resolved = from.join(&link).canonicalize().ok();
            let inside = resolved
                .as_deref()
                .and_then(|path| path.strip_prefix(root).ok());
            let link = match inside {
                Some(_) if link.is_relative() => link,
                Some(rest) => published.join(rest),
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        format!("{} links outside the install", source.display()),
                    ));
                }
            };
            std::os::unix::fs::symlink(link, &target)?;
        } else if kind.is_dir() {
            link_tree(root, published, &source, &target)?;
        } else {
            std::fs::hard_link(&source, &target)?;
        }
    }
    Ok(())
}

impl Runner {
    /// Before downloading a tool, borrow the user's own mise install of the
    /// same version when there is one: hard links make it instant and use no
    /// extra disk. Only relocatable tools, only once per version, and only
    /// when both stores are on one filesystem; otherwise mise installs it.
    #[cfg(unix)]
    pub(super) fn seed(&self, tools: &[(String, String)]) {
        let Some(source) = &self.seed else {
            return;
        };
        if source == &self.mise_data {
            return;
        }
        let _guard = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.mise_data.join(SEEDED);
        let mut seeded: BTreeSet<String> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut changed = false;
        for (tool, version) in tools {
            let spec = format!("{tool}@{version}");
            if !SEEDABLE.contains(&tool.as_str()) || seeded.contains(&spec) {
                continue;
            }
            let installs = source.join("installs").join(tool);
            let Ok(from) = installs.join(version).canonicalize() else {
                continue;
            };
            let to = self.mise_data.join("installs").join(tool);
            // A version that resolves outside its tool's directory lives in a
            // root shared with other versions, which a copy cannot reproduce.
            let own = installs
                .canonicalize()
                .is_ok_and(|dir| from.starts_with(dir));
            if !own || !from.is_dir() || to.join(version).exists() {
                continue;
            }
            seeded.insert(spec.clone());
            changed = true;
            // Build the copy where only this process writes, then publish it in
            // one rename. Another run seeding the same version either finishes
            // first, and this rename fails, or finds a complete install.
            let staging = to.join(format!(".patchula-seed-{}-{version}", std::process::id()));
            // The tool's backend metadata goes first: once the version is
            // published, mise treats it as installed, so nothing may fail after.
            let linked = std::fs::create_dir_all(&to)
                .and_then(|()| link_tree(&from, &to.join(version), &from, &staging))
                .and_then(|()| {
                    let backend = installs.join(".mise.backend.toml");
                    if backend.is_file() && !to.join(".mise.backend.toml").exists() {
                        std::fs::copy(backend, to.join(".mise.backend.toml")).map(drop)
                    } else {
                        Ok(())
                    }
                })
                .and_then(|()| std::fs::rename(&staging, to.join(version)));
            match linked {
                Ok(()) => tracing::info!(tool = %spec, "seeded from the user's mise install"),
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&staging);
                    tracing::debug!(tool = %spec, %error, "not seeded; mise will install it");
                }
            }
        }
        if changed {
            let write = std::fs::create_dir_all(&self.mise_data).and_then(|()| {
                std::fs::write(&path, serde_json::to_vec(&seeded).expect("set serializes"))
            });
            if let Err(error) = write {
                tracing::warn!(%error, "seeded tools were not recorded");
            }
        }
    }
    #[cfg(not(unix))]
    pub(super) fn seed(&self, _: &[(String, String)]) {}
    /// Whether every `tool@version` has run before, so mise will not install
    /// it within this call; pruning forgets versions as it removes them.
    pub(super) fn installed(&self, tools: &[(String, String)]) -> bool {
        let usage = read_usage(&self.mise_data);
        tools
            .iter()
            .all(|(tool, version)| usage.contains_key(&format!("{tool}@{version}")))
    }
    /// Uninstall versions no run has completed with. An install cut short (a
    /// timeout, a crash) can leave a directory mise treats as installed but
    /// that holds no binaries; only a recorded run proves an install works.
    pub(super) async fn clear_unproven(&self, tools: &[(String, String)]) {
        let usage = read_usage(&self.mise_data);
        for (tool, version) in tools {
            let spec = format!("{tool}@{version}");
            if tool != "rust"
                && !usage.contains_key(&spec)
                && let Err(error) = self.mise(vec!["uninstall".into(), spec]).await
                && !error.contains("is not installed")
            {
                tracing::warn!(%error, "unproven tool install was not removed");
            }
        }
    }
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
        spec.timeout = 2 * INSTALL_TIMEOUT;
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
        self.clear_unproven(&tools).await;
        self.seed(&tools);
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
