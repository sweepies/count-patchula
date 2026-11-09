//! The report flattened into selectable updates: the model `patchula
//! lookup --json`, `patchula run` output and `patchula apply` share,
//! with no terminal types.
use patchula_report::Report;
use patchula_words::{DependencyKind, Manager, ResolutionKind, Update as UpdateKind};

/// One proposed update, flattened from the report.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub repository: String,
    pub file: String,
    pub manager: Manager,
    pub name: String,
    pub package: String,
    pub current: String,
    pub value: Option<String>,
    pub version: Option<String>,
    pub digest: Option<String>,
    pub kind: UpdateKind,
    pub published_at: Option<String>,
    /// Advisories this update leaves behind, with their CVE aliases.
    pub fixes: Vec<String>,
    /// Byte offsets of every occurrence, used to apply the edit locally.
    pub starts: Vec<usize>,
    /// A package only its lockfile names, which only its package manager can update.
    pub transitive: bool,
    /// The current version came from a lockfile, which an edit here leaves stale.
    pub locked: bool,
    pub pick: Pick,
}
/// Where an update stands in this session: an applied update can no longer
/// be selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pick {
    #[default]
    Open,
    Selected,
    Applied,
}
impl Update {
    pub fn target(&self) -> String {
        match (&self.value, &self.version, &self.digest) {
            (Some(value), _, Some(digest)) => format!("{value}@{}", short(digest)),
            (Some(value), _, None) => value.clone(),
            (None, Some(version), _) => version.clone(),
            (None, None, Some(digest)) => short(digest),
            (None, None, None) => self.current.clone(),
        }
    }
}
fn short(digest: &str) -> String {
    digest
        .split(':')
        .next_back()
        .unwrap_or(digest)
        .chars()
        .take(12)
        .collect()
}

/// Every proposed update in the report, one per dependency and target.
pub fn updates(report: &Report) -> Vec<Update> {
    let mut updates = Vec::new();
    for repo in &report.repositories {
        for file in &repo.package_files {
            for dep in &file.dependencies {
                let current = dep
                    .current
                    .value
                    .clone()
                    .or_else(|| dep.current.version.clone())
                    .unwrap_or_default();
                let metadata = dep.metadata.as_ref();
                let starts: Vec<usize> = metadata
                    .map(|m| m.locations.iter().map(|l| l.start).collect())
                    .unwrap_or_default();
                for update in &dep.updates {
                    updates.push(Update {
                        repository: repo.repository.clone(),
                        file: file.path.clone(),
                        manager: dep.manager,
                        name: dep.name.clone(),
                        package: dep.package.clone(),
                        current: current.clone(),
                        value: update.new_value.clone(),
                        version: update.new_version.clone(),
                        digest: update.new_digest.clone(),
                        kind: update.update_type,
                        published_at: update.release_age.as_ref().map(|a| a.published_at.clone()),
                        fixes: update
                            .fixes
                            .iter()
                            .map(|id| {
                                let cves: Vec<_> = metadata
                                    .into_iter()
                                    .flat_map(|m| &m.advisories)
                                    .filter(|a| &a.id == id)
                                    .flat_map(|a| &a.aliases)
                                    .filter(|alias| alias.starts_with("CVE-"))
                                    .cloned()
                                    .collect();
                                if cves.is_empty() {
                                    id.clone()
                                } else {
                                    format!("{id} ({})", cves.join(", "))
                                }
                            })
                            .collect(),
                        starts: starts.clone(),
                        transitive: metadata
                            .is_some_and(|m| m.kinds.contains(&DependencyKind::Transitive)),
                        locked: metadata
                            .and_then(|m| m.resolution.as_ref())
                            .is_some_and(|r| r.kind == ResolutionKind::Lockfile),
                        pick: Pick::Open,
                    });
                }
            }
        }
    }
    updates
}
