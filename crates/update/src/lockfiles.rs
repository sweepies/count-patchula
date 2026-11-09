//! Real ecosystem tools run before any Git Data mutation, in disposable trees.
mod apm;
mod caches;
mod cargo;
mod checkout;
mod credentials;
mod dotnet;
mod go;
mod mise;
mod nix;
mod node;
mod php;
mod pin;
mod process;
mod python;
mod ruby;
mod terraform;
mod tool;
mod tools;
use tool::{Install, Tool, descriptor, descriptors};
pub use tools::{bootstrap_mise, tools_dir};
mod wrapper;
use crate::{Blob, Plan, Snapshot, blob_sha, plan::unified};
use futures_util::StreamExt;
use patchula_cache::State;
use patchula_config::{Config, NpmEngine};
use patchula_manager::roles::{lock_names, manifest_names};
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
/// What a lock job asks its ecosystem's tool to do.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Regenerate the directory's lockfile(s) whole.
    Directory,
    /// Move `package` to `target`; `current` is what the declaration or
    /// lock records, where the tool needs it.
    Package {
        package: String,
        current: Option<String>,
        target: Option<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Job {
    pub ecosystem: Ecosystem,
    pub directory: String,
    /// What the run is asked to move.
    pub scope: Scope,
    /// The manifest is not edited, so the tool must be told the exact version.
    pub lock_only: bool,
}
impl Job {
    /// The package a targeted job moves.
    pub fn package(&self) -> Option<&str> {
        match &self.scope {
            Scope::Package { package, .. } => Some(package),
            Scope::Directory => None,
        }
    }
    /// What the declaration or lock records the package at.
    pub fn current(&self) -> Option<&str> {
        match &self.scope {
            Scope::Package { current, .. } => current.as_deref(),
            Scope::Directory => None,
        }
    }
    /// What the job moves the package to.
    pub fn target(&self) -> Option<&str> {
        match &self.scope {
            Scope::Package { target, .. } => target.as_deref(),
            Scope::Directory => None,
        }
    }
}
// The wire shape is the old field layout: `package`, `current` and `target`
// stay top-level so serialized job identities do not change.
impl Serialize for Job {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let (package, current, target) = match &self.scope {
            Scope::Directory => (None, None, None),
            Scope::Package {
                package,
                current,
                target,
            } => (
                Some(package.as_str()),
                current.as_deref(),
                target.as_deref(),
            ),
        };
        let mut state = serializer.serialize_struct("Job", 6)?;
        state.serialize_field("ecosystem", &self.ecosystem)?;
        state.serialize_field("directory", &self.directory)?;
        state.serialize_field("package", &package)?;
        state.serialize_field("current", &current)?;
        state.serialize_field("target", &target)?;
        state.serialize_field("lock_only", &self.lock_only)?;
        state.end()
    }
}
impl<'de> Deserialize<'de> for Job {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            ecosystem: Ecosystem,
            directory: String,
            package: Option<String>,
            current: Option<String>,
            target: Option<String>,
            lock_only: bool,
        }
        let fields = Fields::deserialize(deserializer)?;
        let scope = match fields.package {
            Some(package) => Scope::Package {
                package,
                current: fields.current,
                target: fields.target,
            },
            None => Scope::Directory,
        };
        Ok(Self {
            ecosystem: fields.ecosystem,
            directory: fields.directory,
            scope,
            lock_only: fields.lock_only,
        })
    }
}

/// Whether `path`, materialized for a run of `descriptor`'s ecosystem, could
/// steer what its tools fetch: a manifest, lockfile or settings file of the
/// ecosystem, anywhere in the snapshot. A sibling project reached through a
/// `ProjectReference`, a `.sln`, a `file:../x`, a uv or Poetry path
/// dependency or a Composer path repository can steer the run as much as one
/// inside its directory, so the directory does not bound it.
fn steers(descriptor: &Tool, path: &str) -> bool {
    descriptor
        .steers
        .iter()
        .any(|manager| patchula_manager::roles::eco_roles(*manager, path).steers)
}

/// Whether a run for `descriptor`'s ecosystem can fetch only from the
/// ecosystem's well-known public registries: every materialized file of its
/// kinds, anywhere in the snapshot, decided public-only. A file with no
/// verdict is not public. Fail closed.
fn public_only(
    descriptor: &Tool,
    baseline: &BTreeMap<String, Blob>,
    verdicts: &BTreeMap<String, bool>,
) -> bool {
    baseline
        .keys()
        .all(|path| !steers(descriptor, path) || verdicts.get(path) == Some(&true))
}

fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}
fn under(path: &str, root: &str) -> bool {
    root.is_empty() || path.strip_prefix(root).is_some_and(|p| p.starts_with('/'))
}
/// The repo files `names` names in `directory` or a directory above it,
/// ordered root-first — by directory depth, then by `names` order (an
/// earlier name loses to a later one in the same directory), then by path —
/// so applying them in order leaves the nearest, highest-precedence file
/// winning. A candidate counts when its directory is `directory` itself or
/// an ancestor of it, the same reach `under` gives.
fn upward<'a>(
    paths: impl IntoIterator<Item = &'a String>,
    directory: &str,
    names: &[&str],
) -> Vec<&'a str> {
    let mut found: Vec<&str> = paths
        .into_iter()
        .map(String::as_str)
        .filter(|path| {
            let leaf = path.rsplit('/').next().unwrap_or(path);
            names.contains(&leaf)
                && (directory == self::directory(path) || under(directory, self::directory(path)))
        })
        .collect();
    found.sort_by_key(|path| {
        (
            self::directory(path).len(),
            names
                .iter()
                .position(|name| *name == path.rsplit('/').next().unwrap_or(path))
                .unwrap_or(usize::MAX),
            path.len(),
        )
    });
    found
}
/// The directory holding the nearest file `names` names at or above
/// `directory`, from `paths`.
fn nearest_dir<'a>(
    paths: impl IntoIterator<Item = &'a String>,
    directory: &str,
    names: &[&str],
) -> Option<&'a str> {
    upward(paths, directory, names)
        .last()
        .map(|path| self::directory(path))
}
/// The Node ecosystem's supported lockfile names, from the managers' roles.
fn node_locks() -> Vec<&'static str> {
    [Manager::Npm, Manager::Bun]
        .iter()
        .flat_map(|manager| lock_names(*manager).supported.iter().copied())
        .collect()
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
            if !patchula_manager::roles::lock_names(Manager::Mise)
                .supported
                .iter()
                .any(|lock| snapshot.paths.contains(&node::join(root, lock)))
            {
                return Ok(None);
            }
            return Ok(Some(Job {
                ecosystem: Ecosystem::Mise,
                directory: root.into(),
                scope: Scope::Package {
                    package: dep.name.clone(),
                    current: Some(dep.current.clone()),
                    target: update.new_version.clone().or(update.new_value.clone()),
                },
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
            let locks: Vec<&str> = [Manager::Npm, Manager::Bun]
                .iter()
                .flat_map(|manager| {
                    let names = patchula_manager::roles::lock_names(*manager);
                    names.supported.iter().chain(names.unsupported.iter())
                })
                .copied()
                .collect();
            let Some(root) = nearest_dir(snapshot.paths.iter(), directory(&dep.file), &locks)
            else {
                return Ok(None);
            };
            if [Manager::Npm, Manager::Bun].iter().any(|manager| {
                patchula_manager::roles::lock_names(*manager)
                    .unsupported
                    .iter()
                    .any(|lock| snapshot.paths.contains(&node::join(root, lock)))
            }) {
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
            if !patchula_manager::roles::manifest_names(Manager::Apm)
                .iter()
                .any(|name| dep.file.ends_with(name))
                || !patchula_manager::roles::lock_names(Manager::Apm)
                    .supported
                    .iter()
                    .any(|lock| snapshot.paths.contains(&node::join(root, lock)))
            {
                return Ok(None);
            }
            (Ecosystem::Apm, root)
        }
        Manager::Bundler if dep.kind == patchula_words::DependencyKind::Toolchain => {
            return Ok(None);
        }
        Manager::Bundler => {
            let Some(root) = nearest_dir(
                snapshot.paths.iter(),
                directory(&dep.file),
                lock_names(Manager::Bundler).supported,
            ) else {
                return Ok(None);
            };
            (Ecosystem::Ruby, root)
        }
        Manager::Composer => {
            let Some(root) = nearest_dir(
                snapshot.paths.iter(),
                directory(&dep.file),
                lock_names(Manager::Composer).supported,
            ) else {
                return Ok(None);
            };
            (Ecosystem::Php, root)
        }
        // Each flake input is moved by name; nix resolves its new revision.
        Manager::Nix => {
            return Ok(Some(Job {
                ecosystem: Ecosystem::Nix,
                directory: directory(&dep.file).into(),
                scope: Scope::Package {
                    package: dep.name.clone(),
                    current: dep.digest.clone(),
                    target: update.new_digest.clone(),
                },
                lock_only: false,
            }));
        }
        // NuGet locks sit beside each project; a manifest under a directory
        // with locks restores that directory's solution or project.
        Manager::Nuget => {
            let dir = directory(&dep.file);
            let locked = dep.kind != patchula_words::DependencyKind::Toolchain
                && !dep.file.ends_with("dotnet-tools.json")
                && subtree(&snapshot.paths, dir).any(|p| {
                    patchula_manager::roles::lock_names(Manager::Nuget)
                        .supported
                        .contains(&p.rsplit('/').next().unwrap_or(p))
                });
            if !locked {
                return Ok(None);
            }
            return Ok(Some(Job {
                ecosystem: Ecosystem::Dotnet,
                directory: dir.into(),
                scope: Scope::Directory,
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
                scope: Scope::Directory,
                lock_only: false,
            }));
        }
        // Gradle dependency locking writes per-configuration lockfiles that
        // need the build itself to regenerate; hold such updates.
        Manager::Gradle => {
            if nearest_dir(
                snapshot.paths.iter(),
                directory(&dep.file),
                lock_names(Manager::Gradle).unsupported,
            )
            .is_some()
            {
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
            let lock = nearest_dir(
                snapshot.paths.iter(),
                directory(&dep.file),
                lock_names(Manager::Cargo).supported,
            );
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
            let names = lock_names(Manager::Pyproject);
            let supported =
                nearest_dir(snapshot.paths.iter(), directory(&dep.file), names.supported);
            if let Some(lock) = nearest_dir(
                snapshot.paths.iter(),
                directory(&dep.file),
                names.unsupported,
            )
            .filter(|lock| supported.is_none_or(|root| lock.len() >= root.len()))
            {
                return Err(format!(
                    "unsupported lockfile format: {}",
                    names
                        .unsupported
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
    let targeted = dep.movable_lockfile();
    Ok(Some(Job {
        ecosystem,
        directory: root.into(),
        scope: if targeted {
            Scope::Package {
                package: dep.package.clone(),
                current: Some(dep.resolved.clone().unwrap_or_else(|| dep.current.clone())),
                target: Some(
                    update
                        .new_version
                        .clone()
                        .or(update.new_value.clone())
                        .unwrap_or_else(|| dep.current.clone()),
                ),
            }
        } else {
            Scope::Directory
        },
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
        let locks = |manager: Manager| lock_names(manager).supported.contains(&leaf);
        let ecosystem = match leaf {
            _ if [Manager::Npm, Manager::Bun]
                .iter()
                .any(|manager| locks(*manager))
                && config
                    .managers
                    .iter()
                    .any(|m| matches!(m, Manager::Npm | Manager::Bun)) =>
            {
                Ecosystem::Node
            }
            _ if locks(Manager::Cargo) && config.managers.contains(&Manager::Cargo) => {
                Ecosystem::Cargo
            }
            _ if locks(Manager::Pyproject) && config.managers.contains(&Manager::Pyproject) => {
                Ecosystem::Python
            }
            _ if locks(Manager::Nuget) && config.managers.contains(&Manager::Nuget) => {
                Ecosystem::Dotnet
            }
            _ if locks(Manager::Bundler) && config.managers.contains(&Manager::Bundler) => {
                Ecosystem::Ruby
            }
            _ if locks(Manager::Composer) && config.managers.contains(&Manager::Composer) => {
                Ecosystem::Php
            }
            _ if locks(Manager::Nix)
                && config.managers.contains(&Manager::Nix)
                && manifest_names(Manager::Nix)
                    .iter()
                    .any(|name| snapshot.paths.contains(&node::join(directory(path), name))) =>
            {
                Ecosystem::Nix
            }
            _ if locks(Manager::Apm)
                && config.managers.contains(&Manager::Apm)
                && manifest_names(Manager::Apm)
                    .iter()
                    .any(|name| snapshot.paths.contains(&node::join(directory(path), name))) =>
            {
                Ecosystem::Apm
            }
            _ if locks(Manager::Mise) && config.managers.contains(&Manager::Mise) => {
                Ecosystem::Mise
            }
            _ if manifest_names(Manager::Go).contains(&leaf)
                && config.managers.contains(&Manager::Go) =>
            {
                Ecosystem::Go
            }
            _ if locks(Manager::Terraform) && config.managers.contains(&Manager::Terraform) => {
                Ecosystem::Terraform
            }
            _ => continue,
        };
        jobs.insert(Job {
            ecosystem,
            directory: directory(path).into(),
            scope: Scope::Directory,
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
    let mut plan = Plan::new(BranchAction {
        repository: repository.repository.clone(),
        branch: crate::branch_name(&config.prs.branch, "lock-file-maintenance")?,
        base_commit: repository.commit.clone().ok_or("missing base")?,
        status: BranchStatus::Planned,
        reason: None,
        message: format!("{} lock file maintenance", config.defaults.commit_prefix),
        previous_commit: None,
        commit: None,
        verified: None,
        lockfile_pending: false,
        files: Vec::new(),
    });
    plan.identity = hash(b"lock-file-maintenance");
    plan.fingerprint = fingerprint.clone();
    plan.input_fingerprint = fingerprint;
    plan.lock_jobs = jobs.into_iter().collect();
    plan.public_only = snapshot.public_only.clone();
    plan.maintenance = true;
    if !due {
        plan.hold(crate::plan::Hold::MaintenanceWindow);
    }
    Ok(Some(plan))
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
    if !plan.maintenance || plan.action().status != BranchStatus::Planned {
        return Ok(());
    }
    if let Some(last) = cache
        .state::<MaintenanceSuccess>(&refresh_scope(&plan.action().repository))
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
                a.branch == plan.action().branch
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
                    &refresh_scope(&plan.action().repository),
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
    /// The `installation-<id>` partition's id for runs that hold no
    /// operator credentials but are not provably public-only: the hash of
    /// the GitHub credentials scope on the server, `local` for the CLI.
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
            installation: "local".into(),
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
        self.installation = hash(credentials.scope().as_bytes());
        self.credentials = Some(credentials);
        self
    }
    /// The GitHub token this run fetches with: the installation's
    /// credentials when it has them, else the runner's own.
    async fn github_token(&self) -> Result<String, String> {
        match &self.credentials {
            Some(credentials) => credentials
                .token()
                .await
                .map_err(|_| "GitHub authentication failed".to_owned()),
            None => Ok(self.token.clone()),
        }
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
        if plan.action().status != BranchStatus::Planned || plan.lockfiles_ready {
            return BTreeMap::new();
        }
        match self.generate(plan, config).await {
            Ok(added) => added,
            Err(error) => {
                plan.lockfiles_failed(error);
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
            let tool = descriptor(ecosystem);
            let versions = if tool.versions {
                versions(&baseline, &directory)?
            } else {
                BTreeMap::new()
            };
            let jobs: Vec<_> = plan
                .lock_jobs
                .iter()
                .filter(|j| j.ecosystem == ecosystem && j.directory == directory)
                .collect();
            // Whether every file the run can read decided public-only, so
            // its tools may share the `public` cache partition. Fail
            // closed: any file without a verdict makes it not public.
            let public = public_only(tool, &baseline, &plan.public_only);
            let context = node::NodeContext {
                driver: &driver,
                root: &root,
                baseline: &baseline,
                directory: &directory,
                config,
                versions: &versions,
                maintenance: plan.maintenance,
                public,
            };
            match ecosystem {
                Ecosystem::Node => self.node(context).await?,
                Ecosystem::Cargo => {
                    // Cargo runs outside the repository and reads none of its
                    // configuration. It gets each credentialed registry the
                    // `.cargo/config.toml` files at or above the project
                    // define, by name, from the environment.
                    self.cargo_locks(context, &jobs, &cargo_configs).await?;
                }
                Ecosystem::Go => self.go_locks(context, &jobs).await?,
                Ecosystem::Terraform => self.terraform_lock(context).await?,
                Ecosystem::Mise => self.mise_lock(context, &jobs).await?,
                Ecosystem::Python => self.python_locks(context, &jobs).await?,
                Ecosystem::GradleWrapper => self.gradle_wrapper(context).await?,
                Ecosystem::Dotnet => {
                    let edited = plan.contents.keys().any(|p| {
                        context.directory.is_empty()
                            || p.starts_with(&format!("{}/", context.directory))
                    }) && !context.maintenance;
                    self.dotnet_restore(context, edited).await?;
                }
                Ecosystem::Ruby => self.bundle_lock(context, &jobs).await?,
                Ecosystem::Php => self.composer_lock(context, &jobs).await?,
                Ecosystem::Nix => self.flake_lock(context, &jobs).await?,
                Ecosystem::Apm => self.apm_lock(context).await?,
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
        plan.lockfiles_generated(contents, deleted, files.into_values().collect());
        if plan.action().files.is_empty() {
            plan.hold(crate::plan::Hold::LockfilesCurrent);
        }
        Ok(added)
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
        let partition = if !scopes.is_empty() {
            caches::Partition::private(&scopes)
        } else if call.public {
            caches::Partition::Public
        } else {
            caches::Partition::Installation(self.installation.clone())
        };
        let root = self.cache.join(caches::ROOT);
        let bin = call.bin.clone();
        let call_env = call.env.clone();
        let (env, hold) = tokio::task::spawn_blocking(move || {
            caches::acquire(&root, &partition, &tool, &bin, &call_env)
        })
        .await
        .map_err(|e| e.to_string())??;
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
        if lock_names(Manager::Cargo).supported.contains(&leaf) {
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
        let manifest = [
            Manager::Npm,
            Manager::Cargo,
            Manager::Go,
            Manager::Pyproject,
            Manager::Bundler,
            Manager::Composer,
        ]
        .iter()
        .any(|manager| manifest_names(*manager).contains(&leaf));
        if !manifest && !change.file.ends_with(".tf") {
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
            && nearest_dir(paths.iter(), directory(&change.file), &node_locks()).is_some_and(
                |dir| {
                    ![Manager::Npm, Manager::Bun]
                        .iter()
                        .flat_map(|manager| lock_names(*manager).readable.iter().copied())
                        .any(|leaf| paths.contains(&node::join(dir, leaf)))
                },
            );
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
    let mut versions: BTreeMap<String, String> = BTreeMap::new();
    for tool in descriptors()
        .flat_map(|descriptor| descriptor.tools.iter().copied())
        .collect::<BTreeSet<_>>()
    {
        versions.insert(
            tool.into(),
            defaults["tools"][tool]
                .as_str()
                .ok_or("invalid tool defaults")?
                .into(),
        );
    }
    // mise reads `.tool-versions` below `mise.toml` and `.mise.toml` in the
    // same directory, and a deeper file over a shallower one; `upward`
    // orders the same way, so applying in order leaves the winner last.
    for path in upward(
        baseline.keys(),
        directory,
        &[".tool-versions", "mise.toml", ".mise.toml"],
    ) {
        let blob = &baseline[path];
        if path.ends_with(".tool-versions") {
            for line in blob.content.lines() {
                let mut words = line.split('#').next().unwrap_or("").split_whitespace();
                let (Some(name), Some(version)) = (words.next(), words.next()) else {
                    continue;
                };
                let Some(slot) = versions.get_mut(name) else {
                    continue;
                };
                check_tool_version(name, version)?;
                *slot = version.into();
            }
            continue;
        }
        let doc = toml_edit::Document::parse(blob.content.as_str()).map_err(|e| e.to_string())?;
        if let Some(tools) = doc.get("tools") {
            for (tool, version) in &mut versions {
                if let Some(spec) = tools.get(tool) {
                    let value = spec
                        .as_str()
                        .or_else(|| spec.get("version").and_then(toml_edit::Item::as_str))
                        .ok_or_else(|| format!("{tool}: expected a version string or table"))?;
                    check_tool_version(tool, value)?;
                    *version = value.into();
                }
            }
        }
    }
    Ok(versions)
}
/// A repository's pin for a tool the runner knows, accepted only as a plain
/// version spec.
fn check_tool_version(tool: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
    {
        return Err(format!("{tool}: unsupported tool version"));
    }
    Ok(())
}
/// Whether a locked version is the requested one. SemVer build metadata does
/// not order versions, so a request without it matches any build: the advisory
/// for `zstd-sys` fixes `2.1.2`, which the lock records as `2.1.2+zstd.1.5.7`.
pub(crate) fn same_release(locked: &str, requested: &str) -> bool {
    locked == requested || (!requested.contains('+') && locked.split('+').next() == Some(requested))
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
/// The pinned tool versions used when a repository does not pin its own,
/// parsed once: the file is embedded, so it cannot change between calls.
fn tool_defaults() -> Result<&'static toml_edit::Document<&'static str>, String> {
    static DEFAULTS: std::sync::OnceLock<Result<toml_edit::Document<&'static str>, String>> =
        std::sync::OnceLock::new();
    DEFAULTS
        .get_or_init(|| {
            toml_edit::Document::parse(include_str!("lockfiles/defaults.toml"))
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}
/// A Cargo configuration file, read only for its `registries`.
fn cargo_config(path: &str) -> bool {
    patchula_manager::roles::cargo_config(path)
}
fn writable(path: &str) -> bool {
    patchula_manager::roles::roles(path).writable
}
fn materialized(path: &str) -> bool {
    let roles = patchula_manager::roles::roles(path);
    roles.writable || roles.read
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
