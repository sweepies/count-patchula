//! Which Python package index each package comes from: pinned in the
//! manifest, recorded in a lockfile, or the only index a project names. A
//! package that could come from several is ambiguous: its lookup fails rather
//! than guess, since the public package of its name may be someone else's.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) mod settings;

/// What an index setting names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Named {
    /// PyPI itself.
    Public,
    Url(String),
    /// A value the Count cannot read from, such as `${VAR}` or plain http,
    /// kept as written so its packages' lookups fail.
    Unusable(String),
}

impl Named {
    pub(crate) fn parse(value: &str) -> Self {
        let raw = value.trim().trim_matches(['"', '\'']);
        if public(raw) {
            return Self::Public;
        }
        match crate::registry_url(raw) {
            Some(url) => Self::Url(url),
            None => Self::Unusable(crate::redacted(raw)),
        }
    }
    /// The registry a dependency records: none for PyPI.
    pub(crate) fn recorded(&self) -> Option<String> {
        match self {
            Self::Public => None,
            Self::Url(value) | Self::Unusable(value) => Some(value.clone()),
        }
    }
    fn label(&self) -> String {
        self.recorded()
            .unwrap_or_else(|| "https://pypi.org/simple".into())
    }
}

/// Whether `url` is PyPI's own index, over https or plain http.
fn public(url: &str) -> bool {
    crate::web_host(url).is_some_and(|host| {
        matches!(
            host.as_str(),
            "pypi.org" | "pypi.python.org" | "files.pythonhosted.org"
        )
    })
}

/// Where a package comes from, as a pin or a lockfile records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Index(Named),
    /// Any of several indexes, which the Count does not choose between.
    Ambiguous(Vec<Named>),
    /// Not an index at all: git, a path, a URL or a workspace member. The
    /// Count leaves such a package alone, since its name on PyPI may be
    /// someone else's.
    Other,
}

impl Origin {
    /// One origin for several that should agree: the same, or ambiguous.
    pub(crate) fn of(origins: impl IntoIterator<Item = Origin>) -> Option<Self> {
        let mut indexes: Vec<Named> = Vec::new();
        for origin in origins {
            match origin {
                Origin::Other => return Some(Origin::Other),
                Origin::Index(named) => indexes.push(named),
                Origin::Ambiguous(named) => indexes.extend(named),
            }
        }
        indexes.dedup();
        let mut unique: Vec<Named> = Vec::new();
        for named in indexes {
            if !unique.contains(&named) {
                unique.push(named);
            }
        }
        match unique.len() {
            0 => None,
            1 => unique.pop().map(Origin::Index),
            _ => Some(Origin::Ambiguous(unique)),
        }
    }
}

/// Where a project's packages come from. The tools' own lockfiles decide:
/// a package one records comes from what it records. A package none records
/// is PyPI's only in a project that configures no index; anywhere else the
/// Count does not guess which index the tool would pick.
#[derive(Debug, Default)]
pub(crate) struct Indexes {
    /// The indexes and other sources the project configures; empty when it
    /// configures none.
    pub configured: Vec<Named>,
    /// What each lockfile that applies records, by normalised name.
    pub locks: Vec<BTreeMap<String, Origin>>,
    /// Settings that could move any package, recorded or not.
    pub uncertain: Vec<Named>,
    /// Packages the manifest itself takes from git, a path, a URL or a
    /// workspace member, by normalised name: never looked up.
    pub outside: BTreeSet<String>,
}

impl Indexes {
    fn origin(&self, package: &str) -> Origin {
        if self.outside.contains(package) {
            return Origin::Other;
        }
        if let Some(origin) = Origin::of(self.uncertain.iter().cloned().map(Origin::Index)) {
            return origin;
        }
        let recorded = self
            .locks
            .iter()
            .filter_map(|lock| lock.get(package).cloned());
        if let Some(origin) = Origin::of(recorded) {
            return origin;
        }
        let candidates = std::iter::once(Named::Public).chain(self.configured.iter().cloned());
        match Origin::of(candidates.map(Origin::Index)) {
            Some(origin) => origin,
            None => Origin::Index(Named::Public),
        }
    }
    /// Give each PyPI dependency in `deps` its registry, and drop those that
    /// do not come from an index, returning them.
    pub(crate) fn assign(&self, deps: &mut Vec<Dependency>) -> Vec<Dependency> {
        let mut dropped = Vec::new();
        deps.retain_mut(|dep| {
            if dep.source != Source::Pypi {
                return true;
            }
            match self.origin(&dep.package) {
                Origin::Index(named) => dep.registry = named.recorded(),
                Origin::Ambiguous(named) => {
                    let labels: Vec<_> = named.iter().map(Named::label).collect();
                    dep.registry = Some(PackageId::ambiguous_registry(&labels));
                }
                Origin::Other => {
                    dropped.push(dep.clone());
                    return false;
                }
            }
            true
        });
        dropped
    }
}

/// `text`, which a repository wrote, when it is plainly a name; a stand-in
/// otherwise, so nothing a repository wrote reaches a report unchecked.
pub(crate) fn shown(text: &str) -> String {
    if !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        text.to_owned()
    } else {
        "(a name the Count does not repeat)".to_owned()
    }
}

/// PEP 503 normalised project name.
pub(crate) fn normalise(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut separator = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            separator = true;
            continue;
        }
        if separator && !out.is_empty() {
            out.push('-');
        }
        separator = false;
        out.push(c.to_ascii_lowercase());
    }
    out
}
