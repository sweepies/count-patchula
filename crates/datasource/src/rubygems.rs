//! rubygems.org: published versions with their dates. Yanked versions are not
//! listed; platform-specific builds of one version collapse into it.
use super::*;

fn versions(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let values: Vec<serde_json::Value> = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    let mut seen = BTreeMap::<String, Version>::new();
    for value in &values {
        let Some(number) = value["number"].as_str() else {
            continue;
        };
        let ruby = value["platform"] == "ruby";
        let version = Version {
            version: number.into(),
            published_at: value["created_at"].as_str().map(str::to_owned),
            digest: None,
            deprecated: false,
            yanked: false,
        };
        // Prefer the plain `ruby` platform's date for a version.
        if ruby || !seen.contains_key(number) {
            seen.insert(number.into(), version);
        }
    }
    serde_json::to_vec(&seen.into_values().collect::<Vec<_>>())
        .map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const RUBYGEMS_VERSIONS: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("rubygems-versions", 1, versions);

impl Client {
    pub(crate) async fn rubygems(&self, package: &str) -> Result<Catalog> {
        if !package
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(Error::Invalid);
        }
        let response = self
            .http
            .get_derived(
                &format!("https://rubygems.org/api/v1/versions/{package}.json"),
                "application/json",
                &RUBYGEMS_VERSIONS,
            )
            .await?;
        let mut versions: Vec<Version> = serde_json::from_slice(&response.body)?;
        sort(&mut versions, Ecosystem::Rubygems);
        Ok(Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        })
    }
}
