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
/// registry, in the form it is recorded: the registry the repository names,
/// or the set of registries it could come from when the Count cannot choose.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Registry {
    /// The registry the repository names: an https URL standing in for the
    /// source's public registry, or a value the Count cannot read from, kept
    /// as written (without secrets). The lookup decides which it is and fails
    /// for anything it cannot request.
    Named(String),
    /// Any of these registries, which the Count does not choose between.
    Ambiguous {
        /// The recorded form: `ambiguous: ` and the indexes joined by `, `.
        recorded: String,
        /// The registries the package could come from, as recorded.
        indexes: Vec<String>,
    },
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
    /// serializes: the named registry as written, or `ambiguous: …`.
    pub fn recorded(&self) -> &str {
        match self {
            Self::Named(value) => value,
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
    /// The registry a recorded string names: `ambiguous: …` indexes, or the
    /// named registry as written.
    pub fn from_recorded(value: &str) -> Self {
        match value.strip_prefix(AMBIGUOUS) {
            Some(rest) => Self::Ambiguous {
                recorded: value.to_owned(),
                indexes: rest.split(", ").map(str::to_owned).collect(),
            },
            None => Self::Named(value.to_owned()),
        }
    }
}

impl PartialOrd for Registry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
/// Registries order by their recorded string, so changes, PR rows and
/// markers sort by the text they record. Values that record the same text
/// are then told apart by their indexes (none for a named registry), so
/// ordering agrees with equality and ordered collections never merge
/// distinct registries.
impl Ord for Registry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.recorded()
            .cmp(other.recorded())
            .then_with(|| self.indexes().cmp(&other.indexes()))
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

    /// `from_recorded` reads `ambiguous: …` back into its indexes and keeps
    /// anything else as the named registry, as written.
    #[test]
    fn recorded_registries_read_back() {
        for (recorded, expected) in [
            (
                "ambiguous: https://a.dev/, https://b.dev/",
                Registry::Ambiguous {
                    recorded: "ambiguous: https://a.dev/, https://b.dev/".into(),
                    indexes: vec!["https://a.dev/".into(), "https://b.dev/".into()],
                },
            ),
            (
                "HTTPS://NPM.ACME.DEV/simple",
                Registry::Named("HTTPS://NPM.ACME.DEV/simple".into()),
            ),
            (
                "https://$REGISTRY/",
                Registry::Named("https://$REGISTRY/".into()),
            ),
            ("not a url", Registry::Named("not a url".into())),
        ] {
            assert_eq!(Registry::from_recorded(recorded), expected, "{recorded}");
        }
    }

    #[test]
    fn indexes_and_conversions_read_a_registry_back() {
        let ambiguous =
            Registry::ambiguous(&["https://a.dev/".to_owned(), "https://b.dev/".to_owned()]);
        assert_eq!(
            ambiguous.indexes(),
            Some(["https://a.dev/".to_owned(), "https://b.dev/".to_owned()].as_slice())
        );
        assert_eq!(Registry::Named("https://a.dev/".into()).indexes(), None);
        // `From<String>` and `FromStr` read recorded bytes like `from_recorded`.
        assert_eq!(
            Registry::from("https://a.dev/".to_owned()),
            Registry::Named("https://a.dev/".into())
        );
        assert_eq!(
            "ambiguous: https://a.dev/".parse::<Registry>().unwrap(),
            Registry::ambiguous(&["https://a.dev/".to_owned()])
        );
    }

    #[test]
    fn equal_recorded_text_is_told_apart_by_indexes() {
        use std::cmp::Ordering;
        let ambiguous = Registry::ambiguous(&["https://a.dev/".to_owned()]);
        // Same recorded text: the indexes decide.
        let named = Registry::Named(ambiguous.recorded().to_owned());
        assert_eq!(named.cmp(&ambiguous), Ordering::Less);
        let same = Registry::ambiguous(&["https://a.dev/".to_owned()]);
        assert_eq!(ambiguous.cmp(&same), Ordering::Equal);
    }
}
