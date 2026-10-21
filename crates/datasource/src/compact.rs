//! Typed deserialization skips unused registry fields without building a JSON
//! tree for README, dependencies, attestations or artifact metadata.
use super::{Catalog, Version, sort};
use patchula_http::CacheError;
use patchula_versioning::Ecosystem;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize)]
struct StoredCatalog {
    #[serde(flatten)]
    catalog: Catalog,
    modified: Option<String>,
}

fn encode(catalog: Catalog, modified: Option<String>) -> Result<Vec<u8>, CacheError> {
    serde_json::to_vec(&StoredCatalog { catalog, modified }).map_err(|_| CacheError::Document)
}

#[derive(Deserialize)]
struct Npm {
    versions: BTreeMap<String, NpmVersion>,
    #[serde(default)]
    time: BTreeMap<String, String>,
    #[serde(rename = "dist-tags")]
    dist_tags: BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct NpmVersion {
    deprecated: Option<String>,
}

pub(super) fn npm(body: &[u8]) -> Result<Vec<u8>, CacheError> {
    let mut document: Npm = serde_json::from_slice(body).map_err(|_| CacheError::Document)?;
    let mut catalog = Catalog {
        dist_tags: document.dist_tags,
        versions: document
            .versions
            .into_iter()
            .map(|(version, node)| Version {
                published_at: document.time.remove(&version),
                version,
                deprecated: node.deprecated.is_some_and(|v| !v.is_empty()),
                digest: None, // npm integrity/shasum is not consumed by lookup.
                yanked: false,
            })
            .collect(),
    };
    sort(&mut catalog.versions, Ecosystem::Npm);
    encode(catalog, document.time.remove("modified"))
}

#[derive(Deserialize)]
struct CrateVersion {
    vers: String,
    pubtime: Option<String>,
    yanked: bool,
}
pub(super) fn crates(body: &[u8]) -> Result<Vec<u8>, CacheError> {
    let mut catalog = Catalog::default();
    let text = std::str::from_utf8(body).map_err(|_| CacheError::Document)?;
    for line in text.lines().filter(|line| !line.is_empty()) {
        let node: CrateVersion = serde_json::from_str(line).map_err(|_| CacheError::Document)?;
        catalog.versions.push(Version {
            version: node.vers,
            published_at: node.pubtime,
            yanked: node.yanked,
            deprecated: false,
            digest: None,
        });
    }
    catalog.versions.reverse(); // Sparse index is append-only publication order.
    encode(catalog, None)
}

#[derive(Deserialize)]
struct Pypi {
    releases: BTreeMap<String, Vec<PypiFile>>,
}
#[derive(Deserialize)]
struct PypiFile {
    upload_time_iso_8601: Option<String>,
    #[serde(default)]
    yanked: bool,
}
pub(super) fn pypi(body: &[u8]) -> Result<Vec<u8>, CacheError> {
    let document: Pypi = serde_json::from_slice(body).map_err(|_| CacheError::Document)?;
    let mut catalog = Catalog::default();
    for (version, files) in document.releases {
        if files.is_empty() {
            continue;
        }
        let yanked = files.iter().all(|file| file.yanked);
        catalog.versions.push(Version {
            version,
            published_at: files
                .into_iter()
                .filter_map(|file| file.upload_time_iso_8601)
                .min(),
            yanked,
            deprecated: false,
            digest: None,
        });
    }
    sort(&mut catalog.versions, Ecosystem::Pep440);
    encode(catalog, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pypi_keeps_earliest_upload_and_requires_all_files_yanked() {
        let bytes = pypi(br#"{"releases":{"0.1":[],"1.0":[{"upload_time_iso_8601":"2026-01-02T00:00:00Z","yanked":true},{"upload_time_iso_8601":"2026-01-01T00:00:00Z","yanked":false}],"2.0":[{"upload_time_iso_8601":null,"yanked":true}]},"info":{"description":"unused"}}"#).unwrap();
        let catalog: Catalog = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(catalog.versions.len(), 2);
        assert!(catalog.versions[0].yanked);
        assert_eq!(catalog.versions[0].published_at, None);
        assert!(!catalog.versions[1].yanked);
        assert_eq!(
            catalog.versions[1].published_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
    }
}
