//! Tags on GitLab and Gitea/Forgejo repositories, for `forge-tags`
//! dependencies identified as `host/path`.
use super::*;
use crate::upstream::{Forge, ForgeKind, Repository};

/// Newest-first pages; enough history for update proposals without walking
/// every tag of a long-lived project.
const PAGES: u32 = 10;

fn gitlab(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    tags(bytes, |t| {
        Some(Version {
            version: t["name"].as_str()?.into(),
            // Annotated tags carry their own date; commit dates never count.
            published_at: t["created_at"].as_str().map(str::to_owned),
            digest: t["commit"]["id"].as_str().map(str::to_owned),
            deprecated: false,
            yanked: false,
        })
    })
}
fn gitea(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    tags(bytes, |t| {
        Some(Version {
            version: t["name"].as_str()?.into(),
            published_at: None,
            digest: t["commit"]["sha"].as_str().map(str::to_owned),
            deprecated: false,
            yanked: false,
        })
    })
}
fn tags(
    bytes: &[u8],
    map: impl Fn(&serde_json::Value) -> Option<Version>,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let values: Vec<serde_json::Value> =
        serde_json::from_slice(bytes).map_err(|_| patchula_http::CacheError::Document)?;
    let versions: Vec<_> = values.iter().filter_map(map).collect();
    serde_json::to_vec(&versions).map_err(|_| patchula_http::CacheError::Document)
}
const FORGE_GITLAB_TAGS: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("forge-gitlab-tags", gitlab);
const FORGE_GITEA_TAGS: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("forge-gitea-tags", gitea);

fn forge(package: &str) -> Result<Forge> {
    Repository::from_url(&format!("https://{package}"))
        .and_then(|r| r.forge)
        .ok_or(Error::Invalid)
}

/// The URL of one page of tags.
fn page_url(forge: &Forge, page: u32) -> String {
    match forge.kind {
        ForgeKind::Gitlab => format!(
            "https://{}/api/v4/projects/{}/repository/tags?per_page=100&page={page}",
            forge.host,
            forge.path.replace('/', "%2F")
        ),
        ForgeKind::Gitea => format!(
            "https://{}/api/v1/repos/{}/tags?limit=50&page={page}",
            forge.host, forge.path
        ),
    }
}

impl Client {
    pub(crate) async fn forge_tags(&self, package: &str) -> Result<Catalog> {
        let forge = forge(package)?;
        let mut versions = Vec::new();
        let mut complete = false;
        for page in 1..=PAGES {
            let url = page_url(&forge, page);
            let (size, derived) = match forge.kind {
                ForgeKind::Gitlab => (100, FORGE_GITLAB_TAGS),
                ForgeKind::Gitea => (50, FORGE_GITEA_TAGS),
            };
            let response = self
                .http
                .get_derived(&url, "application/json", &derived)
                .await?;
            let page: Vec<Version> = serde_json::from_slice(&response.body)?;
            let full = page.len() == size;
            versions.extend(page);
            if !full {
                complete = true;
                break;
            }
        }
        // Every fetched page was full at the page cap, so older tags may
        // exist; a partial catalog must not pass as complete.
        if !complete {
            return Err(Error::Invalid);
        }
        sort(&mut versions, Ecosystem::Loose);
        Ok(Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        })
    }
}
