//! The report flattened into selectable updates: the model `patchula run`
//! output, `patchula apply` and the TUI share, with no terminal types.
//! `lookup --json` serializes the raw report instead.
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
    /// The line of every occurrence, in order, for showing where it is.
    pub lines: Vec<usize>,
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
    /// What the update moves to. A pinned digest that follows a new version
    /// (a GitHub Action pinned by commit, say) is left out: the version says
    /// it. The digest shows only when it is all that changes.
    pub fn target(&self) -> String {
        match (&self.value, &self.version, &self.digest) {
            (Some(value), _, Some(digest)) if *value == self.current => {
                format!("{value}@{}", short(digest))
            }
            (Some(value), _, _) => value.clone(),
            (None, Some(version), Some(digest)) if *version == self.current => {
                format!("{version}@{}", short(digest))
            }
            (None, Some(version), _) => version.clone(),
            (None, None, Some(digest)) => short(digest),
            (None, None, None) => self.current.clone(),
        }
    }
    /// The file and the first line the dependency appears on, with how many
    /// more occurrences the file holds: `ci.yml:96 (+6)`.
    pub fn place(&self) -> String {
        match self.lines.as_slice() {
            [] => self.file.clone(),
            [line] => format!("{}:{line}", self.file),
            [line, rest @ ..] => format!("{}:{line} (+{})", self.file, rest.len()),
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

/// The current declaration as the CLI shows it: the declared value, or the
/// resolved version when the declaration has none.
pub fn current(dep: &patchula_report::Dependency) -> String {
    dep.current
        .value
        .clone()
        .or_else(|| dep.current.version.clone())
        .unwrap_or_default()
}

/// Every proposed update in the report, one per dependency and target.
pub fn updates(report: &Report) -> Vec<Update> {
    let mut updates = Vec::new();
    for repo in &report.repositories {
        for file in &repo.package_files {
            for dep in &file.dependencies {
                let current = current(dep);
                let metadata = dep.metadata.as_ref();
                let starts: Vec<usize> = metadata
                    .map(|m| m.locations.iter().map(|l| l.start).collect())
                    .unwrap_or_default();
                let mut lines: Vec<usize> = metadata
                    .map(|m| m.locations.iter().map(|l| l.line).collect())
                    .unwrap_or_default();
                lines.sort_unstable();
                lines.dedup();
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
                        lines: lines.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn update(value: Option<&str>, version: Option<&str>, digest: Option<&str>) -> Update {
        Update {
            repository: "owner/repo".into(),
            file: "package.json".into(),
            manager: Manager::Npm,
            name: "dep".into(),
            package: "dep".into(),
            current: "1.0.0".into(),
            value: value.map(str::to_owned),
            version: version.map(str::to_owned),
            digest: digest.map(str::to_owned),
            kind: UpdateKind::Patch,
            published_at: None,
            fixes: Vec::new(),
            starts: Vec::new(),
            lines: Vec::new(),
            transitive: false,
            locked: false,
            pick: Pick::Open,
        }
    }

    #[test]
    fn target_prefers_the_new_value_then_version_then_digest() {
        assert_eq!(
            update(Some("2.0.0"), Some("2.0.0"), Some("sha256:abcdef")).target(),
            "2.0.0"
        );
        assert_eq!(
            update(Some("1.0.0"), None, Some("sha256:abcdef")).target(),
            "1.0.0@abcdef"
        );
        assert_eq!(update(Some("^2.0.0"), None, None).target(), "^2.0.0");
        assert_eq!(
            update(None, Some("2.0.0"), Some("sha256:abcdef")).target(),
            "2.0.0"
        );
        assert_eq!(
            update(None, None, Some("sha256:0123456789abcdef")).target(),
            "0123456789ab"
        );
        assert_eq!(update(None, None, None).target(), "1.0.0");
    }

    #[test]
    fn place_names_the_first_line_and_counts_the_rest() {
        let mut u = update(None, None, None);
        assert_eq!(u.place(), "package.json");
        u.lines = vec![3];
        assert_eq!(u.place(), "package.json:3");
        u.lines = vec![3, 9, 12];
        assert_eq!(u.place(), "package.json:3 (+2)");
    }

    #[test]
    fn fixes_name_the_cves_each_advisory_carries() {
        let report: Report = serde_json::from_value(serde_json::json!({
            "schemaVersion": 8,
            "repositories": [{
                "repository": "owner/repo",
                "status": "complete",
                "packageFiles": [{"path": "package.json", "dependencies": [{
                    "manager": "npm", "name": "dep", "package": "dep",
                    "current": {"value": "1.0.0"},
                    "updates": [{
                        "newValue": "1.0.1", "updateType": "patch",
                        "fixes": ["GHSA-aaaa-bbbb-cccc", "GHSA-dddd-eeee-ffff", "RUSTSEC-0000-0000"]
                    }],
                    "metadata": {"source": "npm", "kinds": ["production"], "locations": [],
                        "advisories": [
                            {"id": "GHSA-aaaa-bbbb-cccc",
                             "aliases": ["CVE-2024-1111", "CVE-2024-2222", "GHSA-other"],
                             "url": "https://osv.dev/GHSA-aaaa-bbbb-cccc"},
                            {"id": "GHSA-dddd-eeee-ffff", "aliases": [],
                             "url": "https://osv.dev/GHSA-dddd-eeee-ffff"},
                            {"id": "RUSTSEC-0000-0000", "aliases": ["CVE-2024-3333"],
                             "url": "https://osv.dev/RUSTSEC-0000-0000"}
                        ]}
                }]}]
            }]
        }))
        .unwrap();
        let found = updates(&report);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].fixes,
            [
                "GHSA-aaaa-bbbb-cccc (CVE-2024-1111, CVE-2024-2222)",
                "GHSA-dddd-eeee-ffff",
                "RUSTSEC-0000-0000 (CVE-2024-3333)",
            ]
        );
    }
}
