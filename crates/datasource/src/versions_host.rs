//! The public mise-versions mirror of GitHub release and tag lists, for plain GitHub lookups.
use super::upstream::{MirrorEntry, MirrorList, mirror_url};
use super::*;
use futures_util::{StreamExt, stream};

const MIRROR_FAN_OUT: usize = 16;

fn fallback(error: &patchula_http::CacheError) -> bool {
    matches!(
        error,
        patchula_http::CacheError::Status(429 | 500..=599)
            | patchula_http::CacheError::Throttled(_)
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
            // document dates them. A tag it leaves undated may be an
            // annotated tag, which GitHub dates by its tagger: the mirror
            // cannot tell, so when the lookup's candidates name such a tag,
            // GitHub's own tag catalog answers instead. An undated tag no
            // candidate names — a floating `v1` below the baseline — is
            // listed unchecked, and the mirror still answers.
            let wanted = self.candidates.wanted();
            let date = |tag: &MirrorEntry| dates.get(&tag.tag).cloned().flatten();
            if tags
                .tags
                .iter()
                .any(|tag| date(tag).is_none() && wanted.contains(&tag.tag))
            {
                return Ok(None);
            }
            tags.tags
                .iter()
                .map(|tag| {
                    let published_at = date(tag);
                    Version {
                        unchecked: published_at.is_none(),
                        ..Version::from(Release {
                            published_at,
                            ..self::tag(tag)
                        })
                    }
                })
                .collect()
        } else {
            releases
                .releases
                .iter()
                .filter(|entry| super::github::release_counts(false, entry.prerelease))
                .map(|entry| {
                    Version::from(Release {
                        digest: None,
                        ..tag(entry)
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
        let mirrored: Vec<(String, Result<Option<Catalog>>)> = stream::iter(packages.to_vec())
            .map(|package: String| async move {
                let result = self.mirror_catalog(Source::GithubReleases, &package).await;
                (package, result)
            })
            .buffer_unordered(MIRROR_FAN_OUT)
            .collect()
            .await;
        let mut catalogs = BTreeMap::new();
        let mut misses = Vec::new();
        for (package, result) in mirrored {
            match result {
                Ok(Some(catalog)) => {
                    catalogs.insert(package, Ok(catalog));
                }
                Ok(None) => misses.push(package),
                Err(error) => {
                    catalogs.insert(package, Err(error));
                }
            }
        }
        // The mirror's misses go to GitHub in one batched call, which
        // answers each of them.
        catalogs.extend(self.api_release_catalogs(&misses).await?);
        Ok(catalogs)
    }
}
