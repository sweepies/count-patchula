//! The lockfile contract: the work a branch's lockfile jobs describe and
//! what a run hands back, shared by update planning and the tool runtime
//! that executes it. Nothing here knows `Plan` or `Snapshot`.
use patchula_config::Config;
use patchula_manager::Dependency;
use patchula_manager::roles::{lock_names, manifest_names};
use patchula_report::Update;
use patchula_words::{Manager, Source};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
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
    /// Serialized as `lock_only`, the name the wire shape already has.
    pub exact_version: bool,
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
        state.serialize_field("lock_only", &self.exact_version)?;
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
            exact_version: fields.lock_only,
        })
    }
}

/// The directory `path` sits in: everything before its last `/`.
pub fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}
/// `leaf` inside `directory`, or `leaf` at the repository root.
pub fn join(directory: &str, leaf: &str) -> String {
    if directory.is_empty() {
        leaf.to_string()
    } else {
        format!("{directory}/{leaf}")
    }
}
/// The paths under `dir`, which sort together.
pub fn subtree<'a>(paths: &'a BTreeSet<String>, dir: &'a str) -> impl Iterator<Item = &'a String> {
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    paths
        .range(prefix.clone()..)
        .take_while(move |p| p.starts_with(&prefix))
}
/// A set of repository paths `upward` can look a candidate up in.
pub trait Paths {
    fn find(&self, path: &str) -> Option<&String>;
}
impl Paths for BTreeSet<String> {
    fn find(&self, path: &str) -> Option<&String> {
        self.get(path)
    }
}
impl<V> Paths for BTreeMap<String, V> {
    fn find(&self, path: &str) -> Option<&String> {
        self.get_key_value(path).map(|(path, _)| path)
    }
}
/// The repo files `names` names in `directory` or a directory above it,
/// ordered root-first — by directory depth, then by `names` order (an
/// earlier name loses to a later one in the same directory) — so applying
/// them in order leaves the nearest, highest-precedence file winning. It
/// walks up from `directory` and looks each candidate up, so its cost grows
/// with the directory's depth, not with the size of the repository.
/// `names` are leaf names, never paths.
pub fn upward<'a>(paths: &'a impl Paths, directory: &str, names: &[&str]) -> Vec<&'a str> {
    let mut dirs = vec![directory];
    let mut dir = directory;
    while !dir.is_empty() {
        dir = self::directory(dir);
        dirs.push(dir);
    }
    dirs.iter()
        .rev()
        .flat_map(|dir| {
            names
                .iter()
                .filter_map(move |name| paths.find(&join(dir, name)))
        })
        .map(String::as_str)
        .collect()
}
/// The directory holding the nearest file `names` names at or above
/// `directory`, from `paths`.
pub fn nearest_dir<'a>(paths: &'a impl Paths, directory: &str, names: &[&str]) -> Option<&'a str> {
    upward(paths, directory, names)
        .last()
        .map(|path| self::directory(path))
}

/// The lockfile work `update` asks of `dep`'s manager, or an error when a
/// lockfile it cannot regenerate would go stale. `paths` is every path the
/// snapshot holds, selected lockfiles included.
pub fn job(
    dep: &Dependency,
    update: &Update,
    paths: &BTreeSet<String>,
) -> Result<Option<Job>, String> {
    let root = match dep.manager {
        Manager::Mise => {
            // A lockfile records the configuration beside it and above it, never
            // below: a nested mise.toml is locked by a mise.lock of its own.
            // A task file is locked with the config that includes it.
            let root = dep
                .included_by
                .as_deref()
                .map_or_else(|| directory(&dep.file), directory);
            if !lock_names(Manager::Mise)
                .supported
                .iter()
                .any(|lock| paths.contains(&join(root, lock)))
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
                exact_version: false,
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
                    let names = lock_names(*manager);
                    names.supported.iter().chain(names.unsupported.iter())
                })
                .copied()
                .collect();
            let Some(root) = nearest_dir(paths, directory(&dep.file), &locks) else {
                return Ok(None);
            };
            if [Manager::Npm, Manager::Bun].iter().any(|manager| {
                lock_names(*manager)
                    .unsupported
                    .iter()
                    .any(|lock| paths.contains(&join(root, lock)))
            }) {
                return Err("unsupported lockfile format".into());
            }
            root
        }
        // `helm dependency update` downloads chart archives into the tree;
        // a committed Chart.lock is not regenerated yet.
        Manager::Helm => {
            let lock = format!(
                "{}Chart.lock",
                dep.file.strip_suffix("Chart.yaml").unwrap_or("")
            );
            if paths.contains(&lock) {
                return Err("unsupported lockfile format: Chart.lock".into());
            }
            return Ok(None);
        }
        // apm.lock.yaml sits beside its apm.yml; APM reads no other name.
        Manager::Apm => {
            let root = directory(&dep.file);
            if !manifest_names(Manager::Apm)
                .iter()
                .any(|name| dep.file.ends_with(name))
                || !lock_names(Manager::Apm)
                    .supported
                    .iter()
                    .any(|lock| paths.contains(&join(root, lock)))
            {
                return Ok(None);
            }
            root
        }
        Manager::Bundler if dep.kind == patchula_words::DependencyKind::Toolchain => {
            return Ok(None);
        }
        Manager::Bundler => {
            let Some(root) = nearest_dir(
                paths,
                directory(&dep.file),
                lock_names(Manager::Bundler).supported,
            ) else {
                return Ok(None);
            };
            root
        }
        Manager::Composer => {
            let Some(root) = nearest_dir(
                paths,
                directory(&dep.file),
                lock_names(Manager::Composer).supported,
            ) else {
                return Ok(None);
            };
            root
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
                exact_version: false,
            }));
        }
        // NuGet locks sit beside each project; a manifest under a directory
        // with locks restores that directory's solution or project.
        Manager::Nuget => {
            let dir = directory(&dep.file);
            let locked = dep.kind != patchula_words::DependencyKind::Toolchain
                && !dep.file.ends_with("dotnet-tools.json")
                && subtree(paths, dir).any(|p| {
                    lock_names(Manager::Nuget)
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
                exact_version: false,
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
                exact_version: false,
            }));
        }
        // Gradle dependency locking writes per-configuration lockfiles that
        // need the build itself to regenerate; hold such updates.
        Manager::Gradle => {
            if nearest_dir(
                paths,
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
                paths,
                directory(&dep.file),
                lock_names(Manager::Cargo).supported,
            );
            // A release the declaration already allows changes the lock and
            // nothing else; with no committed lock there is nothing to change,
            // and cargo would publish one it generated.
            if lock.is_none() && update.new_value.is_none() {
                return Ok(None);
            }
            lock.unwrap_or_else(|| directory(&dep.file))
        }
        Manager::Go => directory(&dep.file),
        // `.python-version` pins the interpreter, which no lockfile records.
        Manager::Pyproject if dep.file.ends_with(".python-version") => return Ok(None),
        Manager::Pyproject => {
            // Lockfiles of other Python tools sit beside the pyproject.toml
            // too; leaving one stale would publish a manifest that disagrees
            // with it. The closest lockfile governs the project, so one above a
            // supported lock does not hold it.
            let names = lock_names(Manager::Pyproject);
            let supported = nearest_dir(paths, directory(&dep.file), names.supported);
            if let Some(lock) = nearest_dir(paths, directory(&dep.file), names.unsupported)
                .filter(|lock| supported.is_none_or(|root| lock.len() >= root.len()))
            {
                return Err(format!(
                    "unsupported lockfile format: {}",
                    names
                        .unsupported
                        .iter()
                        .find(|leaf| paths.contains(&join(lock, leaf)))
                        .unwrap_or(&"lockfile")
                ));
            }
            let Some(root) = supported else {
                return Ok(None);
            };
            root
        }
        Manager::Terraform if dep.source == Source::Terraform => directory(&dep.file),
        // Terraform sources outside the registry have no lock entry to regenerate.
        Manager::Terraform => return Ok(None),
        // No lockfile the Count generates.
        Manager::GithubActions
        | Manager::Dockerfile
        | Manager::Compose
        | Manager::Kubernetes
        | Manager::Kustomize
        | Manager::Flux
        | Manager::HelmValues
        | Manager::PreCommit
        | Manager::Devcontainer
        | Manager::Woodpecker
        | Manager::Copier
        | Manager::Maven
        | Manager::Gitlabci
        | Manager::GitlabciInclude
        | Manager::Regex => return Ok(None),
    };
    let ecosystem = ecosystem(dep.manager).expect("job's dispatch maps every ecosystem it yields");
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
        exact_version: targeted && update.new_value.is_none(),
    }))
}

/// How scheduled lockfile maintenance finds an ecosystem's work in a
/// snapshot, from the same file roles `job()` maps dependencies through.
pub enum Refresh {
    /// A supported lockfile of any listed manager claims the directory;
    /// the run needs one of them enabled in `config.managers`.
    Locks(&'static [Manager]),
    /// `Locks`, but only beside a manifest of the same managers.
    LocksBesideManifest(&'static [Manager]),
    /// The managers' manifest files are the work itself.
    Manifests(&'static [Manager]),
    /// Maintenance never schedules this ecosystem.
    Never,
}

impl Refresh {
    /// Whether `leaf` in `directory` is this ecosystem's maintenance work
    /// under `config`, given the snapshot's paths.
    pub fn claims(
        &self,
        config: &Config,
        leaf: &str,
        directory: &str,
        paths: &BTreeSet<String>,
    ) -> bool {
        let enabled = |managers: &[Manager]| managers.iter().any(|m| config.managers.contains(m));
        match self {
            Self::Never => false,
            Self::Manifests(managers) => {
                enabled(managers) && managers.iter().any(|m| manifest_names(*m).contains(&leaf))
            }
            Self::Locks(managers) => {
                managers
                    .iter()
                    .any(|m| lock_names(*m).supported.contains(&leaf))
                    && enabled(managers)
            }
            Self::LocksBesideManifest(managers) => {
                managers
                    .iter()
                    .any(|m| lock_names(*m).supported.contains(&leaf))
                    && enabled(managers)
                    && managers.iter().any(|m| {
                        manifest_names(*m)
                            .iter()
                            .any(|name| paths.contains(&join(directory, name)))
                    })
            }
        }
    }
}

/// How maintenance discovers `ecosystem`'s work; `job()` does not consult
/// this — extraction already applied `config.managers` to the dependencies
/// it reports, while maintenance sees raw paths.
pub fn refresh(ecosystem: Ecosystem) -> Refresh {
    match ecosystem {
        Ecosystem::Node => Refresh::Locks(&[Manager::Npm, Manager::Bun]),
        Ecosystem::Cargo => Refresh::Locks(&[Manager::Cargo]),
        Ecosystem::Go => Refresh::Manifests(&[Manager::Go]),
        Ecosystem::Terraform => Refresh::Locks(&[Manager::Terraform]),
        Ecosystem::Mise => Refresh::Locks(&[Manager::Mise]),
        Ecosystem::Python => Refresh::Locks(&[Manager::Pyproject]),
        Ecosystem::GradleWrapper => Refresh::Never,
        Ecosystem::Dotnet => Refresh::Locks(&[Manager::Nuget]),
        Ecosystem::Ruby => Refresh::Locks(&[Manager::Bundler]),
        Ecosystem::Php => Refresh::Locks(&[Manager::Composer]),
        Ecosystem::Nix => Refresh::LocksBesideManifest(&[Manager::Nix]),
        Ecosystem::Apm => Refresh::LocksBesideManifest(&[Manager::Apm]),
    }
}

/// The refresh jobs `paths` holds under `config`: a supported lockfile's
/// directory claims its ecosystem's whole-directory work. First match wins,
/// in the order the ecosystems were always claimed: a lock name two
/// ecosystems could read goes to the earlier one.
pub fn refresh_jobs(config: &Config, paths: &BTreeSet<String>) -> BTreeSet<Job> {
    let mut jobs = BTreeSet::new();
    for path in paths {
        let leaf = path.rsplit('/').next().unwrap();
        let Some(ecosystem) = [
            Ecosystem::Node,
            Ecosystem::Cargo,
            Ecosystem::Python,
            Ecosystem::Dotnet,
            Ecosystem::Ruby,
            Ecosystem::Php,
            Ecosystem::Nix,
            Ecosystem::Apm,
            Ecosystem::Mise,
            Ecosystem::Go,
            Ecosystem::Terraform,
        ]
        .into_iter()
        .find(|ecosystem| refresh(*ecosystem).claims(config, leaf, directory(path), paths)) else {
            continue;
        };
        jobs.insert(Job {
            ecosystem,
            directory: directory(path).into(),
            scope: Scope::Directory,
            exact_version: false,
        });
    }
    jobs
}

/// Whether a locked version is the requested one. SemVer build metadata does
/// not order versions, so a request without it matches any build: the advisory
/// for `zstd-sys` fixes `2.1.2`, which the lock records as `2.1.2+zstd.1.5.7`.
pub fn same_release(locked: &str, requested: &str) -> bool {
    locked == requested || (!requested.contains('+') && locked.split('+').next() == Some(requested))
}

/// The ecosystem `manager`'s lockfile work runs through, or `None` for a
/// manager whose changes never produce a lockfile job. `job()`'s dispatch
/// and `validate_targets`' proof lists both read this one mapping, so
/// re-scoping a manager's edit semantics updates validation automatically.
pub fn ecosystem(manager: Manager) -> Option<Ecosystem> {
    match manager {
        Manager::Mise => Some(Ecosystem::Mise),
        Manager::Npm | Manager::Bun => Some(Ecosystem::Node),
        Manager::Apm => Some(Ecosystem::Apm),
        Manager::Bundler => Some(Ecosystem::Ruby),
        Manager::Composer => Some(Ecosystem::Php),
        Manager::Nix => Some(Ecosystem::Nix),
        Manager::Nuget => Some(Ecosystem::Dotnet),
        Manager::Gradle => Some(Ecosystem::GradleWrapper),
        Manager::Cargo => Some(Ecosystem::Cargo),
        Manager::Go => Some(Ecosystem::Go),
        Manager::Pyproject => Some(Ecosystem::Python),
        Manager::Terraform => Some(Ecosystem::Terraform),
        Manager::Helm
        | Manager::GithubActions
        | Manager::Dockerfile
        | Manager::Compose
        | Manager::Kubernetes
        | Manager::Kustomize
        | Manager::Flux
        | Manager::HelmValues
        | Manager::PreCommit
        | Manager::Devcontainer
        | Manager::Woodpecker
        | Manager::Copier
        | Manager::Maven
        | Manager::Gitlabci
        | Manager::GitlabciInclude
        | Manager::Regex => None,
    }
}

/// One branch's lockfile work, borrowed from its plan.
pub struct Work<'a> {
    /// The repository `owner/name` the checkout fetches.
    pub repository: &'a str,
    /// The extraction commit the checkout applies the edits to.
    pub base_commit: &'a str,
    /// The lockfile jobs the plan recorded.
    pub jobs: &'a [Job],
    /// The plan's manifest edits, as `plan.contents` holds them.
    pub edits: &'a BTreeMap<String, String>,
    /// `(path, blob sha)` of each extracted file, for the checkout to agree.
    pub extracted: Vec<(&'a str, &'a str)>,
    /// The plan's recorded targets, for `validate_targets` to read back.
    pub targets: Vec<Target<'a>>,
    /// The snapshot's public-only verdicts, for the run's cache partition.
    pub public_only: &'a BTreeMap<String, bool>,
    /// Whether the plan is lockfile maintenance.
    pub maintenance: bool,
}

/// A requested change the run must prove from the files it writes, as
/// `validate_targets` reads it: `file` must still show `value` or resolve
/// `version` for `package`.
pub struct Target<'a> {
    pub file: &'a str,
    pub package: &'a str,
    /// What the declaration or lock records the package at now.
    pub current: &'a str,
    pub value: Option<&'a str>,
    pub version: Option<&'a str>,
}

/// What a lockfile run produced: every path that differs from base, keyed
/// by path, so the merge sees each path once and in path order whatever
/// generated it.
pub struct Generated {
    pub files: BTreeMap<String, GeneratedFile>,
}

/// One path the run left different from the base commit.
pub struct GeneratedFile {
    /// The file at base; `None` when the run created it.
    pub base: Option<BaseFile>,
    /// The file after the run; `None` when the run deleted it.
    pub content: Option<String>,
}

/// A repository file as the base commit holds it.
#[derive(Clone)]
pub struct BaseFile {
    pub sha: String,
    pub mode: String,
    pub content: String,
}

/// Why lockfile generation failed: a tool's process failure kept structured
/// with what ran it, or reason text.
#[derive(Debug)]
pub enum Failure {
    Tool {
        /// The binary that failed, such as `cargo`.
        context: String,
        failure: patchula_process::Failure,
    },
    Text(String),
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tool { context, failure } => write!(f, "{context}: {failure}"),
            Self::Text(text) => f.write_str(text),
        }
    }
}
impl From<String> for Failure {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}
impl From<&str> for Failure {
    fn from(text: &str) -> Self {
        Self::Text(text.into())
    }
}

/// What executes lockfile work: the runtime `Runner` implements it.
pub trait Generate: Send + Sync {
    fn generate<'a>(
        &'a self,
        work: &'a Work<'a>,
        config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = Result<Generated, Failure>> + Send + 'a>>;
}
