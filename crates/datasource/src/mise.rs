//! Tool versions as the pinned `mise ls-remote --json` lists them.
use super::*;
use patchula_mise::{Lookup, Query, Release};
use std::sync::Arc;

/// Who a mise listing is made for, which decides the GitHub token mise is
/// handed and who may reuse what it printed. Every other lookup is `Run`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MiseAccess {
    /// The run's own token, its listing kept under the run's credential
    /// scope: a trusted repository, and a local directory.
    #[default]
    Run,
    /// No GitHub token: an untrusted repository under credentials that
    /// cannot mint a repository-scoped token. The listing reads public data
    /// alone, so it is public and every repository shares it.
    Anonymous,
    /// A `metadata: read` token scoped to a repository that asked for the
    /// tool. Without `contents` it reads no repository's releases but public
    /// ones (not even its own under an old name), so the listing is public
    /// and every repository shares it.
    Public,
    /// A `contents: read` token scoped to this repository (its name,
    /// lowercased), its listing kept for this repository alone: the tool is
    /// released from it, or the Count cannot tell where the tool comes from
    /// and the repository may be private.
    Repository(String),
}

/// The repository mise lookups are made for: its name, its visibility as
/// GitHub reported it (`None` when not read counts as private), and whether
/// the operator trusts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MiseRepository {
    pub name: String,
    pub private: Option<bool>,
    pub trust: patchula_words::Trust,
}

/// The scope rule alone: the access a lookup for `tool` in `repository`
/// gets when the run's credentials can mint repository-scoped tokens.
pub fn mise_scope_rule(tool: &str, repository: &MiseRepository) -> MiseAccess {
    let name = repository.name.to_ascii_lowercase();
    match patchula_mise::github_sources(tool) {
        Some(sources) if !sources.contains(&name) => MiseAccess::Public,
        None if repository.private == Some(false) => MiseAccess::Public,
        _ => MiseAccess::Repository(name),
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
    /// The access a mise lookup for `tool` made for `repository` gets: the
    /// one place the token mise is handed is chosen. A trusted repository,
    /// or none (a local directory), gets the run's own token. An untrusted
    /// one gets a token scoped to it as the scope rule says, or none when
    /// the credentials cannot mint one: the run's would reach further than
    /// the repository.
    pub fn mise_access(&self, repository: Option<&MiseRepository>, tool: &str) -> MiseAccess {
        match repository {
            None => MiseAccess::Run,
            Some(repository) if repository.trust == patchula_words::Trust::Trusted => {
                MiseAccess::Run
            }
            Some(repository) if self.github.mints_scoped_tokens() => {
                mise_scope_rule(tool, repository)
            }
            Some(_) => MiseAccess::Anonymous,
        }
    }
    /// This client's mise lookups are made for `repository`, each with the
    /// access `mise_access` gives it.
    pub fn with_mise_repository(mut self, repository: Option<MiseRepository>) -> Self {
        self.mise_repository = repository;
        self.mise_expected = None;
        self
    }
    /// The repository this client's mise lookups are made for.
    pub fn mise_repository(&self) -> Option<&MiseRepository> {
        self.mise_repository.as_ref()
    }
    /// A batch lookup under a key's `access`, made for `via`: a repository
    /// that asked for it. A lookup whose access `via` does not give fails
    /// rather than run with another token — a `Public` key with no
    /// repository among them.
    pub fn with_mise_access(mut self, access: MiseAccess, via: Option<MiseRepository>) -> Self {
        self.mise_repository = via;
        self.mise_expected = Some(access);
        self
    }
    /// `tool`'s access on this client, checked against the batch key's.
    fn tool_access(&self, tool: &str) -> Result<MiseAccess> {
        let access = self.mise_access(self.mise_repository.as_ref(), tool);
        match &self.mise_expected {
            Some(expected) if *expected != access => {
                Err(Error::Github(patchula_github::Error::Configuration))
            }
            _ => Ok(access),
        }
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
    /// The token mise is handed under `access`: the run's for `Run`, else a
    /// token scoped to the repository with the permissions the access names,
    /// which fails the lookup rather than fall back to a broader one. Only a
    /// token for github.com is handed out.
    async fn mise_token(&self, access: &MiseAccess) -> Result<Option<String>> {
        if !self.github.versions_host_eligible() {
            return Ok(None);
        }
        let (repository, permissions) = match access {
            MiseAccess::Run => return Ok(self.github.token().await.ok()),
            MiseAccess::Anonymous => return Ok(None),
            MiseAccess::Public => (
                self.mise_repository
                    .as_ref()
                    .map(|repository| repository.name.as_str())
                    .ok_or(patchula_github::Error::Configuration)?,
                patchula_github::Permissions::MetadataRead,
            ),
            MiseAccess::Repository(repository) => (
                repository.as_str(),
                patchula_github::Permissions::ContentsRead,
            ),
        };
        let token = self
            .github
            .scoped_token(repository, permissions)
            .ok_or(patchula_github::Error::Configuration)?
            .await?;
        Ok(Some(token))
    }
    /// Whom a listing under `access` serves. A repository's own listing is
    /// keyed by the numeric id its token is minted for, so a repository that
    /// later takes the name never reads it. No token reaches mise off
    /// github.com, so such a listing is public too.
    async fn listing_credential_scope(&self, access: &MiseAccess) -> Result<String> {
        if !self.github.versions_host_eligible() {
            return Ok(PUBLIC_SCOPE.into());
        }
        Ok(match access {
            MiseAccess::Run => self.github.credential_scope().into(),
            MiseAccess::Public | MiseAccess::Anonymous => PUBLIC_SCOPE.into(),
            MiseAccess::Repository(repository) => format!(
                "{}:{}",
                self.github.credential_scope(),
                self.github
                    .scoped_repository_id(repository)
                    .ok_or(patchula_github::Error::Configuration)?
                    .await?
            ),
        })
    }
    async fn mise_scope(
        &self,
        lookup: &Lookup,
        tool: &str,
        prefix: Option<&str>,
        access: &MiseAccess,
    ) -> Result<ListingScope> {
        Ok(ListingScope {
            tool: tool.into(),
            prefix: prefix.unwrap_or_default().into(),
            credential_scope: self.listing_credential_scope(access).await?,
            mise_version: lookup.version().into(),
            versions_host: self.use_versions_host,
        })
    }
    /// The backend mise picks for `tool`, asked with the token the tool's
    /// access gives, as its listing is.
    pub async fn mise_backend(&self, tool: &str) -> Result<String> {
        let access = self.tool_access(tool)?;
        let token = self.mise_token(&access).await?;
        Ok(self.mise_lookup()?.backend(tool, token.as_deref()).await?)
    }
    /// One tool's listing, from the store while fresh and from mise otherwise.
    /// A manual retry skips the stored listing.
    pub async fn mise_releases(&self, tool: &str, prefix: Option<&str>) -> Result<Vec<Release>> {
        patchula_mise::check(tool)?;
        let lookup = self.mise_lookup()?;
        let access = self.tool_access(tool)?;
        let scope = self.mise_scope(lookup, tool, prefix, &access).await?;
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(saved) = self.fresh_state::<Listing>(&scope).await? {
            return Ok(saved.releases);
        }
        let token = self.mise_token(&access).await?;
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
