//! nuget.org: the flat container lists versions; the registration pages,
//! every one of them, date them and mark unlisted (yanked) and deprecated
//! releases.
use super::*;
use futures_util::{StreamExt, stream};

/// Registration pages read at once.
const PAGE_FAN_OUT: usize = 4;

#[derive(Serialize, Deserialize)]
struct Entry {
    version: String,
    published: Option<String>,
    listed: bool,
    deprecated: bool,
}

fn listing(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    #[derive(Deserialize)]
    struct Index {
        versions: Vec<String>,
    }
    let index: Index = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&index.versions).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NUGET_FLAT: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("nuget-flat", 3, listing);
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
/// The registration index: every page, its entries inline or as a link.
fn registration(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let index: serde_json::Value = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    let pages: Vec<serde_json::Value> = index["items"]
        .as_array()
        .ok_or(patchula_http::CacheError::Document)?
        .iter()
        .map(|page| {
            if page["items"].is_array() {
                serde_json::json!({"entries": entries(&page["items"])})
            } else {
                serde_json::json!({"page": page["@id"]})
            }
        })
        .collect();
    serde_json::to_vec(&pages).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NUGET_REGISTRATION: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("nuget-registration", 3, registration);
fn page(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let page: serde_json::Value = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&serde_json::json!({"entries": entries(&page["items"])}))
        .map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NUGET_REGISTRATION_PAGE: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("nuget-registration-page", 3, page);

/// How a version is matched between the flat container and the
/// registration: case-insensitive and without build metadata, which the
/// flat container leaves out.
fn identity(version: &str) -> String {
    version
        .split_once('+')
        .map_or(version, |(version, _)| version)
        .to_ascii_lowercase()
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
                &NUGET_FLAT,
            )
            .await?;
        let listed: Vec<String> = serde_json::from_slice(&response.body)?;
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            entries: Vec<Entry>,
            page: Option<String>,
        }
        let pages: Vec<Page> = serde_json::from_slice(
            &self
                .http
                .get_derived(
                    &format!("https://api.nuget.org/v3/registration5-gz-semver2/{id}/index.json"),
                    "application/json",
                    &NUGET_REGISTRATION,
                )
                .await?
                .body,
        )?;
        // Every page is read, not just the newest: a page holds 64 versions
        // counted from the oldest, so an older line's new patch release, or
        // an unlisted one, can sit on any of them. A page that fails fails
        // the lookup rather than leave its versions looking listed.
        let pages: Vec<Result<Vec<Entry>>> = stream::iter(pages)
            .map(|page| async move {
                let Some(url) = page.page else {
                    return Ok(page.entries);
                };
                if !url.starts_with("https://api.nuget.org/") {
                    return Err(Error::Invalid);
                }
                Ok(serde_json::from_slice::<Page>(
                    &self
                        .http
                        .get_derived(&url, "application/json", &NUGET_REGISTRATION_PAGE)
                        .await?
                        .body,
                )?
                .entries)
            })
            .buffered(PAGE_FAN_OUT)
            .collect()
            .await;
        let mut metadata = Vec::new();
        for page in pages {
            metadata.extend(page?);
        }
        let known: BTreeMap<String, Entry> = metadata
            .into_iter()
            .map(|e| (identity(&e.version), e))
            .collect();
        let mut versions: Vec<Version> = listed
            .into_iter()
            .map(|version| {
                // A version the registration does not describe yet (it
                // trails the flat container) is unchecked, which policy holds.
                match known.get(&identity(&version)) {
                    Some(entry) => Version {
                        published_at: entry.published.clone(),
                        deprecated: entry.deprecated,
                        yanked: !entry.listed,
                        ..Version::new(version)
                    },
                    None => Version::unchecked(version),
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
