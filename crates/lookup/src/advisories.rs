//! Which advisories a version is affected by, from OSV's affected ranges.
use patchula_config::split_package;
use patchula_datasource::{Advisory, AdvisoryQuery, Event};
use patchula_manager::Dependency;
use patchula_versioning::{Ecosystem, Parsed};
use patchula_words::Source;
use std::cmp::Ordering;

/// The OSV query for a dependency's current version, when OSV covers its
/// ecosystem and the version is known exactly: from a lockfile or an exact
/// declaration, never from the floor of a range.
pub fn advisory_query(dep: &Dependency) -> Option<AdvisoryQuery> {
    // OSV knows packages by name on their public registries.
    if !dep.id().is_public() {
        return None;
    }
    let scheme = Ecosystem::of(dep.scheme, dep.source);
    let (ecosystem, package) = match dep.source {
        Source::Npm if split_package(&dep.package).is_none() => ("npm", dep.package.clone()),
        Source::Crates => ("crates.io", dep.package.clone()),
        Source::Pypi => ("PyPI", dep.package.clone()),
        Source::Go => ("Go", dep.package.clone()),
        Source::Maven => ("Maven", dep.package.clone()),
        Source::Nuget => ("NuGet", dep.package.clone()),
        Source::Rubygems => ("RubyGems", dep.package.clone()),
        Source::Packagist => ("Packagist", dep.package.clone()),
        Source::GithubTags | Source::GithubReleases
            if dep.proposing().advisories == patchula_manager::Advisories::GithubAction =>
        {
            ("GitHub Actions", dep.package.clone())
        }
        Source::Mise if dep.proposing().advisories == patchula_manager::Advisories::GoStdlib => {
            ("Go", "stdlib".into())
        }
        _ => return None,
    };
    let version = match &dep.resolved {
        Some(version) => version.as_str(),
        None if scheme == Ecosystem::Npm => dep.current.as_str(),
        None => return None,
    };
    scheme.parse(version).ok()?;
    if scheme == Ecosystem::Loose && version.trim_start_matches('v').split('.').count() < 3 {
        return None;
    }
    let version = if matches!(ecosystem, "Go" | "GitHub Actions") {
        version.trim_start_matches('v')
    } else {
        version
    };
    Some(AdvisoryQuery {
        ecosystem: ecosystem.into(),
        package,
        version: version.into(),
    })
}

/// The osv.dev page for one advisory.
pub(crate) fn osv_url(id: &str) -> String {
    format!("https://osv.dev/vulnerability/{id}")
}

/// Report entries for the advisories that affect the queried version.
pub fn report_advisories(
    dep: &Dependency,
    query: &AdvisoryQuery,
    advisories: &[std::sync::Arc<Advisory>],
) -> Vec<patchula_report::Advisory> {
    let scheme = Ecosystem::of(dep.scheme, dep.source);
    let current = parse(scheme, &query.version);
    let mut entries: Vec<_> = advisories
        .iter()
        .map(|advisory| patchula_report::Advisory {
            id: advisory.id.clone(),
            aliases: advisory.aliases.clone(),
            severity: advisory.severity,
            summary: advisory.summary.clone(),
            url: osv_url(&advisory.id),
            fixed_in: current
                .as_ref()
                .and_then(|current| fixed_in(advisory, query, scheme, current)),
        })
        .collect();
    entries.sort();
    entries
}

/// IDs of the advisories that affect the current version but not `target`.
/// An advisory whose ranges cannot be evaluated is never counted as fixed.
pub fn fixes(
    dep: &Dependency,
    query: &AdvisoryQuery,
    advisories: &[std::sync::Arc<Advisory>],
    target: &str,
) -> Vec<String> {
    Fixes::new(dep, query, advisories).of(target)
}

/// A dependency's advisories with their affected versions and ranges parsed
/// and sorted once, for asking which of many candidate versions fix them.
pub(crate) struct Fixes {
    scheme: Ecosystem,
    advisories: Vec<(String, Affecting)>,
}
impl Fixes {
    pub(crate) fn new(
        dep: &Dependency,
        query: &AdvisoryQuery,
        advisories: &[std::sync::Arc<Advisory>],
    ) -> Self {
        let scheme = Ecosystem::of(dep.scheme, dep.source);
        Self {
            scheme,
            advisories: advisories
                .iter()
                .map(|advisory| (advisory.id.clone(), Affecting::new(advisory, query, scheme)))
                .collect(),
        }
    }
    /// IDs of the advisories that affect the current version but not
    /// `target`.
    pub(crate) fn of(&self, target: &str) -> Vec<String> {
        let Some(parsed) = parse(self.scheme, target) else {
            return Vec::new();
        };
        self.advisories
            .iter()
            .filter(|(_, affecting)| affecting.affects(self.scheme, &parsed) == Some(false))
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// One advisory's entries for the queried package, parsed.
struct Affecting(Vec<Entry>);
/// One `affected` entry, parsed.
struct Entry {
    /// The listed versions that parse.
    versions: Vec<Parsed>,
    /// Whether the entry lists any versions, parsed or not.
    lists: bool,
    /// Its SEMVER and ECOSYSTEM ranges' events in version order; `None` for
    /// a range with an event that does not parse, which leaves the advisory
    /// undecidable.
    ranges: Vec<Option<Vec<Bound>>>,
}
/// A range event at its version; `None` is `introduced: 0`.
type Bound = (Option<Parsed>, EventKind);
#[derive(Clone, Copy)]
enum EventKind {
    Introduced,
    Fixed,
    LastAffected,
}
impl Affecting {
    fn new(advisory: &Advisory, query: &AdvisoryQuery, scheme: Ecosystem) -> Self {
        Self(
            advisory
                .affected
                .iter()
                .filter(|a| same_package(a, query))
                .map(|affected| {
                    let versions = affected
                        .versions
                        .iter()
                        .filter_map(|v| parse(scheme, v))
                        .collect();
                    let ranges = affected
                        .ranges
                        .iter()
                        .filter(|r| matches!(r.kind.as_str(), "SEMVER" | "ECOSYSTEM"))
                        .map(|range| sorted_events(&range.events, scheme))
                        .collect();
                    Entry {
                        versions,
                        lists: !affected.versions.is_empty(),
                        ranges,
                    }
                })
                .collect(),
        )
    }
    /// Whether `version` falls in the advisory's affected ranges or
    /// versions; `None` when that cannot be decided.
    fn affects(&self, scheme: Ecosystem, version: &Parsed) -> Option<bool> {
        let mut decided = false;
        for Entry {
            versions,
            lists,
            ranges,
        } in &self.0
        {
            if versions
                .iter()
                .any(|v| scheme.order(v, version) == Ordering::Equal)
            {
                return Some(true);
            }
            if *lists {
                decided = true;
            }
            for range in ranges {
                match range
                    .as_deref()
                    .map(|events| in_range(events, scheme, version))
                {
                    Some(true) => return Some(true),
                    Some(false) => decided = true,
                    None => return None,
                }
            }
        }
        decided.then_some(false)
    }
}

/// Whether `version` falls in the advisory's affected ranges or versions for
/// the queried package; `None` when that cannot be decided.
pub fn affects(
    advisory: &Advisory,
    query: &AdvisoryQuery,
    scheme: Ecosystem,
    version: &str,
) -> Option<bool> {
    let parsed = parse(scheme, version)?;
    Affecting::new(advisory, query, scheme).affects(scheme, &parsed)
}

/// The lowest fixed version above `current` in the package's ranges.
pub(crate) fn fixed_in(
    advisory: &Advisory,
    query: &AdvisoryQuery,
    scheme: Ecosystem,
    current: &Parsed,
) -> Option<String> {
    advisory
        .affected
        .iter()
        .filter(|a| same_package(a, query))
        .flat_map(|a| &a.ranges)
        .flat_map(|r| &r.events)
        .filter_map(|event| match event {
            Event::Fixed(v) => Some((parse(scheme, v)?, v)),
            _ => None,
        })
        .filter(|(fixed, _)| scheme.order(fixed, current) == Ordering::Greater)
        .min_by(|(a, _), (b, _)| scheme.order(a, b))
        .map(|(_, v)| v.clone())
}

/// A range's events at their versions, in version order, `introduced: 0`
/// first and `limit` left out; `None` when an event's version does not parse.
fn sorted_events(events: &[Event], scheme: Ecosystem) -> Option<Vec<Bound>> {
    let mut sorted = Vec::with_capacity(events.len());
    for event in events {
        let bound = match event {
            Event::Introduced(v) if v == "0" => (None, EventKind::Introduced),
            Event::Introduced(v) => (Some(parse(scheme, v)?), EventKind::Introduced),
            Event::Fixed(v) => (Some(parse(scheme, v)?), EventKind::Fixed),
            Event::LastAffected(v) => (Some(parse(scheme, v)?), EventKind::LastAffected),
            Event::Limit(_) => continue,
        };
        sorted.push(bound);
    }
    sorted.sort_by(|(a, _), (b, _)| match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => scheme.order(a, b),
    });
    Some(sorted)
}

/// OSV's evaluation: walk the sorted events, entering the range at
/// `introduced` and leaving it at `fixed` or after `last_affected`.
fn in_range(events: &[Bound], scheme: Ecosystem, version: &Parsed) -> bool {
    let mut affected = false;
    for (bound, kind) in events {
        let order = bound
            .as_ref()
            .map_or(Ordering::Greater, |b| scheme.order(version, b));
        match kind {
            EventKind::Introduced if order != Ordering::Less => affected = true,
            EventKind::Fixed if order != Ordering::Less => affected = false,
            EventKind::LastAffected if order == Ordering::Greater => affected = false,
            _ => {}
        }
    }
    affected
}

fn same_package(affected: &patchula_datasource::Affected, query: &AdvisoryQuery) -> bool {
    let normalize = |name: &str| {
        if query.ecosystem == "PyPI" {
            patchula_words::pep503(name)
        } else {
            name.to_ascii_lowercase()
        }
    };
    affected.ecosystem == query.ecosystem
        && normalize(&affected.package) == normalize(&query.package)
}

/// OSV writes Go and GitHub Actions versions without the `v` prefix.
fn parse(scheme: Ecosystem, value: &str) -> Option<Parsed> {
    scheme
        .parse(value)
        .or_else(|_| scheme.parse(&format!("v{value}")))
        .ok()
}
