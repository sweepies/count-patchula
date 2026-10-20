//! Real ecosystem tools run before any Git Data mutation, in disposable trees.
mod checkout;
mod mise;
mod node;
mod process;
use crate::{
    Blob, Plan, Snapshot, blob_sha,
    plan::{hash, unified},
};
pub use process::{CommandSpec, Executor, SystemExecutor};
use rustovate_config::{Config, Manager, NpmEngine, Source};
use rustovate_manager::Dependency;
use rustovate_report::{ActionStatus, BranchAction, FileAction, Repository, Update};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Ecosystem {
    Node,
    Cargo,
    Go,
    Terraform,
    Mise,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Job {
    pub ecosystem: Ecosystem,
    pub directory: String,
    pub package: Option<String>,
    pub current: Option<String>,
    pub target: Option<String>,
}
fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}
fn under(path: &str, root: &str) -> bool {
    root.is_empty() || path.strip_prefix(root).is_some_and(|p| p.starts_with('/'))
}
fn nearest<'a>(file: &str, paths: &'a BTreeSet<String>, names: &[&str]) -> Option<&'a str> {
    paths
        .iter()
        .filter(|p| names.contains(&p.rsplit('/').next().unwrap_or(p)) && under(file, directory(p)))
        .max_by_key(|p| directory(p).len())
        .map(|p| directory(p))
}
pub(crate) fn job(
    dep: &Dependency,
    update: &Update,
    snapshot: &Snapshot,
) -> Result<Option<Job>, String> {
    let (ecosystem, root) = match dep.manager {
        Manager::Mise => {
            let Some(root) = nearest(&dep.file, &snapshot.paths, &["mise.lock"]) else {
                return Ok(None);
            };
            return Ok(Some(Job {
                ecosystem: Ecosystem::Mise,
                directory: root.into(),
                package: Some(dep.name.clone()),
                current: Some(dep.current.clone()),
                target: update.new_version.clone().or(update.new_value.clone()),
            }));
        }
        Manager::Npm | Manager::Bun => {
            let Some(root) = nearest(
                &dep.file,
                &snapshot.paths,
                &[
                    "package-lock.json",
                    "npm-shrinkwrap.json",
                    "bun.lock",
                    "bun.lockb",
                    "pnpm-lock.yaml",
                    "yarn.lock",
                ],
            ) else {
                return Ok(None);
            };
            if snapshot
                .paths
                .iter()
                .any(|p| directory(p) == root && p.rsplit('/').next() == Some("bun.lockb"))
            {
                return Err("unsupported lockfile format".into());
            }
            (Ecosystem::Node, root)
        }
        Manager::Cargo => (
            Ecosystem::Cargo,
            nearest(&dep.file, &snapshot.paths, &["Cargo.lock"]).unwrap_or(directory(&dep.file)),
        ),
        Manager::Go => (Ecosystem::Go, directory(&dep.file)),
        Manager::Terraform if dep.source == Source::Terraform => {
            (Ecosystem::Terraform, directory(&dep.file))
        }
        _ => return Ok(None),
    };
    let targeted = matches!(ecosystem, Ecosystem::Cargo | Ecosystem::Go)
        && matches!(dep.source, Source::Crates | Source::Go);
    Ok(Some(Job {
        ecosystem,
        directory: root.into(),
        package: targeted.then(|| dep.package.clone()),
        current: targeted.then(|| dep.resolved.clone().unwrap_or_else(|| dep.current.clone())),
        target: targeted.then(|| {
            update
                .new_version
                .clone()
                .or(update.new_value.clone())
                .unwrap_or_else(|| dep.current.clone())
        }),
    }))
}

/// Keep the scheduled branch in desired names between windows, so ordinary
/// dependency runs do not close an outstanding maintenance PR.
pub fn maintenance(
    repository: &Repository,
    config: &Config,
    snapshot: &Snapshot,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Plan>, String> {
    use chrono::{Datelike, Timelike};
    if !config.lockfiles.refresh {
        return Ok(None);
    }
    let mut jobs = BTreeSet::new();
    for path in &snapshot.paths {
        let leaf = path.rsplit('/').next().unwrap();
        let ecosystem = match leaf {
            "package-lock.json"
            | "npm-shrinkwrap.json"
            | "bun.lock"
            | "pnpm-lock.yaml"
            | "yarn.lock"
                if config
                    .managers
                    .iter()
                    .any(|m| matches!(m, Manager::Npm | Manager::Bun)) =>
            {
                Ecosystem::Node
            }
            "Cargo.lock" if config.managers.contains(&Manager::Cargo) => Ecosystem::Cargo,
            "mise.lock" if config.managers.contains(&Manager::Mise) => Ecosystem::Mise,
            "go.mod" if config.managers.contains(&Manager::Go) => Ecosystem::Go,
            ".terraform.lock.hcl" if config.managers.contains(&Manager::Terraform) => {
                Ecosystem::Terraform
            }
            _ => continue,
        };
        jobs.insert(Job {
            ecosystem,
            directory: directory(path).into(),
            package: None,
            current: None,
            target: None,
        });
    }
    if jobs.is_empty() {
        return Ok(None);
    }
    let windows: Vec<_> = config
        .defaults
        .windows
        .iter()
        .map(|w| rustovate_report::Window {
            cron: w.cron.clone(),
            zone: w.zone.clone(),
        })
        .collect();
    let due = if windows.is_empty() {
        now.weekday() == chrono::Weekday::Mon && now.hour() < 4
    } else {
        crate::plan::in_window(&windows, now)
    };
    let period = format!("{}-{:02}", now.iso_week().year(), now.iso_week().week());
    let fingerprint = hash(
        serde_json::to_string(&(period, &jobs, &config.lockfiles))
            .unwrap()
            .as_bytes(),
    );
    Ok(Some(Plan {
        action: BranchAction {
            repository: repository.repository.clone(),
            branch: crate::branch_name(&config.prs.branch, "lock-file-maintenance", "maintenance")?,
            base_commit: repository.commit.clone().ok_or("missing base")?,
            status: if due {
                ActionStatus::Planned
            } else {
                ActionStatus::Skipped
            },
            reason: (!due).then(|| "outside lockfile maintenance window".into()),
            message: format!("{} lock file maintenance", config.defaults.commit_prefix),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        },
        contents: BTreeMap::new(),
        identity: hash(b"lock-file-maintenance"),
        fingerprint: fingerprint.clone(),
        input_fingerprint: fingerprint,
        changes: Vec::new(),
        retry: false,
        tree: None,
        cached_pr: None,
        lock_jobs: jobs.into_iter().collect(),
        lockfiles_ready: false,
        deleted: BTreeSet::new(),
        maintenance: true,
    }))
}

fn refresh_key(repo: &str) -> rustovate_cache::ExtractionKey {
    rustovate_cache::ExtractionKey {
        repo: repo.into(),
        default_branch_commit: "maintenance".into(),
        config_hash: "maintenance-success-v1".into(),
    }
}
pub async fn defer_completed_refresh(
    cache: &rustovate_cache::Cache,
    plan: &mut Plan,
    config: &Config,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    use chrono::Datelike;
    if !plan.maintenance || plan.action.status != ActionStatus::Planned {
        return Ok(());
    }
    if let Some(bytes) = cache
        .extraction_get(&refresh_key(&plan.action.repository))
        .await
        .map_err(|e| e.to_string())?
        && let Ok(last) = serde_json::from_slice::<i64>(&bytes)
        && let Some(last) = chrono::DateTime::<chrono::Utc>::from_timestamp(last, 0)
        && (last.iso_week() == now.iso_week()
            || now - last < chrono::Duration::hours(i64::from(config.lockfiles.wait_hours)))
    {
        plan.action.status = ActionStatus::Skipped;
        plan.action.reason = Some(
            "maintenance already completed this week or waiting for lockfiles.waitHours".into(),
        );
    }
    Ok(())
}
pub async fn record_refresh(
    cache: &rustovate_cache::Cache,
    plans: &[Plan],
    actions: &[rustovate_report::PullRequestAction],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    for plan in plans.iter().filter(|p| p.maintenance && p.lockfiles_ready) {
        if (plan.action.status == ActionStatus::Skipped
            && plan.action.reason.as_deref() == Some("lockfiles already current"))
            || actions.iter().any(|a| {
                a.branch == plan.action.branch
                    && matches!(
                        a.status,
                        ActionStatus::Created | ActionStatus::Updated | ActionStatus::Unchanged
                    )
            })
        {
            cache
                .extraction_put(
                    &refresh_key(&plan.action.repository),
                    &serde_json::to_vec(&now.timestamp()).map_err(|e| e.to_string())?,
                )
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub struct Runner {
    credentials: Option<Arc<dyn rustovate_github::Credentials>>,
    cache: PathBuf,
    token: String,
    executor: Arc<dyn Executor>,
    mise_data: PathBuf,
    mise: PathBuf,
    rustup: PathBuf,
    rustup_home: PathBuf,
}
impl Runner {
    pub fn new(cache: PathBuf, token: &str) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let mise_data = std::env::var_os("MISE_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share/mise"));
        let mise = std::env::var_os("PATH")
            .and_then(|p| {
                std::env::split_paths(&p)
                    .map(|p| p.join("mise"))
                    .find(|p| p.is_file())
            })
            .unwrap_or_else(|| "mise".into());
        let rustup = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cargo"))
            .join("bin/rustup");
        let rustup = if rustup.is_file() {
            rustup
        } else {
            "rustup".into()
        };
        let rustup_home = std::env::var_os("RUSTUP_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".rustup"));
        Self {
            credentials: None,
            cache,
            token: token.into(),
            executor: Arc::new(SystemExecutor),
            mise_data,
            mise,
            rustup,
            rustup_home,
        }
    }
    pub fn with_executor(mut self, executor: Arc<dyn Executor>) -> Self {
        self.executor = executor;
        self
    }
    pub fn with_credentials(mut self, credentials: Arc<dyn rustovate_github::Credentials>) -> Self {
        self.credentials = Some(credentials);
        self
    }
    pub async fn prepare(&self, plan: &mut Plan, snapshot: &mut Snapshot, config: &Config) {
        if plan.action.status != ActionStatus::Planned || plan.lockfiles_ready {
            return;
        }
        match self.generate(plan, snapshot, config).await {
            Ok(()) => {}
            Err(error) => {
                plan.action.status = ActionStatus::Error;
                plan.action.reason = Some(format!("lockfile generation failed: {error}"));
            }
        }
    }
    async fn generate(
        &self,
        plan: &mut Plan,
        snapshot: &mut Snapshot,
        config: &Config,
    ) -> Result<(), String> {
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        let run = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(&self.cache)
            .map_err(|e| e.to_string())?;
        let root = run.path().join("tree");
        let driver = run.path().join("driver");
        std::fs::create_dir_all(&driver).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let baseline = self.checkout(plan, &root, &driver).await?;
        for (path, content) in &plan.contents {
            write(&root, path, content)?;
        }
        let groups: BTreeSet<_> = plan
            .lock_jobs
            .iter()
            .map(|j| (j.ecosystem, j.directory.clone()))
            .collect();
        for (ecosystem, directory) in groups {
            let versions = if ecosystem == Ecosystem::Mise {
                BTreeMap::new()
            } else {
                versions(&baseline, &directory)?
            };
            let jobs: Vec<_> = plan
                .lock_jobs
                .iter()
                .filter(|j| j.ecosystem == ecosystem && j.directory == directory)
                .collect();
            let cwd = root.join(&directory);
            match ecosystem {
                Ecosystem::Mise => {
                    self.mise_lock(&driver, &root, &baseline, &jobs, plan.maintenance, config)
                        .await?;
                }
                Ecosystem::Node => {
                    self.node(node::NodeContext {
                        driver: &driver,
                        root: &root,
                        baseline: &baseline,
                        directory: &directory,
                        config,
                        versions: &versions,
                        maintenance: plan.maintenance,
                    })
                    .await?;
                }
                Ecosystem::Cargo => {
                    for job in jobs {
                        let mut args = vec![
                            "update".into(),
                            "--manifest-path".into(),
                            cwd.join("Cargo.toml").to_string_lossy().into_owned(),
                        ];
                        if let Some(package) = &job.package {
                            args.extend([
                                "-p".into(),
                                format!("{}@{}", package, job.current.as_deref().unwrap_or("")),
                                "--precise".into(),
                                job.target.clone().ok_or("missing Cargo target")?,
                            ]);
                        } else if !plan.maintenance {
                            args.push("--workspace".into());
                        }
                        self.tool(
                            &driver,
                            Invocation {
                                tools: vec![("rust".into(), versions["rust"].clone())],
                                bin: "cargo".into(),
                                cwd: None,
                                env: Vec::new(),
                                args,
                            },
                        )
                        .await?;
                    }
                }
                Ecosystem::Go => {
                    for job in jobs {
                        if let Some(package) = &job.package {
                            self.tool(
                                &driver,
                                Invocation {
                                    tools: vec![("go".into(), versions["go"].clone())],
                                    bin: "go".into(),
                                    cwd: None,
                                    env: Vec::new(),
                                    args: vec![
                                        "-C".into(),
                                        cwd.to_string_lossy().into_owned(),
                                        "get".into(),
                                        format!(
                                            "{}@{}",
                                            package,
                                            job.target.as_deref().ok_or("missing Go target")?
                                        ),
                                    ],
                                },
                            )
                            .await?;
                        }
                    }
                    let args = vec![
                        "-C".into(),
                        cwd.to_string_lossy().into_owned(),
                        "mod".into(),
                        if config.lockfiles.tidy_go {
                            "tidy".into()
                        } else {
                            "download".into()
                        },
                    ];
                    self.tool(
                        &driver,
                        Invocation {
                            tools: vec![("go".into(), versions["go"].clone())],
                            bin: "go".into(),
                            cwd: None,
                            env: Vec::new(),
                            args,
                        },
                    )
                    .await?;
                }
                Ecosystem::Terraform => {
                    if cwd.join(".terraform.lock.hcl").exists() {
                        std::fs::remove_file(cwd.join(".terraform.lock.hcl"))
                            .map_err(|e| e.to_string())?;
                    }
                    self.tool(
                        &driver,
                        Invocation {
                            tools: vec![("terraform".into(), versions["terraform"].clone())],
                            bin: "terraform".into(),
                            cwd: None,
                            env: Vec::new(),
                            args: vec![
                                format!("-chdir={}", cwd.display()),
                                "providers".into(),
                                "lock".into(),
                            ],
                        },
                    )
                    .await?;
                }
            }
        }
        // Only manifests and lockfiles can enter the API commit. Reject links,
        // unexpected writes and binary output rather than silently publishing it.
        let generated = collect(&root)?;
        validate_targets(plan, config, &generated)?;
        let mut contents = plan.contents.clone();
        let mut deleted = BTreeSet::new();
        let all: BTreeSet<_> = baseline.keys().chain(generated.keys()).cloned().collect();
        let mut files = BTreeMap::new();
        for path in all {
            let before = baseline.get(&path);
            let after = generated.get(&path);
            if before.map(|b| &b.content) == after {
                continue;
            }
            if !writable(&path) && plan.contents.get(&path) != after {
                return Err(format!("tool modified unexpected file {path}"));
            }
            if path.rsplit('/').next() == Some("mise.lock")
                && !plan
                    .lock_jobs
                    .iter()
                    .any(|j| j.ecosystem == Ecosystem::Mise && j.directory == directory(&path))
            {
                return Err(format!("tool modified unexpected mise lockfile {path}"));
            }
            let old = before.map(|b| b.content.as_str()).unwrap_or("");
            let new = after.map(String::as_str).unwrap_or("");
            files.insert(
                path.clone(),
                FileAction {
                    path: path.clone(),
                    blob_sha: before.map(|b| b.sha.clone()).unwrap_or_default(),
                    diff: unified(&path, old, new),
                },
            );
            contents.insert(path.clone(), new.into());
            if after.is_none() {
                deleted.insert(path.clone());
            }
            snapshot.files.insert(
                path,
                before.cloned().unwrap_or(Blob {
                    sha: String::new(),
                    mode: "100644".into(),
                    content: String::new(),
                }),
            );
        }
        plan.contents = contents;
        plan.deleted = deleted;
        plan.action.files = files.into_values().collect();
        plan.lockfiles_ready = true;
        plan.fingerprint = hash(
            serde_json::to_string(&(&plan.contents, &plan.deleted, &plan.action.message))
                .unwrap()
                .as_bytes(),
        );
        if plan.action.files.is_empty() {
            plan.action.status = ActionStatus::Skipped;
            plan.action.reason = Some("lockfiles already current".into());
        }
        Ok(())
    }
    async fn tool(&self, driver: &Path, call: Invocation) -> Result<(), String> {
        let mut spec = self.command(driver);
        if let Some(cwd) = &call.cwd {
            spec.cwd = cwd.clone();
        }
        for (key, value) in &call.env {
            spec.env.insert(key.into(), value.into());
        }
        spec.args.push("exec".into());
        for (tool, version) in &call.tools {
            spec.args.push(format!("{tool}@{version}"));
        }
        spec.args.push("--".into());
        if call.tools.iter().any(|(tool, _)| tool == "rust") {
            let version = call
                .tools
                .iter()
                .find(|(tool, _)| tool == "rust")
                .map(|(_, version)| version.clone())
                .ok_or("missing tool version")?;
            spec.env
                .insert("RUSTUP_HOME".into(), self.rustup_home.as_os_str().into());
            let cargo_home = spec
                .env
                .get(std::ffi::OsStr::new("CARGO_HOME"))
                .ok_or("missing isolated Cargo home")?
                .clone();
            let install_home = self.cache.join("tool-cargo");
            std::fs::create_dir_all(&install_home).map_err(|e| e.to_string())?;
            spec.env
                .insert("CARGO_HOME".into(), install_home.into_os_string());
            let rustc = self.rustup.with_file_name("rustc");
            if rustc.is_file() {
                spec.env.insert("RUSTC".into(), rustc.into_os_string());
            }
            spec.args.extend([
                "env".into(),
                format!("CARGO_HOME={}", cargo_home.to_string_lossy()),
                self.rustup.to_string_lossy().into_owned(),
                "run".into(),
                version,
                call.bin.clone(),
            ]);
        } else {
            spec.args.push(call.bin.clone());
        }
        spec.args.extend(call.args);
        let bin = call.bin;
        self.executor
            .run(spec)
            .await
            .map(|_| ())
            .map_err(|e| format!("{bin}: {e}"))
    }
}

/// One ecosystem tool invocation: the mise tools it needs, the binary to run
/// and the arguments to pass.
struct Invocation {
    tools: Vec<(String, String)>,
    bin: String,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    args: Vec<String>,
}

fn validate_targets(
    plan: &Plan,
    config: &Config,
    files: &BTreeMap<String, String>,
) -> Result<(), String> {
    if plan.changes.is_empty() {
        return Ok(());
    }
    let extraction = rustovate_manager::Scanner::new(config)?.extract(files)?;
    let paths: BTreeSet<String> = files.keys().cloned().collect();
    for change in &plan.changes {
        let leaf = change.file.rsplit('/').next().unwrap_or("");
        if !matches!(leaf, "package.json" | "Cargo.toml" | "go.mod")
            && !change.file.ends_with(".tf")
        {
            continue;
        }
        let Some(target) = &change.value else {
            continue;
        };
        let Some(dep) = extraction
            .dependencies
            .iter()
            .find(|d| d.file == change.file && d.package == change.package && d.current == *target)
        else {
            return Err(format!(
                "tool did not retain requested target {} in {}",
                change.package, change.file
            ));
        };
        // pnpm and Yarn locks are not read back, so the unchanged-lockfile rule
        // checks them instead. Every lock this runner reads must prove the target.
        let unread = matches!(dep.manager, Manager::Npm | Manager::Bun)
            && nearest(&change.file, &paths, &node::LOCKS).is_some_and(|dir| {
                !["package-lock.json", "npm-shrinkwrap.json", "bun.lock"]
                    .iter()
                    .any(|leaf| paths.contains(&node::join(dir, leaf)))
            });
        if !unread
            && (matches!(dep.manager, Manager::Npm | Manager::Bun | Manager::Cargo)
                || (dep.source == Source::Terraform
                    && dep.kind == rustovate_config::DependencyKind::Provider))
            && let Some(version) = &change.version
            && dep.resolved.as_deref() != Some(version)
        {
            return Err(format!(
                "lockfile did not resolve {} to requested version {version}",
                change.package
            ));
        }
    }
    Ok(())
}

fn versions(
    baseline: &BTreeMap<String, Blob>,
    directory: &str,
) -> Result<BTreeMap<String, String>, String> {
    let defaults = tool_defaults()?;
    let mut versions = BTreeMap::new();
    for tool in ["node", "bun", "rust", "go", "terraform", "aube", "yarn"] {
        versions.insert(
            tool.into(),
            defaults["tools"][tool]
                .as_str()
                .ok_or("invalid tool defaults")?
                .into(),
        );
    }
    let mut configs: Vec<_> = baseline
        .iter()
        .filter(|(path, _)| {
            matches!(path.rsplit('/').next(), Some("mise.toml" | ".mise.toml"))
                && (directory == self::directory(path) || under(directory, self::directory(path)))
        })
        .collect();
    configs.sort_by_key(|(path, _)| path.len());
    for (_, blob) in configs {
        let doc = toml_edit::Document::parse(blob.content.as_str()).map_err(|e| e.to_string())?;
        if let Some(tools) = doc.get("tools") {
            for (tool, version) in &mut versions {
                if let Some(spec) = tools.get(tool) {
                    let value = spec
                        .as_str()
                        .or_else(|| spec.get("version").and_then(toml_edit::Item::as_str))
                        .ok_or_else(|| format!("{tool}: expected a version string or table"))?;
                    if value.is_empty()
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
                    {
                        return Err(format!("{tool}: unsupported tool version"));
                    }
                    *version = value.into();
                }
            }
        }
    }
    Ok(versions)
}
/// The pinned tool versions used when a repository does not pin its own.
fn tool_defaults() -> Result<toml_edit::Document<&'static str>, String> {
    toml_edit::Document::parse(include_str!("lockfiles/defaults.toml")).map_err(|e| e.to_string())
}
fn writable(path: &str) -> bool {
    matches!(
        path.rsplit('/').next().unwrap_or(path),
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lock"
            | "Cargo.toml"
            | "Cargo.lock"
            | "go.mod"
            | "go.sum"
            | ".terraform.lock.hcl"
            | "mise.lock"
    ) || path.ends_with(".tf")
}
fn materialized(path: &str) -> bool {
    writable(path)
        || matches!(
            path.rsplit('/').next().unwrap_or(path),
            "mise.toml"
                | ".mise.toml"
                | ".tool-versions"
                | "go.work"
                | "go.work.sum"
                | "pnpm-workspace.yaml"
        )
        || path.ends_with(".go")
        || path.ends_with(".rs")
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c,std::path::Component::Normal(s) if s != ".git"))
}
fn write(root: &Path, path: &str, content: &str) -> Result<(), String> {
    if !safe_path(path) {
        return Err("unsafe repository path".into());
    }
    let target = root.join(path);
    std::fs::create_dir_all(target.parent().ok_or("missing parent")?).map_err(|e| e.to_string())?;
    std::fs::write(target, content).map_err(|e| e.to_string())
}
fn collect(root: &Path) -> Result<BTreeMap<String, String>, String> {
    fn walk(root: &Path, dir: &Path, files: &mut BTreeMap<String, String>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if matches!(
                entry.file_name().to_str(),
                Some("node_modules" | ".terraform" | "target" | ".git" | ".yarn" | ".aube")
            ) {
                continue;
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("tool produced a symbolic link".into());
            }
            if kind.is_dir() {
                walk(root, &path, files)?;
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|e| e.to_string())?
                    .to_str()
                    .ok_or("non UTF-8 path")?
                    .to_owned();
                if entry.metadata().map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
                    return Err("generated file exceeds 16 MiB".into());
                }
                files.insert(
                    relative,
                    std::fs::read_to_string(path).map_err(|e| e.to_string())?,
                );
            } else {
                return Err("tool produced a special file".into());
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files)?;
    Ok(files)
}

#[cfg(test)]
mod tests;
