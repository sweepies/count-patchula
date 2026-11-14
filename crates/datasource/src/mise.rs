//! Tool versions as the pinned `mise ls-remote --json` lists them.
use super::*;
use patchula_mise::{Lookup, Query, Release};
use std::sync::Arc;

/// Who a mise listing is made for, which decides the GitHub token mise is
/// handed and who may reuse what it printed. Every other lookup is `Run`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MiseAccess {
    /// The run's own token, its listing kept under the run's credential
    /// scope: credentials that cannot mint a repository-scoped token.
    #[default]
    Run,
    /// A read-only token scoped to a repository the tool is not released
    /// from. Such a token sees that repository and public data only, so the
    /// listing is public and every repository shares it.
    Public,
    /// A read-only token scoped to this repository, its listing kept for
    /// this repository alone: the tool is released from it, or the Count
    /// cannot tell where the tool comes from and the repository may be
    /// private.
    Repository(String),
}

/// The access a mise lookup for `tool` in `repository` gets once the run's
/// credentials can mint repository-scoped tokens. `private` is the
/// repository's visibility; unknown counts as private.
pub fn mise_access(tool: &str, repository: &str, private: Option<bool>) -> MiseAccess {
    match patchula_mise::github_sources(tool) {
        Some(sources) if !sources.contains(&repository.to_ascii_lowercase()) => MiseAccess::Public,
        None if private == Some(false) => MiseAccess::Public,
        _ => MiseAccess::Repository(repository.to_owned()),
    }
}

/// The scope a public listing is kept under: shared by every credential.
const PUBLIC_SCOPE: &str = "public";

#[derive(Serialize)]
struct ListingScope {
    tool: String,
    prefix: String,
    /// `public`, the run's credential scope, or the run's credential scope
    /// and the repository a scoped token was minted for.
    credential_scope: String,
    mise_version: String,
    versions_host: bool,
}
#[derive(Serialize, Deserialize)]
struct Listing {
    fetched_at: i64,
    releases: Vec<Release>,
}
impl FetchedAt for Listing {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
}
impl State for Listing {
    const NAMESPACE: &'static str = "mise-ls-remote";
    const VERSION: u32 = 2;
    type Scope = ListingScope;
}

/// What mise lists for a tool installed from a Git ref, with or without a
/// `#tag=`, `#branch=` or `#rev=` fragment.
const GIT_REF: &str = "HEAD";
/// The GitHub repository whose LTS line nodejs.org, not its releases, names.
const NODE_VERSIONS: &str = "actions/node-versions";

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
    pub fn mise_lookup(&self) -> Result<&Arc<Lookup>> {
        self.mise.as_ref().ok_or(Error::NoMise)
    }
    /// This client's mise lookups are made with `access`; `via` names the
    /// repository a `Public` lookup's token is scoped to, which must be one
    /// the tool is not released from (a repository that asked for it).
    pub fn with_mise_access(mut self, access: MiseAccess, via: Option<String>) -> Self {
        self.mise_via = match &access {
            MiseAccess::Repository(repository) => Some(repository.clone()),
            MiseAccess::Public => via,
            MiseAccess::Run => None,
        };
        self.mise_access = access;
        self
    }
    /// The run's own token, for a tool that reads GitHub itself; only a
    /// token for github.com is handed out.
    pub async fn run_tool_token(&self) -> Option<String> {
        if self.github.versions_host_eligible() {
            self.github.token().await.ok()
        } else {
            None
        }
    }
    /// The token mise is handed: the run's for `Run`, otherwise a read-only
    /// token scoped to the repository, which fails the lookup rather than
    /// fall back to a broader one. Only a token for github.com is handed
    /// out.
    async fn mise_token(&self) -> Result<Option<String>> {
        if !self.github.versions_host_eligible() {
            return Ok(None);
        }
        if self.mise_access == MiseAccess::Run {
            return Ok(self.github.token().await.ok());
        }
        let repository = self
            .mise_via
            .as_deref()
            .ok_or(patchula_github::Error::Configuration)?;
        let token = self
            .github
            .scoped_token(repository, patchula_github::Permissions::ContentsRead)
            .ok_or(patchula_github::Error::Configuration)?
            .await?;
        Ok(Some(token))
    }
    /// Whom a listing serves: no token reaches mise off github.com, so
    /// such a listing is public too.
    fn listing_credential_scope(&self) -> String {
        if !self.github.versions_host_eligible() {
            return PUBLIC_SCOPE.into();
        }
        match &self.mise_access {
            MiseAccess::Run => self.github.credential_scope().into(),
            MiseAccess::Public => PUBLIC_SCOPE.into(),
            MiseAccess::Repository(repository) => format!(
                "{}:{}",
                self.github.credential_scope(),
                repository.to_ascii_lowercase()
            ),
        }
    }
    fn mise_scope(&self, lookup: &Lookup, tool: &str, prefix: Option<&str>) -> ListingScope {
        ListingScope {
            tool: tool.into(),
            prefix: prefix.unwrap_or_default().into(),
            credential_scope: self.listing_credential_scope(),
            mise_version: lookup.version().into(),
            versions_host: self.use_versions_host,
        }
    }
    /// One tool's listing, from the store while fresh and from mise otherwise.
    /// A manual retry skips the stored listing.
    pub async fn mise_releases(&self, tool: &str, prefix: Option<&str>) -> Result<Vec<Release>> {
        patchula_mise::check(tool)?;
        let lookup = self.mise_lookup()?;
        let scope = self.mise_scope(lookup, tool, prefix);
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(saved) = self.fresh_state::<Listing>(&scope).await? {
            return Ok(saved.releases);
        }
        let token = self.mise_token().await?;
        let releases = lookup
            .ls_remote(&Query {
                tool,
                prefix,
                token: token.as_deref(),
                versions_host: self.use_versions_host,
            })
            .await?;
        let saved = Listing {
            fetched_at: now,
            releases,
        };
        self.cache.set_state(&scope, &saved).await?;
        Ok(saved.releases)
    }
    pub(super) async fn mise(&self, tool: &str) -> Result<Catalog> {
        let releases = self.mise_releases(tool, None).await?;
        let mut versions: Vec<Version> = releases
            .into_iter()
            .map(|release| Version {
                published_at: release.created_at.as_deref().map(date),
                ..Version::new(release.version)
            })
            .collect();
        sort(&mut versions, Ecosystem::Loose);
        // A backend that installs from a Git ref, such as `cargo:` with a Git
        // URL, lists the one version `HEAD`: there is nothing to update to.
        // Any other listing is kept, even when its versions do not order.
        if versions.iter().all(|v| v.version == GIT_REF) {
            return Err(Error::NoReleases {
                tool: tool.into(),
                sample: GIT_REF.into(),
            });
        }
        let mut dist_tags = BTreeMap::new();
        // mise lists no channels, but `lts` selects the newest LTS line. When
        // that listing fails the catalog stays whole and has no LTS evidence,
        // which only a dependency on the LTS channel reports.
        if is_node(tool)
            && let Ok(lts) = self.mise_releases(tool, Some("lts")).await
            && let Some(newest) = lts
                .into_iter()
                .filter_map(|r| Some((Ecosystem::Loose.parse(&r.version).ok()?, r.version)))
                .max_by(|(a, _), (b, _)| Ecosystem::Loose.order(a, b))
        {
            dist_tags.insert("lts".into(), newest.1);
        }
        Ok(Catalog {
            versions,
            dist_tags,
        })
    }
    /// The `lts` dist-tag `actions/node-versions` catalogs carry. Its
    /// releases are GitHub releases, which mise does not list, so the
    /// evidence is nodejs.org's (see `node_lts`). A failed read leaves the
    /// catalog without it: stable setup-node targets remain usable during a
    /// Node-index outage, and an explicit LTS policy fails closed without
    /// this evidence.
    pub(super) async fn node_lts_tags(&self, package: &str) -> BTreeMap<String, String> {
        if !package.eq_ignore_ascii_case(NODE_VERSIONS) {
            return BTreeMap::new();
        }
        self.node_lts().await.unwrap_or_default()
    }
    /// The LTS evidence nodejs.org's release index publishes.
    async fn node_lts(&self) -> Result<BTreeMap<String, String>> {
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
