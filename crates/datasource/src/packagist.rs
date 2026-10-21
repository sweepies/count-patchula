//! Packagist: `p2/<vendor>/<package>.json` lists tagged (non-dev) releases
//! with their release times. Minified entries always carry version and time.
use super::*;

fn versions(bytes: &[u8]) -> std::result::Result<Vec<u8>, rustovate_http::CacheError> {
    let document: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| rustovate_http::CacheError::Document)?;
    let entries = document["packages"]
        .as_object()
        .and_then(|p| p.values().next())
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let versions: Vec<Version> = entries
        .iter()
        .filter_map(|e| {
            Some(Version {
                version: e["version"].as_str()?.into(),
                published_at: e["time"].as_str().map(str::to_owned),
                digest: None,
                deprecated: false,
                yanked: false,
            })
        })
        .collect();
    serde_json::to_vec(&versions).map_err(|_| rustovate_http::CacheError::Document)
}

impl Client {
    pub(crate) async fn packagist(&self, package: &str) -> Result<Catalog> {
        let (vendor, name) = package.split_once('/').ok_or(Error::Invalid)?;
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        };
        if !valid(vendor) || !valid(name) {
            return Err(Error::Invalid);
        }
        let response = self
            .http
            .get_derived(
                &format!(
                    "https://repo.packagist.org/p2/{}/{}.json",
                    vendor.to_ascii_lowercase(),
                    name.to_ascii_lowercase()
                ),
                "application/json",
                "packagist-p2-v1",
                versions,
            )
            .await?;
        let mut versions: Vec<Version> = serde_json::from_slice(&response.body)?;
        sort(&mut versions, Ecosystem::Composer);
        Ok(Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn p2_releases_keep_versions_and_times() {
        let body = br#"{"packages":{"monolog/monolog":[{"name":"monolog/monolog","version":"3.7.0","version_normalized":"3.7.0.0","time":"2024-06-28T09:40:51+00:00"},{"version":"3.6.0","time":"2024-04-12T21:02:21+00:00"}]},"minified":"composer/2.0"}"#;
        let versions: Vec<super::Version> =
            serde_json::from_slice(&super::versions(body).unwrap()).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, "3.7.0");
        assert_eq!(
            versions[1].published_at.as_deref(),
            Some("2024-04-12T21:02:21+00:00")
        );
    }
}
