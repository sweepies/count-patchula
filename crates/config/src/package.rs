//! The identity of a package as the Count looks it up.
use patchula_words::Source;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A package where the Count looks it up: its source, the registry it comes
/// from when that is not the source's public one, and its name there.
///
/// Everything that identifies a package (lookups, caches, keys, advisories,
/// release notes) takes this type, so none can leave the registry out. Two
/// packages with one name from different registries are different packages.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PackageId {
    pub source: Source,
    /// The registry the package comes from, when it is not the source's
    /// public one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<Registry>,
    pub name: String,
}

/// Where a package is looked up when that is not its source's public
/// registry, in the form it is recorded: an https URL, the set of registries
/// it could come from when the Count cannot choose, or a value kept as
/// written so its lookup fails.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Registry {
    /// An https URL standing in for the source's public registry.
    Url(String),
    /// Any of these registries, which the Count does not choose between.
    Ambiguous {
        /// The recorded form: `ambiguous: ` and the indexes joined by `, `.
        recorded: String,
        /// The registries the package could come from, as recorded.
        indexes: Vec<String>,
    },
    /// A value the Count cannot read from, kept as written (without secrets)
    /// so its lookup fails.
    Unusable(String),
}

/// How a package that could come from several registries records its
/// registry: its lookup fails rather than guess, since the public one may be
/// someone else's.
const AMBIGUOUS: &str = "ambiguous: ";

/// Whether `value` is an https registry URL a repository may name: an
/// `https://` scheme in any case, a non-empty host, and no user, query,
/// fragment, space or `$` to expand. `Registry::from_recorded` sorts stored
/// bytes by it and producers apply it before normalising what they record.
pub fn acceptable_registry_url(value: &str) -> bool {
    value
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
        && {
            let rest = &value[8..];
            let host = rest.split('/').next().unwrap_or_default();
            !host.is_empty() && !value.contains(['@', '?', '#', ' ', '$'])
        }
}

impl Registry {
    /// The registry of a package that could come from any of `indexes`.
    pub fn ambiguous(indexes: &[String]) -> Self {
        Self::Ambiguous {
            recorded: format!("{AMBIGUOUS}{}", indexes.join(", ")),
            indexes: indexes.to_vec(),
        }
    }
    /// The string the registry is recorded as, which is also how it
    /// serializes: an https URL, `ambiguous: …`, or the unusable value as
    /// written.
    pub fn recorded(&self) -> &str {
        match self {
            Self::Url(value) | Self::Unusable(value) => value,
            Self::Ambiguous { recorded, .. } => recorded,
        }
    }
    /// The registries an ambiguous package could come from.
    pub fn indexes(&self) -> Option<&[String]> {
        match self {
            Self::Ambiguous { indexes, .. } => Some(indexes),
            _ => None,
        }
    }
    /// The registry a recorded string names: `ambiguous: …` indexes, a URL a
    /// repository could have written as one, or anything else kept as
    /// written. Producers decide the kind when they record; this is the
    /// read-back for stored bytes, which were written in the same form.
    pub fn from_recorded(value: &str) -> Self {
        if let Some(rest) = value.strip_prefix(AMBIGUOUS) {
            return Self::Ambiguous {
                recorded: value.to_owned(),
                indexes: rest.split(", ").map(str::to_owned).collect(),
            };
        }
        // Recorded bytes were written normalized, so the shared acceptance
        // check only sorts them back into kinds.
        if acceptable_registry_url(value) {
            Self::Url(value.to_owned())
        } else {
            Self::Unusable(value.to_owned())
        }
    }
}

impl PartialOrd for Registry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
/// The recorded string is what ordered `Option<String>` before the enum:
/// changes, PR rows and markers keep the order they had. Values that record
/// the same text are then told apart by kind and contents, so ordering agrees
/// with equality and ordered collections never merge distinct registries.
impl Ord for Registry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let kind = |registry: &Self| match registry {
            Self::Url(_) => 0,
            Self::Ambiguous { .. } => 1,
            Self::Unusable(_) => 2,
        };
        let indexes = |registry: &Self| match registry {
            Self::Ambiguous { indexes, .. } => Some(indexes.clone()),
            _ => None,
        };
        self.recorded()
            .cmp(other.recorded())
            .then_with(|| kind(self).cmp(&kind(other)))
            .then_with(|| indexes(self).cmp(&indexes(other)))
    }
}
impl From<&str> for Registry {
    fn from(value: &str) -> Self {
        Self::from_recorded(value)
    }
}
impl From<String> for Registry {
    fn from(value: String) -> Self {
        Self::from_recorded(&value)
    }
}
impl std::str::FromStr for Registry {
    type Err = std::convert::Infallible;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_recorded(value))
    }
}

impl Serialize for Registry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.recorded())
    }
}
impl<'de> Deserialize<'de> for Registry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_recorded(&String::deserialize(deserializer)?))
    }
}

/// `registry|name`, the package encoding `split_package` reads: a package
/// qualified by the registry URL or repository it comes from.
pub fn join_package(registry: &str, name: &str) -> String {
    format!("{registry}|{name}")
}

/// `package` split at its last `|` into the registry or repository it comes
/// from and the name there. No package name contains `|`, so the last one
/// is the separator; `None` is an unqualified package.
pub fn split_package(package: &str) -> Option<(&str, &str)> {
    package.rsplit_once('|')
}

impl PackageId {
    pub fn new(source: Source, registry: Option<Registry>, name: impl Into<String>) -> Self {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `from_recorded` sorts stored registry bytes into the same variants
    /// the producers wrote: `ambiguous: …` indexes, an acceptable https URL,
    /// or the value kept as written.
    #[test]
    fn recorded_registries_sort_into_their_kinds() {
        for (recorded, expected) in [
            (
                "ambiguous: https://a.dev/, https://b.dev/",
                Registry::Ambiguous {
                    recorded: "ambiguous: https://a.dev/, https://b.dev/".into(),
                    indexes: vec!["https://a.dev/".into(), "https://b.dev/".into()],
                },
            ),
            (
                "https://npm.acme.dev/",
                Registry::Url("https://npm.acme.dev/".into()),
            ),
            // The scheme check is case-insensitive; the bytes are kept.
            (
                "HTTPS://NPM.ACME.DEV/simple",
                Registry::Url("HTTPS://NPM.ACME.DEV/simple".into()),
            ),
            (
                "http://npm.acme.dev/",
                Registry::Unusable("http://npm.acme.dev/".into()),
            ),
            ("https://", Registry::Unusable("https://".into())),
            ("https:///path", Registry::Unusable("https:///path".into())),
            (
                "https://user@npm.acme.dev/",
                Registry::Unusable("https://user@npm.acme.dev/".into()),
            ),
            (
                "https://npm.acme.dev/?q=1",
                Registry::Unusable("https://npm.acme.dev/?q=1".into()),
            ),
            (
                "https://npm.acme.dev/#frag",
                Registry::Unusable("https://npm.acme.dev/#frag".into()),
            ),
            (
                "https://npm.acme .dev/",
                Registry::Unusable("https://npm.acme .dev/".into()),
            ),
            (
                "https://$REGISTRY/",
                Registry::Unusable("https://$REGISTRY/".into()),
            ),
            (
                "sparse+https://index.crates.io/",
                Registry::Unusable("sparse+https://index.crates.io/".into()),
            ),
            ("not a url", Registry::Unusable("not a url".into())),
        ] {
            assert_eq!(Registry::from_recorded(recorded), expected, "{recorded}");
        }
    }
}
