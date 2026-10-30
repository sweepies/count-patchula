use patchula_config::Config;
use patchula_manager::{Dependency, Edit, word};
use patchula_report::{ActionStatus, BranchAction, FileAction, Repository, Update};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// Includes unselected lockfile paths, for conservative write gating.
    pub paths: BTreeSet<String>,
    pub files: BTreeMap<String, Blob>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blob {
    pub sha: String,
    pub mode: String,
    pub content: String,
}
#[derive(Debug)]
pub struct Plan {
    pub action: BranchAction,
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
    pub deleted: BTreeSet<String>,
    pub maintenance: bool,
    /// The batch PR's items; `None` for every other plan.
    pub batch: Option<Vec<crate::batch::Item>>,
    /// Tooling warnings shown on this PR.
    pub tooling: Vec<crate::tooling::Callout>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Change {
    pub source: String,
    pub package: String,
    pub file: String,
    pub current: String,
    pub current_version: Option<String>,
    pub value: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
    pub kind: String,
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
pub(crate) fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
pub fn branch_name(prefix: &str, label: &str, _identity: &str) -> Result<String, String> {
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
    let mut package_names = BTreeMap::<String, BTreeSet<(String, String)>>::new();
    for dep in dependencies {
        package_names
            .entry(package_slug(&dep.package))
            .or_default()
            .insert((word(&dep.source), dep.package.clone()));
    }
    let mut groups = BTreeMap::<String, (String, Vec<Item<'_>>)>::new();
    for file in &repo.package_files {
        for reported in &file.dependencies {
            for update in &reported.updates {
                for dep in dependencies.iter().filter(|d| {
                    d.file == file.path
                        && word(&d.manager) == reported.manager
                        && d.name == reported.name
                        && d.package == reported.package
                        && reported.current.value.as_deref() == Some(&d.current)
                        && d.digest == reported.current.digest
                }) {
                    // One best proposal per occurrence and track avoids overlapping edits.
                    let track = |u: &Update| match u.update_type.as_str() {
                        "pin" => "pin",
                        "major" => "major",
                        _ => "non-major",
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
        if labels[&label] > 1 {
            let managers: BTreeSet<_> = items.iter().map(|i| manager_slug(i.dep)).collect();
            label = format!(
                "{}-{label}",
                managers.into_iter().collect::<Vec<_>>().join("-")
            );
        }
        let branch = branch_name(&config.prs.branch, &label, &identity)?;
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
    dep.kind == patchula_config::DependencyKind::Transitive
        || dep.manager == patchula_config::Manager::Nix
        || (update.new_value.is_none() && update.new_version.is_some())
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
    let mut reason = None;
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
            reason = Some(format!(
                "{}: required target digest is unavailable; retry lookup",
                dep.name
            ));
        }
        if let Some(policy) = &update.policy {
            if !secure || !update.fixes.is_empty() {
                prefixes.insert(policy.commit_prefix.clone());
            }
            if !in_window(&policy.windows, since, now) {
                reason = Some("outside the configured update windows".into());
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
            reason = Some(format!("unsupported manifest mode {}", blob.mode));
            continue;
        }
        match patchula_manager::edits(dep, &blob.content, value, update.new_digest.as_deref()) {
            Ok(edits) => changes.entry(dep.file.clone()).or_default().extend(edits),
            Err(error) => reason = Some(format!("{}: {error}", dep.file)),
        }
    }
    if prefixes.len() > 1 {
        reason = Some("group has conflicting commitPrefix policies".into());
    }
    let prefix = prefixes.first().unwrap_or(&config.defaults.commit_prefix);
    let mut message = if items.iter().all(|item| item.update.update_type == "pin") {
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
            Err(error) => reason = Some(format!("{path}: {error}")),
        }
    }
    let lockfile_only = items.iter().all(|item| lock_only(item.dep, item.update));
    if lockfile_only && lock_jobs.iter().any(|job| job.target.is_none()) {
        reason = Some("no lockfile tool can move this package to an exact version".into());
    }
    if contents.is_empty() && reason.is_none() && !(lockfile_only && !lock_jobs.is_empty()) {
        reason = Some(
            "no editable manifest change: the declaration already allows the target, or is inherited from its workspace root"
                .into(),
        );
    }
    if let Some(error) = pending.as_ref().filter(|_| !allow_pending) {
        reason = Some(format!(
            "lockfile pending: {error}; --allow-lockfile-pending permits manifest-only testing"
        ));
    }
    let fingerprint = hash(
        serde_json::to_string(&(&contents, &message, pending.is_some(), &lock_jobs))
            .expect("serializable plan")
            .as_bytes(),
    );
    let action = BranchAction {
        repository: repo.repository.clone(),
        branch,
        base_commit: repo.commit.clone().ok_or("missing extraction commit")?,
        status: if reason.is_some() {
            ActionStatus::Skipped
        } else {
            ActionStatus::Planned
        },
        reason,
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
                source: word(&item.dep.source),
                package: item.dep.package.clone(),
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
                kind: u.update_type.clone(),
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
        deleted: BTreeSet::new(),
        maintenance: false,
        batch: None,
        tooling: Vec::new(),
    })
}

/// Whether a schedule lets this run propose updates: one of the times its cron
/// expressions name fell after the repository's previous run (`since`) and at
/// or before `now`. However often runs happen, a daily time is due on the first
/// run after it. With no previous run recorded, the current minute must be one
/// of those times.
pub(crate) fn in_window(
    windows: &[patchula_report::Window],
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    windows.is_empty()
        || windows.iter().any(|w| {
            let Ok(zone) = w.zone.parse::<chrono_tz::Tz>() else {
                return false;
            };
            let local = now.with_timezone(&zone);
            let from = since
                .filter(|since| *since < now)
                .map_or(local - chrono::Duration::minutes(1), |since| {
                    since.with_timezone(&zone)
                });
            w.cron.iter().any(|expr| {
                patchula_config::schedule(expr, false)
                    .ok()
                    .and_then(|s| s.after(&from).next())
                    .is_some_and(|t| t <= local)
            })
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
    slug(package.rsplit_once('|').map_or(package, |(_, name)| name))
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
        patchula_config::Source::Docker | patchula_config::Source::Oci
    ) {
        "docker".into()
    } else {
        word(&dep.manager)
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
    if let Ok(scheme) = patchula_lookup::scheme(dep)
        && let (Ok(a), Ok(b)) = (scheme.parse(&value(a)), scheme.parse(&value(b)))
    {
        return scheme.order(&a, &b);
    }
    value(a).cmp(&value(b))
}
/// The branch an update belongs to, as its identity and name label. `snapshot`
/// is `None` when lockfile-pending branches are allowed; otherwise a pin joins
/// the shared pins branch only when its lockfile can be regenerated.
fn branch_identity(
    dep: &Dependency,
    reported: &patchula_report::Dependency,
    update: &Update,
    dependencies: &[Dependency],
    package_names: &BTreeMap<String, BTreeSet<(String, String)>>,
    snapshot: Option<&Snapshot>,
) -> (String, String) {
    // Pins share one branch, except reference data: a pin branch of
    // its own lets it merge without waiting on unrelated pins, and
    // keeps it apart from that package's version upgrade. Like
    // other pins it never joins a group, where it would share a
    // line with the grouped upgrade.
    let reference_data = dep
        .kinds()
        .contains(&patchula_config::DependencyKind::ReferenceData);
    if update.update_type == "pin"
        && !reference_data
        && snapshot.is_none_or(|snapshot| crate::lockfiles::job(dep, update, snapshot).is_ok())
    {
        ("pins".into(), "pin-dependencies".into())
    } else if update.update_type == "pin" && reference_data {
        (
            serde_json::json!(["pin", word(&dep.source), dep.package]).to_string(),
            format!("pin-{}", package_slug(&dep.package)),
        )
    } else if let Some(group) = &update.group {
        let major = update.update_type == "major";
        (
            serde_json::json!(["group", group, major]).to_string(),
            format!("{}{}", if major { "major-" } else { "" }, slug(group)),
        )
    } else if dep.manager == patchula_config::Manager::Nix {
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
            serde_json::json!(["dependency", word(&dep.source), dep.package, line]).to_string(),
            {
                let package = package_slug(&dep.package);
                let manager = if package_names[&package].len() > 1 {
                    format!("{}-", manager_slug(dep))
                } else {
                    String::default()
                };
                let variants: BTreeSet<_> = dependencies
                    .iter()
                    .filter(|d| d.package == dep.package && d.source == dep.source)
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
    let value = if update.update_type == "major" {
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
        patchula_config::Source::Docker | patchula_config::Source::Oci
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
                d.file == target.file
                    && d.package == target.package
                    && word(&d.source) == target.source
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
        let Some(dep) = candidates
            .iter()
            .find(|d| current(d) == target.current)
            .or_else(|| candidates.first())
        else {
            continue;
        };
        // A lockfile-only target is complete once the lock holds its version.
        let applied = if target.value.is_none() && target.digest.is_none() {
            target
                .version
                .as_deref()
                .is_none_or(|v| dep.resolved.as_deref() == Some(v))
        } else {
            target.value.as_deref().is_none_or(|v| v == dep.current) && target.digest == dep.digest
        };
        if applied {
            continue;
        }
        pending.push((
            *dep,
            Update {
                new_value: target.value.clone(),
                new_version: target.version.clone(),
                new_digest: target.digest.clone(),
                update_type: target.kind.clone(),
                group: None,
                policy: None,
                release_age: None,
                fixes: Vec::new(),
                image: target.image.clone(),
            },
        ));
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
    let branch = branch_name(&config.prs.branch, "batch", "batch")?;
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
    plan.action.message = format!(
        "{} the Count's batch of {} update{}",
        config.defaults.commit_prefix,
        plan.changes.len(),
        if plan.changes.len() == 1 { "" } else { "s" }
    );
    plan.fingerprint = hash(
        serde_json::to_string(&(
            &plan.contents,
            &plan.action.message,
            plan.action.lockfile_pending,
            &plan.lock_jobs,
        ))
        .expect("serializable plan")
        .as_bytes(),
    );
    plan.input_fingerprint.clone_from(&plan.fingerprint);
    Ok(Some(plan))
}

#[cfg(test)]
mod window_tests {
    use super::in_window;
    use chrono::{DateTime, Utc};
    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().to_utc()
    }
    fn window(cron: &str) -> Vec<patchula_report::Window> {
        vec![patchula_report::Window {
            cron: vec![cron.into()],
            zone: "UTC".into(),
        }]
    }

    #[test]
    fn a_daily_time_is_due_once_on_the_first_run_after_it() {
        let daily = window("0 3 * * *");
        // Hourly runs: only the one after 03:00 proposes.
        assert!(in_window(
            &daily,
            Some(at("2026-10-22T02:00:00Z")),
            at("2026-10-22T03:00:05Z")
        ));
        assert!(!in_window(
            &daily,
            Some(at("2026-10-22T03:00:05Z")),
            at("2026-10-22T04:00:00Z")
        ));
        // A run every six hours still catches it.
        assert!(in_window(
            &daily,
            Some(at("2026-10-22T00:00:00Z")),
            at("2026-10-22T06:00:00Z")
        ));
        // A webhook run at 03:30 takes the day's turn; the next sweep does not.
        assert!(in_window(
            &daily,
            Some(at("2026-10-22T02:59:00Z")),
            at("2026-10-22T03:30:00Z")
        ));
        assert!(!in_window(
            &daily,
            Some(at("2026-10-22T03:30:00Z")),
            at("2026-10-22T04:00:00Z")
        ));
    }

    #[test]
    fn without_a_previous_run_only_the_current_minute_counts() {
        let daily = window("0 3 * * *");
        assert!(in_window(&daily, None, at("2026-10-22T03:00:30Z")));
        assert!(!in_window(&daily, None, at("2026-10-22T04:00:00Z")));
    }

    #[test]
    fn ranges_stay_open_and_empty_windows_always_allow() {
        let mornings = window("* 0-6 * * 1");
        // 2026-10-19 is a Monday.
        assert!(in_window(
            &mornings,
            Some(at("2026-10-19T02:00:00Z")),
            at("2026-10-19T03:00:00Z")
        ));
        assert!(!in_window(
            &mornings,
            Some(at("2026-10-19T08:00:00Z")),
            at("2026-10-19T09:00:00Z")
        ));
        assert!(in_window(&[], None, at("2026-10-22T12:00:00Z")));
    }
}
