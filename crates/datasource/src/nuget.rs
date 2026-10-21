//! nuget.org: the flat container lists versions; the newest registration
//! page dates them and marks unlisted (yanked) and deprecated releases.
use super::*;

#[derive(Serialize, Deserialize)]
struct Entry {
    version: String,
    published: Option<String>,
    listed: bool,
    deprecated: bool,
}

fn listing(bytes: &[u8]) -> std::result::Result<Vec<u8>, rustovate_http::CacheError> {
    #[derive(Deserialize)]
    struct Index {
        versions: Vec<String>,
    }
    let index: Index =
        serde_json::from_slice(bytes).map_err(|_| rustovate_http::CacheError::Document)?;
    serde_json::to_vec(&index.versions).map_err(|_| rustovate_http::CacheError::Document)
}
fn entries(items: &serde_json::Value) -> Vec<Entry> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let entry = &item["catalogEntry"];
            Some(Entry {
                version: entry["version"].as_str()?.into(),
                // Unlisted packages report a 1900 publication date.
                published: entry["published"]
                    .as_str()
                    .filter(|p| !p.starts_with("1900-"))
                    .map(str::to_owned),
                listed: entry["listed"].as_bool().unwrap_or(true),
                deprecated: !entry["deprecation"].is_null(),
            })
        })
        .collect()
}
/// The registration index: the newest page's entries, inline or as a link.
fn registration(bytes: &[u8]) -> std::result::Result<Vec<u8>, rustovate_http::CacheError> {
    let index: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| rustovate_http::CacheError::Document)?;
    let newest = index["items"].as_array().and_then(|pages| pages.last());
    let compact = match newest {
        Some(page) if page["items"].is_array() => {
            serde_json::json!({"entries": entries(&page["items"])})
        }
        Some(page) => serde_json::json!({"page": page["@id"]}),
        None => serde_json::json!({"entries": []}),
    };
    serde_json::to_vec(&compact).map_err(|_| rustovate_http::CacheError::Document)
}
fn page(bytes: &[u8]) -> std::result::Result<Vec<u8>, rustovate_http::CacheError> {
    let page: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| rustovate_http::CacheError::Document)?;
    serde_json::to_vec(&serde_json::json!({"entries": entries(&page["items"])}))
        .map_err(|_| rustovate_http::CacheError::Document)
}

impl Client {
    pub(crate) async fn nuget(&self, package: &str) -> Result<Catalog> {
        if !package
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(Error::Invalid);
        }
        let id = package.to_ascii_lowercase();
        let response = self
            .http
            .get_derived(
                &format!("https://api.nuget.org/v3-flatcontainer/{id}/index.json"),
                "application/json",
                "nuget-flat-v1",
                listing,
            )
            .await?;
        let listed: Vec<String> = serde_json::from_slice(&response.body)?;
        #[derive(Deserialize)]
        struct Metadata {
            #[serde(default)]
            entries: Vec<Entry>,
            page: Option<String>,
        }
        let mut metadata: Metadata = serde_json::from_slice(
            &self
                .http
                .get_derived(
                    &format!("https://api.nuget.org/v3/registration5-gz-semver2/{id}/index.json"),
                    "application/json",
                    "nuget-registration-v1",
                    registration,
                )
                .await?
                .body,
        )?;
        if let Some(url) = metadata
            .page
            .take()
            .filter(|u| u.starts_with("https://api.nuget.org/"))
        {
            metadata.entries = serde_json::from_slice::<Metadata>(
                &self
                    .http
                    .get_derived(&url, "application/json", "nuget-registration-page-v1", page)
                    .await?
                    .body,
            )?
            .entries;
        }
        let known: BTreeMap<String, Entry> = metadata
            .entries
            .into_iter()
            .map(|e| (e.version.to_ascii_lowercase(), e))
            .collect();
        let mut versions: Vec<Version> = listed
            .into_iter()
            .map(|version| {
                let entry = known.get(&version.to_ascii_lowercase());
                Version {
                    published_at: entry.and_then(|e| e.published.clone()),
                    deprecated: entry.is_some_and(|e| e.deprecated),
                    yanked: entry.is_some_and(|e| !e.listed),
                    digest: None,
                    version,
                }
            })
            .collect();
        sort(&mut versions, Ecosystem::Nuget);
        Ok(Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        })
    }
}
