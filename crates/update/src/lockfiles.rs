//! Real ecosystem tools run before any Git Data mutation, in disposable trees.
mod apm;
mod checkout;
mod dotnet;
mod mise;
mod nix;
mod node;
mod php;
mod pin;
mod process;
mod python;
mod ruby;
mod tools;
pub use tools::{bootstrap_mise, tools_dir};
mod wrapper;
use crate::{
    Blob, Plan, Snapshot, blob_sha,
    plan::{hash, unified},
};
use patchula_config::{Config, Manager, NpmEngine, Source};
use patchula_manager::Dependency;
use patchula_process::{Executor, SystemExecutor};
use patchula_report::{ActionStatus, BranchAction, FileAction, Repository, Update};
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
    Python,
    GradleWrapper,
    Dotnet,
    Ruby,
    Php,
    Nix,
    Apm,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Job {
    pub ecosystem: Ecosystem,
    pub directory: String,
    pub package: Option<String>,
    pub current: Option<String>,
    pub target: Option<String>,
    /// The manifest is not edited, so the tool must be told the exact version.
    pub lock_only: bool,
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
            // A lockfile records the configuration beside it and above it, never
            // below: a nested mise.toml is locked by a mise.lock of its own.
            let root = directory(&dep.file);
            if !snapshot.paths.contains(&node::join(root, "mise.lock")) {
                return Ok(None);
            }
            return Ok(Some(Job {
                ecosystem: Ecosystem::Mise,
                directory: root.into(),
                package: Some(dep.name.clone()),
                current: Some(dep.current.clone()),
                target: update.new_version.clone().or(update.new_value.clone()),
                lock_only: false,
            }));
        }
        // Node version files pin the runtime, which no lockfile records.
        Manager::Npm | Manager::Bun
            if matches!(
                dep.file.rsplit('/').next(),
                Some(".nvmrc" | ".node-version")
            ) =>
        {
            return Ok(None);
        }
        Manager::Npm | Manager::Bun => {
            let Some(root) = nearest(
                &dep.file,
                &snapshot.paths,
                &[
                    "package-lock.json",
                    "npm-shrinkwrap.json",
                    "aube-lock.yaml",
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
        // `helm dependency update` downloads chart archives into the tree;
        // a committed Chart.lock is not regenerated yet.
        Manager::Helm => {
            let lock = format!(
                "{}Chart.lock",
                dep.file.strip_suffix("Chart.yaml").unwrap_or("")
            );
            if snapshot.paths.contains(&lock) {
                return Err("unsupported lockfile format: Chart.lock".into());
            }
            return Ok(None);
        }
        // apm.lock.yaml sits beside its apm.yml; APM reads no other name.
        Manager::Apm => {
            let root = directory(&dep.file);
            if !dep.file.ends_with("apm.yml")
                || !snapshot.paths.contains(&node::join(root, "apm.lock.yaml"))
            {
                return Ok(None);
            }
            (Ecosystem::Apm, root)
        }
        Manager::Bundler if dep.kind == patchula_config::DependencyKind::Toolchain => {
            return Ok(None);
        }
        Manager::Bundler => {
            let Some(root) = nearest(&dep.file, &snapshot.paths, &["Gemfile.lock"]) else {
                return Ok(None);
            };
            (Ecosystem::Ruby, root)
        }
        Manager::Composer => {
            let Some(root) = nearest(&dep.file, &snapshot.paths, &["composer.lock"]) else {
                return Ok(None);
            };
            (Ecosystem::Php, root)
        }
        // Each flake input is moved by name; nix resolves its new revision.
        Manager::Nix => {
            return Ok(Some(Job {
                ecosystem: Ecosystem::Nix,
                directory: directory(&dep.file).into(),
                package: Some(dep.name.clone()),
                current: dep.digest.clone(),
                target: update.new_digest.clone(),
                lock_only: false,
            }));
        }
        // NuGet locks sit beside each project; a manifest under a directory
        // with locks restores that directory's solution or project.
        Manager::Nuget => {
            let dir = directory(&dep.file);
            let locked = dep.kind != patchula_config::DependencyKind::Toolchain
                && !dep.file.ends_with("dotnet-tools.json")
                && snapshot.paths.iter().any(|p| {
                    p.rsplit('/').next() == Some("packages.lock.json")
                        && (dir.is_empty() || under(p, dir))
                });
            if !locked {
                return Ok(None);
            }
            return Ok(Some(Job {
                ecosystem: Ecosystem::Dotnet,
                directory: dir.into(),
                package: None,
                current: None,
                target: None,
                lock_only: false,
            }));
        }
        Manager::Gradle
            if dep
                .file
                .ends_with("gradle/wrapper/gradle-wrapper.properties") =>
        {
            return Ok(Some(Job {
                ecosystem: Ecosystem::GradleWrapper,
                directory: directory(&dep.file).into(),
                package: None,
                current: None,
                target: None,
                lock_only: false,
            }));
        }
        // Gradle dependency locking writes per-configuration lockfiles that
        // need the build itself to regenerate; hold such updates.
        Manager::Gradle => {
            if nearest(&dep.file, &snapshot.paths, &["gradle.lockfile"]).is_some() {
                return Err("unsupported lockfile format: gradle.lockfile".into());
            }
            return Ok(None);
        }
        // Toolchain files pin a tool, which no lockfile records.
        Manager::Cargo | Manager::Terraform
            if matches!(
                dep.file.rsplit('/').next(),
                Some("rust-toolchain" | "rust-toolchain.toml" | ".terraform-version")
            ) =>
        {
            return Ok(None);
        }
        Manager::Cargo => {
            let lock = nearest(&dep.file, &snapshot.paths, &["Cargo.lock"]);
            // A release the declaration already allows changes the lock and
            // nothing else; with no committed lock there is nothing to change,
            // and cargo would publish one it generated.
            if lock.is_none() && update.new_value.is_none() {
                return Ok(None);
            }
            (
                Ecosystem::Cargo,
                lock.unwrap_or_else(|| directory(&dep.file)),
            )
        }
        Manager::Go => (Ecosystem::Go, directory(&dep.file)),
        // `.python-version` pins the interpreter, which no lockfile records.
        Manager::Pyproject if dep.file.ends_with(".python-version") => return Ok(None),
        Manager::Pyproject => {
            // Lockfiles of other Python tools sit beside the pyproject.toml
            // too; leaving one stale would publish a manifest that disagrees
            // with it. The closest lockfile governs the project, so one above a
            // supported lock does not hold it.
            let supported = nearest(&dep.file, &snapshot.paths, &python::LOCKS);
            if let Some(lock) = nearest(&dep.file, &snapshot.paths, &python::UNSUPPORTED_LOCKS)
                .filter(|lock| supported.is_none_or(|root| lock.len() >= root.len()))
            {
                return Err(format!(
                    "unsupported lockfile format: {}",
                    python::UNSUPPORTED_LOCKS
                        .iter()
                        .find(|leaf| snapshot.paths.contains(&node::join(lock, leaf)))
                        .unwrap_or(&"lockfile")
                ));
            }
            let Some(root) = supported else {
                return Ok(None);
            };
            (Ecosystem::Python, root)
        }
        Manager::Terraform if dep.source == Source::Terraform => {
            (Ecosystem::Terraform, directory(&dep.file))
        }
        _ => return Ok(None),
    };
    let targeted = matches!(
        ecosystem,
        Ecosystem::Cargo | Ecosystem::Go | Ecosystem::Python | Ecosystem::Ruby | Ecosystem::Php
    ) && matches!(
        dep.source,
        Source::Crates | Source::Go | Source::Pypi | Source::Rubygems | Source::Packagist
    );
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
        lock_only: targeted && update.new_value.is_none(),
    }))
}

/// Keep the scheduled branch in desired names between windows, so ordinary
/// dependency runs do not close an outstanding maintenance PR.
pub fn maintenance(
    repository: &Repository,
    config: &Config,
    snapshot: &Snapshot,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Plan>, String> {
    use chrono::Datelike;
    if !config.lockfiles.refresh {
        return Ok(None);
    }
    let mut jobs = BTreeSet::new();
    for path in &snapshot.paths {
        let leaf = path.rsplit('/').next().unwrap();
        let ecosystem = match leaf {
            "package-lock.json"
            | "npm-shrinkwrap.json"
            | "aube-lock.yaml"
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
            "uv.lock" | "poetry.lock" if config.managers.contains(&Manager::Pyproject) => {
                Ecosystem::Python
            }
            "packages.lock.json" if config.managers.contains(&Manager::Nuget) => Ecosystem::Dotnet,
            "Gemfile.lock" if config.managers.contains(&Manager::Bundler) => Ecosystem::Ruby,
            "composer.lock" if config.managers.contains(&Manager::Composer) => Ecosystem::Php,
            "flake.lock"
                if config.managers.contains(&Manager::Nix)
                    && snapshot
                        .paths
                        .contains(&node::join(directory(path), "flake.nix")) =>
            {
                Ecosystem::Nix
            }
            "apm.lock.yaml"
                if config.managers.contains(&Manager::Apm)
                    && snapshot
                        .paths
                        .contains(&node::join(directory(path), "apm.yml")) =>
            {
                Ecosystem::Apm
            }
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
            lock_only: false,
        });
    }
    if jobs.is_empty() {
        return Ok(None);
    }
    let mut windows: Vec<_> = config
        .defaults
        .windows
        .iter()
        .map(|w| patchula_report::Window {
            cron: w.cron.clone(),
            zone: w.zone.clone(),
        })
        .collect();
    if windows.is_empty() {
        // Monday 00:00 to 04:00 UTC, unless the repository sets windows.
        windows.push(patchula_report::Window {
            cron: vec!["* 0-3 * * 1".into()],
            zone: "UTC".into(),
        });
    }
    let due = crate::plan::in_window(&windows, since, now);
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
        batch: None,
        tooling: Vec::new(),
    }))
}

fn refresh_key(repo: &str) -> patchula_cache::ExtractionKey {
    patchula_cache::ExtractionKey {
        repo: repo.into(),
        default_branch_commit: "maintenance".into(),
        config_hash: "maintenance-success-v1".into(),
    }
}
pub async fn defer_completed_refresh(
    cache: &patchula_cache::Cache,
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
    cache: &patchula_cache::Cache,
    plans: &[Plan],
    actions: &[patchula_report::PullRequestAction],
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

#[derive(Clone)]
pub struct Runner {
    credentials: Option<Arc<dyn patchula_github::Credentials>>,
    cache: PathBuf,
    token: String,
    executor: Arc<dyn Executor>,
    mise_data: PathBuf,
    mise: PathBuf,
    rustup: PathBuf,
    rustup_home: PathBuf,
    /// Fetches the Gradle wrapper checksum; injectable for offline tests.
    http: Arc<dyn patchula_http::Transport>,
    /// Serialises tool usage records within this process.
    usage: Arc<std::sync::Mutex<()>>,
    /// The user's mise data directory, whose installs seed the tool store.
    seed: Option<PathBuf>,
    /// Held by the first nix call while it installs nix, so concurrent
    /// lookups neither install it twice nor clear each other's install.
    nix_install: Arc<tokio::sync::Mutex<()>>,
}
impl Runner {
    pub fn new(cache: PathBuf, token: &str) -> Self {
        let home = std::env::var_os("HOME").map_or_else(std::env::temp_dir, PathBuf::from);
        let mise_data = tools::tools_dir(&cache);
        let mise = tools::find_mise(&mise_data).unwrap_or_else(|| "mise".into());
        let rustup = std::env::var_os("CARGO_HOME")
            .map_or_else(|| home.join(".cargo"), PathBuf::from)
            .join("bin/rustup");
        let rustup = if rustup.is_file() {
            rustup
        } else {
            "rustup".into()
        };
        let rustup_home =
            std::env::var_os("RUSTUP_HOME").map_or_else(|| home.join(".rustup"), PathBuf::from);
        Self {
            credentials: None,
            cache,
            token: token.into(),
            executor: Arc::new(SystemExecutor),
            mise_data,
            mise,
            rustup,
            rustup_home,
            http: Arc::new(NetworkTransport),
            usage: Default::default(),
            seed: None,
            nix_install: Default::default(),
        }
    }
    /// Borrow tool versions from this mise data directory, usually the user's
    /// own, instead of downloading them. Local runs only: the server keeps a
    /// store of its own.
    pub fn with_seed(mut self, mise_data: PathBuf) -> Self {
        self.seed = Some(mise_data);
        self
    }
    pub fn with_http(mut self, http: Arc<dyn patchula_http::Transport>) -> Self {
        self.http = http;
        self
    }
    pub fn with_executor(mut self, executor: Arc<dyn Executor>) -> Self {
        self.executor = executor;
        self
    }
    /// Version lookups through the pinned mise, in this runner's tool store and
    /// through its executor.
    pub fn mise_lookup(&self) -> Arc<patchula_mise::Lookup> {
        let defaults = tool_defaults().expect("embedded tool defaults parse");
        let version = defaults["tools"]["mise"]
            .as_str()
            .expect("embedded defaults pin mise");
        Arc::new(patchula_mise::Lookup::new(
            self.executor.clone(),
            self.mise.clone(),
            self.mise_data.clone(),
            version.into(),
        ))
    }
    pub fn with_credentials(mut self, credentials: Arc<dyn patchula_github::Credentials>) -> Self {
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
            let versions = if matches!(ecosystem, Ecosystem::Mise | Ecosystem::Nix) {
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
                Ecosystem::Ruby => {
                    self.bundle_lock(
                        node::NodeContext {
                            driver: &driver,
                            root: &root,
                            baseline: &baseline,
                            directory: &directory,
                            config,
                            versions: &versions,
                            maintenance: plan.maintenance,
                        },
                        &jobs,
                    )
                    .await?;
                }
                Ecosystem::Php => {
                    self.composer_lock(
                        node::NodeContext {
                            driver: &driver,
                            root: &root,
                            baseline: &baseline,
                            directory: &directory,
                            config,
                            versions: &versions,
                            maintenance: plan.maintenance,
                        },
                        &jobs,
                    )
                    .await?;
                }
                Ecosystem::Apm => {
                    self.apm_lock(node::NodeContext {
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
                Ecosystem::Dotnet => {
                    let edited = plan
                        .contents
                        .keys()
                        .any(|p| directory.is_empty() || p.starts_with(&format!("{directory}/")));
                    self.dotnet_restore(
                        &driver,
                        &root,
                        &directory,
                        &versions,
                        edited && !plan.maintenance,
                    )
                    .await?;
                }
                Ecosystem::GradleWrapper => {
                    self.gradle_wrapper(&root, &directory).await?;
                }
                Ecosystem::Python => {
                    self.python_locks(
                        node::NodeContext {
                            driver: &driver,
                            root: &root,
                            baseline: &baseline,
                            directory: &directory,
                            config,
                            versions: &versions,
                            maintenance: plan.maintenance,
                        },
                        &jobs,
                    )
                    .await?;
                }
                Ecosystem::Nix => {
                    self.flake_lock(
                        &driver,
                        &root,
                        &baseline,
                        &directory,
                        &jobs,
                        plan.maintenance,
                    )
                    .await?;
                }
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
                        self.cargo_update(&driver, &cwd, &versions["rust"], job, plan.maintenance)
                            .await?;
                    }
                }
                Ecosystem::Go => {
                    let go = go_version(&cwd, &versions["go"])?;
                    for job in jobs {
                        if let Some(package) = &job.package {
                            self.tool(
                                &driver,
                                Invocation {
                                    tools: vec![("go".into(), go.clone())],
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
                            tools: vec![("go".into(), go.clone())],
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
            let old = before.map_or("", |b| b.content.as_str());
            let new = after.map_or("", String::as_str);
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
                before.cloned().unwrap_or_else(|| Blob {
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
    /// Moves Cargo.lock for one job, against the lock as it stands: a job whose
    /// target is already locked has nothing to do. A package moves with
    /// `--precise` to the exact target. A target across a semver-incompatible boundary is a second
    /// lock entry beside the old one, which other crates may still require, so
    /// `--precise` on the old entry cannot reach it; `--workspace` locks what
    /// the edited manifests need and leaves every other package where it is.
    async fn cargo_update(
        &self,
        driver: &Path,
        cwd: &Path,
        rust: &str,
        job: &Job,
        maintenance: bool,
    ) -> Result<(), String> {
        let update = |args: Vec<String>| {
            let mut all = vec![
                "update".into(),
                "--manifest-path".into(),
                cwd.join("Cargo.toml").to_string_lossy().into_owned(),
            ];
            all.extend(args);
            self.tool(
                driver,
                Invocation {
                    tools: vec![("rust".into(), rust.into())],
                    bin: "cargo".into(),
                    cwd: None,
                    env: Vec::new(),
                    args: all,
                },
            )
        };
        let Some(package) = &job.package else {
            let args = if maintenance {
                vec![]
            } else {
                vec!["--workspace".into()]
            };
            update(args).await?;
            return Ok(());
        };
        let current = job.current.as_deref().unwrap_or("");
        let target = job.target.as_deref().ok_or("missing Cargo target")?;
        let crossing = crosses_compatibility(current, target);
        if crossing {
            update(vec!["--workspace".into()]).await?;
        }
        // Address what the lock holds now: an earlier job or the workspace
        // update may have moved the entry this job was planned against.
        let lock = std::fs::read_to_string(cwd.join("Cargo.lock")).map_err(|e| e.to_string())?;
        let locked = locked_versions(&lock, package);
        if locked.iter().any(|version| same_release(version, target)) {
            return Ok(());
        }
        // Cargo keeps one entry per compatibility range, so this is the one
        // entry that can still move to the target.
        let mut movable: Vec<_> = locked
            .iter()
            .filter(|version| !crosses_compatibility(version, target))
            .cloned()
            .collect();
        if movable.is_empty() && !crossing {
            // Nothing matches; let cargo name the package it cannot find.
            movable.push(current.into());
        }
        for version in movable {
            update(vec![
                "-p".into(),
                format!("{package}@{version}"),
                "--precise".into(),
                target.into(),
            ])
            .await?;
        }
        Ok(())
    }
    async fn tool(&self, driver: &Path, call: Invocation) -> Result<Vec<u8>, String> {
        let mut spec = self.command(driver);
        // A first use installs the tool inside this call, from scratch.
        if !self.installed(&call.tools) {
            {
                let _store = tools::lock_store(&self.mise_data, true).await?;
                if !self.installed(&call.tools) {
                    self.clear_unproven(&call.tools).await;
                    self.seed(&call.tools);
                }
            }
            spec.timeout = tools::INSTALL_TIMEOUT;
        }
        let _store = tools::lock_store(&self.mise_data, false).await?;
        if let Some(cwd) = &call.cwd {
            spec.cwd.clone_from(cwd);
        }
        // nix answers in JSON, which is read whole.
        if call.bin == "nix" {
            spec.output_limit = 1 << 20;
            spec.complete_output = true;
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
        let output = self
            .executor
            .run(spec)
            .await
            .map_err(|e| format!("{bin}: {e}"))?;
        self.record_use(&call.tools);
        Ok(output)
    }
}

/// One ecosystem tool invocation: the mise tools it needs, the binary to run
/// and the arguments to pass.
#[derive(Clone)]
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
    let extraction = patchula_manager::Scanner::new(config)?.extract(files)?;
    let paths: BTreeSet<String> = files.keys().cloned().collect();
    for change in &plan.changes {
        let leaf = change.file.rsplit('/').next().unwrap_or("");
        // A lockfile-only change must have moved the lock to its exact target.
        if leaf == "Cargo.lock" {
            let lock = files
                .get(&change.file)
                .and_then(|text| toml_edit::Document::parse(text.as_str()).ok());
            let moved = lock.as_ref().is_some_and(|lock| {
                lock.get("package")
                    .and_then(toml_edit::Item::as_array_of_tables)
                    .is_some_and(|packages| {
                        packages.iter().any(|p| {
                            p.get("name").and_then(toml_edit::Item::as_str) == Some(&change.package)
                                && p.get("version")
                                    .and_then(toml_edit::Item::as_str)
                                    .zip(change.version.as_deref())
                                    .is_some_and(|(locked, requested)| {
                                        same_release(locked, requested)
                                    })
                        })
                    })
            });
            if !moved {
                return Err(format!(
                    "lockfile did not resolve {} to requested version {}",
                    change.package,
                    change.version.as_deref().unwrap_or("?")
                ));
            }
            continue;
        }
        if !matches!(
            leaf,
            "package.json"
                | "Cargo.toml"
                | "go.mod"
                | "pyproject.toml"
                | "Gemfile"
                | "composer.json"
        ) && !change.file.ends_with(".tf")
        {
            continue;
        }
        // A lockfile-only change leaves the declaration as it was, so the lock
        // alone has to show the target.
        let declared = match (&change.value, &change.version) {
            (Some(value), _) => value,
            (None, Some(_)) => &change.current,
            (None, None) => continue,
        };
        let Some(dep) = extraction.dependencies.iter().find(|d| {
            d.file == change.file && d.package == change.package && d.current == *declared
        }) else {
            return Err(format!(
                "tool did not retain requested target {} in {}",
                change.package, change.file
            ));
        };
        // pnpm and Yarn locks are not read back (aube-lock.yaml is), so the unchanged-lockfile rule
        // checks them instead. Every lock this runner reads must prove the target.
        let unread = matches!(dep.manager, Manager::Npm | Manager::Bun)
            && nearest(&change.file, &paths, &node::LOCKS).is_some_and(|dir| {
                ![
                    "package-lock.json",
                    "npm-shrinkwrap.json",
                    "bun.lock",
                    "aube-lock.yaml",
                ]
                .iter()
                .any(|leaf| paths.contains(&node::join(dir, leaf)))
            });
        if !unread
            && (matches!(
                dep.manager,
                Manager::Npm
                    | Manager::Bun
                    | Manager::Cargo
                    | Manager::Pyproject
                    | Manager::Bundler
                    | Manager::Composer
            ) || (dep.source == Source::Terraform
                && dep.kind == patchula_config::DependencyKind::Provider))
            && let Some(version) = &change.version
            && !dep
                .resolved
                .as_deref()
                .is_some_and(|resolved| same_release(resolved, version))
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
    for tool in [
        "node",
        "bun",
        "rust",
        "go",
        "terraform",
        "aube",
        "yarn",
        "python",
        "uv",
        "poetry",
        "dotnet",
        "ruby",
        "php",
        "composer",
        "apm",
    ] {
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
/// Whether a locked version is the requested one. SemVer build metadata does
/// not order versions, so a request without it matches any build: the advisory
/// for `zstd-sys` fixes `2.1.2`, which the lock records as `2.1.2+zstd.1.5.7`.
pub(crate) fn same_release(locked: &str, requested: &str) -> bool {
    locked == requested || (!requested.contains('+') && locked.split('+').next() == Some(requested))
}
/// Whether Cargo treats `target` as incompatible with `current`: a different
/// major version, or a different minor (or patch) one below 1.0. Cargo then
/// keeps both in the lock.
fn crosses_compatibility(current: &str, target: &str) -> bool {
    use patchula_config::Update;
    use patchula_versioning::{Ecosystem as Scheme, Versioning};
    match (Scheme::Cargo.parse(current), Scheme::Cargo.parse(target)) {
        (Ok(current), Ok(target)) => Scheme::Cargo.classify(&current, &target) == Update::Major,
        _ => false,
    }
}
/// The versions of `package` a Cargo.lock records.
fn locked_versions(lock: &str, package: &str) -> Vec<String> {
    let Ok(lock) = toml_edit::Document::parse(lock) else {
        return Vec::new();
    };
    lock.get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("name").and_then(toml_edit::Item::as_str) == Some(package))
        .filter_map(|entry| entry.get("version")?.as_str())
        .map(str::to_owned)
        .collect()
}
/// The Go to run: the newest of the pin and the edited `go.mod`'s `go` and
/// `toolchain` directives. With GOTOOLCHAIN=local, an older Go refuses a
/// module that requires a newer one, which an update itself may introduce.
fn go_version(cwd: &Path, pinned: &str) -> Result<String, String> {
    use patchula_versioning::{Ecosystem as Scheme, Versioning};
    let text = std::fs::read_to_string(cwd.join("go.mod")).map_err(|e| e.to_string())?;
    let mut best = pinned.to_string();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let version = match (words.next(), words.next()) {
            (Some("go"), Some(v)) => v,
            (Some("toolchain"), Some(v)) => v.strip_prefix("go").unwrap_or(v),
            _ => continue,
        };
        // Only plain release versions select a toolchain.
        if !version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            continue;
        }
        if let (Ok(candidate), Ok(current)) =
            (Scheme::Loose.parse(version), Scheme::Loose.parse(&best))
            && Scheme::Loose.order(&candidate, &current).is_gt()
        {
            best = version.to_string();
        }
    }
    Ok(best)
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
            | "aube-lock.yaml"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lock"
            | "Cargo.toml"
            | "Cargo.lock"
            | "go.mod"
            | "go.sum"
            | ".terraform.lock.hcl"
            | "mise.lock"
            | "pyproject.toml"
            | "gradle-wrapper.properties"
            | "packages.lock.json"
            | "Gemfile"
            | "Gemfile.lock"
            | "Directory.Packages.props"
            | "Directory.Build.props"
            | "dotnet-tools.json"
            | "global.json"
            | "uv.lock"
            | "poetry.lock"
            | "composer.json"
            | "composer.lock"
            | "flake.lock"
            | "apm.lock.yaml"
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
                | ".yarnrc.yml"
                | ".python-version"
                | "uv.toml"
                | "poetry.toml"
                | "flake.nix"
                | "apm.yml"
        )
        || path.ends_with(".go")
        || path.ends_with(".rs")
        || [".csproj", ".fsproj", ".vbproj", ".sln", ".slnx", ".gemspec"]
            .iter()
            .any(|e| path.ends_with(e))
        || matches!(
            path.rsplit('/').next(),
            Some("Directory.Build.targets" | "nuget.config" | "NuGet.Config")
        )
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
                Some(
                    "node_modules"
                        | ".terraform"
                        | "target"
                        | ".git"
                        | ".yarn"
                        | ".aube"
                        | "apm_modules"
                )
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

/// The default checksum transport: a network client built on first use, so a
/// runner that never touches a Gradle wrapper needs no network setup.
struct NetworkTransport;
impl patchula_http::Transport for NetworkTransport {
    fn send(&self, request: reqwest::Request) -> patchula_http::Exchange<'_> {
        Box::pin(async move {
            static NETWORK: std::sync::OnceLock<Option<patchula_http::Network>> =
                std::sync::OnceLock::new();
            let network = NETWORK
                .get_or_init(|| patchula_http::Network::new().ok())
                .as_ref()
                .ok_or(patchula_http::Error::Configuration)?;
            network.send(request).await
        })
    }
}
