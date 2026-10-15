//! Versioned caller interface for lookup/run batches, independent of log output.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid report JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported report schemaVersion {0}; expected 2")]
    Version(u32),
    #[error("invalid report: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    pub repositories: Vec<Repository>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    /// Versioned write-path extension; lookup reports omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Actions>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Actions {
    pub schema_version: u32,
    pub dry_run: bool,
    pub branches: Vec<BranchAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<PullRequestAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PullRequestAction {
    pub repository: String,
    pub branch: String,
    pub number: Option<u64>,
    pub url: Option<String>,
    pub status: ActionStatus,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActionStatus {
    Planned,
    Deleted,
    Closed,
    Created,
    Updated,
    Rebased,
    Unchanged,
    Skipped,
    Protected,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchAction {
    pub repository: String,
    pub branch: String,
    pub base_commit: String,
    pub status: ActionStatus,
    pub reason: Option<String>,
    pub message: String,
    pub previous_commit: Option<String>,
    pub commit: Option<String>,
    pub verified: Option<bool>,
    pub lockfile_pending: bool,
    pub files: Vec<FileAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileAction {
    pub path: String,
    pub blob_sha: String,
    pub diff: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Complete,
    Skipped,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Repository {
    pub repository: String,
    pub status: Status,
    pub reason: Option<String>,
    pub package_files: Vec<PackageFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageFile {
    pub path: String,
    pub dependencies: Vec<Dependency>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dependency {
    /// Canonical rustovate manager identifier; unknown upstream names stay literal.
    pub manager: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<String>,
    /// Registry identity, which may differ from the name in the manifest.
    pub package: String,
    pub current: Current,
    pub updates: Vec<Update>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<DependencyMetadata>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DependencyMetadata {
    /// Independently observed floating action tag commits, keyed by ref.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tag_digests: std::collections::BTreeMap<String, String>,
    pub source: String,
    pub kinds: Vec<String>,
    pub locations: Vec<Location>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held_back: Vec<HeldBack>,
    /// Registry cache provenance, present only after a rate-limited lookup.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub stale: std::collections::BTreeMap<String, i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lookup: Option<LookupEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupEvidence {
    pub evaluated_at: String,
    pub disabled_updates: Vec<String>,
    pub rejected: Vec<RejectedVersion>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RejectedVersion {
    pub version: String,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resolution {
    /// declared, lockfile, pinned-commit, floating-ref, release-line or supported-range.
    pub kind: String,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeldBack {
    pub version: String,
    /// All blocked releases in this class, including the summary version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<String>,
    #[serde(default = "one")]
    pub count: usize,
    pub update_type: String,
    pub published_at: String,
    pub eligible_at: String,
    /// Exact summary target's eligibility; eligibleAt remains the earliest in the class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_eligible_at: Option<String>,
}
fn one() -> usize {
    1
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseAge {
    pub published_at: String,
    pub evaluated_at: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
    pub pointer: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Stats {
    pub requests: u64,
    pub not_modified: u64,
    pub cache_hits: u64,
    pub extraction_hits: u64,
    pub lookups: u64,
    pub phase_micros: std::collections::BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Current {
    pub value: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Update {
    pub new_value: Option<String>,
    pub new_version: Option<String>,
    pub new_digest: Option<String>,
    /// major, minor, patch, digest, pin; comparison retains other upstream types.
    pub update_type: String,
    pub group: Option<String>,
    /// Lookup metadata; absent in black-box comparison inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<UpdatePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_age: Option<ReleaseAge>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    pub wait_hours: u32,
    pub windows: Vec<Window>,
    pub merge: bool,
    pub labels: Vec<String>,
    pub commit_prefix: String,
    pub release_notes: bool,
    pub body_notes: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub cron: Vec<String>,
    pub zone: String,
}

impl Report {
    /// Check the version before deserializing its payload, including future schemas.
    pub fn from_json(bytes: &[u8]) -> Result<Self, Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Header {
            schema_version: u32,
        }
        let header: Header = serde_json::from_slice(bytes)?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(Error::Version(header.schema_version));
        }
        let report: Self = serde_json::from_slice(bytes)?;
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::Version(self.schema_version));
        }
        if let Some(actions) = &self.actions {
            if actions.schema_version != 1 {
                return Err(Error::Invalid("unsupported actions.schemaVersion".into()));
            }
            let mut prs = BTreeSet::new();
            for pr in &actions.pull_requests {
                if pr.branch.is_empty()
                    || !prs.insert((&pr.repository, &pr.branch))
                    || !self
                        .repositories
                        .iter()
                        .any(|r| r.repository == pr.repository)
                {
                    return Err(Error::Invalid(
                        "invalid or duplicate pull request action".into(),
                    ));
                }
                if actions.dry_run && (pr.number.is_some() || pr.status != ActionStatus::Planned) {
                    return Err(Error::Invalid(
                        "dry run cannot contain applied pull requests".into(),
                    ));
                }
                if matches!(
                    pr.status,
                    ActionStatus::Created
                        | ActionStatus::Updated
                        | ActionStatus::Unchanged
                        | ActionStatus::Closed
                ) && pr.number.is_none()
                {
                    return Err(Error::Invalid(
                        "applied pull request action needs a number".into(),
                    ));
                }
                if matches!(
                    pr.status,
                    ActionStatus::Protected | ActionStatus::Error | ActionStatus::Skipped
                ) && pr.reason.as_ref().is_none_or(String::is_empty)
                {
                    return Err(Error::Invalid(
                        "failed pull request action needs a reason".into(),
                    ));
                }
            }
            let mut branches = BTreeSet::new();
            for action in &actions.branches {
                if action.branch.is_empty()
                    || action.base_commit.is_empty()
                    || !branches.insert((&action.repository, &action.branch))
                    || !self
                        .repositories
                        .iter()
                        .any(|r| r.repository == action.repository)
                {
                    return Err(Error::Invalid(
                        "invalid or duplicate branch action identity".into(),
                    ));
                }
                if matches!(
                    action.status,
                    ActionStatus::Skipped | ActionStatus::Protected | ActionStatus::Error
                ) && action.reason.as_ref().is_none_or(String::is_empty)
                {
                    return Err(Error::Invalid(
                        "skipped/protected/error action needs a reason".into(),
                    ));
                }
                if actions.dry_run
                    && (action.commit.is_some()
                        || matches!(
                            action.status,
                            ActionStatus::Created
                                | ActionStatus::Updated
                                | ActionStatus::Rebased
                                | ActionStatus::Deleted
                        ))
                {
                    return Err(Error::Invalid(
                        "dry run cannot contain applied actions".into(),
                    ));
                }
                if matches!(
                    action.status,
                    ActionStatus::Created | ActionStatus::Updated | ActionStatus::Rebased
                ) && action.commit.is_none()
                {
                    return Err(Error::Invalid("applied action needs a commit".into()));
                }
                let mut paths = BTreeSet::new();
                if action
                    .files
                    .iter()
                    .any(|f| f.path.is_empty() || f.blob_sha.is_empty() || !paths.insert(&f.path))
                {
                    return Err(Error::Invalid("invalid or duplicate action file".into()));
                }
            }
        }
        let mut repos = BTreeSet::new();
        for repo in &self.repositories {
            if repo.repository.is_empty() || !repos.insert(&repo.repository) {
                return Err(Error::Invalid("empty or duplicate repository".into()));
            }
            if repo.status != Status::Complete && repo.reason.as_ref().is_none_or(String::is_empty)
            {
                return Err(Error::Invalid(
                    "skipped/error repository needs a reason".into(),
                ));
            }
            let mut files = BTreeSet::new();
            for file in &repo.package_files {
                if file.path.is_empty() || !files.insert(&file.path) {
                    return Err(Error::Invalid(
                        "empty or duplicate package-file path".into(),
                    ));
                }
                let mut dependencies = BTreeSet::new();
                for dep in &file.dependencies {
                    if dep.manager.is_empty()
                        || dep.name.is_empty()
                        || dep.package.is_empty()
                        || dep.occurrence.as_ref().is_some_and(String::is_empty)
                        || !dependencies.insert((&dep.manager, &dep.name, &dep.occurrence))
                    {
                        return Err(Error::Invalid(
                            "empty dependency identity or ambiguous duplicate manager/name".into(),
                        ));
                    }
                    let mut updates = BTreeSet::new();
                    for update in &dep.updates {
                        if update.update_type.is_empty()
                            || (update.new_value.is_none()
                                && update.new_version.is_none()
                                && update.new_digest.is_none())
                            || !updates.insert(update)
                        {
                            return Err(Error::Invalid(
                                "empty or duplicate proposed update".into(),
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// Merge identical occurrences and retain different current values with IDs.
/// IDs follow sorted identities, independent of source order; singleton names
/// keep their original identity without an occurrence field.
pub fn normalize_dependencies(dependencies: Vec<Dependency>) -> Vec<Dependency> {
    let mut groups = std::collections::BTreeMap::<(String, String), Vec<Dependency>>::new();
    for mut dep in dependencies {
        let group = groups
            .entry((dep.manager.clone(), dep.name.clone()))
            .or_default();
        if let Some(existing) = group
            .iter_mut()
            .find(|d| d.package == dep.package && d.current == dep.current)
        {
            existing.updates.append(&mut dep.updates);
            if let (Some(a), Some(mut b)) = (&mut existing.metadata, dep.metadata) {
                a.locations.append(&mut b.locations);
                a.kinds.append(&mut b.kinds);
                a.kinds.sort();
                a.kinds.dedup();
                a.held_back.append(&mut b.held_back);
                a.held_back.sort();
                a.held_back.dedup();
                a.stale.append(&mut b.stale);
                a.tag_digests.append(&mut b.tag_digests);
                if let (Some(a), Some(b)) = (&mut a.lookup, b.lookup) {
                    a.disabled_updates
                        .retain(|kind| b.disabled_updates.contains(kind));
                    a.rejected.extend(b.rejected);
                    a.rejected
                        .sort_by(|a, b| (&a.version, &a.reason).cmp(&(&b.version, &b.reason)));
                    a.rejected.dedup();
                }
                if a.resolution != b.resolution {
                    a.resolution = None;
                }
            }
        } else {
            group.push(dep);
        }
    }
    groups
        .into_values()
        .flat_map(|mut group| {
            group.sort_by(|a, b| (&a.package, &a.current).cmp(&(&b.package, &b.current)));
            let multiple = group.len() > 1;
            for (i, dep) in group.iter_mut().enumerate() {
                dep.occurrence = multiple.then(|| (i + 1).to_string());
                dep.updates.sort();
                dep.updates.dedup();
            }
            group
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_future_schema_before_interpreting_payload() {
        assert!(matches!(
            Report::from_json(br#"{"schemaVersion":3,"future":true}"#),
            Err(Error::Version(3))
        ));
        assert!(
            Report::from_json(br#"{"schemaVersion":2,"repositories":[],"typo":true}"#).is_err()
        );
        assert!(matches!(
            Report::from_json(br#"{"schemaVersion":1,"repositories":[]}"#),
            Err(Error::Version(1))
        ));
    }

    #[test]
    fn action_versions_and_dry_run_states_are_validated() {
        let mut value = serde_json::json!({
            "schemaVersion": 2,
            "repositories": [{"repository":"demo/app","status":"complete","reason":null,"packageFiles":[]}],
            "actions": {"schemaVersion":1,"dryRun":true,"branches":[{
                "repository":"demo/app","branch":"rustovate/example","baseCommit":"base",
                "status":"planned","reason":null,"message":"update example","previousCommit":null,
                "commit":null,"verified":null,"lockfilePending":false,"files":[]
            }]}
        });
        let parse = |v: &serde_json::Value| Report::from_json(&serde_json::to_vec(v).unwrap());
        parse(&value).unwrap();
        value["actions"]["schemaVersion"] = serde_json::json!(2);
        assert!(parse(&value).is_err());
        value["actions"]["schemaVersion"] = serde_json::json!(1);
        value["actions"]["branches"][0]["commit"] = serde_json::json!("unexpected-write");
        assert!(parse(&value).is_err());
        value["actions"]["branches"][0]["commit"] = serde_json::Value::Null;
        value["actions"]["branches"][0]["status"] = serde_json::json!("protected");
        assert!(parse(&value).is_err());
        value["actions"]["branches"][0]["reason"] = serde_json::json!("human commit");
        parse(&value).unwrap();
    }
}
