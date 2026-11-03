//! Where a package comes from, decided the same way in every ecosystem:
//!
//! 1. A registry the declaration itself names.
//! 2. Otherwise what the tool's own lockfile records.
//! 3. Otherwise the public registry, but only where the repository configures
//!    no other; anywhere else the lookup fails rather than guess, since the
//!    public package of the name may be someone else's.
//!
//! Settings the Count cannot read or check make every lookup fail, locked or
//! not. The Count never works out which registry a tool would pick from its
//! configuration: the tool's lockfile already says.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// An ecosystem's public registry: its packages' source, how to tell its
/// URLs, and how to name it in a report.
pub(crate) struct Public {
    pub source: Source,
    pub label: &'static str,
    pub is_public: fn(&str) -> bool,
}

/// What a registry setting or lockfile entry names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Named {
    /// The ecosystem's public registry.
    Public,
    Url(String),
    /// A value the Count cannot read from, such as `${VAR}` or plain http,
    /// kept as written (without secrets) so its packages' lookups fail.
    Unusable(String),
}

impl Named {
    pub(crate) fn parse(value: &str, public: &Public) -> Self {
        let raw = value.trim().trim_matches(['"', '\'']);
        if (public.is_public)(raw) {
            return Self::Public;
        }
        match crate::registry_url(raw) {
            Some(url) => Self::Url(url),
            None => Self::Unusable(crate::redacted(raw)),
        }
    }
    /// The registry a dependency records: none for the public one.
    pub(crate) fn recorded(&self) -> Option<String> {
        match self {
            Self::Public => None,
            Self::Url(value) | Self::Unusable(value) => Some(value.clone()),
        }
    }
    fn label(&self, public: &Public) -> String {
        self.recorded().unwrap_or_else(|| public.label.into())
    }
}

/// Where a package comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Index(Named),
    /// Any of several registries, which the Count does not choose between.
    Ambiguous(Vec<Named>),
    /// Not a registry at all: git, a path, a URL or a workspace member. The
    /// Count leaves such a package alone, since its public name may be
    /// someone else's.
    Other,
}

impl Origin {
    /// One origin for several that should agree: the same, or ambiguous.
    pub(crate) fn of(origins: impl IntoIterator<Item = Origin>) -> Option<Self> {
        let mut unique: Vec<Named> = Vec::new();
        for origin in origins {
            let named = match origin {
                Origin::Other => return Some(Origin::Other),
                Origin::Index(named) => vec![named],
                Origin::Ambiguous(named) => named,
            };
            for named in named {
                if !unique.contains(&named) {
                    unique.push(named);
                }
            }
        }
        match unique.len() {
            0 => None,
            1 => unique.pop().map(Origin::Index),
            _ => Some(Origin::Ambiguous(unique)),
        }
    }
}

/// What decides where one project's packages come from.
#[derive(Debug, Default)]
pub(crate) struct Origins {
    /// Registries the declarations name for their own packages, by package.
    pub declared: BTreeMap<String, Origin>,
    /// The registries and other sources the project configures besides the
    /// public one; empty when it configures none.
    pub configured: Vec<Named>,
    /// What each lockfile that applies records, by package.
    pub locks: Vec<BTreeMap<String, Origin>>,
    /// Settings that could move any package, recorded or not.
    pub uncertain: Vec<Named>,
    /// Packages the declarations take from git, a path, a URL or a
    /// workspace member: never looked up.
    pub outside: BTreeSet<String>,
}

impl Origins {
    fn origin(&self, package: &str) -> Origin {
        if self.outside.contains(package) {
            return Origin::Other;
        }
        if let Some(origin) = Origin::of(self.uncertain.iter().cloned().map(Origin::Index)) {
            return origin;
        }
        if let Some(origin) = self.declared.get(package) {
            return origin.clone();
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
    /// Give each dependency of `public`'s source in `deps` its registry,
    /// keyed by `package`, and drop those that do not come from a registry,
    /// returning them.
    pub(crate) fn assign(
        &self,
        deps: &mut Vec<Dependency>,
        public: &Public,
        package: impl Fn(&Dependency) -> String,
    ) -> Vec<Dependency> {
        let mut dropped = Vec::new();
        deps.retain_mut(|dep| {
            if dep.source != public.source {
                return true;
            }
            match self.origin(&package(dep)) {
                Origin::Index(named) => dep.registry = named.recorded(),
                Origin::Ambiguous(named) => {
                    let labels: Vec<_> = named.iter().map(|n| n.label(public)).collect();
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
