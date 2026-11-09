use patchula_config::{Config, PackageId, Registry, split_package};
use patchula_manager::{Dependency, Edit, hash};
use patchula_report::{BranchAction, BranchStatus, FileAction, Repository, Update};
use patchula_words::{Source, Update as UpdateKind};
use serde::{Deserialize, Serialize};
use sha1::Digest;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// Includes unselected lockfile paths, for conservative write gating.
    pub paths: BTreeSet<String>,
    pub files: BTreeMap<String, Blob>,
    /// For each file that can steer a lockfile tool's fetches, whether it
    /// can fetch only from the ecosystem's well-known public registry. A
    /// run may share the `public` cache partition only when every file it
    /// reads decides so; a missing verdict is not public.
    #[serde(default)]
    pub public_only: BTreeMap<String, bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blob {
    pub sha: String,
    pub mode: String,
    pub content: String,
}
/// The plan fingerprint: the exact bytes the commit's `Patchula-Plan`
/// trailer and the write receipts hash. `lock_jobs` may be the building
/// `BTreeSet` or the plan's sorted `Vec`; both serialize the same array.
fn plan_fingerprint(
    contents: &BTreeMap<String, String>,
    message: &str,
    lockfile_pending: bool,
    lock_jobs: &impl Serialize,
) -> String {
    hash(
        serde_json::to_string(&(contents, message, lockfile_pending, lock_jobs))
            .expect("serializable plan")
            .as_bytes(),
    )
}
#[derive(Debug)]
pub struct Plan {
    action: BranchAction,
    pub contents: BTreeMap<String, String>,
    pub identity: String,
    pub fingerprint: String,
    pub changes: Vec<Change>,
    pub retry: bool,
    pub tree: Option<String>,
    pub cached_pr: Option<serde_json::Value>,
    pub input_fingerprint: String,
    pub lock_jobs: Vec<crate::lockfiles::Job>,
    pub lockfiles_ready: bool,
    /// The snapshot's public-only verdicts, for the lockfile runs'
    /// cache-partition choice.
    pub public_only: BTreeMap<String, bool>,
    pub deleted: BTreeSet<String>,
    pub maintenance: bool,
    /// The batch PR's items; `None` for every other plan.
    pub batch: Option<Vec<crate::batch::Item>>,
    /// Tooling warnings shown on this PR.
    pub tooling: Vec<crate::tooling::Callout>,
    /// Why the plan is not written this run, if it is held; private so the
    /// status and reason it implies can only change through the named
    /// transitions on [`Plan`].
    hold: Option<Hold>,
    /// The dashboard's rendering of `action.reason`, kept in step with it
    /// by every transition: the report reads the reason, the dashboard
    /// this typed form, so the two cannot disagree.
    failure: Option<crate::text::Failure>,
}

/// Why a plan is not written this run. A plan has at most one: when several
/// apply, one no tick can lift wins over one a tick can, so ticking "Create
/// it now anyway" never writes a plan that is also blocked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Outside its update windows.
    Window,
    /// `prs.concurrent` PRs are open.
    Concurrent,
    /// `prs.perHour` PRs opened in the last hour.
    PerHour,
    /// Its PR moved into the batch and has not closed yet.
    WaitingForBatch,
    /// The batch PR carries it.
    Batched,
    /// Its owner closed its PR; a newer target opens a new one.
    ClosedByOwner,
    /// A lockfile refresh that found nothing to change.
    LockfilesCurrent,
    /// Lockfile maintenance outside its schedule.
    MaintenanceWindow,
    /// Lockfile maintenance already ran this week, or waits for
    /// `lockfiles.waitHours`.
    MaintenanceDone,
    /// It cannot be written as it stands.
    Blocked(String),
}
impl Hold {
    /// The report's reason.
    pub fn text(&self) -> &str {
        match self {
            Self::Window => "outside the configured update windows",
            Self::Concurrent => "prs.concurrent limit reached",
            Self::PerHour => "prs.perHour limit reached",
            Self::WaitingForBatch => "waiting for its batched PR to close",
            Self::Batched => "batched: carried by the batch PR",
            Self::ClosedByOwner => {
                "closed-by-owner: this update was manually closed; waiting for a newer target"
            }
            Self::LockfilesCurrent => "lockfiles already current",
            Self::MaintenanceWindow => "outside lockfile maintenance window",
            Self::MaintenanceDone => {
                "maintenance already completed this week or waiting for lockfiles.waitHours"
            }
            Self::Blocked(reason) => reason,
        }
    }
    /// Whether a dashboard tick to create the update anyway lifts it.
    pub fn overridable(&self) -> bool {
        matches!(self, Self::Window | Self::Concurrent | Self::PerHour)
    }
    fn rank(&self) -> u8 {
        match self {
            Self::Window | Self::Concurrent | Self::PerHour => 0,
            Self::WaitingForBatch
            | Self::ClosedByOwner
            | Self::LockfilesCurrent
            | Self::MaintenanceWindow
            | Self::MaintenanceDone => 1,
            Self::Blocked(_) => 2,
            // The batch PR carries it and its own PR closes as batched, whatever
            // else would have held it.
            Self::Batched => 3,
        }
    }
}
impl Plan {
    /// A plan for `action`'s branch, with no edits or results yet.
    pub fn new(action: BranchAction) -> Self {
        Self {
            failure: action.reason.as_deref().map(crate::text::failure),
            action,
            contents: BTreeMap::new(),
            identity: String::new(),
            fingerprint: String::new(),
            changes: Vec::new(),
            retry: false,
            tree: None,
            cached_pr: None,
            input_fingerprint: String::new(),
            lock_jobs: Vec::new(),
            lockfiles_ready: false,
            public_only: BTreeMap::new(),
            deleted: BTreeSet::new(),
            maintenance: false,
            batch: None,
            tooling: Vec::new(),
            hold: None,
        }
    }
    /// The branch action the report carries.
    pub fn action(&self) -> &BranchAction {
        &self.action
    }
    /// The branch action, once the plan's work is done and the report reads it.
    pub fn into_action(self) -> BranchAction {
        self.action
    }
    /// The dashboard's rendering of `action.reason`; every transition keeps
    /// it in step, so it is present exactly when the reason is.
    pub fn failure(&self) -> Option<&crate::text::Failure> {
        self.failure.as_ref()
    }
    /// `action.reason` and its dashboard `failure`, always set together.
    fn set_reason(&mut self, reason: Option<String>) {
        self.failure = reason.as_deref().map(crate::text::failure);
        self.action.reason = reason;
    }
    /// Hold the plan, unless a stronger hold already does; of equals, the
    /// later wins, as in [`keep`].
    pub fn hold(&mut self, hold: Hold) {
        if self
            .hold
            .as_ref()
            .is_some_and(|held| held.rank() > hold.rank())
        {
            return;
        }
        self.action.status = BranchStatus::Skipped;
        self.set_reason(Some(hold.text().to_owned()));
        self.hold = Some(hold);
    }
    /// Whether `hold` is what holds the plan.
    pub fn held(&self, hold: &Hold) -> bool {
        self.hold.as_ref() == Some(hold)
    }
    /// Whether a dashboard tick to create the update anyway lifts the hold.
    pub fn overridable(&self) -> bool {
        self.hold.as_ref().is_some_and(Hold::overridable)
    }
    /// Release a hold a dashboard tick may lift; a plan held for any other
    /// reason stays held.
    pub fn lift(&mut self) {
        if self.hold.as_ref().is_some_and(Hold::overridable) {
            self.hold = None;
            self.action.status = BranchStatus::Planned;
            self.set_reason(None);
        }
    }
    /// An open PR on the branch is not the Count's to reconcile.
    pub(crate) fn protect(&mut self, reason: impl Into<String>) {
        self.action.status = BranchStatus::Protected;
        self.set_reason(Some(reason.into()));
    }
    /// The write failed; an errored plan names no commit.
    pub fn error(&mut self, reason: impl Into<String>) {
        self.action.status = BranchStatus::Error;
        self.action.commit = None;
        self.set_reason(Some(reason.into()));
    }
    /// The default branch moved before the write; a rerun retries it.
    pub(crate) fn stale(&mut self, reason: impl Into<String>) {
        self.action.status = BranchStatus::Stale;
        self.set_reason(Some(reason.into()));
    }
    /// Lockfile generation failed; the report keeps the reason and the
    /// dashboard the typed failure, both from the same error.
    pub fn lockfiles_failed(&mut self, error: crate::lockfiles::Error) {
        self.action.status = BranchStatus::Error;
        let reason = format!("lockfile generation failed: {error}");
        self.action.reason = Some(reason.clone());
        self.failure = Some(match error {
            crate::lockfiles::Error::Tool(tool) => crate::text::tool_failure(
                &format!("lockfile generation failed: {}", tool.context),
                &tool.failure,
            ),
            crate::lockfiles::Error::Text(_) => crate::text::failure(&reason),
        });
    }
    /// GitHub's verdict on the commit's signature.
    pub(crate) fn set_verified(&mut self, verified: Option<bool>) {
        self.action.verified = verified;
    }
    /// The update branch's head observed before the write.
    pub(crate) fn record_head(&mut self, previous: Option<String>) {
        self.action.previous_commit = previous;
    }
    /// The plan's commit is on its branch with `status`.
    pub(crate) fn committed(&mut self, commit: String, status: BranchStatus) {
        self.action.commit = Some(commit);
        self.action.status = status;
        if self.action.lockfile_pending {
            self.set_reason(Some(
                "lockfile pending: manifest-only test branch; regenerate before merging".into(),
            ));
        }
    }
    /// The update branch already holds exactly this plan's tree.
    pub fn unchanged(&mut self, commit: String, tree: String) {
        self.action.status = BranchStatus::Unchanged;
        self.action.previous_commit = Some(commit.clone());
        self.action.commit = Some(commit);
        self.action.verified = Some(true);
        self.tree = Some(tree);
    }
    /// A verified write receipt proves the branch is already at the plan.
    pub(crate) fn restored(
        &mut self,
        commit: &str,
        tree: &str,
        fingerprint: &str,
        files: Vec<patchula_report::FileAction>,
        pr: serde_json::Value,
    ) {
        self.action.status = BranchStatus::Unchanged;
        self.action.previous_commit = Some(commit.into());
        self.action.commit = Some(commit.into());
        self.action.verified = Some(true);
        self.tree = Some(tree.into());
        self.cached_pr = Some(pr);
        self.fingerprint = fingerprint.into();
        self.action.files = files;
        self.lockfiles_ready = true;
    }
    /// Lockfile generation produced these files for the plan.
    pub(crate) fn lockfiles_generated(
        &mut self,
        contents: BTreeMap<String, String>,
        deleted: BTreeSet<String>,
        files: Vec<patchula_report::FileAction>,
    ) {
        self.contents = contents;
        self.deleted = deleted;
        self.action.files = files;
        self.lockfiles_ready = true;
        self.fingerprint = hash(
            serde_json::to_string(&(&self.contents, &self.deleted, &self.action.message))
                .unwrap()
                .as_bytes(),
        );
    }
    /// The pull request merged.
    pub(crate) fn merged(&mut self) {
        self.action.status = BranchStatus::Merged;
    }
    /// The branch the plan publishes to.
    pub fn rename_branch(&mut self, branch: String) {
        self.action.branch = branch;
    }
    /// The extraction commit the plan's edits and write apply to.
    pub fn retarget_base(&mut self, base_commit: String) {
        self.action.base_commit = base_commit;
    }
    /// The commit message the write uses.
    pub fn set_message(&mut self, message: String) {
        self.action.message = message;
    }
    /// Whether the write is a manifest-only lockfile-pending test branch.
    pub fn set_lockfile_pending(&mut self, pending: bool) {
        self.action.lockfile_pending = pending;
    }
    /// A file the plan changes, as the planner records it.
    pub fn add_file(&mut self, file: patchula_report::FileAction) {
        self.action.files.push(file);
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Change {
    pub source: Source,
    pub package: String,
    /// The registry the package comes from, when it is not the public one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<Registry>,
    pub file: String,
    pub current: String,
    pub current_version: Option<String>,
    pub value: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
    pub kind: UpdateKind,
    pub merge: bool,
    pub labels: Vec<String>,
    pub release_notes: bool,
    pub body_notes: Vec<String>,
    /// Advisories affecting the current version that this change leaves behind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<patchula_report::Advisory>,
    /// The image SBOM comparison, for a pinned image's new digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<patchula_report::ImageCheck>,
}
impl Change {
    /// The package the change updates.
    pub fn id(&self) -> PackageId {
        PackageId::new(self.source, self.registry.clone(), &self.package)
    }
    /// Whether the change updates this declaration: same file, source,
    /// package and registry. The registry counts — same-named packages from
    /// different registries are different declarations.
    pub fn touches(
        &self,
        file: &str,
        source: Source,
        package: &str,
        registry: Option<&str>,
    ) -> bool {
        self.file == file
            && self.source == source
            && self.package == package
            && self.registry.as_ref().map(Registry::recorded) == registry
    }
    /// Whether `update` proposes exactly this change's target.
    pub fn same_target(&self, update: &Update) -> bool {
        update.new_value == self.value
            && update.new_version == self.version
            && update.new_digest == self.digest
            && update.update_type == self.kind
    }
}
pub fn blob_sha(content: &str) -> String {
    let mut digest = sha1::Sha1::new();
    digest.update(format!("blob {}\0", content.len()));
    digest.update(content.as_bytes());
    digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
/// Readable branch labels; repository planning resolves manager collisions.
pub fn branch_name(prefix: &str, label: &str) -> Result<String, String> {
    let name = format!("{prefix}{}", label.to_ascii_lowercase());
    if name.len() > 240
        || name.starts_with('/')
        || name.ends_with('.')
        || name.contains([' ', '~', '^', ':', '?', '*', '[', '\\'])
        || name.contains("..")
        || name.contains("@{")
        || name.chars().any(char::is_control)
        || name
            .split('/')
            .any(|s| s.is_empty() || s.starts_with('.') || s.ends_with(".lock"))
    {
        return Err("prs.branch produces an invalid Git branch name".into());
    }
    Ok(name)
}
/// Each branch label's packages, by source, package and registry; a label
/// shared by several gets a prefix to tell their branches apart.
type PackageNames = BTreeMap<String, BTreeSet<(Source, String, Option<Registry>)>>;

struct Item<'a> {
    dep: &'a Dependency,
    update: &'a Update,
    current_version: Option<&'a str>,
    advisories: &'a [patchula_report::Advisory],
}

pub fn plan(
    repo: &Repository,
    config: &Config,
    dependencies: &[Dependency],
    snapshot: &Snapshot,
    allow_pending: bool,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Plan>, String> {
    let evaluator = patchula_lookup::Evaluator::new(config)?;
    let mut package_names = PackageNames::new();
    for dep in dependencies {
        package_names
            .entry(package_slug(&dep.package))
            .or_default()
            .insert((dep.source, dep.package.clone(), dep.registry.clone()));
    }
    let mut groups = BTreeMap::<String, (String, Vec<Item<'_>>)>::new();
    for file in &repo.package_files {
        for reported in &file.dependencies {
            let declared = crate::dashboard::declaration(&file.path, reported);
            for update in &reported.updates {
                for dep in dependencies
                    .iter()
                    .filter(|d| declared.equivalent(&d.declaration()))
                {
                    // One best proposal per occurrence and track avoids overlapping edits.
                    let track = |u: &Update| match u.update_type {
                        UpdateKind::Pin => "pin",
                        UpdateKind::Major => "major",
                        UpdateKind::Minor | UpdateKind::Patch | UpdateKind::Digest => "non-major",
                    };
                    let best = reported
                        .updates
                        .iter()
                        .filter(|u| track(u) == track(update))
                        .max_by(|a, b| proposal_order(dep, a, b));
                    if best != Some(update) {
                        continue;
                    }
                    let (identity, label) = branch_identity(
                        dep,
                        reported,
                        update,
                        dependencies,
                        &package_names,
                        (!allow_pending).then_some(snapshot),
                    );
                    groups
                        .entry(identity)
                        .or_insert_with(|| (label, Vec::new()))
                        .1
                        .push(Item {
                            dep,
                            update,
                            current_version: reported.current.version.as_deref(),
                            advisories: reported
                                .metadata
                                .as_ref()
                                .map_or(&[], |m| m.advisories.as_slice()),
                        });
                }
            }
        }
    }
    let mut labels = BTreeMap::<String, usize>::new();
    for (label, _) in groups.values() {
        *labels.entry(label.clone()).or_default() += 1;
    }
    let mut names = BTreeSet::new();
    let mut plans = Vec::new();
    for (identity, (mut label, items)) in groups {
        let items = superseding(items);
        if labels[&label] > 1 {
            let managers: BTreeSet<_> = items.iter().map(|i| manager_slug(i.dep)).collect();
            label = format!(
                "{}-{label}",
                managers.into_iter().collect::<Vec<_>>().join("-")
            );
        }
        let branch = branch_name(&config.prs.branch, &label)?;
        if !names.insert(branch.clone()) {
            return Err(
                "branch name collision after manager disambiguation; set explicit groups".into(),
            );
        }
        plans.push(build(
            repo,
            config,
            &evaluator,
            snapshot,
            allow_pending,
            since,
            now,
            &identity,
            branch,
            &items,
        )?);
    }
    Ok(plans)
}

/// An update that changes a lockfile and no manifest: a package recorded only
/// in a lockfile, which its tool alone can move (a transitive package, or a
/// flake input that nix relocks), or a release a direct dependency's declared
/// range already allows, which has no `newValue`.
fn lock_only(dep: &Dependency, update: &Update) -> bool {
    dep.kind == patchula_words::DependencyKind::Transitive
        || dep.manager == patchula_words::Manager::Nix
        || (update.new_value.is_none() && update.new_version.is_some())
}

/// Keep the stronger of `hold` and what `slot` holds, or the later of equals,
/// so a plan with several problems of one kind names the last, as it did.
fn keep(slot: &mut Option<Hold>, hold: Hold) {
    if slot.as_ref().is_none_or(|held| held.rank() <= hold.rank()) {
        *slot = Some(hold);
    }
}

/// One branch plan from its items: edits, lock jobs, message and changes.
#[allow(clippy::too_many_arguments)]
fn build(
    repo: &Repository,
    config: &Config,
    evaluator: &patchula_lookup::Evaluator,
    snapshot: &Snapshot,
    allow_pending: bool,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    identity: &str,
    branch: String,
    items: &[Item<'_>],
) -> Result<Plan, String> {
    let mut changes = BTreeMap::<String, Vec<Edit>>::new();
    let mut hold = None;
    let mut pending = None;
    let mut lock_jobs = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    let mut titles = BTreeSet::new();
    // A branch with a security fix is named for it: the occurrences that do not
    // need the fix move to the same version in the same commit.
    let secure = items.iter().any(|item| !item.update.fixes.is_empty());
    for item in items {
        let dep = item.dep;
        let update = item.update;
        if evaluator.needs_digest(dep, update) && update.new_digest.is_none() {
            keep(
                &mut hold,
                Hold::Blocked(format!(
                    "{}: required target digest is unavailable; retry lookup",
                    dep.name
                )),
            );
        }
        if let Some(policy) = &update.policy {
            if !secure || !update.fixes.is_empty() {
                prefixes.insert(policy.commit_prefix.clone());
            }
            let windows: Vec<patchula_config::Window> = policy
                .windows
                .iter()
                .map(|w| patchula_config::Window {
                    cron: w.cron.clone(),
                    zone: w.zone.clone(),
                })
                .collect();
            if !patchula_config::due(&windows, since, now) {
                keep(&mut hold, Hold::Window);
            }
        }
        match crate::lockfiles::job(dep, update, snapshot) {
            Ok(Some(job)) => {
                lock_jobs.insert(job);
            }
            Ok(None) => {}
            Err(error) => pending = Some(error),
        }
        let value = update.new_value.as_deref().unwrap_or(&dep.current);
        titles.insert(format!(
            "{} to {}",
            dep.name,
            // The version reads better than a specifier such as `==2.0.0`.
            update
                .new_version
                .as_deref()
                .or(update.new_value.as_deref())
                .or(update.new_digest.as_deref())
                .unwrap_or(value)
        ));
        // A lockfile-only package changes through its lockfile tool alone.
        if lock_only(dep, update) {
            continue;
        }
        let Some(blob) = snapshot.files.get(&dep.file) else {
            return Err(format!("missing extracted file {}", dep.file));
        };
        if !matches!(blob.mode.as_str(), "100644" | "100755") {
            keep(
                &mut hold,
                Hold::Blocked(format!("unsupported manifest mode {}", blob.mode)),
            );
            continue;
        }
        match patchula_manager::edits(dep, &blob.content, value, update.new_digest.as_deref()) {
            Ok(edits) => changes.entry(dep.file.clone()).or_default().extend(edits),
            Err(error) => keep(&mut hold, Hold::Blocked(format!("{}: {error}", dep.file))),
        }
    }
    if prefixes.len() > 1 {
        keep(
            &mut hold,
            Hold::Blocked("group has conflicting commitPrefix policies".into()),
        );
    }
    let prefix = prefixes.first().unwrap_or(&config.defaults.commit_prefix);
    let mut message = if items
        .iter()
        .all(|item| item.update.update_type == UpdateKind::Pin)
    {
        format!("{prefix} pin dependencies")
    } else {
        format!(
            "{prefix} update {}",
            titles.into_iter().collect::<Vec<_>>().join(", ")
        )
    };
    if items.iter().any(|item| !item.update.fixes.is_empty()) {
        message.push_str(" [security]");
    }
    let mut contents = BTreeMap::new();
    let mut files = Vec::new();
    for (path, edits) in changes {
        let blob = &snapshot.files[&path];
        match patchula_manager::apply(&blob.content, edits) {
            Ok(content) if content != blob.content => {
                files.push(FileAction {
                    path: path.clone(),
                    blob_sha: blob.sha.clone(),
                    diff: unified(&path, &blob.content, &content),
                });
                contents.insert(path, content);
            }
            Ok(_) => {}
            Err(error) => keep(&mut hold, Hold::Blocked(format!("{path}: {error}"))),
        }
    }
    let lockfile_only = items.iter().all(|item| lock_only(item.dep, item.update));
    if lockfile_only && lock_jobs.iter().any(|job| job.target().is_none()) {
        keep(
            &mut hold,
            Hold::Blocked("no lockfile tool can move this package to an exact version".into()),
        );
    }
    if contents.is_empty() && hold.is_none() && !(lockfile_only && !lock_jobs.is_empty()) {
        keep(
            &mut hold,
            Hold::Blocked(
                "no editable manifest change: the declaration already allows the target, or is inherited from its workspace root"
                    .into(),
            ),
        );
    }
    if let Some(error) = pending.as_ref().filter(|_| !allow_pending) {
        keep(
            &mut hold,
            Hold::Blocked(format!(
                "lockfile pending: {error}; --allow-lockfile-pending permits manifest-only testing"
            )),
        );
    }
    let fingerprint = plan_fingerprint(&contents, &message, pending.is_some(), &lock_jobs);
    let action = BranchAction {
        repository: repo.repository.clone(),
        branch,
        base_commit: repo.commit.clone().ok_or("missing extraction commit")?,
        status: if hold.is_some() {
            BranchStatus::Skipped
        } else {
            BranchStatus::Planned
        },
        reason: hold.as_ref().map(|hold| hold.text().to_owned()),
        message,
        previous_commit: None,
        commit: None,
        verified: None,
        lockfile_pending: pending.is_some(),
        files,
    };
    let changes: BTreeSet<_> = items
        .iter()
        .map(|item| {
            let u = item.update;
            let p = u.policy.as_ref();
            Change {
                current_version: item.current_version.map(str::to_owned),
                source: item.dep.source,
                package: item.dep.package.clone(),
                registry: item.dep.registry.clone(),
                file: item.dep.file.clone(),
                current: format!(
                    "{}{}",
                    item.dep.current,
                    item.dep
                        .digest
                        .as_ref()
                        .map(|d| format!("@{d}"))
                        .unwrap_or_default()
                ),
                value: u.new_value.clone(),
                version: u.new_version.clone(),
                digest: u.new_digest.clone(),
                kind: u.update_type,
                merge: p.map_or(config.defaults.merge, |p| p.merge),
                labels: p.map_or_else(|| config.defaults.labels.clone(), |p| p.labels.clone()),
                release_notes: p.map_or(config.defaults.release_notes, |p| p.release_notes),
                body_notes: p.map_or_else(
                    || config.defaults.body_notes.clone(),
                    |p| p.body_notes.clone(),
                ),
                advisories: item
                    .advisories
                    .iter()
                    .filter(|a| u.fixes.contains(&a.id))
                    .cloned()
                    .collect(),
                image: u.image.clone(),
            }
        })
        .collect();
    Ok(Plan {
        action,
        contents,
        identity: hash(identity.as_bytes()),
        fingerprint: fingerprint.clone(),
        changes: changes.into_iter().collect(),
        retry: false,
        tree: None,
        cached_pr: None,
        input_fingerprint: fingerprint.clone(),
        lockfiles_ready: lock_jobs.is_empty(),
        lock_jobs: lock_jobs.into_iter().collect(),
        public_only: snapshot.public_only.clone(),
        deleted: BTreeSet::new(),
        maintenance: false,
        batch: None,
        tooling: Vec::new(),
        failure: hold.as_ref().map(|hold| crate::text::failure(hold.text())),
        hold,
    })
}

/// A one-hunk unified diff of a file's change, as dry runs print it.
pub fn unified(path: &str, old: &str, new: &str) -> String {
    let a: Vec<_> = old.split_inclusive('\n').collect();
    let b: Vec<_> = new.split_inclusive('\n').collect();
    let prefix = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let start = prefix.saturating_sub(3);
    let end_a = (a.len() - suffix + 3).min(a.len());
    let end_b = (b.len() - suffix + 3).min(b.len());
    let mut result = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -{},{} +{},{} @@\n",
        start + 1,
        end_a - start,
        start + 1,
        end_b - start
    );
    let mut line = |marker: char, text: &str| {
        result.push(marker);
        result.push_str(text);
        if !text.ends_with('\n') {
            result.push_str("\n\\ No newline at end of file\n");
        }
    };
    for text in &a[start..prefix] {
        line(' ', text);
    }
    for text in &a[prefix..a.len() - suffix] {
        line('-', text);
    }
    for text in &b[prefix..b.len() - suffix] {
        line('+', text);
    }
    for text in &a[a.len() - suffix..end_a] {
        line(' ', text);
    }
    result
}

/// Registry-qualified identities (`registry|name`) name their branch after the
/// package, not the registry URL.
fn package_slug(package: &str) -> String {
    slug(split_package(package).map_or(package, |(_, name)| name))
}
fn slug(value: &str) -> String {
    let s = value
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { "update".into() } else { s }
}
fn manager_slug(dep: &Dependency) -> String {
    if matches!(
        dep.source,
        patchula_words::Source::Docker | patchula_words::Source::Oci
    ) {
        "docker".into()
    } else {
        dep.manager.to_string()
    }
}
fn proposal_order(dep: &Dependency, a: &Update, b: &Update) -> std::cmp::Ordering {
    use patchula_versioning::Versioning;
    let value = |u: &Update| {
        u.new_version
            .clone()
            .or_else(|| u.new_value.clone())
            .unwrap_or_else(|| dep.current.clone())
    };
    let scheme = patchula_versioning::Ecosystem::of(dep.scheme, dep.source);
    if let (Ok(a), Ok(b)) = (scheme.parse(&value(a)), scheme.parse(&value(b))) {
        return scheme.order(&a, &b);
    }
    value(a).cmp(&value(b))
}
/// A batch target the dependency has already moved past, such as an update
/// whose PR was ticked into the batch after a later batch carried a newer
/// release. Writing it would go backwards, so it is dropped.
fn superseded(dep: &Dependency, target: &Change) -> bool {
    use patchula_versioning::Versioning;
    let Some(version) = target.version.as_deref() else {
        return false;
    };
    let scheme = patchula_versioning::Ecosystem::of(dep.scheme, dep.source);
    let Ok(wanted) = scheme.parse(version) else {
        return false;
    };
    // The locked version is what is installed; a ranged declaration such as
    // `^2.0.0` names no single version, so the declared value only counts when
    // it is one. A lockfile-only target is measured against the lock alone.
    let lock_only = target.value.is_none() && target.digest.is_none();
    let declared = (!lock_only).then_some(dep.current.as_str());
    dep.resolved
        .as_deref()
        .into_iter()
        .chain(declared)
        .filter_map(|current| scheme.parse(current).ok())
        .any(|current| scheme.order(&wanted, &current).is_lt())
}
/// One update per occurrence of a dependency. A `0.x` crate's in-range release
/// and its next minor share a branch line, so both can reach one branch; only
/// the higher can be written, and it contains the other.
fn superseding(items: Vec<Item<'_>>) -> Vec<Item<'_>> {
    let mut kept: Vec<Item<'_>> = Vec::new();
    for item in items {
        match kept.iter_mut().find(|k| std::ptr::eq(k.dep, item.dep)) {
            Some(held) => {
                if proposal_order(item.dep, held.update, item.update).is_lt() {
                    *held = item;
                }
            }
            None => kept.push(item),
        }
    }
    kept
}
/// The branch an update belongs to, as its identity and name label. `snapshot`
/// is `None` when lockfile-pending branches are allowed; otherwise a pin joins
/// the shared pins branch only when its lockfile can be regenerated.
fn branch_identity(
    dep: &Dependency,
    reported: &patchula_report::Dependency,
    update: &Update,
    dependencies: &[Dependency],
    package_names: &PackageNames,
    snapshot: Option<&Snapshot>,
) -> (String, String) {
    // Pins share one branch, except reference data: a pin branch of
    // its own lets it merge without waiting on unrelated pins, and
    // keeps it apart from that package's version upgrade. Like
    // other pins it never joins a group, where it would share a
    // line with the grouped upgrade.
    let reference_data = dep
        .kinds()
        .contains(&patchula_words::DependencyKind::ReferenceData);
    if update.update_type == UpdateKind::Pin
        && !reference_data
        && snapshot.is_none_or(|snapshot| crate::lockfiles::job(dep, update, snapshot).is_ok())
    {
        ("pins".into(), "pin-dependencies".into())
    } else if update.update_type == UpdateKind::Pin && reference_data {
        (
            serde_json::json!(["pin", dep.source, dep.package]).to_string(),
            format!("pin-{}", package_slug(&dep.package)),
        )
    } else if let Some(group) = &update.group {
        let major = update.update_type == UpdateKind::Major;
        (
            serde_json::json!(["group", group, major]).to_string(),
            format!("{}{}", if major { "major-" } else { "" }, slug(group)),
        )
    } else if dep.manager == patchula_words::Manager::Nix {
        // An input belongs to its own lock: the same name in two flakes moves
        // on two branches, named after the input rather than its long URL. A
        // slug never holds "--", so it keeps the directory apart from the name.
        let directory = dep.file.strip_suffix("flake.lock").unwrap_or_default();
        let name = if directory.is_empty() {
            slug(&dep.name)
        } else {
            format!("{}--{}", slug(directory), slug(&dep.name))
        };
        (
            serde_json::json!(["flake", dep.file, dep.name]).to_string(),
            format!("nix-{name}"),
        )
    } else {
        let line = line(dep, reported, update);
        (
            // A package from its own registry is another branch than the public
            // package of its name; other identities stay as they were.
            match &dep.registry {
                Some(registry) => {
                    serde_json::json!(["dependency", dep.source, dep.package, line, registry])
                }
                None => serde_json::json!(["dependency", dep.source, dep.package, line]),
            }
            .to_string(),
            {
                let package = package_slug(&dep.package);
                let manager = match &dep.registry {
                    _ if package_names[&package].len() <= 1 => String::default(),
                    // The whole registry, path included: one host can serve several.
                    Some(registry) => {
                        format!(
                            "{}-",
                            slug(registry.recorded().trim_start_matches("https://"))
                        )
                    }
                    None => format!("{}-", manager_slug(dep)),
                };
                let variants: BTreeSet<_> = dependencies
                    .iter()
                    .filter(|d| {
                        d.package == dep.package
                            && d.source == dep.source
                            && d.registry == dep.registry
                    })
                    .map(|d| d.current.split_once('-').map(|(_, v)| v))
                    .collect();
                let display = if variants.len() == 1 && line.contains(".x-") {
                    line.split_once(".x-")
                        .map_or_else(|| line.clone(), |(major, _)| format!("{major}.x"))
                } else {
                    line.clone()
                };
                format!("{manager}{package}-{display}")
            },
        )
    }
}

fn line(dep: &Dependency, reported: &patchula_report::Dependency, update: &Update) -> String {
    let value = if update.update_type == UpdateKind::Major {
        update
            .new_version
            .as_deref()
            .or(update.new_value.as_deref())
    } else {
        reported
            .current
            .version
            .as_deref()
            .or(dep.resolved.as_deref())
            .or(Some(&dep.current))
    }
    .unwrap_or(&dep.current);
    let normalized = value.trim_start_matches(|c: char| !c.is_ascii_digit());
    let major = normalized
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("");
    if major.is_empty() {
        return slug(&dep.current);
    }
    // Keep container variants independent while sharing the numeric major line.
    let variant = if matches!(
        dep.source,
        patchula_words::Source::Docker | patchula_words::Source::Oci
    ) {
        dep.current
            .split_once('-')
            .map(|(_, v)| format!("-{}", slug(v)))
            .unwrap_or_default()
    } else {
        String::new()
    };
    format!("{major}.x{variant}")
}

/// The batch branch: the approved targets of every batched update, applied to
/// the current extraction. Targets the base already satisfies are complete.
#[allow(clippy::too_many_arguments)]
pub fn batch_plan(
    repo: &Repository,
    config: &Config,
    dependencies: &[Dependency],
    snapshot: &Snapshot,
    targets: &[Change],
    allow_pending: bool,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Plan>, String> {
    let evaluator = patchula_lookup::Evaluator::new(config)?;
    let mut pending = Vec::new();
    for target in targets {
        let candidates: Vec<_> = dependencies
            .iter()
            .filter(|d| {
                target.touches(
                    &d.file,
                    d.source,
                    &d.package,
                    d.registry.as_ref().map(Registry::recorded),
                )
            })
            .collect();
        let current = |d: &Dependency| {
            format!(
                "{}{}",
                d.current,
                d.digest
                    .as_ref()
                    .map(|x| format!("@{x}"))
                    .unwrap_or_default()
            )
        };
        // A target names its file once, but the file can declare the dependency
        // several times, such as one action in many jobs; every declaration
        // the PR moved moves in the batch too.
        let matching: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|d| current(d) == target.current)
            .collect();
        let deps = if matching.is_empty() {
            candidates.first().copied().into_iter().collect()
        } else {
            matching
        };
        for dep in deps {
            if pending
                .iter()
                .any(|(held, _): &(&Dependency, Update)| std::ptr::eq(*held, dep))
            {
                continue;
            }
            // A lockfile-only target is complete once the lock holds its version.
            let applied = if target.value.is_none() && target.digest.is_none() {
                target.version.as_deref().is_none_or(|v| {
                    dep.resolved
                        .as_deref()
                        .is_some_and(|locked| crate::lockfiles::same_release(locked, v))
                })
            } else {
                target.value.as_deref().is_none_or(|v| v == dep.current)
                    && target.digest == dep.digest
            };
            if applied || superseded(dep, target) {
                continue;
            }
            pending.push((
                dep,
                Update {
                    new_value: target.value.clone(),
                    new_version: target.version.clone(),
                    new_digest: target.digest.clone(),
                    update_type: target.kind,
                    group: None,
                    policy: None,
                    release_age: None,
                    fixes: Vec::new(),
                    image: target.image.clone(),
                },
            ));
        }
    }
    if pending.is_empty() {
        return Ok(None);
    }
    let items: Vec<_> = pending
        .iter()
        .map(|(dep, update)| Item {
            dep,
            update,
            current_version: None,
            advisories: &[],
        })
        .collect();
    let branch = branch_name(&config.prs.branch, "batch")?;
    let mut plan = build(
        repo,
        config,
        &evaluator,
        snapshot,
        allow_pending,
        since,
        now,
        "[\"batch\"]",
        branch,
        &items,
    )?;
    // The batch is merged by a person unless explicitly configured otherwise.
    for change in &mut plan.changes {
        change.merge = config.prs.batch_merge;
    }
    plan.set_message(format!(
        "{} batch of {} update{}",
        config.defaults.commit_prefix,
        plan.changes.len(),
        if plan.changes.len() == 1 { "" } else { "s" }
    ));
    plan.fingerprint = plan_fingerprint(
        &plan.contents,
        &plan.action().message,
        plan.action().lockfile_pending,
        &plan.lock_jobs,
    );
    plan.input_fingerprint.clone_from(&plan.fingerprint);
    Ok(Some(plan))
}

#[cfg(test)]
mod frozen_bytes {
    //! Rule 3 pins: the bytes `registry` and `package` persist as — the
    //! declaration key, the branch slug, the branch identity and the
    //! serialized `Change` — computed from the code before `Registry` was
    //! typed. Every registry form (public, URL, ambiguous, unusable) against
    //! every package encoding (plain, `registry|name`, helm `repo|name`,
    //! gradle plugin portal) must keep producing them.
    use super::*;
    use patchula_manager::{Dependency, EditForm, Location, Syntax};
    use patchula_words::{DependencyKind, Manager, Scheme};

    fn dep(registry: Option<&str>, package: &str) -> Dependency {
        Dependency {
            manager: Manager::Npm,
            file: "package.json".into(),
            location: Location {
                start: 0,
                end: 0,
                line: 1,
                column: 1,
                pointer: "/dependencies/lodash".into(),
            },
            form: EditForm::Scalar(Syntax::Json),
            name: "lodash".into(),
            package: package.into(),
            kind: DependencyKind::Production,
            current: "1.0.0".into(),
            resolved: None,
            digest: None,
            source: Source::Npm,
            scheme: Scheme::Semver,
            extract: None,
            included_by: None,
            registry: registry.map(Registry::from_recorded),
        }
    }

    fn change(dep: &Dependency) -> Change {
        Change {
            source: dep.source,
            package: dep.package.clone(),
            registry: dep.registry.clone(),
            file: dep.file.clone(),
            current: dep.current.clone(),
            current_version: None,
            value: Some("1.2.0".into()),
            version: Some("1.2.0".into()),
            digest: None,
            kind: UpdateKind::Minor,
            merge: false,
            labels: Vec::new(),
            release_notes: true,
            body_notes: Vec::new(),
            advisories: Vec::new(),
            image: None,
        }
    }

    #[test]
    fn registry_and_package_bytes_are_frozen() {
        // (registry, package, key, package slug, registry slug, identity, change)
        type Case = (
            Option<&'static str>,
            &'static str,
            &'static str,
            &'static str,
            Option<&'static str>,
            &'static str,
            &'static str,
        );
        let cases: [Case; 16] = [
            (
                None,
                "lodash",
                "cb360af58a71732d87bd6bbacf80b9294db02dffe0914bd10fe4536f5dc7083c",
                "lodash",
                None,
                r#"["dependency","npm","lodash","1.x"]"#,
                r#"{"source":"npm","package":"lodash","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                None,
                "https://npm.acme.dev|lodash",
                "8d6356b6f28e614a95be109042e4b1b478a83dcfbb7f84ff93a926cf1878c719",
                "lodash",
                None,
                r#"["dependency","npm","https://npm.acme.dev|lodash","1.x"]"#,
                r#"{"source":"npm","package":"https://npm.acme.dev|lodash","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                None,
                "https://charts.acme.dev|mychart",
                "49fa9d2169120fb496ba3ea0e797093271a26aac3e41d8200e99617dd32dcfa2",
                "mychart",
                None,
                r#"["dependency","npm","https://charts.acme.dev|mychart","1.x"]"#,
                r#"{"source":"npm","package":"https://charts.acme.dev|mychart","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                None,
                "https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin",
                "680ad27edf7ed85317291f1d740e82fccfff3b66e75bd79fc9c124d0b1f956db",
                "com-acme-plugin-com-acme-plugin-gradle-plugin",
                None,
                r#"["dependency","npm","https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin","1.x"]"#,
                r#"{"source":"npm","package":"https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("https://npm.acme.dev"),
                "lodash",
                "f0fa3fd0c5da69b981a696a6c79707a309bc9999fba1c52499d321d1834cbc12",
                "lodash",
                Some("npm-acme-dev"),
                r#"["dependency","npm","lodash","1.x","https://npm.acme.dev"]"#,
                r#"{"source":"npm","package":"lodash","registry":"https://npm.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("https://npm.acme.dev"),
                "https://npm.acme.dev|lodash",
                "3ff51b432f6b25c649b96f1be7b22989d57f4fb259625e7c681d76983a080e89",
                "lodash",
                Some("npm-acme-dev"),
                r#"["dependency","npm","https://npm.acme.dev|lodash","1.x","https://npm.acme.dev"]"#,
                r#"{"source":"npm","package":"https://npm.acme.dev|lodash","registry":"https://npm.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("https://npm.acme.dev"),
                "https://charts.acme.dev|mychart",
                "666ab325667a8ac68dce0957db9ed066153b09bf2806d27a5e00c79bd7a804ae",
                "mychart",
                Some("npm-acme-dev"),
                r#"["dependency","npm","https://charts.acme.dev|mychart","1.x","https://npm.acme.dev"]"#,
                r#"{"source":"npm","package":"https://charts.acme.dev|mychart","registry":"https://npm.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("https://npm.acme.dev"),
                "https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin",
                "46b451c1faef05470af7738835138b8eb7319962501d00168f60953aea17a881",
                "com-acme-plugin-com-acme-plugin-gradle-plugin",
                Some("npm-acme-dev"),
                r#"["dependency","npm","https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin","1.x","https://npm.acme.dev"]"#,
                r#"{"source":"npm","package":"https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin","registry":"https://npm.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("ambiguous: https://a.acme.dev, https://b.acme.dev"),
                "lodash",
                "cdef217c26da17014b8f4d13fffb4a50a6c587a96087642255322ff2adf28a6c",
                "lodash",
                Some("ambiguous-https-a-acme-dev-https-b-acme-dev"),
                r#"["dependency","npm","lodash","1.x","ambiguous: https://a.acme.dev, https://b.acme.dev"]"#,
                r#"{"source":"npm","package":"lodash","registry":"ambiguous: https://a.acme.dev, https://b.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("ambiguous: https://a.acme.dev, https://b.acme.dev"),
                "https://npm.acme.dev|lodash",
                "a56161de2f4d43d7851010b0c498df1ae0396d38984fec6c4d080967ae6838e3",
                "lodash",
                Some("ambiguous-https-a-acme-dev-https-b-acme-dev"),
                r#"["dependency","npm","https://npm.acme.dev|lodash","1.x","ambiguous: https://a.acme.dev, https://b.acme.dev"]"#,
                r#"{"source":"npm","package":"https://npm.acme.dev|lodash","registry":"ambiguous: https://a.acme.dev, https://b.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("ambiguous: https://a.acme.dev, https://b.acme.dev"),
                "https://charts.acme.dev|mychart",
                "cf820a0cda4a8d54de429fb9a1f98bbde2438b835c3cca7809ab665450636bf1",
                "mychart",
                Some("ambiguous-https-a-acme-dev-https-b-acme-dev"),
                r#"["dependency","npm","https://charts.acme.dev|mychart","1.x","ambiguous: https://a.acme.dev, https://b.acme.dev"]"#,
                r#"{"source":"npm","package":"https://charts.acme.dev|mychart","registry":"ambiguous: https://a.acme.dev, https://b.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("ambiguous: https://a.acme.dev, https://b.acme.dev"),
                "https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin",
                "23257b357d833adf409a1c5d8969bf9ae9089b7cc0a7ba9f83cf66282dfe22ed",
                "com-acme-plugin-com-acme-plugin-gradle-plugin",
                Some("ambiguous-https-a-acme-dev-https-b-acme-dev"),
                r#"["dependency","npm","https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin","1.x","ambiguous: https://a.acme.dev, https://b.acme.dev"]"#,
                r#"{"source":"npm","package":"https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin","registry":"ambiguous: https://a.acme.dev, https://b.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("${NPM_REGISTRY}"),
                "lodash",
                "be5cd094f73b6dfaf788210d52e1bb5f1e1fc9b704f30a4008493ed87f9cf0bc",
                "lodash",
                Some("npm-registry"),
                r#"["dependency","npm","lodash","1.x","${NPM_REGISTRY}"]"#,
                r#"{"source":"npm","package":"lodash","registry":"${NPM_REGISTRY}","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("${NPM_REGISTRY}"),
                "https://npm.acme.dev|lodash",
                "1c1f215e654d25b5ece6aeafda33961545c52f69865ecc790d82a3d947bd7d69",
                "lodash",
                Some("npm-registry"),
                r#"["dependency","npm","https://npm.acme.dev|lodash","1.x","${NPM_REGISTRY}"]"#,
                r#"{"source":"npm","package":"https://npm.acme.dev|lodash","registry":"${NPM_REGISTRY}","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("${NPM_REGISTRY}"),
                "https://charts.acme.dev|mychart",
                "6f8f98d2cd57d3ac4d6bb413c1cc1c5a84da91fa1883af5357cd9587306bbc6d",
                "mychart",
                Some("npm-registry"),
                r#"["dependency","npm","https://charts.acme.dev|mychart","1.x","${NPM_REGISTRY}"]"#,
                r#"{"source":"npm","package":"https://charts.acme.dev|mychart","registry":"${NPM_REGISTRY}","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
            (
                Some("${NPM_REGISTRY}"),
                "https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin",
                "9a612f63dd398354a60fcfbf85650bf92702e346c28b674559a6cb97bacd642d",
                "com-acme-plugin-com-acme-plugin-gradle-plugin",
                Some("npm-registry"),
                r#"["dependency","npm","https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin","1.x","${NPM_REGISTRY}"]"#,
                r#"{"source":"npm","package":"https://plugins.gradle.org/m2|com.acme.plugin:com.acme.plugin.gradle.plugin","registry":"${NPM_REGISTRY}","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}"#,
            ),
        ];
        for (registry, package, key, pslug, rslug, identity, change_json) in cases {
            let dep = dep(registry, package);
            assert_eq!(
                dep.declaration().key(),
                key,
                "key for {registry:?} {package}"
            );
            assert_eq!(
                package_slug(&dep.package),
                pslug,
                "package slug for {package}"
            );
            assert_eq!(
                dep.registry
                    .as_ref()
                    .map(|r| slug(r.recorded().trim_start_matches("https://"))),
                rslug.map(str::to_owned),
                "registry slug for {registry:?}"
            );
            let identity_json = match &dep.registry {
                Some(registry) => {
                    serde_json::json!(["dependency", dep.source, dep.package, "1.x", registry])
                }
                None => serde_json::json!(["dependency", dep.source, dep.package, "1.x"]),
            }
            .to_string();
            assert_eq!(
                identity_json, identity,
                "identity for {registry:?} {package}"
            );
            assert_eq!(
                serde_json::to_string(&change(&dep)).unwrap(),
                change_json,
                "change for {registry:?} {package}"
            );
            if let Some(registry) = registry {
                let recorded = dep.registry.as_ref().unwrap().recorded();
                assert_eq!(recorded, registry, "recorded round-trip");
                assert_eq!(
                    Registry::from_recorded(recorded).recorded(),
                    recorded,
                    "serde round-trip"
                );
                let json = serde_json::to_string(&dep.registry).unwrap();
                assert_eq!(json, format!("\"{recorded}\""), "registry serde");
            }
        }
    }

    /// `registry` was `Option<String>` and sorted as one: a plan's changes,
    /// the PR body rows and the `patchula-targets` marker order by the
    /// recorded string, not the enum's variant order. `None`, then
    /// `${NPM_REGISTRY}`, `ambiguous: …` and `https://…` is that order.
    #[test]
    fn mixed_registry_changes_keep_their_recorded_order() {
        use std::collections::BTreeSet;
        let registries: [Option<&str>; 4] = [
            None,
            Some("${NPM_REGISTRY}"),
            Some("ambiguous: https://a.acme.dev, https://b.acme.dev"),
            Some("https://npm.acme.dev"),
        ];
        let changes: BTreeSet<Change> = registries
            .iter()
            .map(|registry| change(&dep(*registry, "lodash")))
            .collect();
        let order: Vec<Option<String>> = changes
            .iter()
            .map(|c| c.registry.as_ref().map(|r| r.recorded().to_owned()))
            .collect();
        assert_eq!(
            order,
            vec![
                None,
                Some("${NPM_REGISTRY}".to_owned()),
                Some("ambiguous: https://a.acme.dev, https://b.acme.dev".to_owned()),
                Some("https://npm.acme.dev".to_owned()),
            ]
        );
        assert_eq!(
            serde_json::to_string(&changes).unwrap(),
            r#"[{"source":"npm","package":"lodash","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]},{"source":"npm","package":"lodash","registry":"${NPM_REGISTRY}","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]},{"source":"npm","package":"lodash","registry":"ambiguous: https://a.acme.dev, https://b.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]},{"source":"npm","package":"lodash","registry":"https://npm.acme.dev","file":"package.json","current":"1.0.0","current_version":null,"value":"1.2.0","version":"1.2.0","digest":null,"kind":"minor","merge":false,"labels":[],"release_notes":true,"body_notes":[]}]"#
        );
    }
}
