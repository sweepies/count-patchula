use rustovate_config::{Config, Manager};
use rustovate_manager::{Dependency, Edit, word};
use rustovate_report::{ActionStatus, BranchAction, FileAction, Repository, Update};
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
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Change {
    pub source: String,
    pub package: String,
    pub file: String,
    pub current: String,
    pub value: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
    pub kind: String,
    pub merge: bool,
    pub labels: Vec<String>,
    pub release_notes: bool,
    pub body_notes: Vec<String>,
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
}

pub fn plan(
    repo: &Repository,
    config: &Config,
    dependencies: &[Dependency],
    snapshot: &Snapshot,
    allow_pending: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Plan>, String> {
    let evaluator = rustovate_lookup::Evaluator::new(config)?;
    let mut package_names = BTreeMap::<String, BTreeSet<(String, String)>>::new();
    for dep in dependencies {
        package_names
            .entry(slug(&dep.package))
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
                    let (identity, label) = if update.update_type == "pin"
                        && (allow_pending || !needs_lockfile(dep, snapshot))
                    {
                        ("pins".into(), "pin-dependencies".into())
                    } else if let Some(group) = &update.group {
                        let major = update.update_type == "major";
                        (
                            serde_json::json!(["group", group, major]).to_string(),
                            format!("{}{}", if major { "major-" } else { "" }, slug(group)),
                        )
                    } else {
                        let line = line(dep, reported, update);
                        (
                            serde_json::json!(["dependency", word(&dep.source), dep.package, line])
                                .to_string(),
                            {
                                let package = slug(&dep.package);
                                let manager = if package_names[&package].len() > 1 {
                                    format!("{}-", manager_slug(dep))
                                } else {
                                    String::new()
                                };
                                let variants: BTreeSet<_> = dependencies
                                    .iter()
                                    .filter(|d| d.package == dep.package && d.source == dep.source)
                                    .map(|d| d.current.split_once('-').map(|(_, v)| v))
                                    .collect();
                                let display = if variants.len() == 1 && line.contains(".x-") {
                                    line.split_once(".x-")
                                        .map(|(major, _)| format!("{major}.x"))
                                        .unwrap_or_else(|| line.clone())
                                } else {
                                    line.clone()
                                };
                                format!("{manager}{package}-{display}")
                            },
                        )
                    };
                    groups
                        .entry(identity)
                        .or_insert_with(|| (label, Vec::new()))
                        .1
                        .push(Item { dep, update });
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
        let mut changes = BTreeMap::<String, Vec<Edit>>::new();
        let mut reason = None;
        let mut pending = false;
        let mut prefixes = BTreeSet::new();
        let mut titles = BTreeSet::new();
        for item in &items {
            let dep = item.dep;
            let update = item.update;
            if evaluator.needs_digest(dep, update) && update.new_digest.is_none() {
                reason = Some(format!(
                    "{}: required target digest is unavailable; retry lookup",
                    dep.name
                ));
            }
            if let Some(policy) = &update.policy {
                prefixes.insert(policy.commit_prefix.clone());
                if !in_window(&policy.windows, now) {
                    reason = Some("outside the configured update windows".into());
                }
            }
            pending |= needs_lockfile(dep, snapshot);
            let value = update.new_value.as_deref().unwrap_or(&dep.current);
            titles.insert(format!(
                "{} to {}",
                dep.name,
                update
                    .new_value
                    .as_deref()
                    .or(update.new_version.as_deref())
                    .or(update.new_digest.as_deref())
                    .unwrap_or(value)
            ));
            let Some(blob) = snapshot.files.get(&dep.file) else {
                return Err(format!("missing extracted file {}", dep.file));
            };
            if !matches!(blob.mode.as_str(), "100644" | "100755") {
                reason = Some(format!("unsupported manifest mode {}", blob.mode));
                continue;
            }
            match rustovate_manager::edits(dep, &blob.content, value, update.new_digest.as_deref())
            {
                Ok(edits) => changes.entry(dep.file.clone()).or_default().extend(edits),
                Err(error) => reason = Some(format!("{}: {error}", dep.file)),
            }
        }
        if prefixes.len() > 1 {
            reason = Some("group has conflicting commitPrefix policies".into());
        }
        let prefix = prefixes.first().unwrap_or(&config.defaults.commit_prefix);
        let message = if items.iter().all(|item| item.update.update_type == "pin") {
            format!("{prefix} pin dependencies")
        } else {
            format!(
                "{prefix} update {}",
                titles.into_iter().collect::<Vec<_>>().join(", ")
            )
        };
        let mut contents = BTreeMap::new();
        let mut files = Vec::new();
        for (path, edits) in changes {
            let blob = &snapshot.files[&path];
            match rustovate_manager::apply(&blob.content, edits) {
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
        if contents.is_empty() && reason.is_none() {
            reason = Some(
                "no editable manifest change (inherited declarations are owned by their workspace)"
                    .into(),
            );
        }
        if pending && !allow_pending {
            reason = Some("lockfile pending: skipped until SWE-109; --allow-lockfile-pending permits manifest-only testing".into());
        }
        let fingerprint = hash(
            serde_json::to_string(&(&contents, &message, pending))
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
            lockfile_pending: pending,
            files,
        };
        let changes: BTreeSet<_> = items
            .iter()
            .map(|item| {
                let u = item.update;
                let p = u.policy.as_ref();
                Change {
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
                }
            })
            .collect();
        plans.push(Plan {
            action,
            contents,
            identity: hash(identity.as_bytes()),
            fingerprint,
            changes: changes.into_iter().collect(),
            retry: false,
            tree: None,
            cached_pr: None,
        });
    }
    Ok(plans)
}

fn needs_lockfile(dep: &Dependency, snapshot: &Snapshot) -> bool {
    let locks: &[&str] = match dep.manager {
        Manager::Npm | Manager::Bun => &[
            "package-lock.json",
            "npm-shrinkwrap.json",
            "bun.lock",
            "bun.lockb",
            "pnpm-lock.yaml",
            "yarn.lock",
        ],
        Manager::Cargo => &["Cargo.lock"],
        Manager::Go => &["go.sum"],
        Manager::Terraform => &[".terraform.lock.hcl"],
        _ => return false,
    };
    snapshot
        .paths
        .iter()
        .any(|p| locks.contains(&p.rsplit('/').next().unwrap_or(p)))
}
fn in_window(windows: &[rustovate_report::Window], now: chrono::DateTime<chrono::Utc>) -> bool {
    use std::str::FromStr;
    windows.is_empty()
        || windows.iter().any(|w| {
            let Ok(zone) = w.zone.parse::<chrono_tz::Tz>() else {
                return false;
            };
            let local = now.with_timezone(&zone);
            w.cron.iter().any(|expr| {
                cron::Schedule::from_str(&format!("0 {expr}"))
                    .ok()
                    .and_then(|s| s.after(&(local - chrono::Duration::minutes(1))).next())
                    .is_some_and(|t| t <= local)
            })
        })
}
fn unified(path: &str, old: &str, new: &str) -> String {
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
        rustovate_config::Source::Docker | rustovate_config::Source::Oci
    ) {
        "docker".into()
    } else {
        word(&dep.manager)
    }
}
fn proposal_order(dep: &Dependency, a: &Update, b: &Update) -> std::cmp::Ordering {
    use rustovate_versioning::Versioning;
    let value = |u: &Update| {
        u.new_version
            .clone()
            .or_else(|| u.new_value.clone())
            .unwrap_or_else(|| dep.current.clone())
    };
    if let Ok(scheme) = rustovate_lookup::scheme(dep)
        && let (Ok(a), Ok(b)) = (scheme.parse(&value(a)), scheme.parse(&value(b)))
    {
        return scheme.order(&a, &b);
    }
    value(a).cmp(&value(b))
}
fn line(dep: &Dependency, reported: &rustovate_report::Dependency, update: &Update) -> String {
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
        rustovate_config::Source::Docker | rustovate_config::Source::Oci
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
