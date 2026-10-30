//! The public mise-versions mirror of GitHub release and tag lists, for plain GitHub lookups.
use super::*;
use futures_util::{StreamExt, stream};

const HOST: &str = "https://mise-versions.jdx.dev/data";
#[derive(Deserialize)]
struct Lists {
    #[serde(default)]
    releases: Vec<Entry>,
    #[serde(default)]
    tags: Vec<Entry>,
    truncated: bool,
}
#[derive(Deserialize)]
struct Entry {
    tag: String,
    date: Option<String>,
    sha: Option<String>,
    #[serde(default)]
    prerelease: bool,
}
fn fallback(error: &patchula_http::CacheError) -> bool {
    matches!(
        error,
        patchula_http::CacheError::Status(429 | 500..=599)
            | patchula_http::CacheError::Transport(_)
    )
}
impl Client {
    pub fn with_versions_host(mut self, enabled: bool) -> Self {
        self.use_versions_host = enabled;
        self
    }
    async fn mirror_list(&self, package: &str, kind: &str) -> Result<Option<Lists>> {
        if !self.use_versions_host
            || !self.github.versions_host_eligible()
            || !patchula_mise::listed_github_repository(package)
        {
            return Ok(None);
        }
        let response = self
            .http
            .get(
                &format!("{HOST}/github/{}/{kind}.json", package.to_ascii_lowercase()),
                "application/json",
            )
            .await;
        match response {
            Ok(response) => Ok(Some(serde_json::from_slice(&response.body)?)),
            Err(error) if fallback(&error) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    pub(super) async fn mirror_catalog(
        &self,
        source: Source,
        package: &str,
    ) -> Result<Option<Catalog>> {
        let Some(releases) = self.mirror_list(package, "releases").await? else {
            return Ok(None);
        };
        if releases.truncated {
            return Err(Error::Notes(
                "mise-versions release catalog is truncated; full range unavailable".into(),
            ));
        }
        let dates: BTreeMap<_, _> = releases
            .releases
            .iter()
            .map(|r| (r.tag.clone(), r.date.clone()))
            .collect();
        let versions = if source == Source::GithubTags {
            let Some(tags) = self.mirror_list(package, "tags").await? else {
                return Ok(None);
            };
            if tags.truncated {
                return Err(Error::Notes(
                    "mise-versions tag catalog is truncated; full range unavailable".into(),
                ));
            }
            tags.tags
                .into_iter()
                .map(|tag| Version {
                    published_at: dates.get(&tag.tag).cloned().flatten(),
                    version: tag.tag,
                    digest: tag.sha,
                    deprecated: false,
                    yanked: false,
                })
                .collect()
        } else {
            releases
                .releases
                .into_iter()
                .filter(|r| !r.prerelease)
                .map(|release| Version {
                    version: release.tag,
                    published_at: release.date,
                    digest: None,
                    deprecated: false,
                    yanked: false,
                })
                .collect()
        };
        let mut catalog = Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        };
        if package.eq_ignore_ascii_case("actions/node-versions")
            && let Ok(lts) = self.node_lts().await
        {
            catalog.dist_tags = lts;
        }
        Ok(Some(catalog))
    }
    pub(super) async fn mirror_release_catalogs(
        &self,
        packages: &[String],
    ) -> Result<BTreeMap<String, Result<Catalog>>> {
        Ok(stream::iter(packages.to_vec())
            .map(|package: String| async move {
                let result = match self.mirror_catalog(Source::GithubReleases, &package).await {
                    Ok(Some(catalog)) => Ok(catalog),
                    Ok(None) => match self
                        .api_release_catalogs(std::slice::from_ref(&package))
                        .await
                    {
                        Ok(mut catalogs) => catalogs
                            .remove(&package)
                            .unwrap_or_else(|| Err(Error::Invalid)),
                        Err(error) => Err(error),
                    },
                    Err(error) => Err(error),
                };
                (package, result)
            })
            .buffer_unordered(16)
            .collect()
            .await)
    }
}
