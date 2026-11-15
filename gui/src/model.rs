//! The report flattened into the rows the window shows, with no UI types:
//! updates, held-back releases, dependencies, templates and warnings. The
//! same flattening as the TUI's, so the two show the same lists.
use patchula_report::Report;
use patchula_words::{DependencyKind, Manager, ResolutionKind, Severity, Update as UpdateKind};
use std::collections::{BTreeMap, BTreeSet};

/// An advisory an update leaves behind.
#[derive(Clone, Debug, PartialEq)]
pub struct Fix {
    pub id: String,
    pub cves: Vec<String>,
    pub severity: Option<Severity>,
    pub summary: Option<String>,
    pub url: Option<String>,
}

/// One proposed update.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    /// The row's id in this report: its position, so it is unique and holds
    /// no characters a key path reads.
    pub id: String,
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
    pub fixes: Vec<Fix>,
    pub group: Option<String>,
    /// A package only its lockfile names, which only its package manager can update.
    pub transitive: bool,
    /// The current version came from a lockfile, which an edit here leaves stale.
    pub locked: bool,
}

impl Update {
    /// What the update moves to, as the CLI writes it.
    pub fn target(&self) -> String {
        match (&self.value, &self.version, &self.digest) {
            (Some(value), _, Some(digest)) => format!("{value}@{}", short(digest)),
            (Some(value), _, None) => value.clone(),
            (None, Some(version), _) => version.clone(),
            (None, None, Some(digest)) => short(digest),
            (None, None, None) => self.current.clone(),
        }
    }

    /// What names this update across two lookups of the same tree, so a
    /// selection survives a refresh.
    pub fn identity(&self) -> (String, String, Manager, String, UpdateKind, String) {
        (
            self.repository.clone(),
            self.file.clone(),
            self.manager,
            self.name.clone(),
            self.kind,
            self.target(),
        )
    }

    /// The worst severity among the advisories this update fixes.
    pub fn severity(&self) -> Option<Severity> {
        self.fixes.iter().filter_map(|fix| fix.severity).min()
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

/// A release still inside the release-age wait.
#[derive(Clone, Debug, PartialEq)]
pub struct Held {
    pub id: String,
    pub repository: String,
    pub file: String,
    pub manager: Manager,
    pub name: String,
    pub current: String,
    pub version: String,
    /// How many releases wait behind this one.
    pub count: usize,
    pub kind: UpdateKind,
    pub published_at: String,
    pub eligible_at: String,
}

/// A dependency the Count found.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub id: String,
    pub repository: String,
    pub file: String,
    pub manager: Manager,
    pub name: String,
    pub package: String,
    pub current: String,
    pub resolved: Option<String>,
    pub updates: usize,
    pub held: usize,
    pub advisories: usize,
}

/// A template update the Count reports but never edits.
#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    pub repository: String,
    pub path: String,
    pub name: String,
    pub current: String,
    pub available: String,
    pub compare_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// A lookup that failed, so updates may be missing.
    Failure,
    /// Something the Count wants read.
    Warning,
    /// A newer release he saw but did not propose.
    Notice,
}

/// One entry on the Warnings page.
#[derive(Clone, Debug, PartialEq)]
pub struct Warning {
    pub repository: String,
    pub tone: Tone,
    pub title: String,
    pub detail: String,
    /// A short word for the entry's badge, such as the failure's kind.
    pub tag: String,
}

/// The numbers on the Stats page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    /// Updates of each kind, in the Count's ranked order.
    pub kinds: Vec<(UpdateKind, usize)>,
    /// Each manager's dependencies and updates, most dependencies first.
    pub managers: Vec<(Manager, usize, usize)>,
    pub network: Option<Network>,
    /// The time each phase took, in microseconds, longest first.
    pub phases: Vec<(String, u64)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Network {
    pub requests: u64,
    pub not_modified: u64,
    pub cache_hits: u64,
    pub lookups: u64,
}

/// Everything one report holds, ready to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Data {
    pub repositories: Vec<String>,
    pub updates: Vec<Update>,
    pub held: Vec<Held>,
    pub found: Vec<Found>,
    pub templates: Vec<Template>,
    pub warnings: Vec<Warning>,
    pub stats: Stats,
}

/// The declaration as the CLI shows it: the declared value, or the resolved
/// version when the declaration has none.
fn current(dep: &patchula_report::Dependency) -> String {
    dep.current
        .value
        .clone()
        .or_else(|| dep.current.version.clone())
        .unwrap_or_default()
}

impl Data {
    pub fn new(report: &Report) -> Self {
        let mut data = Data::default();
        let mut managers = BTreeMap::<Manager, (usize, usize)>::new();
        for repo in &report.repositories {
            let name = &repo.repository;
            data.repositories.push(name.clone());
            let mut warn = |tone, tag: &str, title: String, detail: String| {
                data.warnings.push(Warning {
                    repository: name.clone(),
                    tone,
                    title,
                    detail,
                    tag: tag.to_owned(),
                });
            };
            for warning in &repo.warnings {
                warn(Tone::Warning, "warning", warning.clone(), String::new());
            }
            for notice in &repo.notices {
                if notice.is_template() {
                    data.templates.push(Template {
                        repository: name.clone(),
                        path: notice.path.clone(),
                        name: notice.name.clone(),
                        current: notice.current.clone(),
                        available: notice.available.clone(),
                        compare_url: notice.compare_url.clone(),
                    });
                } else {
                    warn(
                        Tone::Notice,
                        "not proposed",
                        format!("{} {} → {}", notice.name, notice.current, notice.available),
                        format!(
                            "{} ({})",
                            notice.reason.as_deref().unwrap_or_default(),
                            notice.path
                        ),
                    );
                }
            }
            for failure in &repo.lookup_failures {
                warn(
                    Tone::Failure,
                    failure.kind.as_str(),
                    format!("{} in {}", failure.name, failure.path),
                    format!(
                        "{} {}: {}",
                        failure.datasource, failure.package, failure.message
                    ),
                );
            }
            if let Some(reason) = &repo.reason {
                warn(
                    Tone::Warning,
                    &format!("{:?}", repo.status).to_lowercase(),
                    reason.clone(),
                    String::new(),
                );
            }
            for file in &repo.package_files {
                for dep in &file.dependencies {
                    let metadata = dep.metadata.as_ref();
                    let advisories = metadata
                        .map(|m| m.advisories.as_slice())
                        .unwrap_or_default();
                    let held = metadata.map(|m| m.held_back.as_slice()).unwrap_or_default();
                    let current = current(dep);
                    let entry = managers.entry(dep.manager).or_default();
                    entry.0 += 1;
                    entry.1 += dep.updates.len();
                    data.found.push(Found {
                        id: format!("d{}", data.found.len()),
                        repository: name.clone(),
                        file: file.path.clone(),
                        manager: dep.manager,
                        name: dep.name.clone(),
                        package: dep.package.clone(),
                        current: current.clone(),
                        resolved: dep.current.version.clone(),
                        updates: dep.updates.len(),
                        held: held.len(),
                        advisories: advisories.len(),
                    });
                    for hold in held {
                        data.held.push(Held {
                            id: format!("h{}", data.held.len()),
                            repository: name.clone(),
                            file: file.path.clone(),
                            manager: dep.manager,
                            name: dep.name.clone(),
                            current: current.clone(),
                            version: hold.version.clone(),
                            count: hold.count,
                            kind: hold.update_type,
                            published_at: hold.published_at.clone(),
                            eligible_at: hold.eligible_at.clone(),
                        });
                    }
                    for update in &dep.updates {
                        let fixes = update
                            .fixes
                            .iter()
                            .map(|id| {
                                let advisory = advisories.iter().find(|a| &a.id == id);
                                Fix {
                                    id: id.clone(),
                                    cves: advisory
                                        .into_iter()
                                        .flat_map(|a| &a.aliases)
                                        .filter(|alias| alias.starts_with("CVE-"))
                                        .cloned()
                                        .collect(),
                                    severity: advisory.and_then(|a| a.severity),
                                    summary: advisory.and_then(|a| a.summary.clone()),
                                    url: advisory.map(|a| a.url.clone()),
                                }
                            })
                            .collect();
                        data.updates.push(Update {
                            id: format!("u{}", data.updates.len()),
                            repository: name.clone(),
                            file: file.path.clone(),
                            manager: dep.manager,
                            name: dep.name.clone(),
                            package: dep.package.clone(),
                            current: current.clone(),
                            value: update.new_value.clone(),
                            version: update.new_version.clone(),
                            digest: update.new_digest.clone(),
                            kind: update.update_type,
                            published_at: update
                                .release_age
                                .as_ref()
                                .map(|age| age.published_at.clone()),
                            fixes,
                            group: update.group.clone(),
                            transitive: metadata
                                .is_some_and(|m| m.kinds.contains(&DependencyKind::Transitive)),
                            locked: metadata
                                .and_then(|m| m.resolution.as_ref())
                                .is_some_and(|r| r.kind == ResolutionKind::Lockfile),
                        });
                    }
                }
            }
        }
        data.stats.kinds = UpdateKind::RANKED
            .iter()
            .map(|kind| {
                (
                    *kind,
                    data.updates.iter().filter(|u| u.kind == *kind).count(),
                )
            })
            .collect();
        data.stats.managers = managers
            .into_iter()
            .map(|(manager, (found, updates))| (manager, found, updates))
            .collect();
        data.stats
            .managers
            .sort_by_key(|(_, found, _)| std::cmp::Reverse(*found));
        if let Some(stats) = &report.stats {
            data.stats.network = Some(Network {
                requests: stats.requests,
                not_modified: stats.not_modified,
                cache_hits: stats.cache_hits,
                lookups: stats.lookups,
            });
            data.stats.phases = stats
                .phase_micros
                .iter()
                .map(|(phase, micros)| (phase.clone(), *micros))
                .collect();
            data.stats
                .phases
                .sort_by_key(|(_, micros)| std::cmp::Reverse(*micros));
        }
        data
    }

    /// The position of the update with this row id.
    pub fn position(&self, id: &str) -> Option<usize> {
        let at: usize = id.strip_prefix('u')?.parse().ok()?;
        (at < self.updates.len()).then_some(at)
    }
}

/// Whether every word of `query` is in one of `fields`, ignoring case.
pub fn found(query: &str, fields: &[&str]) -> bool {
    query.split_whitespace().all(|word| {
        let word = word.to_lowercase();
        fields
            .iter()
            .any(|field| field.to_lowercase().contains(&word))
    })
}

/// One `patchula apply`: the filters that pick its updates. The CLI picks by
/// manager, name and type, so that is all one call can say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub managers: Vec<Manager>,
    pub kinds: Vec<UpdateKind>,
    pub names: Vec<String>,
}

impl Call {
    /// Whether `patchula apply` with these filters takes `update`: the
    /// manager and type must be listed, and a name must equal the
    /// dependency's name or its package.
    pub fn takes(&self, update: &Update) -> bool {
        self.managers.contains(&update.manager)
            && self.kinds.contains(&update.kind)
            && self
                .names
                .iter()
                .any(|name| *name == update.name || *name == update.package)
    }

    /// The filter flags. A name goes as an anchored regex, because a bare
    /// `--dependency` is a glob and a name may hold a glob's characters.
    pub fn flags(&self) -> Vec<String> {
        let mut flags = Vec::new();
        for manager in &self.managers {
            flags.extend(["--manager".to_owned(), manager.to_string()]);
        }
        for kind in &self.kinds {
            flags.extend(["--type".to_owned(), kind.to_string()]);
        }
        for name in &self.names {
            flags.extend([
                "--dependency".to_owned(),
                format!("regex:^{}$", escape(name)),
            ]);
        }
        flags
    }
}

/// `text` with every character a regex reads as syntax escaped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The fewest `patchula apply` calls that write the picked updates: one call
/// when its filters take exactly the picks, else one per manager and type.
/// A call can still take an update nobody picked, when two files declare the
/// same dependency; [`extras`] names those.
pub fn calls(updates: &[Update], picked: &BTreeSet<usize>) -> Vec<Call> {
    let picks: Vec<&Update> = picked.iter().filter_map(|i| updates.get(*i)).collect();
    if picks.is_empty() {
        return Vec::new();
    }
    let call = |picks: &[&Update]| {
        let mut call = Call {
            managers: Vec::new(),
            kinds: Vec::new(),
            names: Vec::new(),
        };
        for update in picks {
            if !call.managers.contains(&update.manager) {
                call.managers.push(update.manager);
            }
            if !call.kinds.contains(&update.kind) {
                call.kinds.push(update.kind);
            }
            if !call.names.contains(&update.name) {
                call.names.push(update.name.clone());
            }
        }
        call
    };
    let whole = call(&picks);
    if extras(updates, picked, std::slice::from_ref(&whole)).is_empty() {
        return vec![whole];
    }
    let mut groups = BTreeMap::<(Manager, u8), Vec<&Update>>::new();
    for update in picks {
        groups
            .entry((update.manager, update.kind.rank()))
            .or_default()
            .push(update);
    }
    groups.values().map(|group| call(group)).collect()
}

/// The updates `calls` would write that are not among the picks.
pub fn extras(updates: &[Update], picked: &BTreeSet<usize>, calls: &[Call]) -> Vec<usize> {
    updates
        .iter()
        .enumerate()
        .filter(|(i, update)| !picked.contains(i) && calls.iter().any(|call| call.takes(update)))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(manager: Manager, file: &str, name: &str, kind: UpdateKind) -> Update {
        Update {
            id: String::new(),
            repository: ".".into(),
            file: file.into(),
            manager,
            name: name.into(),
            package: name.into(),
            current: "1.0.0".into(),
            value: None,
            version: Some("2.0.0".into()),
            digest: None,
            kind,
            published_at: None,
            fixes: Vec::new(),
            group: None,
            transitive: false,
            locked: false,
        }
    }

    fn picks(indexes: &[usize]) -> BTreeSet<usize> {
        indexes.iter().copied().collect()
    }

    #[test]
    fn one_call_when_its_filters_take_exactly_the_picks() {
        let updates = [
            update(Manager::Npm, "package.json", "react", UpdateKind::Major),
            update(Manager::Npm, "package.json", "vite", UpdateKind::Major),
            update(Manager::Cargo, "Cargo.toml", "serde", UpdateKind::Patch),
        ];
        let picked = picks(&[0, 1]);
        let calls = calls(&updates, &picked);
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].flags(),
            [
                "--manager",
                "npm",
                "--type",
                "major",
                "--dependency",
                "regex:^react$",
                "--dependency",
                "regex:^vite$"
            ]
        );
        assert!(extras(&updates, &picked, &calls).is_empty());
    }

    #[test]
    fn a_cross_product_that_takes_more_splits_by_manager_and_type() {
        // One call for both picks would also take react's minor and vite's major.
        let updates = [
            update(Manager::Npm, "package.json", "react", UpdateKind::Major),
            update(Manager::Npm, "package.json", "react", UpdateKind::Minor),
            update(Manager::Npm, "package.json", "vite", UpdateKind::Major),
            update(Manager::Npm, "package.json", "vite", UpdateKind::Minor),
        ];
        let picked = picks(&[0, 3]);
        let calls = calls(&updates, &picked);
        assert_eq!(calls.len(), 2);
        assert!(extras(&updates, &picked, &calls).is_empty());
    }

    #[test]
    fn the_same_dependency_in_two_files_cannot_be_told_apart() {
        let updates = [
            update(Manager::Npm, "a/package.json", "react", UpdateKind::Major),
            update(Manager::Npm, "b/package.json", "react", UpdateKind::Major),
        ];
        let picked = picks(&[0]);
        let calls = calls(&updates, &picked);
        assert_eq!(extras(&updates, &picked, &calls), [1]);
    }

    #[test]
    fn a_name_also_matches_another_dependencys_package() {
        let mut alias = update(Manager::Npm, "package.json", "my-react", UpdateKind::Major);
        alias.package = "react".into();
        let updates = [
            update(Manager::Npm, "package.json", "react", UpdateKind::Major),
            alias,
        ];
        let picked = picks(&[0]);
        assert_eq!(extras(&updates, &picked, &calls(&updates, &picked)), [1]);
    }

    #[test]
    fn names_are_escaped_into_exact_regexes() {
        assert_eq!(escape("@types/node"), "@types/node");
        assert_eq!(escape("actions/setup-node"), r"actions/setup\-node");
        assert_eq!(escape("lodash.merge"), r"lodash\.merge");
        assert_eq!(escape("c++"), r"c\+\+");
    }

    #[test]
    fn search_needs_every_word_somewhere() {
        assert!(found("", &["react"]));
        assert!(found("REACT json", &["react", "web/package.json"]));
        assert!(!found("react yaml", &["react", "web/package.json"]));
    }

    #[test]
    fn the_demo_report_flattens_like_the_tui() {
        let data = Data::new(&crate::demo::report());
        assert_eq!(data.repositories, ["local/castle-web"]);
        assert!(data.updates.len() > 12, "{}", data.updates.len());
        assert!(!data.held.is_empty() && !data.templates.is_empty());
        assert!(data.warnings.iter().any(|w| w.tone == Tone::Failure));
        assert_eq!(
            data.stats.kinds.iter().map(|(_, n)| n).sum::<usize>(),
            data.updates.len()
        );
        // Row ids are positions.
        for (i, update) in data.updates.iter().enumerate() {
            assert_eq!(data.position(&update.id), Some(i));
        }
        assert!(data.updates.iter().any(|u| !u.fixes.is_empty()));
    }
}
