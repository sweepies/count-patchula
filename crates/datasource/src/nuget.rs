//! nuget.org: the flat container lists versions; the registration pages
//! date them and mark unlisted (yanked) and deprecated releases. A page is
//! read when it may hold a version the lookup's candidates name.
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
    patchula_http::Derived::new("nuget-flat", 4, listing);

/// A registration leaf: one version's catalog entry. Only the fields the
/// catalog keeps are read.
#[derive(Deserialize)]
struct Leaf {
    #[serde(rename = "catalogEntry")]
    entry: Option<CatalogEntry>,
}
#[derive(Deserialize)]
struct CatalogEntry {
    version: Option<String>,
    published: Option<String>,
    listed: Option<bool>,
    /// Present and not null on a deprecated version; its contents are not
    /// read.
    #[serde(default)]
    deprecation: Option<serde::de::IgnoredAny>,
}
fn entries(items: Vec<Leaf>) -> Vec<Entry> {
    items
        .into_iter()
        .filter_map(|item| {
            let entry = item.entry?;
            Some(Entry {
                version: entry.version?,
                // Unlisted packages report a 1900 publication date.
                published: entry.published.filter(|p| !p.starts_with("1900-")),
                listed: entry.listed.unwrap_or(true),
                deprecated: entry.deprecation.is_some(),
            })
        })
        .collect()
}
/// A registration page as the index describes it: its entries inline, or a
/// link, with the range of versions it holds.
#[derive(Serialize, Deserialize)]
struct Page {
    #[serde(default)]
    entries: Vec<Entry>,
    page: Option<String>,
    lower: Option<String>,
    upper: Option<String>,
}
/// The registration index: every page, its entries inline or as a link,
/// with its version bounds.
fn registration(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    #[derive(Deserialize)]
    struct Index {
        items: Vec<IndexPage>,
    }
    #[derive(Deserialize)]
    struct IndexPage {
        #[serde(rename = "@id")]
        id: Option<String>,
        lower: Option<String>,
        upper: Option<String>,
        items: Option<Vec<Leaf>>,
    }
    let index: Index = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    let pages: Vec<Page> = index
        .items
        .into_iter()
        .map(|page| match page.items {
            Some(items) => Page {
                entries: entries(items),
                page: None,
                lower: page.lower,
                upper: page.upper,
            },
            None => Page {
                entries: Vec::new(),
                page: page.id,
                lower: page.lower,
                upper: page.upper,
            },
        })
        .collect();
    serde_json::to_vec(&pages).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NUGET_REGISTRATION: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("nuget-registration", 4, registration);
fn page(body: &mut dyn std::io::Read) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    #[derive(Deserialize)]
    struct Linked {
        #[serde(default)]
        items: Vec<Leaf>,
    }
    let page: Linked = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&entries(page.items)).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NUGET_REGISTRATION_PAGE: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("nuget-registration-page", 4, page);

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
        // A page holds 64 versions counted from the oldest, so an older
        // line's new patch release, or an unlisted one, can sit on any of
        // them. A linked page is read unless its upper bound is at or below
        // the candidates' floor, so it holds no candidate; its versions are
        // listed unchecked. A page without bounds is always read. A page
        // that fails fails the lookup rather than leave its versions looking
        // listed.
        let wanted = self.candidates.wanted();
        let pages: Vec<Result<Vec<Entry>>> = stream::iter(pages)
            .filter(|page| {
                let read = page.page.is_none()
                    || page
                        .upper
                        .as_deref()
                        .is_none_or(|upper| wanted.reaches_above(upper));
                std::future::ready(read)
            })
            .map(|page| async move {
                let Some(url) = page.page else {
                    return Ok(page.entries);
                };
                if !url.starts_with("https://api.nuget.org/") {
                    return Err(Error::Invalid);
                }
                Ok(serde_json::from_slice::<Vec<Entry>>(
                    &self
                        .http
                        .get_derived(&url, "application/json", &NUGET_REGISTRATION_PAGE)
                        .await?
                        .body,
                )?)
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
