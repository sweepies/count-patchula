//! Real ecosystem tools run before any Git Data mutation, in disposable trees.
mod apm;
mod caches;
mod checkout;
mod credentials;
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
use crate::{Blob, Plan, Snapshot, blob_sha, plan::unified};
use futures_util::StreamExt;
use patchula_cache::State;
use patchula_config::{Config, NpmEngine};
use patchula_manager::{Dependency, hash};
use patchula_process::{Executor, Failure, SystemExecutor};
use patchula_report::{BranchAction, BranchStatus, FileAction, Repository, Update};
use patchula_words::{Manager, Source};
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
/// Whether `path`, materialized for a run of `ecosystem`, could steer
/// what its tools fetch: a manifest, lockfile or settings file of the
/// ecosystem, anywhere in the snapshot. A sibling project reached
/// through a `ProjectReference`, a `.sln`, a `file:../x`, a uv or
/// Poetry path dependency or a Composer path repository can steer the
/// run as much as one inside its directory, so the directory does not
/// bound it.
fn steers(ecosystem: Ecosystem, path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    match ecosystem {
        Ecosystem::Node => matches!(
            leaf,
            "package.json"
                | ".npmrc"
                | ".yarnrc.yml"
                | "pnpm-workspace.yaml"
                | "package-lock.json"
                | "npm-shrinkwrap.json"
                | "aube-lock.yaml"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "bun.lock"
        ),
        Ecosystem::Python => matches!(
            leaf,
            "pyproject.toml"
                | "uv.toml"
                | "poetry.toml"
                | "pdm.toml"
                | "pixi.toml"
                | "hatch.toml"
                | "uv.lock"
                | "poetry.lock"
        ),
        // The repository's `.bundle/config` never steers a run: Bundler's
        // environment overrides it. A `.gemspec` steers nothing a verdict
        // can prove either — it is arbitrary Ruby, so it has no verdict
        // and a run that can read one fails closed.
        Ecosystem::Ruby => matches!(leaf, "Gemfile" | "Gemfile.lock") || leaf.ends_with(".gemspec"),
        Ecosystem::Php => matches!(leaf, "composer.json" | "composer.lock"),
        Ecosystem::Cargo => matches!(leaf, "Cargo.toml" | "Cargo.lock"),
        Ecosystem::Dotnet => {
            leaf.eq_ignore_ascii_case("nuget.config")
                || matches!(
                    leaf,
                    "packages.lock.json"
                        | "Directory.Packages.props"
                        | "Directory.Build.props"
                        | "Directory.Build.targets"
                        | "global.json"
                        | "dotnet-tools.json"
                )
                || [
                    ".csproj", ".fsproj", ".vbproj", ".sln", ".slnx", ".props", ".targets",
                ]
                .iter()
                .any(|extension| leaf.ends_with(extension))
        }
        // Go needs no verdicts: in the `public` partition it runs with
        // `GOPROXY=https://proxy.golang.org` — no `direct`, which could
        // fetch anonymously from a host only the server's network
        // reaches — so a fetch is public by construction, and a run that
        // fails there is retried in the installation partition with Go's
        // environment as it was.
        Ecosystem::Go => true,
        _ => false,
    }
}

/// Whether a run for `ecosystem` can fetch only from the ecosystem's
/// well-known public registries: every materialized file of its kinds,
/// anywhere in the snapshot, decided public-only. A file with no
/// verdict is not public. Fail closed.
fn public_only(
    ecosystem: Ecosystem,
    baseline: &BTreeMap<String, Blob>,
    verdicts: &BTreeMap<String, bool>,
) -> bool {
    matches!(ecosystem, Ecosystem::Go)
        || baseline
            .keys()
            .all(|path| !steers(ecosystem, path) || verdicts.get(path) == Some(&true))
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}
fn under(path: &str, root: &str) -> bool {
    root.is_empty() || path.strip_prefix(root).is_some_and(|p| p.starts_with('/'))
}
/// The deepest directory holding `file` (its own included) that has one of
/// `names`. Walks up from the file, so the cost follows its depth, not the
/// size of the tree.
fn nearest<'a>(file: &'a str, paths: &BTreeSet<String>, names: &[&str]) -> Option<&'a str> {
    let mut dir = directory(file);
    loop {
        if names
            .iter()
            .any(|name| paths.contains(&node::join(dir, name)))
        {
            return Some(dir);
        }
        if dir.is_empty() {
            return None;
        }
        dir = directory(dir);
    }
}
/// The paths under `dir`, which sort together.
fn subtree<'a>(paths: &'a BTreeSet<String>, dir: &'a str) -> impl Iterator<Item = &'a String> {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    paths
        .range(prefix.clone()..)
        .take_while(move |p| p.starts_with(&prefix))
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
            // A task file is locked with the config that includes it.
            let root = dep
                .included_by
                .as_deref()
                .map_or_else(|| directory(&dep.file), directory);
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
            if snapshot.paths.contains(&node::join(root, "bun.lockb")) {
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
        Manager::Bundler if dep.kind == patchula_words::DependencyKind::Toolchain => {
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
            let locked = dep.kind != patchula_words::DependencyKind::Toolchain
                && !dep.file.ends_with("dotnet-tools.json")
                && subtree(&snapshot.paths, dir)
                    .any(|p| p.rsplit('/').next() == Some("packages.lock.json"));
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
    let due = patchula_config::due(&config.maintenance_windows(), since, now);
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
                BranchStatus::Planned
            } else {
                BranchStatus::Skipped
            },
            reason: (!due).then(|| crate::plan::Hold::MaintenanceWindow.text().to_owned()),
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
        public_only: snapshot.public_only.clone(),
        deleted: BTreeSet::new(),
        maintenance: true,
        batch: None,
        tooling: Vec::new(),
        hold: (!due).then_some(crate::plan::Hold::MaintenanceWindow),
        failure: None,
    }))
}

/// When a repository's lockfile maintenance last completed.
#[derive(Serialize, Deserialize)]
struct MaintenanceSuccess(i64);
#[derive(Serialize)]
struct MaintenanceScope {
    repository: String,
}
impl State for MaintenanceSuccess {
    const NAMESPACE: &'static str = "maintenance-success";
    const VERSION: u32 = 1;
    type Scope = MaintenanceScope;
}
fn refresh_scope(repo: &str) -> MaintenanceScope {
    MaintenanceScope {
        repository: repo.into(),
    }
}
pub async fn defer_completed_refresh(
    cache: &patchula_cache::Cache,
    plan: &mut Plan,
    config: &Config,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    use chrono::Datelike;
    if !plan.maintenance || plan.action.status != BranchStatus::Planned {
        return Ok(());
    }
    if let Some(last) = cache
        .state::<MaintenanceSuccess>(&refresh_scope(&plan.action.repository))
        .await
        .map_err(|e| e.to_string())?
        && let Some(last) = chrono::DateTime::<chrono::Utc>::from_timestamp(last.0, 0)
        && (last.iso_week() == now.iso_week()
            || now - last < chrono::Duration::hours(i64::from(config.lockfiles.wait_hours)))
    {
        plan.hold(crate::plan::Hold::MaintenanceDone);
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
        if plan.held(&crate::plan::Hold::LockfilesCurrent)
            || actions.iter().any(|a| {
                a.branch == plan.action.branch
                    && matches!(
                        a.status,
                        patchula_report::PullRequestStatus::Created
                            | patchula_report::PullRequestStatus::Updated
                            | patchula_report::PullRequestStatus::Unchanged
                    )
            })
        {
            cache
                .set_state(
                    &refresh_scope(&plan.action.repository),
                    &MaintenanceSuccess(now.timestamp()),
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
    /// Operator registry entries whose credentials lockfile tools may use.
    registries: Vec<patchula_http::Credential>,
    /// Every operator registry entry, which decides which of `registries`
    /// a URL or host falls under.
    entries: Vec<patchula_http::Credential>,
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
    /// The persistent tool caches' size cap in GiB; 0 turns them off.
    cache_gib: u64,
    /// The `installation-<hash>` partition name for runs that hold no
    /// operator credentials but are not provably public-only:
    /// `installation-<sha256 of the GitHub credentials scope>` on the
    /// server, `installation-local` for the CLI.
    installation: String,
}

/// The persistent tool caches' default size cap in GiB.
pub const DEFAULT_CACHE_GIB: u64 = 20;
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
            registries: Vec::new(),
            entries: Vec::new(),
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
            cache_gib: DEFAULT_CACHE_GIB,
            installation: "installation-local".into(),
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
        // The installation's own partition, for runs that are not provably
        // public-only but hold no operator credentials.
        self.installation = format!("installation-{}", hash(credentials.scope().as_bytes()));
        self.credentials = Some(credentials);
        self
    }
    /// Set the persistent tool caches' size cap in GiB; 0 turns them off.
    pub fn set_cache_limit(&mut self, cache_gib: u64) {
        self.cache_gib = cache_gib;
    }
    /// Give lockfile tools the credentials of the entries that allow it
    /// (`tools = true`), among all of `registries`.
    pub fn with_registries(mut self, registries: &[patchula_http::Credential]) -> Self {
        self.entries = registries.to_vec();
        self.registries = registries
            .iter()
            .filter(|entry| entry.tools() && entry.auth.sends() && entry.mirror_of().is_none())
            .cloned()
            .collect();
        self
    }
    pub async fn prepare(&self, plan: &mut Plan, snapshot: &mut Snapshot, config: &Config) {
        let added = self.prepare_plan(plan, config).await;
        for (path, blob) in added {
            snapshot.files.entry(path).or_insert(blob);
        }
    }
    /// Every plan's lockfiles, `concurrency` plans at a time, calling `done`
    /// as each finishes. Each plan works in its own scratch tree; the base
    /// blobs they add, the same for every plan, join `snapshot` afterwards.
    pub async fn prepare_all(
        &self,
        plans: &mut [Plan],
        snapshot: &mut Snapshot,
        config: &Config,
        concurrency: usize,
        done: &(dyn Fn() + Sync),
    ) {
        type Task<'a> = std::pin::Pin<
            Box<dyn std::future::Future<Output = BTreeMap<String, Blob>> + Send + 'a>,
        >;
        let tasks: Vec<Task<'_>> = plans
            .iter_mut()
            .map(|plan| -> Task<'_> {
                Box::pin(async move {
                    let added = self.prepare_plan(plan, config).await;
                    done();
                    added
                })
            })
            .collect();
        let added: Vec<BTreeMap<String, Blob>> = futures_util::stream::iter(tasks)
            .buffer_unordered(concurrency.max(1))
            .collect()
            .await;
        for (path, blob) in added.into_iter().flatten() {
            snapshot.files.entry(path).or_insert(blob);
        }
    }
    /// One plan's lockfiles, and the base blobs of the files they change.
    async fn prepare_plan(&self, plan: &mut Plan, config: &Config) -> BTreeMap<String, Blob> {
        if plan.action.status != BranchStatus::Planned || plan.lockfiles_ready {
            return BTreeMap::new();
        }
        match self.generate(plan, config).await {
            Ok(added) => added,
            Err(error) => {
                plan.action.status = BranchStatus::Error;
                match error {
                    Error::Tool(tool) => {
                        plan.action.reason = Some(format!("lockfile generation failed: {tool}"));
                        plan.failure = Some(crate::text::tool_failure(
                            &format!("lockfile generation failed: {}", tool.context),
                            &tool.failure,
                        ));
                    }
                    Error::Text(text) => {
                        plan.action.reason = Some(format!("lockfile generation failed: {text}"));
                    }
                }
                BTreeMap::new()
            }
        }
    }
    async fn generate(
        &self,
        plan: &mut Plan,
        config: &Config,
    ) -> Result<BTreeMap<String, Blob>, Error> {
        std::fs::create_dir_all(&self.cache).map_err(|e| e.to_string())?;
        self.prune_caches();
        let run = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(&self.cache)
            .map_err(|e| e.to_string())?;
        let root = run.path().join("tree");
        let driver = run.path().join("driver");
        std::fs::create_dir_all(&driver).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let (baseline, cargo_configs) = self.checkout(plan, &root, &driver).await?;
        // Backstop for the engine's refusal: every file the tools can
        // read and the engine would check was checked before any of them
        // runs, so a committed credential can never fetch into the shared
        // public cache partition. Paths the repository excludes are not
        // checked, as the engine's are not.
        let scanner = patchula_manager::Scanner::new(config)?;
        for (path, text) in baseline
            .iter()
            .map(|(path, blob)| (path, blob.content.as_str()))
            .chain(
                plan.contents
                    .iter()
                    .chain(cargo_configs.iter())
                    .map(|(path, text)| (path, text.as_str())),
            )
            // Source files are checked out so tools can build, but no tool
            // reads registry credentials from them; a test's connection
            // string is not a registry credential.
            .filter(|(path, _)| scanner.credential_checked(path))
        {
            if let Some(kind) = patchula_manager::committed_credentials(path, text) {
                return Err(patchula_manager::credential_reason(path, &kind).into());
            }
        }
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
            // Whether every file the run can read decided public-only, so
            // its tools may share the `public` cache partition. Fail
            // closed: any file without a verdict makes it not public.
            let public = public_only(ecosystem, &baseline, &plan.public_only);
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
                            public,
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
                            public,
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
                        public,
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
                        public,
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
                            public,
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
                        public,
                    })
                    .await?;
                }
                Ecosystem::Cargo => {
                    // Cargo runs outside the repository and reads none of its
                    // configuration. It gets each credentialed registry the
                    // `.cargo/config.toml` files at or above the project
                    // define, by name, from the environment.
                    let registries: Vec<_> = cargo_configs
                        .iter()
                        .filter(|(path, _)| {
                            let owner = directory_of_cargo_config(path);
                            owner == directory || under(&directory, owner)
                        })
                        .map(|(_, text)| credentials::cargo_registries(text))
                        .collect();
                    let (env, entries) = self.cargo_env(&registries);
                    let credentials = credentials::scopes(entries.iter().copied());
                    let secrets = credentials::secrets(entries.iter().copied());
                    for job in jobs {
                        self.cargo_update(
                            &driver,
                            &cwd,
                            &versions["rust"],
                            job,
                            plan.maintenance,
                            (&env, &credentials, &secrets, public),
                        )
                        .await?;
                    }
                }
                Ecosystem::Go => {
                    let go = go_version(&cwd, &versions["go"])?;
                    // Credentials for Go and Git in `~/.netrc`, gone when Go
                    // finishes, and their hosts private to Go.
                    let (netrc, hosts, entries) = self.netrc();
                    let credentials = credentials::scopes(entries.iter().copied());
                    let secrets = credentials::secrets(entries.iter().copied());
                    let held = !hosts.is_empty();
                    let mut _netrc = node::Private::default();
                    let mut env = Vec::new();
                    if held {
                        let home = driver.join("home");
                        std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
                        _netrc.write(&home.join(".netrc"), &netrc)?;
                        env.push(("GOPRIVATE".to_string(), hosts.join(",")));
                    }
                    let mut calls = Vec::new();
                    for job in jobs {
                        if let Some(package) = &job.package {
                            calls.push(vec![
                                "-C".into(),
                                cwd.to_string_lossy().into_owned(),
                                "get".into(),
                                format!(
                                    "{package}@{}",
                                    job.target.as_deref().ok_or("missing Go target")?
                                ),
                            ]);
                        }
                    }
                    calls.push(vec![
                        "-C".into(),
                        cwd.to_string_lossy().into_owned(),
                        "mod".into(),
                        if config.lockfiles.tidy_go {
                            "tidy".into()
                        } else {
                            "download".into()
                        },
                    ]);
                    // What a Go run may fetch is bounded by GOPROXY, not
                    // by file verdicts. In the `public` partition it is
                    // `https://proxy.golang.org` alone — never `direct`,
                    // which could fetch anonymously from a host only this
                    // server's network reaches, such as a cluster-internal
                    // Git host, and leave what it got in a cache every
                    // installation shares; GOSUMDB keeps its default. A
                    // run that fails there — an internal module the proxy
                    // cannot serve — is rerun in the installation
                    // partition with Go's environment as it was (GOPROXY
                    // unset), so repositories with internal modules keep
                    // working. A credentialed run goes straight to its
                    // credential partition with that environment.
                    let mut attempts = vec![(false, Vec::new())];
                    if credentials.is_empty() {
                        attempts.insert(
                            0,
                            (
                                true,
                                vec![("GOPROXY".into(), "https://proxy.golang.org".into())],
                            ),
                        );
                    }
                    for (public, extra) in attempts {
                        let mut failed = None;
                        for args in &calls {
                            let mut run_env = env.clone();
                            run_env.extend(extra.iter().cloned());
                            let result = self
                                .held_tool(
                                    &driver,
                                    Invocation {
                                        tools: vec![("go".into(), go.clone())],
                                        bin: "go".into(),
                                        cwd: None,
                                        env: run_env,
                                        credentials: credentials.clone(),
                                        secrets: secrets.clone(),
                                        public,
                                        args: args.clone(),
                                    },
                                )
                                .await;
                            if let Err(error) = result {
                                failed = Some(error);
                                break;
                            }
                        }
                        match failed {
                            None => break,
                            // The last attempt's error stands; only the
                            // `public` attempt retries.
                            Some(error) if !public => return Err(error),
                            Some(_) => {}
                        }
                    }
                }
                Ecosystem::Terraform => {
                    if cwd.join(".terraform.lock.hcl").exists() {
                        std::fs::remove_file(cwd.join(".terraform.lock.hcl"))
                            .map_err(|e| e.to_string())?;
                    }
                    let (env, entries) = self.terraform_env();
                    self.held_tool(
                        &driver,
                        Invocation {
                            tools: vec![("terraform".into(), versions["terraform"].clone())],
                            bin: "terraform".into(),
                            cwd: None,
                            env,
                            credentials: credentials::scopes(entries.iter().copied()),
                            secrets: credentials::secrets(entries.iter().copied()),
                            public: false,
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
        let mut added = BTreeMap::new();
        let all: BTreeSet<_> = baseline.keys().chain(generated.keys()).cloned().collect();
        let mut files = BTreeMap::new();
        for path in all {
            let before = baseline.get(&path);
            let after = generated.get(&path);
            if before.map(|b| &b.content) == after {
                continue;
            }
            if !writable(&path) && plan.contents.get(&path) != after {
                return Err(format!("tool modified unexpected file {path}").into());
            }
            if path.rsplit('/').next() == Some("mise.lock")
                && !plan
                    .lock_jobs
                    .iter()
                    .any(|j| j.ecosystem == Ecosystem::Mise && j.directory == directory(&path))
            {
                return Err(format!("tool modified unexpected mise lockfile {path}").into());
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
            added.insert(
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
            plan.hold(crate::plan::Hold::LockfilesCurrent);
        }
        Ok(added)
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
        access: CargoAccess<'_>,
    ) -> Result<(), Error> {
        let (env, credentials, secrets, public) = access;
        let update = |args: Vec<String>| {
            let mut all = vec![
                "update".into(),
                "--manifest-path".into(),
                cwd.join("Cargo.toml").to_string_lossy().into_owned(),
            ];
            all.extend(args);
            self.held_tool(
                driver,
                Invocation {
                    tools: vec![("rust".into(), rust.into())],
                    bin: "cargo".into(),
                    cwd: None,
                    env: env.to_vec(),
                    credentials: credentials.to_vec(),
                    secrets: secrets.to_vec(),
                    public,
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
        let locked = crates_io_entries(&lock, package);
        if locked
            .iter()
            .any(|(version, _)| same_release(version, target))
        {
            return Ok(());
        }
        // Cargo keeps one entry per compatibility range, so this is the one
        // crates.io entry that can still move to the target. A git or other
        // registry copy of the name is another package, and the source in the
        // spec keeps an equal name and version from being ambiguous.
        let mut movable: Vec<String> = locked
            .iter()
            .filter(|(version, _)| !crosses_compatibility(version, target))
            .map(|(version, source)| format!("{source}#{package}@{version}"))
            .collect();
        if movable.is_empty() && !crossing {
            // Nothing matches; let cargo name the package it cannot find.
            movable.push(format!("{package}@{current}"));
        }
        for spec in movable {
            update(vec!["-p".into(), spec, "--precise".into(), target.into()]).await?;
        }
        Ok(())
    }
    /// Evict aged and oversized tool-cache partitions, at most once an hour
    /// per cache root per process, without holding the generation up. Lock
    /// files outlive the partitions they guard.
    fn prune_caches(&self) {
        let root = self.cache.join(caches::ROOT);
        if !caches::due(&root) {
            return;
        }
        let cap = self.cache_gib.saturating_mul(1 << 30);
        tokio::task::spawn_blocking(move || {
            if let Err(error) = caches::prune(&root, cap, std::time::SystemTime::now()) {
                tracing::warn!(%error, "tool cache pruning failed");
            }
        });
    }
    /// The tool's persistent cache partition and its environment, held for the
    /// run. A run's partition is `public` unless it receives registry
    /// credentials, in which case their scopes name it.
    async fn use_caches(
        &self,
        call: &Invocation,
    ) -> Result<(Vec<(String, String)>, Option<caches::Hold>), String> {
        if self.cache_gib == 0 {
            return Ok((Vec::new(), None));
        }
        let Some(tool) = caches::tool(&call.bin) else {
            return Ok((Vec::new(), None));
        };
        let mut scopes = call.credentials.clone();
        scopes.sort_unstable();
        scopes.dedup();
        // Credentialed runs partition on the credential set as before.
        // Uncredentialed runs share `public` only when every file they can
        // read decided public-only; anything else gets the installation's
        // own partition, so a run that might fetch private content never
        // leaves it where other installations could pick it up.
        let name = if !scopes.is_empty() {
            caches::partition(&scopes)
        } else if call.public {
            caches::PUBLIC.into()
        } else {
            self.installation.clone()
        };
        let root = self.cache.join(caches::ROOT);
        let bin = call.bin.clone();
        // `-modcacherw` keeps the module cache writable for eviction; merge
        // it with any GOFLAGS the call already set.
        let goflags = call
            .env
            .iter()
            .find(|(key, _)| key == "GOFLAGS")
            .map(|(_, value)| value.clone());
        let (mut env, hold) =
            tokio::task::spawn_blocking(move || caches::acquire(&root, &name, &tool, &bin))
                .await
                .map_err(|e| e.to_string())??;
        if let Some(flags) = goflags.filter(|flags| !flags.is_empty()) {
            for (key, value) in &mut env {
                if key == "GOFLAGS" {
                    *value = format!("{flags} {value}");
                }
            }
        }
        Ok((env, Some(hold)))
    }
    async fn tool(&self, driver: &Path, call: Invocation) -> Result<Vec<u8>, Error> {
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
        let (cache_env, _caches) = self.use_caches(&call).await?;
        for (key, value) in cache_env {
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
        spec.secrets = call.secrets;
        let bin = call.bin;
        let output = self.executor.run(spec).await.map_err(|failure| {
            Error::Tool(ToolFailure {
                context: bin,
                failure,
            })
        })?;
        self.record_use(&call.tools);
        Ok(output)
    }
    /// Run a tool that may hold registry credentials. When it does, a failure
    /// shows none of its output, which may quote what a private registry
    /// answered: failure reasons reach reports, dashboards and public pages.
    async fn held_tool(&self, driver: &Path, call: Invocation) -> Result<Vec<u8>, Error> {
        let bin = call.bin.clone();
        let held = !call.credentials.is_empty();
        let result = self.tool(driver, call).await;
        if !held {
            return result;
        }
        result.map_err(|_| {
            Error::Text(format!(
                "{bin} failed while it held registry credentials, so its output is not shown"
            ))
        })
    }
}

/// A lockfile tool's process failure with what it ran, kept structured so
/// the report renders it without reparsing prose.
#[derive(Debug)]
pub struct ToolFailure {
    /// The binary that failed, such as `cargo`.
    pub context: String,
    pub failure: Failure,
}
impl std::fmt::Display for ToolFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.failure)
    }
}

/// Why lockfile generation failed: a tool's process failure kept
/// structured, or reason text.
#[derive(Debug)]
pub enum Error {
    Tool(ToolFailure),
    Text(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool(tool) => write!(f, "{tool}"),
            Self::Text(text) => f.write_str(text),
        }
    }
}
impl From<String> for Error {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}
impl From<&str> for Error {
    fn from(text: &str) -> Self {
        Self::Text(text.into())
    }
}

/// What a Cargo run is given: its registry environment, the credential
/// scopes it holds, every secret value a failure must not repeat, and
/// whether it may share the `public` cache partition.
type CargoAccess<'a> = (&'a [(String, String)], &'a [String], &'a [String], bool);

/// One ecosystem tool invocation: the mise tools it needs, the binary to run
/// and the arguments to pass.
#[derive(Clone)]
struct Invocation {
    tools: Vec<(String, String)>,
    bin: String,
    cwd: Option<PathBuf>,
    env: Vec<(String, String)>,
    /// The `Credential::scope()`s of the registry entries this run receives.
    /// They decide its cache partition and whether a failure shows its output.
    credentials: Vec<String>,
    /// Every secret value the run was given, in every form: a failure must
    /// never repeat one.
    secrets: Vec<String>,
    /// Whether the run can fetch only from its ecosystem's well-known
    /// public registry, which lets it share the `public` cache partition
    /// when it holds no operator credentials.
    public: bool,
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
    let extraction =
        patchula_manager::Scanner::new(config)?.extract(&patchula_manager::all_read(files))?;
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
                                && crates_io(p).is_some()
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
        // The registry is left out: it records where the lock says the package
        // came from, which the tool's own rewrite may state differently.
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
                && dep.kind == patchula_words::DependencyKind::Provider))
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
    use patchula_versioning::{Ecosystem as Scheme, Versioning};
    use patchula_words::Update;
    match (Scheme::Cargo.parse(current), Scheme::Cargo.parse(target)) {
        (Ok(current), Ok(target)) => Scheme::Cargo.classify(&current, &target) == Update::Major,
        _ => false,
    }
}
/// The source a Cargo.lock entry records when it comes from crates.io.
fn crates_io(entry: &toml_edit::Table) -> Option<&str> {
    entry
        .get("source")
        .and_then(toml_edit::Item::as_str)
        .filter(|source| {
            source.starts_with("registry+https://github.com/rust-lang/crates.io-index")
                || source.starts_with("sparse+https://index.crates.io/")
        })
}
/// The version and source of each crates.io entry for `package` in a
/// Cargo.lock. A git or other registry package of the same name is another
/// package.
fn crates_io_entries(lock: &str, package: &str) -> Vec<(String, String)> {
    let Ok(lock) = toml_edit::Document::parse(lock) else {
        return Vec::new();
    };
    lock.get("package")
        .and_then(toml_edit::Item::as_array_of_tables)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("name").and_then(toml_edit::Item::as_str) == Some(package))
        .filter_map(|entry| {
            Some((
                entry.get("version")?.as_str()?.to_owned(),
                crates_io(entry)?.to_owned(),
            ))
        })
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
/// Manifest and lockfile leaf names a tool may modify: `writable`, minus the
/// `*.tf` extension.
const WRITABLE_LEAVES: &[&str] = &[
    "package.json",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "aube-lock.yaml",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "Cargo.toml",
    "Cargo.lock",
    "go.mod",
    "go.sum",
    ".terraform.lock.hcl",
    "mise.lock",
    "pyproject.toml",
    "gradle-wrapper.properties",
    "packages.lock.json",
    "Gemfile",
    "Gemfile.lock",
    "Directory.Packages.props",
    "Directory.Build.props",
    "dotnet-tools.json",
    "global.json",
    "uv.lock",
    "poetry.lock",
    "composer.json",
    "composer.lock",
    "flake.lock",
    "apm.lock.yaml",
];
/// Leaf names tools read but never modify.
const READ_LEAVES: &[&str] = &[
    "mise.toml",
    ".mise.toml",
    ".tool-versions",
    "go.work",
    "go.work.sum",
    "pnpm-workspace.yaml",
    ".yarnrc.yml",
    ".npmrc",
    ".python-version",
    "uv.toml",
    "poetry.toml",
    "flake.nix",
    "apm.yml",
    "Directory.Build.targets",
];
/// Extensions of files tools read (and `*.tf`, which they may modify).
const READ_EXTENSIONS: &[&str] = &[
    ".tf", ".go", ".rs", ".csproj", ".fsproj", ".vbproj", ".sln", ".slnx", ".gemspec",
];
fn writable(path: &str) -> bool {
    WRITABLE_LEAVES.contains(&path.rsplit('/').next().unwrap_or(path)) || path.ends_with(".tf")
}
fn materialized(path: &str) -> bool {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    writable(path)
        || READ_LEAVES.contains(&leaf)
        || READ_EXTENSIONS
            .iter()
            .any(|extension| path.ends_with(extension))
        || leaf.eq_ignore_ascii_case("nuget.config")
}
/// A Cargo configuration file, read only for its `registries`.
fn cargo_config(path: &str) -> bool {
    [".cargo/config.toml", ".cargo/config"]
        .iter()
        .any(|leaf| path == *leaf || path.ends_with(&format!("/{leaf}")))
}
/// The directory whose `.cargo` holds `path`, a Cargo configuration file.
fn directory_of_cargo_config(path: &str) -> &str {
    directory(directory(path))
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
