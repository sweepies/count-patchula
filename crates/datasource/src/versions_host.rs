//! Public mise-versions catalogs, with explicit coverage and fallback rules.
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
    pub fn catalog_batch(&self) -> Self {
        let mut client = self.clone();
        client.versions_index = Default::default();
        client
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
            && let Ok(node) = self.node().await
        {
            catalog.dist_tags = node.dist_tags;
        }
        Ok(Some(catalog))
    }
    async fn versions_index(&self) -> std::result::Result<&Value, &str> {
        self.versions_index
            .get_or_init(|| async {
                let response = self
                    .http
                    .get(&format!("{HOST}/index.v1.json"), "application/json")
                    .await
                    .map_err(|e| e.to_string())?;
                let index: Value =
                    serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
                if index["format"] != 1 {
                    return Err("unsupported mise versions index format".into());
                }
                Ok(index)
            })
            .await
            .as_ref()
            .map_err(String::as_str)
    }
    /// Patchula reports every update class and held-back version. A subset is
    /// not a complete catalog: use G4 only, otherwise read the full tool file.
    async fn tool_catalog(&self, tool: &str) -> Result<Option<Catalog>> {
        if let Ok(index) = self.versions_index().await {
            // An authenticated tool absent from the host's current catalog is
            // routed upstream before making a request that would return 404.
            if index["tools"]
                .as_object()
                .is_some_and(|tools| !tools.contains_key(tool))
            {
                return Ok(None);
            }
            let entry = &index["tools"][tool];
            if let Some(versions) = entry["versions"].as_array()
                && entry["count"].as_u64() == Some(versions.len() as u64)
                && !versions.is_empty()
            {
                let parsed = versions
                    .iter()
                    .map(|entry| -> Result<Option<Version>> {
                        let flags = entry[2].as_u64().ok_or(Error::Invalid)?;
                        let version = entry[0].as_str().ok_or(Error::Invalid)?;
                        let published = chrono::DateTime::from_timestamp_millis(
                            entry[1].as_i64().ok_or(Error::Invalid)?,
                        )
                        .ok_or(Error::Invalid)?;
                        Ok((flags & 3 == 0).then(|| Version {
                            version: version.into(),
                            published_at: Some(published.to_rfc3339()),
                            digest: None,
                            deprecated: false,
                            yanked: false,
                        }))
                    })
                    .collect::<Result<Vec<_>>>();
                if let Ok(versions) = parsed {
                    return Ok(Some(Catalog {
                        versions: versions.into_iter().flatten().collect(),
                        dist_tags: BTreeMap::new(),
                    }));
                }
            }
        }
        let response = self
            .http
            .get(&format!("{HOST}/{tool}.toml"), "application/toml")
            .await?;
        let text = std::str::from_utf8(&response.body).map_err(|_| Error::Invalid)?;
        let document = toml_edit::Document::parse(text).map_err(|_| Error::Invalid)?;
        let entries = document["versions"].as_table_like().ok_or(Error::Invalid)?;
        let mut versions = Vec::new();
        for (version, entry) in entries.iter() {
            if entry.get("rolling").and_then(toml_edit::Item::as_bool) == Some(true)
                || entry.get("prerelease").and_then(toml_edit::Item::as_bool) == Some(true)
            {
                continue;
            }
            let date = entry
                .get("created_at")
                .and_then(toml_edit::Item::as_datetime)
                .map(ToString::to_string)
                .or_else(|| {
                    entry
                        .get("created_at")
                        .and_then(toml_edit::Item::as_str)
                        .map(str::to_owned)
                });
            versions.push(Version {
                version: version.into(),
                published_at: date,
                digest: None,
                deprecated: false,
                yanked: false,
            });
        }
        Ok(Some(Catalog {
            versions,
            dist_tags: BTreeMap::new(),
        }))
    }
    /// Keep the manager's upstream identity and namespace extraction unchanged.
    pub async fn mise_tool_catalog(&self, name: &str) -> Result<Catalog> {
        let mut from_host = false;
        let mut catalog = if self.use_versions_host
            && let Some(tool) = patchula_mise::versions_tool(name)
        {
            match self.tool_catalog(&tool).await {
                Ok(Some(catalog)) => {
                    from_host = true;
                    catalog
                }
                Ok(None) => self.clone().with_versions_host(false).mise(name).await?,
                Err(Error::Http(ref error)) if fallback(error) => {
                    self.clone().with_versions_host(false).mise(name).await?
                }
                Err(error) => return Err(error),
            }
        } else {
            self.mise(name).await?
        };
        if from_host
            && matches!(name, "node" | "core:node")
            && let Ok(node) = self.node().await
        {
            catalog.dist_tags = node.dist_tags;
        }
        if let Some(extract) = patchula_mise::resolve(name).and_then(|r| r.extract)
            && let Some(prefix) = extract
                .strip_prefix('^')
                .and_then(|v| v.strip_suffix("(?<version>.+)$"))
            && prefix != "v?"
            && !prefix.contains(['?', '[', '(', '\\'])
        {
            for version in &mut catalog.versions {
                version.version.insert_str(0, prefix);
            }
        }
        Ok(catalog)
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
                        Ok(mut catalogs) => {
                            catalogs.remove(&package).unwrap_or(Err(Error::Invalid))
                        }
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
