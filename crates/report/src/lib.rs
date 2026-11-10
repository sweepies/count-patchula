//! Versioned caller interface for lookup/run batches, independent of log output.
#![forbid(unsafe_code)]

use patchula_words::{
    DependencyKind, LookupErrorKind, Manager, ReleaseChannel, ResolutionKind, Severity, Source,
    ToolingDetail, ToolingRule, Update as UpdateKind, VersionPolicy,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SCHEMA_VERSION: u32 = 6;
/// The `actions` object's own version: 2 gave branches and pull requests
/// their own statuses, with `stale` for branches.
pub const ACTIONS_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid report JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "unsupported report schemaVersion {0}; expected {expected}",
        expected = SCHEMA_VERSION
    )]
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
    pub status: PullRequestStatus,
    pub reason: Option<String>,
}

patchula_words::words!(
    /// What happened to a branch.
    BranchStatus {
    /// Would be written; dry runs only, or not yet reached.
    Planned => "planned",
    Created => "created",
    Updated => "updated",
    Rebased => "rebased",
    Unchanged => "unchanged",
    /// Its pull request was merged.
    Merged => "merged",
    /// Obsolete and deleted.
    Deleted => "deleted",
    /// Held; the reason says why.
    Skipped => "skipped",
    /// The default branch changed or moved since extraction, so nothing
    /// was written; a rerun retries it.
    Stale => "stale",
    /// Left alone because it holds work that is not the Count's.
    Protected => "protected",
    Error => "error",
});

patchula_words::words!(
    /// What happened to a pull request.
    PullRequestStatus {
    /// Would be opened or updated; dry runs only.
    Planned => "planned",
    Created => "created",
    Updated => "updated",
    Unchanged => "unchanged",
    Closed => "closed",
    Merged => "merged",
    /// Held; the reason says why.
    Skipped => "skipped",
    Protected => "protected",
    Error => "error",
});

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchAction {
    pub repository: String,
    pub branch: String,
    pub base_commit: String,
    pub status: BranchStatus,
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
    /// Declarations found but not read as versions, each with its reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Skipped>,
    /// Newer versions the Count reports but never proposes, such as a Copier
    /// template's tags: only regenerating the project applies them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<Notice>,
    /// Dependencies whose registry lookup failed, so their updates are unknown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lookup_failures: Vec<LookupFailure>,
    /// Updates that merge on their own without the CI, required checks or
    /// delivery to back them, one entry per rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tooling: Vec<ToolingWarning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolingWarning {
    /// `merge-without-ci`, `merge-without-required-checks` or
    /// `security-merge-without-delivery`.
    pub rule: ToolingRule,
    /// The case with its own wording: `reviews-only` when the default branch
    /// requires a review but no checks, `tagged-release` when only tags
    /// trigger delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ToolingDetail>,
    /// Branches of the updates it applies to.
    pub branches: Vec<String>,
    /// Why it is left off PRs, from `warnings.quiet`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silenced: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Skipped {
    pub manager: Manager,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub value: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupFailure {
    pub manager: Manager,
    pub path: String,
    pub name: String,
    pub package: String,
    /// The datasource that failed, such as npm, docker or github-tags.
    pub datasource: Source,
    pub kind: LookupErrorKind,
    pub message: String,
}

impl std::fmt::Display for LookupFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} in {} ({} {}, {}): {}",
            self.name,
            self.path,
            self.datasource,
            self.package,
            self.kind.as_str(),
            self.message
        )
    }
}

impl Repository {
    /// Whether anything in this repository's report is uncertain: a warning
    /// or a failed lookup.
    pub fn degraded(&self) -> bool {
        !self.warnings.is_empty() || !self.lookup_failures.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Notice {
    pub manager: Manager,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub name: String,
    pub package: String,
    pub current: String,
    pub available: String,
    /// The changes between the two versions, where the host has a compare page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare_url: Option<String>,
    /// Why the version is not proposed, when the Count looked it up and
    /// could not write it into the declaration. Without one, the notice is a
    /// template update that only `copier update` applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
impl Notice {
    /// One line in the Count's words, for summaries and the dashboard.
    pub fn text(&self) -> String {
        match &self.reason {
            Some(reason) => format!(
                "{} {} → {} available, not proposed: {reason}",
                self.name, self.current, self.available
            ),
            None => format!(
                "template {}: {} → {} available",
                self.name, self.current, self.available
            ),
        }
    }
    /// Whether `copier update` applies it.
    pub fn is_template(&self) -> bool {
        self.reason.is_none()
    }
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
    /// Canonical patchula manager identifier; unknown upstream names stay literal.
    pub manager: Manager,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrence: Option<String>,
    /// Registry identity, which may differ from the name in the manifest.
    pub package: String,
    /// The registry the package was looked up in, as an https URL, when it is
    /// not the source's public one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
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
    pub source: Source,
    pub kinds: Vec<DependencyKind>,
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
    /// Published advisories that affect the current version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<Advisory>,
    /// The current image digest's warning state, when its SBOM can't show
    /// what a rebuild fixes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageCheck>,
    /// Whether a request made to look it up carried an operator entry's
    /// credentials or a registry account's token, so that what was found
    /// may be private.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub credentialed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Advisory {
    /// OSV identifier, such as a GHSA or RUSTSEC ID.
    pub id: String,
    /// Other identifiers for the same advisory, CVE IDs among them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// low, moderate, high or critical, when the advisory database rates it.
    #[serde(
        default,
        deserialize_with = "patchula_words::severity",
        skip_serializing_if = "Option::is_none"
    )]
    pub severity: Option<Severity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub url: String,
    /// The lowest release above the current version that the advisory lists as fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_in: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupEvidence {
    pub evaluated_at: String,
    pub disabled_updates: Vec<UpdateKind>,
    pub rejected: Vec<RejectedVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogEvidence>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogEvidence {
    /// Client retrieval/validation time, distinct from release publication time.
    pub fetched_at: String,
    pub reused: bool,
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
    /// How the current value was resolved; see [`ResolutionKind`].
    pub kind: ResolutionKind,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeldBack {
    pub version: String,
    /// All blocked releases in this class, including the summary version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<String>,
    #[serde(default = "one")]
    pub count: usize,
    pub update_type: UpdateKind,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Update {
    /// The new declaration. `None` with a `new_version` is a lockfile-only
    /// update: the declaration already allows the version.
    pub new_value: Option<String>,
    pub new_version: Option<String>,
    pub new_digest: Option<String>,
    /// major, minor, patch, digest or pin.
    pub update_type: UpdateKind,
    pub group: Option<String>,
    /// Lookup metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<UpdatePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_age: Option<ReleaseAge>,
    /// IDs of the current version's advisories that the target no longer has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<String>,
    /// The comparison of a pinned image's old and new SBOMs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageCheck>,
}

patchula_words::words!(
    /// How far an image comparison got, from worst to best: a comparison takes
    /// the worst of its platforms. Declaration order is that order.
    #[derive(Default)]
    ImageState {
    #[default]
    Unreadable => "unreadable",
    PreviousUnavailable => "previous-unavailable",
    NoSbom => "no-sbom",
    SbomWithoutOs => "sbom-without-os",
    OsNotCovered => "os-not-covered",
    PartialSources => "partial-sources",
    Checked => "checked",
    NoOsPackages => "no-os-packages",
});
impl ImageState {
    /// Whether the state means the Count can't tell, so it is a warning.
    pub fn warns(self) -> bool {
        matches!(
            self,
            Self::PreviousUnavailable | Self::NoSbom | Self::SbomWithoutOs | Self::OsNotCovered
        )
    }
}

/// What the Count found comparing the OS packages in two image digests.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageCheck {
    pub state: ImageState,
    /// Platforms compared, where both digests have them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// The SBOM read for each digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sboms: Vec<ImageSbom>,
    /// The OS as the new SBOM names it, such as "debian 12".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    /// OS packages the new SBOM lists.
    #[serde(default)]
    pub packages: usize,
    /// Advisories the new digest no longer has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed: Vec<ImageAdvisory>,
    /// Advisories the new digest has and the old one did not, most severe
    /// first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub introduced: Vec<String>,
    /// OSV.dev's records for the most severe of `introduced`, at most five.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub introduced_details: Vec<Advisory>,
    /// What made the comparison fail, for unreadable, or the SBOM's
    /// generator, for sbom-without-os.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Why a warning is left off PRs, from `security.quietImages`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub silenced: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageSbom {
    pub digest: String,
    /// attestation, cosign-att or cosign-sbom.
    pub source: patchula_words::SbomSource,
    pub generator: String,
}

/// One advisory a rebuild fixes, with the source package that carried it.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageAdvisory {
    pub id: String,
    /// The source package as OSV names it, such as "openssl".
    pub package: String,
    pub old: String,
    /// None when the new image no longer has the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
    pub platforms: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versions: Option<VersionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ReleaseChannel>,
    pub wait_hours: u32,
    pub windows: Vec<Window>,
    pub merge: bool,
    pub labels: Vec<String>,
    pub commit_prefix: String,
    pub release_notes: bool,
    pub body_notes: Vec<String>,
}
pub use patchula_words::Window;

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
            if actions.schema_version != ACTIONS_SCHEMA_VERSION {
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
                if actions.dry_run
                    && (pr.number.is_some() || pr.status != PullRequestStatus::Planned)
                {
                    return Err(Error::Invalid(
                        "dry run cannot contain applied pull requests".into(),
                    ));
                }
                if matches!(
                    pr.status,
                    PullRequestStatus::Created
                        | PullRequestStatus::Updated
                        | PullRequestStatus::Unchanged
                        | PullRequestStatus::Closed
                        | PullRequestStatus::Merged
                ) && pr.number.is_none()
                {
                    return Err(Error::Invalid(
                        "applied pull request action needs a number".into(),
                    ));
                }
                if matches!(
                    pr.status,
                    PullRequestStatus::Protected
                        | PullRequestStatus::Error
                        | PullRequestStatus::Skipped
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
                    BranchStatus::Skipped
                        | BranchStatus::Stale
                        | BranchStatus::Protected
                        | BranchStatus::Error
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
                            BranchStatus::Created
                                | BranchStatus::Updated
                                | BranchStatus::Rebased
                                | BranchStatus::Merged
                                | BranchStatus::Deleted
                                | BranchStatus::Stale
                        ))
                {
                    return Err(Error::Invalid(
                        "dry run cannot contain applied actions".into(),
                    ));
                }
                if matches!(
                    action.status,
                    BranchStatus::Created | BranchStatus::Updated | BranchStatus::Rebased
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
                    if dep.name.is_empty()
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
                        if (update.new_value.is_none()
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
    let mut groups = std::collections::BTreeMap::<(&str, String), Vec<Dependency>>::new();
    for mut dep in dependencies {
        let group = groups
            .entry((dep.manager.as_str(), dep.name.clone()))
            .or_default();
        if let Some(existing) = group
            .iter_mut()
            .find(|d| d.package == dep.package && d.current == dep.current)
        {
            existing.updates.append(&mut dep.updates);
            // An entry without metadata takes the other's, flags included.
            if existing.metadata.is_none() {
                existing.metadata = dep.metadata.take();
            }
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
                a.advisories.append(&mut b.advisories);
                a.advisories.sort();
                a.advisories.dedup();
                // Either one found with credentials makes the merged one so.
                a.credentialed |= b.credentialed;
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
    fn failures_sort_by_the_text_of_their_words() {
        let failure = |manager, datasource| LookupFailure {
            manager,
            path: "p".into(),
            name: "n".into(),
            package: "x".into(),
            datasource,
            kind: LookupErrorKind::Network,
            message: "m".into(),
        };
        // Npm is declared before Bun and Cargo; the report lists by word.
        let mut failures = [
            failure(Manager::Npm, Source::Npm),
            failure(Manager::Cargo, Source::Crates),
            failure(Manager::Bun, Source::Npm),
            failure(Manager::Bun, Source::GithubTags),
        ];
        failures.sort();
        let order: Vec<_> = failures
            .iter()
            .map(|f| (f.manager.as_str(), f.datasource.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                ("bun", "github-tags"),
                ("bun", "npm"),
                ("cargo", "crates"),
                ("npm", "npm")
            ]
        );
    }
    /// Version 6 gave branches and pull requests their own statuses: a status
    /// only the other one has is not a report.
    #[test]
    fn branch_and_pull_request_statuses_are_their_own() {
        let report = |branch: &str, pr: &str| {
            format!(
                r#"{{"schemaVersion":6,"repositories":[{{"repository":"o/r","status":"complete","reason":null,"packageFiles":[]}}],"actions":{{"schemaVersion":2,"dryRun":false,"branches":[{{"repository":"o/r","branch":"b","baseCommit":"c","status":"{branch}","reason":"why","message":"m","previousCommit":null,"commit":null,"verified":null,"lockfilePending":false,"files":[]}}],"pullRequests":[{{"repository":"o/r","branch":"b","number":1,"url":null,"status":"{pr}","reason":"why"}}]}}}}"#
            )
        };
        assert!(Report::from_json(report("stale", "closed").as_bytes()).is_ok());
        assert!(Report::from_json(report("closed", "closed").as_bytes()).is_err());
        assert!(Report::from_json(report("stale", "stale").as_bytes()).is_err());
        assert!(Report::from_json(report("deleted", "rebased").as_bytes()).is_err());
    }

    #[test]
    fn reject_future_schema_before_interpreting_payload() {
        assert!(matches!(
            Report::from_json(br#"{"schemaVersion":7,"future":true}"#),
            Err(Error::Version(7))
        ));
        assert!(
            Report::from_json(br#"{"schemaVersion":6,"repositories":[],"typo":true}"#).is_err()
        );
        for old in [1, 2, 3, 4, 5] {
            let report = format!(r#"{{"schemaVersion":{old},"repositories":[]}}"#);
            assert!(matches!(
                Report::from_json(report.as_bytes()),
                Err(Error::Version(version)) if version == old
            ));
        }
    }

    #[test]
    fn the_version_error_names_the_supported_schema() {
        assert_eq!(
            Error::Version(7).to_string(),
            format!("unsupported report schemaVersion 7; expected {SCHEMA_VERSION}")
        );
    }

    /// Version 5 added a dependency's `registry`; a version 4 reader would reject
    /// the field, so a version 4 report no longer parses.
    #[test]
    fn a_registry_is_part_of_version_5() {
        let report = |version: u32| {
            format!(
                r#"{{"schemaVersion":{version},"repositories":[{{"repository":"demo/app","status":"complete","reason":null,"packageFiles":[{{"path":"package.json","dependencies":[{{"manager":"npm","name":"@acme/ui","package":"@acme/ui","registry":"https://npm.acme.dev","current":{{"value":"1.0.0","version":"1.0.0","digest":null}},"updates":[]}}]}}]}}]}}"#
            )
        };
        let parsed = Report::from_json(report(SCHEMA_VERSION).as_bytes()).unwrap();
        let dependency = &parsed.repositories[0].package_files[0].dependencies[0];
        assert_eq!(dependency.registry.as_deref(), Some("https://npm.acme.dev"));
        assert!(matches!(
            Report::from_json(report(4).as_bytes()),
            Err(Error::Version(4))
        ));
    }

    #[test]
    fn a_notice_reason_parses() {
        let report = r#"{"schemaVersion":6,"repositories":[{"repository":"demo/app","status":"complete","reason":null,"packageFiles":[],"notices":[{"manager":"composer","path":"composer.json","name":"acme/lib","package":"acme/lib","current":"^1 | ^2","available":"3.0.0","reason":"cannot be rewritten"}]}]}"#;
        let parsed = Report::from_json(report.as_bytes()).unwrap();
        let notice = &parsed.repositories[0].notices[0];
        assert_eq!(notice.reason.as_deref(), Some("cannot be rewritten"));
        assert!(!notice.is_template());
    }

    #[test]
    fn a_proposed_update_needs_a_target_and_no_duplicates() {
        let report = |updates: serde_json::Value| {
            serde_json::json!({
                "schemaVersion": SCHEMA_VERSION,
                "repositories": [{
                    "repository": "demo/app",
                    "status": "complete",
                    "reason": null,
                    "packageFiles": [{
                        "path": "package.json",
                        "dependencies": [{
                            "manager": "npm",
                            "name": "left-pad",
                            "package": "left-pad",
                            "current": {"value": "1.0.0", "version": "1.0.0", "digest": null},
                            "updates": updates
                        }]
                    }]
                }]
            })
        };
        let parse = |v: &serde_json::Value| Report::from_json(&serde_json::to_vec(v).unwrap());
        // An update with no new value, version or digest proposes nothing.
        let empty = report(serde_json::json!([{"updateType": "patch"}]));
        assert!(parse(&empty).is_err());
        // Two identical proposals are ambiguous.
        let update = serde_json::json!({
            "newValue": "1.0.1", "newVersion": "1.0.1", "newDigest": null,
            "updateType": "patch", "group": null
        });
        let duplicate = report(serde_json::json!([update.clone(), update]));
        assert!(parse(&duplicate).is_err());
        // A lockfile-only update carries just a version and is fine.
        let lockfile_only = report(serde_json::json!([{
            "newValue": null, "newVersion": "1.0.1", "newDigest": null,
            "updateType": "patch", "group": null
        }]));
        parse(&lockfile_only).unwrap();
    }

    #[test]
    fn action_versions_and_dry_run_states_are_validated() {
        let mut value = serde_json::json!({
            "schemaVersion": 6,
            "repositories": [{"repository":"demo/app","status":"complete","reason":null,"packageFiles":[]}],
            "actions": {"schemaVersion":2,"dryRun":true,"branches":[{
                "repository":"demo/app","branch":"patchula/example","baseCommit":"base",
                "status":"planned","reason":null,"message":"update example","previousCommit":null,
                "commit":null,"verified":null,"lockfilePending":false,"files":[]
            }]}
        });
        let parse = |v: &serde_json::Value| Report::from_json(&serde_json::to_vec(v).unwrap());
        parse(&value).unwrap();
        value["actions"]["schemaVersion"] = serde_json::json!(3);
        assert!(parse(&value).is_err());
        value["actions"]["schemaVersion"] = serde_json::json!(2);
        value["actions"]["branches"][0]["commit"] = serde_json::json!("unexpected-write");
        assert!(parse(&value).is_err());
        value["actions"]["branches"][0]["commit"] = serde_json::Value::Null;
        value["actions"]["branches"][0]["status"] = serde_json::json!("protected");
        assert!(parse(&value).is_err());
        value["actions"]["branches"][0]["reason"] = serde_json::json!("human commit");
        parse(&value).unwrap();
    }
}

// These records sorted on string fields before their words were typed, and
// reports, CLI output and the dashboard list them in that order. They keep
// sorting by the word's text, not by the enum's declaration order.
macro_rules! ordered_by {
    ($name:ident, |$v:ident| $key:expr) => {
        impl Ord for $name {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                let mine = {
                    let $v = self;
                    $key
                };
                let theirs = {
                    let $v = other;
                    $key
                };
                mine.cmp(&theirs)
            }
        }
        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
    };
}
ordered_by!(LookupFailure, |f| (
    f.manager.as_str(),
    &f.path,
    &f.name,
    &f.package,
    f.datasource.as_str(),
    f.kind,
    &f.message,
));
ordered_by!(HeldBack, |h| (
    &h.version,
    &h.versions,
    h.count,
    h.update_type.as_str(),
    &h.published_at,
    &h.eligible_at,
    &h.target_eligible_at,
));
ordered_by!(Update, |u| (
    (&u.new_value, &u.new_version, &u.new_digest),
    u.update_type.as_str(),
    (&u.group, &u.policy, &u.release_age, &u.fixes, &u.image),
));
ordered_by!(UpdatePolicy, |p| (
    (
        p.versions.map(VersionPolicy::as_str),
        p.channel.map(ReleaseChannel::as_str)
    ),
    (p.wait_hours, &p.windows, p.merge, &p.labels),
    (&p.commit_prefix, p.release_notes, &p.body_notes),
));
