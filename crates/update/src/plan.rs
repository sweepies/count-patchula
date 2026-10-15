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
/// Hash the full identity (including source and track), and refuse any actual
/// name collision within a repository before applying any plans.
pub fn branch_name(prefix: &str, label: &str, identity: &str) -> Result<String, String> {
    let slug = label
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() { "update" } else { &slug };
    let name = format!(
        "{prefix}{}-{}",
        &slug[..slug.len().min(64)],
        &hash(identity.as_bytes())[..16]
    );
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
                    let (identity, label) = if let Some(group) = &update.group {
                        (
                            serde_json::json!(["group", group, update.update_type]).to_string(),
                            group.clone(),
                        )
                    } else {
                        (
                            serde_json::json!([
                                "dependency",
                                word(&dep.source),
                                dep.package,
                                update.update_type
                            ])
                            .to_string(),
                            dep.package.clone(),
                        )
                    };
                    groups
                        .entry(identity)
                        .or_insert_with(|| (format!("{label}-{}", update.update_type), Vec::new()))
                        .1
                        .push(Item { dep, update });
                }
            }
        }
    }
    let mut names = BTreeSet::new();
    let mut plans = Vec::new();
    for (identity, (label, items)) in groups {
        let branch = branch_name(&config.prs.branch, &label, &identity)?;
        if !names.insert(branch.clone()) {
            return Err("branch name collision; refusing ambiguous plans".into());
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
        let message = format!(
            "{prefix} update {}",
            titles.into_iter().collect::<Vec<_>>().join(", ")
        );
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
        plans.push(Plan {
            action,
            contents,
            identity: hash(identity.as_bytes()),
            fingerprint,
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
