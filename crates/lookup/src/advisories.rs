//! Which advisories a version is affected by, from OSV's affected ranges.
use crate::scheme;
use patchula_config::{DependencyKind, Manager, Source};
use patchula_datasource::{Advisory, AdvisoryQuery, Event};
use patchula_manager::Dependency;
use patchula_versioning::{Ecosystem, Parsed, Versioning};
use std::cmp::Ordering;

/// The OSV query for a dependency's current version, when OSV covers its
/// ecosystem and the version is known exactly: from a lockfile or an exact
/// declaration, never from the floor of a range.
pub fn advisory_query(dep: &Dependency) -> Option<AdvisoryQuery> {
    let scheme = scheme(dep).ok()?;
    let (ecosystem, package) = match dep.source {
        Source::Npm if !dep.package.contains('|') => ("npm", dep.package.clone()),
        Source::Crates => ("crates.io", dep.package.clone()),
        Source::Pypi => ("PyPI", dep.package.clone()),
        Source::Go => ("Go", dep.package.clone()),
        Source::Maven => ("Maven", dep.package.clone()),
        Source::Nuget => ("NuGet", dep.package.clone()),
        Source::Rubygems => ("RubyGems", dep.package.clone()),
        Source::Packagist => ("Packagist", dep.package.clone()),
        Source::GithubTags | Source::GithubReleases
            if dep.manager == Manager::GithubActions
                && matches!(
                    dep.kind,
                    DependencyKind::Action | DependencyKind::ReusableWorkflow
                ) =>
        {
            ("GitHub Actions", dep.package.clone())
        }
        Source::Mise
            if dep.manager == Manager::Go
                && dep.kind == DependencyKind::Toolchain
                && dep.package == "go" =>
        {
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

/// Report entries for the advisories that affect the queried version.
pub fn report_advisories(
    dep: &Dependency,
    query: &AdvisoryQuery,
    advisories: &[std::sync::Arc<Advisory>],
) -> Vec<patchula_report::Advisory> {
    let scheme = scheme(dep).ok();
    let current = scheme.and_then(|s| parse(s, &query.version));
    let mut entries: Vec<_> = advisories
        .iter()
        .map(|advisory| patchula_report::Advisory {
            id: advisory.id.clone(),
            aliases: advisory.aliases.clone(),
            severity: advisory.severity.clone(),
            summary: advisory.summary.clone(),
            url: format!("https://osv.dev/vulnerability/{}", advisory.id),
            fixed_in: scheme
                .zip(current.as_ref())
                .and_then(|(scheme, current)| fixed_in(advisory, query, scheme, current)),
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
    let Ok(scheme) = scheme(dep) else {
        return Vec::new();
    };
    advisories
        .iter()
        .filter(|advisory| affects(advisory, query, scheme, target) == Some(false))
        .map(|advisory| advisory.id.clone())
        .collect()
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
    let mut decided = false;
    for affected in advisory.affected.iter().filter(|a| same_package(a, query)) {
        if affected
            .versions
            .iter()
            .filter_map(|v| parse(scheme, v))
            .any(|v| scheme.order(&v, &parsed) == Ordering::Equal)
        {
            return Some(true);
        }
        if !affected.versions.is_empty() {
            decided = true;
        }
        for range in affected
            .ranges
            .iter()
            .filter(|r| matches!(r.kind.as_str(), "SEMVER" | "ECOSYSTEM"))
        {
            match in_range(&range.events, scheme, &parsed) {
                Some(true) => return Some(true),
                Some(false) => decided = true,
                None => return None,
            }
        }
    }
    decided.then_some(false)
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

/// OSV's evaluation: walk the events in version order, entering the range at
/// `introduced` and leaving it at `fixed` or after `last_affected`.
fn in_range(events: &[Event], scheme: Ecosystem, version: &Parsed) -> Option<bool> {
    let mut sorted = Vec::with_capacity(events.len());
    for event in events {
        let bound = match event {
            Event::Introduced(v) if v == "0" => None,
            Event::Introduced(v) | Event::Fixed(v) | Event::LastAffected(v) => {
                Some(parse(scheme, v)?)
            }
            Event::Limit(_) => continue,
        };
        sorted.push((bound, event));
    }
    sorted.sort_by(|(a, _), (b, _)| match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => scheme.order(a, b),
    });
    let mut affected = false;
    for (bound, event) in sorted {
        let order = bound.map_or(Ordering::Greater, |b| scheme.order(version, &b));
        match event {
            Event::Introduced(_) if order != Ordering::Less => affected = true,
            Event::Fixed(_) if order != Ordering::Less => affected = false,
            Event::LastAffected(_) if order == Ordering::Greater => affected = false,
            _ => {}
        }
    }
    Some(affected)
}

fn same_package(affected: &patchula_datasource::Affected, query: &AdvisoryQuery) -> bool {
    let normalize = |name: &str| {
        let name = name.to_ascii_lowercase();
        if query.ecosystem == "PyPI" {
            name.split(['-', '_', '.'])
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("-")
        } else {
            name
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
