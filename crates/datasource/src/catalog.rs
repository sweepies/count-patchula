//! The shape every catalog transform compacts a registry document into:
//! `Catalog`/`Version` and the ordering `sort` writes, plus `sha256_hex` for
//! digest-addressed derived bodies. `tests/derived-versions.tsv` hashes this
//! file into every transform whose bytes it feeds.
use patchula_cache::Release;
use patchula_versioning::{Ecosystem, Versioning};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub versions: Vec<Version>,
    pub dist_tags: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub version: String,
    pub published_at: Option<String>,
    pub digest: Option<String>,
    pub deprecated: bool,
    pub yanked: bool,
}
impl Version {
    /// A listed version with no date or digest, neither deprecated nor
    /// yanked.
    pub fn new(version: String) -> Self {
        Self {
            version,
            published_at: None,
            digest: None,
            deprecated: false,
            yanked: false,
        }
    }
}
impl From<Release> for Version {
    fn from(r: Release) -> Self {
        Self {
            published_at: r.published_at,
            digest: r.digest,
            ..Self::new(r.version)
        }
    }
}

pub(crate) fn sort(versions: &mut Vec<Version>, scheme: Ecosystem) {
    // Parse once per release, not once per comparator.
    let mut parsed: Vec<_> = versions
        .drain(..)
        .map(|v| (scheme.parse(&v.version).ok(), v))
        .collect();
    parsed.sort_by(|(a, _), (b, _)| match (a, b) {
        (Some(a), Some(b)) => scheme.order(b, a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    versions.extend(parsed.into_iter().map(|(_, v)| v));
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
