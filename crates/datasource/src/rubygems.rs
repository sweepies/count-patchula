//! rubygems.org: published versions with their dates. Yanked versions are not
//! listed; platform-specific builds of one version collapse into it.
use super::*;

fn versions(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    /// One published build; other fields are skipped.
    #[derive(Deserialize)]
    struct Build {
        number: Option<String>,
        platform: Option<String>,
        created_at: Option<String>,
    }
    let builds: Vec<Build> = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    let mut seen = BTreeMap::<String, Version>::new();
    for build in builds {
        let Some(number) = build.number else {
            continue;
        };
        let ruby = build.platform.as_deref() == Some("ruby");
        // Prefer the plain `ruby` platform's date for a version.
        if ruby || !seen.contains_key(&number) {
            let version = Version {
                published_at: build.created_at,
                ..Version::new(number.clone())
            };
            seen.insert(number, version);
        }
    }
    serde_json::to_vec(&seen.into_values().collect::<Vec<_>>())
        .map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const RUBYGEMS_VERSIONS: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("rubygems-versions", 4, versions);

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
