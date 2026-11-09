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
        // The same acceptance `registry_url` applies to what a repository
        // writes, minus its normalisation: recorded URLs are already
        // normalised, so the check only sorts stored bytes back into kinds.
        let url = value
            .get(..8)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
            && {
                let rest = &value[8..];
                let host = rest.split('/').next().unwrap_or_default();
                !host.is_empty() && !value.contains(['@', '?', '#', ' ', '$'])
            };
        if url {
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
/// changes, PR rows and markers keep the order they had.
impl Ord for Registry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.recorded().cmp(other.recorded())
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
