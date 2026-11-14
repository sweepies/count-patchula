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
    /// `.github/workflows/actions.lock`, for the workflows it already lists.
    Actions,
}

/// The GitHub Actions workflow lockfile's one path: not a leaf name
/// matched anywhere in the tree.
pub const ACTIONS_LOCK: &str = ".github/workflows/actions.lock";

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
    /// A security fix inside the repository's Cargo publish-age cooldown:
    /// the run's Cargo is told to allow too-new releases for this job only.
    pub publish_age_allow: bool,
    /// The release-age wait the job's update answered to, in hours: Cargo
    /// gets it for the crates this job newly locks that no update names —
    /// transitive crates and whole-directory work wait like direct updates.
    /// Non-Cargo ecosystems ignore it.
    pub wait_hours: u32,
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
        let mut state = serializer.serialize_struct("Job", 8)?;
        state.serialize_field("ecosystem", &self.ecosystem)?;
        state.serialize_field("directory", &self.directory)?;
        state.serialize_field("package", &package)?;
        state.serialize_field("current", &current)?;
        state.serialize_field("target", &target)?;
        state.serialize_field("lock_only", &self.exact_version)?;
        state.serialize_field("publish_age_allow", &self.publish_age_allow)?;
        state.serialize_field("wait_hours", &self.wait_hours)?;
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
            publish_age_allow: bool,
            wait_hours: u32,
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
            publish_age_allow: fields.publish_age_allow,
            wait_hours: fields.wait_hours,
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
                publish_age_allow: false,
                wait_hours: 0,
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
                publish_age_allow: false,
                wait_hours: 0,
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
                publish_age_allow: false,
                wait_hours: 0,
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
                publish_age_allow: false,
                wait_hours: 0,
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
        // actions.lock sits beside the workflows it lists: a step-level
        // action edit in a `.github/workflows/` workflow asks for it
        // regenerated. An in-repository `action.yml` edit needs it only
        // when a listed workflow calls the file through `$/`, which paths
        // cannot tell: `actions_composite_job` decides from the contents.
        // Nothing else the manager extracts — reusable workflows, images,
        // setup inputs, runner labels — is in the lock.
        Manager::GithubActions => {
            if dep.kind != patchula_words::DependencyKind::Action
                || !patchula_manager::roles::github_workflow(&dep.file)
                || !paths.contains(ACTIONS_LOCK)
            {
                return Ok(None);
            }
            return Ok(Some(Job {
                ecosystem: Ecosystem::Actions,
                directory: ".github/workflows".into(),
                scope: Scope::Directory,
                exact_version: false,
                publish_age_allow: false,
                wait_hours: 0,
            }));
        }
        // No lockfile the Count generates.
        Manager::Dockerfile
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
    let targeted = dep.proposing().lock.movable;
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
        publish_age_allow: update.cooldown.is_some(),
        // The wait the update's policy answered to, floor folded in; a
        // batched update records no policy, so the planner re-derives it.
        wait_hours: update.policy.as_ref().map_or(0, |policy| policy.wait_hours),
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
    /// Exactly this repository path claims its directory, not its leaf
    /// name anywhere in the tree; the run needs one of the managers
    /// enabled.
    Path(&'static str, &'static [Manager]),
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
            Self::Path(path, managers) => join(directory, leaf) == *path && enabled(managers),
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
        // `--relock` advances the moved refs of the workflows the lock
        // already lists; only the one committed path counts.
        Ecosystem::Actions => Refresh::Path(ACTIONS_LOCK, &[Manager::GithubActions]),
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
            Ecosystem::Actions,
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
            publish_age_allow: false,
            wait_hours: 0,
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

/// What an `actions.lock` lists under `workflows:`: each workflow's
/// repository path and its canonical `owner/repo@ref` entries. Read as
/// strictly as `gh actions-lock`'s parser reads it (actions-lockfile's
/// `checkWorkflowPathKey` and `ParsePin`): a path key that is empty,
/// absolute, holds a control character, a backslash or a colon, or
/// climbs with `..`, or an entry that is not a repository-scoped pin, is
/// `None` — corrupt, or a newer format — never an empty map.
pub fn actions_lock_workflows(text: &str) -> Option<BTreeMap<String, BTreeSet<String>>> {
    #[derive(Deserialize)]
    struct Lock {
        workflows: BTreeMap<String, Option<Vec<String>>>,
    }
    let lock = serde_yaml_ng::from_str::<Lock>(text).ok()?;
    lock.workflows
        .into_iter()
        .map(|(path, entries)| {
            let safe = !path.is_empty()
                && !path.starts_with('/')
                && !path
                    .chars()
                    .any(|c| c <= '\u{1f}' || c == '\u{7f}' || c == '\\' || c == ':')
                && !path.split('/').any(|segment| segment == "..");
            if !safe {
                return None;
            }
            let entries = entries
                .unwrap_or_default()
                .iter()
                .map(|entry| actions_lock_pin(entry))
                .collect::<Option<BTreeSet<_>>>()?;
            Some((path, entries))
        })
        .collect()
}

/// The canonical key `gh actions-lock` writes for an `owner/repo@ref`
/// pin: owner and repository lowercased, a full commit SHA lowercased,
/// any other ref as written (git refs are case-sensitive). `None` for
/// anything actions-lockfile's `ParsePin` rejects: no `@` (the first one
/// splits), an empty owner, repository or ref, a ref holding `:`, or a
/// sub-path.
pub fn actions_lock_pin(pin: &str) -> Option<String> {
    let (repository, reference) = pin.split_once('@')?;
    let (owner, repo) = repository.split_once('/')?;
    if owner.is_empty()
        || repo.is_empty()
        || repo.contains('/')
        || reference.is_empty()
        || reference.contains(':')
    {
        return None;
    }
    Some(actions_lock_key(repository, reference))
}

/// The lock key for repository `owner/repo` at `reference`, canonical as
/// `gh actions-lock` writes it (actions-lockfile's `Pin.Canonical` and
/// gh-actions-lock's `CanonicalRef`).
pub fn actions_lock_key(repository: &str, reference: &str) -> String {
    let full_sha =
        matches!(reference.len(), 40 | 64) && reference.bytes().all(|b| b.is_ascii_hexdigit());
    let reference = if full_sha {
        reference.to_ascii_lowercase()
    } else {
        reference.to_owned()
    };
    format!("{}@{reference}", repository.to_ascii_lowercase())
}

/// How a caller reads a repository file for `actions_lock_callers`:
/// `Ok(None)` when the path does not exist, `Err` when it exists but its
/// text is not at hand.
pub type ReadFile<'a> = dyn Fn(&str) -> Result<Option<String>, String> + 'a;

/// How deep gh-actions-lock follows `$/` composites before it gives up
/// (`maxYAMLWalkDepth`).
const SELF_ACTION_DEPTH: usize = 100;

/// The workflows among `listed` whose step-level `uses:` reach the
/// in-repository action file `file` through gh-actions-lock's
/// same-repository syntax `$/<dir>`, followed as the tool's
/// `ScanSelfRepositoryActions` follows it: a bare `$/` value (one with an
/// `@` is invalid and never followed) at `jobs.<id>.steps[*]` or inside a
/// `parallel:` group names the directory `<dir>`, cleaned like
/// `filepath.Join` and kept inside the repository, whose `action.yml` —
/// else `action.yaml` — is read; a composite's own `runs.steps[*]` `$/`
/// values are followed in turn. The tool locks every remote `uses:` it
/// finds there under the calling workflow, so these are the workflows an
/// edit to `file` relocks. A local `./` action is never followed.
///
/// Whatever the tool would fail to read on the way — a workflow or
/// action file that does not parse, a path that leaves the repository, a
/// file whose text is not at hand, a chain deeper than the tool follows
/// — leaves the answer unknown, and is an `Err` naming it rather than a
/// guess.
pub fn actions_lock_callers(
    file: &str,
    listed: &BTreeSet<String>,
    read: &ReadFile<'_>,
) -> Result<BTreeSet<String>, String> {
    let unknown = |what: String| {
        format!("which workflows the lockfile tool relocks for {file} is unknown: {what}")
    };
    let yaml = |path: &str, text: &str| {
        serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text)
            .map_err(|e| unknown(format!("{path} does not parse: {e}")))
    };
    let text = |path: &str| read(path).map_err(|e| unknown(format!("{path}: {e}")));
    let uses = |steps: Option<&serde_yaml_ng::Value>| -> Vec<String> {
        steps
            .and_then(|steps| steps.as_sequence())
            .into_iter()
            .flatten()
            .filter_map(|step| step.get("uses")?.as_str().map(|u| u.trim().to_owned()))
            .collect()
    };
    let self_action = |value: &str| value.starts_with("$/") && !value.contains('@');
    let mut callers = BTreeSet::new();
    for workflow in listed
        .iter()
        .filter(|w| patchula_manager::roles::github_workflow(w))
    {
        let Some(content) = text(workflow)? else {
            continue;
        };
        let document = yaml(workflow, &content)?;
        let mut pending: Vec<(String, usize)> = Vec::new();
        if let Some(jobs) = document.get("jobs").and_then(|jobs| jobs.as_mapping()) {
            for job in jobs.values() {
                let steps = job.get("steps");
                let mut found = uses(steps);
                for step in steps.and_then(|s| s.as_sequence()).into_iter().flatten() {
                    found.extend(uses(step.get("parallel")));
                }
                pending.extend(found.into_iter().filter(|u| self_action(u)).map(|u| (u, 0)));
            }
        }
        let mut seen = BTreeSet::new();
        while let Some((reference, depth)) = pending.pop() {
            if !seen.insert(reference.clone()) {
                continue;
            }
            if depth > SELF_ACTION_DEPTH {
                return Err(unknown(format!(
                    "{workflow} nests `$/` actions deeper than the tool follows"
                )));
            }
            // `filepath.Join(repoRoot, rel)`: empty and `.` segments go,
            // `..` climbs, and climbing out of the repository is refused.
            let mut segments: Vec<&str> = Vec::new();
            for segment in reference["$/".len()..].split('/') {
                match segment {
                    "" | "." => {}
                    ".." => {
                        if segments.pop().is_none() {
                            return Err(unknown(format!(
                                "{workflow} names {reference}, outside the repository"
                            )));
                        }
                    }
                    segment => segments.push(segment),
                }
            }
            let directory = segments.join("/");
            let mut action = None;
            for name in ["action.yml", "action.yaml"] {
                let path = if directory.is_empty() {
                    name.to_owned()
                } else {
                    format!("{directory}/{name}")
                };
                if let Some(content) = text(&path)? {
                    action = Some((path, content));
                    break;
                }
            }
            // A directory without an action file reaches nothing.
            let Some((path, content)) = action else {
                continue;
            };
            if path == file {
                callers.insert(workflow.clone());
            }
            let definition = yaml(&path, &content)?;
            let runs = definition.get("runs");
            // The tool decodes `runs.steps[*].uses` into a typed struct:
            // a `runs` that is no mapping, `steps` that are no sequence
            // or a step that is no mapping fail it.
            let steps = runs.and_then(|runs| runs.get("steps"));
            let malformed = runs.is_some_and(|runs| !runs.is_mapping() && !runs.is_null())
                || steps.is_some_and(|steps| {
                    !steps.is_null()
                        && steps.as_sequence().is_none_or(|steps| {
                            steps
                                .iter()
                                .any(|step| !step.is_mapping() && !step.is_null())
                        })
                });
            if malformed {
                return Err(unknown(format!(
                    "{path} is not an action definition the tool reads"
                )));
            }
            if runs
                .and_then(|runs| runs.get("using"))
                .and_then(|u| u.as_str())
                != Some("composite")
            {
                continue;
            }
            pending.extend(
                uses(steps)
                    .into_iter()
                    .filter(|u| self_action(u))
                    .map(|u| (u, depth + 1)),
            );
        }
    }
    Ok(callers)
}

/// The Actions job a step-level action edit in an in-repository action
/// file needs, which `job()` cannot tell from paths alone: one when a
/// workflow the base lock lists calls the file through `$/`, none when no
/// listed workflow does — a `./` composite, or one nothing calls — and an
/// `Err` when the lockfile or the workflows cannot say.
pub fn actions_composite_job(
    dep: &Dependency,
    paths: &BTreeSet<String>,
    read: &ReadFile<'_>,
) -> Result<Option<Job>, String> {
    if dep.manager != Manager::GithubActions
        || dep.kind != patchula_words::DependencyKind::Action
        || patchula_manager::roles::github_workflow(&dep.file)
        || !patchula_manager::roles::action_file(&dep.file)
        || !paths.contains(ACTIONS_LOCK)
    {
        return Ok(None);
    }
    let lock = read(ACTIONS_LOCK)?.ok_or("actions.lock is not readable")?;
    let listed = actions_lock_workflows(&lock)
        .ok_or("actions.lock: a corrupt or newer lockfile; regenerate it with `gh actions-lock`")?;
    let callers = actions_lock_callers(&dep.file, &listed.into_keys().collect(), read)?;
    Ok((!callers.is_empty()).then(|| Job {
        ecosystem: Ecosystem::Actions,
        directory: ".github/workflows".into(),
        scope: Scope::Directory,
        exact_version: false,
        publish_age_allow: false,
        wait_hours: 0,
    }))
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
        Manager::GithubActions => Some(Ecosystem::Actions),
        Manager::Helm
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
    /// The plan's manifest edits, as `plan.contents()` holds them.
    pub edits: &'a BTreeMap<String, String>,
    /// `(path, blob sha)` of each extracted file, for the checkout to agree.
    pub extracted: Vec<(&'a str, &'a str)>,
    /// The plan's recorded targets, for `validate_targets` to read back.
    pub targets: Vec<Target<'a>>,
    /// The snapshot's public-only verdicts, for the run's cache partition.
    pub public_only: &'a BTreeMap<String, bool>,
    /// Each Cargo directory's committed publish-age cooldown, for the run's
    /// Cargo environment: its verbatim value, or the reason the directory's
    /// jobs must fail rather than run without the repository's.
    pub cargo_cooldowns: &'a BTreeMap<String, patchula_manager::CargoCooldown>,
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
    /// What the run wants the report to warn about: facts it could not
    /// enforce, such as a pre-1.100 Cargo ignoring the publish-age keys.
    pub warnings: Vec<String>,
    /// The `actions.lock` refs a maintenance `--relock` advanced, for the
    /// pull request body.
    pub moved: Vec<MovedRef>,
}

/// One ref a maintenance `--relock` advanced: under `workflow`, which
/// reaches it directly or through a composite, `dependency`
/// (`owner/repo@ref`, as the lock keys it) now resolves to `to`; the lock
/// recorded `from`. Both are commit SHAs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MovedRef {
    pub workflow: String,
    pub dependency: String,
    pub from: String,
    pub to: String,
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
