//! The public mise-versions mirror of GitHub release and tag lists, for plain GitHub lookups.
use super::upstream::{MirrorEntry, MirrorList, mirror_url};
use super::*;
use futures_util::{StreamExt, stream};

const MIRROR_FAN_OUT: usize = 16;

fn fallback(error: &patchula_http::CacheError) -> bool {
    matches!(
        error,
        patchula_http::CacheError::Status(429 | 500..=599)
            | patchula_http::CacheError::Transport(_)
    )
}
/// A mirror tag as the typed release the catalog stores, with its commit.
fn tag(entry: &MirrorEntry) -> Release {
    Release {
        version: entry.tag.clone(),
        published_at: entry.date.clone(),
        digest: entry.sha.clone(),
    }
}
impl Client {
    pub fn with_versions_host(mut self, enabled: bool) -> Self {
        self.use_versions_host = enabled;
        self
    }
    async fn mirror_list(&self, package: &str, kind: &str) -> Result<Option<MirrorList>> {
        if !self.use_versions_host
            || !self.github.versions_host_eligible()
            || !patchula_mise::listed_github_repository(package)
        {
            return Ok(None);
        }
        let url = mirror_url(package, kind);
        let response = self.http.get(&url, "application/json").await;
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
            return Err(Error::Registry(
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
                return Err(Error::Registry(
                    "mise-versions tag catalog is truncated; full range unavailable".into(),
                ));
            }
            // Tags carry no reliable dates of their own; the releases
            // document dates them.
            tags.tags
                .iter()
                .map(|tag| {
                    Version::from(Release {
                        published_at: dates.get(&tag.tag).cloned().flatten(),
                        ..self::tag(tag)
                    })
                })
                .collect()
        } else {
            releases
                .releases
                .iter()
                .filter(|entry| super::github::release_counts(false, entry.prerelease))
                .map(|entry| {
                    Version::from(Release {
                        version: entry.tag.clone(),
                        published_at: entry.date.clone(),
                        digest: None,
                    })
                })
                .collect()
        };
        Ok(Some(Catalog {
            versions,
            dist_tags: self.node_lts_tags(package).await,
        }))
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
            .buffer_unordered(MIRROR_FAN_OUT)
            .collect()
            .await)
    }
}
