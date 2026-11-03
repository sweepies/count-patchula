//! The identity of a package as the Count looks it up.
use crate::Source;
use serde::{Deserialize, Serialize};

/// A package where the Count looks it up: its source, the registry it comes
/// from when that is not the source's public one, and its name there.
///
/// Everything that identifies a package (lookups, caches, keys, advisories,
/// release notes) takes this type, so none can leave the registry out. Two
/// packages with one name from different registries are different packages.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PackageId {
    pub source: Source,
    /// An https URL, or a value the Count cannot read from kept as written so
    /// that its lookup fails. None is the source's public registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
    pub name: String,
}

impl PackageId {
    pub fn new(source: Source, registry: Option<String>, name: impl Into<String>) -> Self {
        Self {
            source,
            registry,
            name: name.into(),
        }
    }
    /// A package on its source's public registry.
    pub fn public(source: Source, name: impl Into<String>) -> Self {
        Self::new(source, None, name)
    }
    /// Whether anything about this package may be read by its name alone from
    /// public services: advisories, release notes, the public registry. A
    /// package from its own registry is not the public package of its name,
    /// which may be someone else's.
    pub fn is_public(&self) -> bool {
        self.registry.is_none()
    }
    /// The source's word, as in configuration and reports.
    pub fn source_word(&self) -> String {
        serde_json::to_value(self.source)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }
}

/// How a package that could come from several registries records its registry:
/// its lookup fails rather than guess, since the public one may be someone
/// else's.
const AMBIGUOUS: &str = "ambiguous: ";

impl PackageId {
    /// The registry of a package that could come from any of `indexes`.
    pub fn ambiguous_registry(indexes: &[String]) -> String {
        format!("{AMBIGUOUS}{}", indexes.join(", "))
    }
    /// The indexes an ambiguous package could come from.
    pub fn ambiguous(&self) -> Option<&str> {
        self.registry.as_deref()?.strip_prefix(AMBIGUOUS)
    }
}
