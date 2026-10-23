//! Tool versions as the pinned `mise ls-remote --json` lists them.
use super::*;
use patchula_cache::ExtractionKey;
use patchula_mise::{Lookup, Query, Release};
use std::sync::Arc;

/// A stored listing is reused for this long. mise keeps its own version lists
/// for about as long; a release-age wait is evaluated against the dates in the
/// listing, so a stored listing never hides a release for longer than this.
const FRESH_MS: i64 = 3_600_000;

#[derive(Serialize, Deserialize)]
struct Listing {
    fetched_at: i64,
    releases: Vec<Release>,
}

fn is_node(tool: &str) -> bool {
    matches!(tool, "node" | "core:node")
}

fn date(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw).map_or_else(
        |_| raw.to_owned(),
        |date| date.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

impl Client {
    /// Where mise runs. Without it (an in-memory cache has no tool store) a
    /// mise tool cannot be looked up.
    pub fn with_mise(mut self, lookup: Arc<Lookup>) -> Self {
        self.mise = Some(lookup);
        self
    }
    pub(crate) fn mise_lookup(&self) -> Result<&Arc<Lookup>> {
        self.mise.as_ref().ok_or(Error::NoMise)
    }
    /// Only a token for github.com is passed to mise.
    pub(crate) async fn mise_token(&self) -> Option<String> {
        if self.github.versions_host_eligible() {
            self.github.token().await.ok()
        } else {
            None
        }
    }
    fn mise_key(&self, lookup: &Lookup, tool: &str, prefix: Option<&str>) -> ExtractionKey {
        ExtractionKey {
            repo: format!("mise-ls-remote:{tool}:{}", prefix.unwrap_or("")),
            default_branch_commit: self.github.credential_scope().into(),
            config_hash: format!(
                "mise-ls-remote-v1:{}:{}",
                lookup.version(),
                self.use_versions_host
            ),
        }
    }
    /// One tool's listing, from the store while fresh and from mise otherwise.
    /// A manual retry skips the stored listing.
    pub(crate) async fn mise_releases(
        &self,
        tool: &str,
        prefix: Option<&str>,
    ) -> Result<Vec<Release>> {
        patchula_mise::check(tool)?;
        let lookup = self.mise_lookup()?;
        let key = self.mise_key(lookup, tool, prefix);
        let now = chrono::Utc::now().timestamp_millis();
        if !self.http.is_revalidating()
            && let Some(bytes) = self.cache.extraction_get(&key).await?
            && let Ok(saved) = serde_json::from_slice::<Listing>(&bytes)
            && (0..FRESH_MS).contains(&now.saturating_sub(saved.fetched_at))
        {
            return Ok(saved.releases);
        }
        let token = self.mise_token().await;
        let releases = lookup
            .ls_remote(&Query {
                tool,
                prefix,
                token: token.as_deref(),
                versions_host: self.use_versions_host,
            })
            .await?;
        self.cache
            .extraction_put(
                &key,
                &serde_json::to_vec(&Listing {
                    fetched_at: now,
                    releases: releases.clone(),
                })?,
            )
            .await?;
        Ok(releases)
    }
    pub(super) async fn mise(&self, tool: &str) -> Result<Catalog> {
        let releases = self.mise_releases(tool, None).await?;
        let mut versions: Vec<Version> = releases
            .into_iter()
            .filter(|release| release.prerelease != Some(true))
            .map(|release| Version {
                version: release.version,
                published_at: release.created_at.as_deref().map(date),
                digest: None,
                deprecated: false,
                yanked: false,
            })
            .collect();
        sort(&mut versions, Ecosystem::Loose);
        // A backend that installs from a Git ref, such as `cargo:` with a Git
        // URL, lists `HEAD` and no releases: there is nothing to update to.
        if !versions
            .iter()
            .any(|v| Ecosystem::Loose.parse(&v.version).is_ok())
        {
            return Err(patchula_mise::Error::NoReleases {
                tool: tool.into(),
                sample: versions
                    .first()
                    .map_or_else(String::new, |v| v.version.clone()),
            }
            .into());
        }
        let mut dist_tags = BTreeMap::new();
        // mise lists no channels, but `lts` selects the newest LTS line.
        if is_node(tool) {
            let lts = self.mise_releases(tool, Some("lts")).await?;
            if let Some(newest) = lts
                .into_iter()
                .filter_map(|r| Some((Ecosystem::Loose.parse(&r.version).ok()?, r.version)))
                .max_by(|(a, _), (b, _)| Ecosystem::Loose.order(a, b))
            {
                dist_tags.insert("lts".into(), newest.1);
            }
        }
        Ok(Catalog {
            versions,
            dist_tags,
        })
    }
    /// The LTS evidence nodejs.org publishes, for `actions/node-versions`: its
    /// releases are GitHub releases, so mise does not list them.
    pub(super) async fn node_lts(&self) -> Result<BTreeMap<String, String>> {
        let response = self
            .http
            .get("https://nodejs.org/dist/index.json", "application/json")
            .await?;
        let values: Vec<Value> = serde_json::from_slice(&response.body)?;
        let mut lts = Vec::new();
        for value in values {
            let version = value["version"]
                .as_str()
                .ok_or(Error::Invalid)?
                .trim_start_matches('v');
            if value["lts"].as_str().is_some_and(|s| !s.is_empty()) {
                lts.push(version.to_owned());
            }
        }
        Ok(lts
            .into_iter()
            .filter_map(|v| Some((Ecosystem::Loose.parse(&v).ok()?, v)))
            .max_by(|(a, _), (b, _)| Ecosystem::Loose.order(a, b))
            .map(|(_, v)| BTreeMap::from([("lts".into(), v)]))
            .unwrap_or_default())
    }
}
