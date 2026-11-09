//! Shared registry lookups with conditional GETs and incremental GitHub history.
#![forbid(unsafe_code)]
mod advisories;
pub use advisories::{Advisory, AdvisoryListing, AdvisoryQuery, Affected, Event, Range};
mod compact;
mod docker;
mod forge;
mod github;
mod images;
pub use images::{ImageSbom, Sbom};
mod maven;
mod mise;
mod nix;
pub use nix::Flakes;
mod nuget;
mod packagist;
mod registries;
pub mod upstream;
pub use registries::HelmChart;
mod rubygems;
mod rules;
use patchula_cache::{Cache, Release, State};
use patchula_config::PackageId;
use patchula_github::Error as GithubError;
use patchula_http::Cached;
use patchula_versioning::{Ecosystem, Versioning};
use patchula_words::{LookupErrorKind, Source};
pub use rules::{cached, http_options, registry_rules};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
mod versions_host;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Http(#[from] patchula_http::CacheError),
    #[error(transparent)]
    Github(#[from] patchula_github::Error),
    #[error(transparent)]
    Cache(#[from] patchula_cache::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Mise(#[from] patchula_mise::Error),
    #[error("mise lookups need the Count's tool store, which this cache does not have")]
    NoMise,
    #[error("flake input lookups need the Count's tool store, which this cache does not have")]
    NoNix,
    #[error("nix failed: {0}")]
    Nix(String),
    #[error("invalid package identity or upstream payload")]
    Invalid,
    #[error("{0}")]
    Registry(String),
    #[error("release-note read failed: {0}")]
    Notes(String),
    #[error("registry rate limit reached; retry after {0} seconds")]
    RateLimit(u64),
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    /// Every variant is classified explicitly: a new one must pick a kind.
    pub fn kind(&self) -> LookupErrorKind {
        use patchula_http::{CacheError, Error as HttpError};
        match self {
            Error::Http(CacheError::Status(status))
            | Error::Github(GithubError::Status(status)) => status_kind(*status),
            Error::Http(CacheError::Transport(HttpError::Network))
            | Error::Github(GithubError::Transport) => LookupErrorKind::Network,
            Error::Http(CacheError::Document)
            | Error::Github(GithubError::Json)
            | Error::Json(_)
            | Error::Mise(patchula_mise::Error::Unreadable(_)) => LookupErrorKind::Parse,
            Error::Github(GithubError::RateLimit) | Error::RateLimit(_) => {
                LookupErrorKind::RateLimit
            }
            Error::Mise(
                patchula_mise::Error::Empty(_) | patchula_mise::Error::NoReleases { .. },
            ) => LookupErrorKind::NotFound,
            Error::Http(
                CacheError::Worker
                | CacheError::Configuration
                | CacheError::MissingBody
                | CacheError::Cache(_)
                | CacheError::Transport(HttpError::Configuration),
            )
            | Error::Github(
                GithubError::Configuration
                | GithubError::Url
                | GithubError::Cache
                | GithubError::Graphql(_)
                | GithubError::Pagination
                | GithubError::MissingBody,
            )
            | Error::Cache(_)
            | Error::Mise(patchula_mise::Error::Refused(_) | patchula_mise::Error::Failed(_))
            | Error::NoMise
            | Error::NoNix
            | Error::Nix(_)
            | Error::Invalid
            | Error::Registry(_)
            | Error::Notes(_) => LookupErrorKind::Other,
        }
    }
}
fn status_kind(status: u16) -> LookupErrorKind {
    match status {
        401 | 403 | 407 => LookupErrorKind::Auth,
        404 | 410 => LookupErrorKind::NotFound,
        429 => LookupErrorKind::RateLimit,
        500.. => LookupErrorKind::Network,
        _ => LookupErrorKind::Other,
    }
}
/// A lookup error kept for the report, which several dependencies may share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub kind: LookupErrorKind,
    pub message: String,
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self {
            kind: error.kind(),
            message: error.to_string(),
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
/// Go releases dated through `.info` on a cold lookup, newest first.
const GO_DATED_RELEASES: usize = 20;
/// A stored listing or resolution is reused for this long. The tools keep
/// their own version lists for about as long, and a release-age wait is
/// evaluated against the dates in the listing, so a stored one never hides
/// a release for longer than this.
pub(crate) const FRESH_MS: i64 = 3_600_000;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub versions: Vec<Version>,
    pub dist_tags: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    pub version: String,
    pub published_at: Option<String>,
    pub digest: Option<String>,
    pub deprecated: bool,
    pub yanked: bool,
}
impl From<Release> for Version {
    fn from(r: Release) -> Self {
        Self {
            version: r.version,
            published_at: r.published_at,
            digest: r.digest,
            deprecated: false,
            yanked: false,
        }
    }
}
/// npm's public registry, which a dependency without its own registry reads.
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org";

/// A cached registry read a rate limit made stale: the package's whole tag
/// catalog, or one tag's manifest.
/// Built only through [`StaleKey::CATALOG`] and [`StaleKey::digest`], so a
/// digest key always holds the normalized tag.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct StaleKey(Stale);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Stale {
    /// The package's tag catalog.
    Catalog,
    /// One tag's manifest, by the reference it was requested with.
    Digest(String),
}
impl StaleKey {
    /// The package's tag catalog.
    pub const CATALOG: Self = Self(Stale::Catalog);
    /// The key a tag's manifest read is recorded under. OCI tags cannot hold
    /// `+`, which a catalog of OCI versions writes as `_`.
    pub fn digest(tag: &str) -> Self {
        Self(Stale::Digest(tag.replace('+', "_")))
    }
    /// Whether this key is the manifest `reference` was read through.
    pub fn is_manifest_of(&self, reference: &str) -> bool {
        matches!(&self.0, Stale::Digest(tag) if *tag == reference.replace('+', "_"))
    }
}
impl std::fmt::Display for StaleKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Stale::Catalog => f.write_str("catalog"),
            Stale::Digest(tag) => write!(f, "digest:{tag}"),
        }
    }
}

/// The URL of `package`'s packument in `registry`, or in npm's public one.
fn npm_url(registry: Option<&str>, package: &str) -> String {
    format!(
        "{}/{}",
        registry.unwrap_or(NPM_REGISTRY).trim_end_matches('/'),
        package.replace('/', "%2f")
    )
}

#[derive(Clone)]
pub struct Client {
    pub github: patchula_github::Client,
    pub http: Cached,
    pub cache: Cache,
    tokens: docker::Tokens,
    use_versions_host: bool,
    mise: Option<std::sync::Arc<patchula_mise::Lookup>>,
    nix: Option<std::sync::Arc<dyn Flakes>>,
    stale_registry: std::sync::Arc<tokio::sync::Mutex<BTreeMap<String, BTreeMap<StaleKey, i64>>>>,
}
impl Client {
    pub fn revalidating(&self) -> Self {
        let mut client = self.clone();
        client.http = client.http.revalidating();
        client
    }
    pub fn new(github: patchula_github::Client, http: Cached, cache: Cache) -> Self {
        Self {
            github,
            http,
            cache,
            tokens: Default::default(),
            use_versions_host: true,
            mise: None,
            nix: None,
            stale_registry: Default::default(),
        }
    }

    /// Cached registry reads used because of rate limiting, keyed by catalog or digest tag.
    pub async fn registry_stale(&self, package: &str) -> BTreeMap<StaleKey, i64> {
        let Ok((host, path)) = docker::identity(package) else {
            return BTreeMap::new();
        };
        self.stale_registry
            .lock()
            .await
            .get(&format!("{host}/{path}"))
            .cloned()
            .unwrap_or_default()
    }
    /// The versions of `package`, from `registry` when it is given: an https
    /// URL standing in for the source's public registry. npm and PyPI take one;
    /// an ambiguous or unusable registry fails without a request.
    pub async fn lookup(&self, id: &PackageId) -> Result<Catalog> {
        let (source, registry, package) = (id.source, id.registry.as_deref(), id.name.as_str());
        if let Some(registry) = registry {
            if let Some(registries) = id.ambiguous() {
                return Err(Error::Registry(format!(
                    "{package} could come from several registries ({registries}); commit a lockfile that records where it comes from"
                )));
            }
            // A value the manager kept as written (`${VAR}`, a redacted URL,
            // find-links) is no URL to request, though a URL parser takes it.
            if !matches!(source, Source::Npm | Source::Pypi)
                || registry.contains([' ', '$', '{', '(', '@', '?', '#'])
                || patchula_http::url_prefix(registry).is_none()
            {
                return Err(Error::Registry(format!(
                    "cannot read {package} from the registry {registry}"
                )));
            }
        }
        // npm names are URL-safe; an escape could reach another path on a
        // registry that decodes it.
        if source == Source::Npm && package.contains(['%', ' ']) {
            return Err(Error::Invalid);
        }
        // A flake reference is a URL with a query; the runner checks its scheme.
        if source != Source::Nix
            && (package.is_empty()
                || package.contains(['?', '#', '\\'])
                || package.split('/').any(|s| s == ".." || s == "."))
        {
            return Err(Error::Invalid);
        }
        match source {
            Source::Npm => self.npm(registry, package).await,
            Source::GithubReleases | Source::GithubTags => {
                self.github_versions(source, package).await
            }
            Source::Crates => self.crates(package).await,
            Source::Go => self.go(package).await,
            Source::Pypi => match registry {
                Some(index) => self.pypi_index(index, package).await,
                None => self.pypi(package).await,
            },
            Source::Mise => self.mise(package).await,
            Source::GithubRunners => runners(package),
            Source::Docker => self.docker(package).await,
            Source::Oci => {
                let mut catalog = self.docker(package).await?;
                for version in &mut catalog.versions {
                    version.version = version.version.replace('_', "+");
                }
                sort(&mut catalog.versions, Ecosystem::Npm);
                Ok(catalog)
            }
            Source::Terraform => self.terraform(package).await,
            Source::Helm => self.helm(package).await,
            Source::ForgeTags => self.forge_tags(package).await,
            Source::Maven => self.maven(package).await,
            Source::Nuget => self.nuget(package).await,
            Source::Rubygems => self.rubygems(package).await,
            Source::Packagist => self.packagist(package).await,
            Source::Nix => self.nix(package).await,
        }
    }
    async fn npm(&self, registry: Option<&str>, package: &str) -> Result<Catalog> {
        let response = self
            .http
            .get_derived(
                &npm_url(registry, package),
                "application/json",
                &compact::NPM_CATALOG,
            )
            .await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
    async fn crates(&self, package: &str) -> Result<Catalog> {
        if !package
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err(Error::Invalid);
        }
        let package = package.to_ascii_lowercase();
        let suffix = match package.len() {
            1 => format!("1/{package}"),
            2 => format!("2/{package}"),
            3 => format!("3/{}/{package}", &package[..1]),
            _ => format!("{}/{}/{package}", &package[..2], &package[2..4]),
        };
        let response = self
            .http
            .get_derived(
                &format!("https://index.crates.io/{suffix}"),
                "text/plain",
                &compact::CRATES_CATALOG,
            )
            .await?;
        Ok(serde_json::from_slice(&response.body)?)
    }
    async fn go(&self, package: &str) -> Result<Catalog> {
        let escaped = escape_go(package)?;
        let prefix = format!("https://proxy.golang.org/{escaped}/@v");
        let response = self
            .http
            .get(&format!("{prefix}/list"), "text/plain")
            .await?;
        let text = std::str::from_utf8(&response.body).map_err(|_| Error::Invalid)?;
        let mut versions: Vec<Version> = text
            .lines()
            .map(|v| Version {
                version: v.into(),
                published_at: None,
                digest: None,
                deprecated: false,
                yanked: false,
            })
            .collect();
        sort(&mut versions, Ecosystem::Go);
        // Go prefers a tagged release over prereleases and only needs a default
        // branch pseudo-version when no tags exist. Keep @latest for that case.
        let tagged = versions
            .iter()
            .find(|v| {
                Ecosystem::Go
                    .parse(&v.version)
                    .is_ok_and(|v| v.0.pre.is_empty())
            })
            .or_else(|| {
                versions
                    .iter()
                    .find(|v| Ecosystem::Go.parse(&v.version).is_ok())
            });
        let latest_version = if let Some(tagged) = tagged {
            tagged.version.clone()
        } else {
            let latest: Value = serde_json::from_slice(
                &self
                    .http
                    .get(
                        &format!("https://proxy.golang.org/{escaped}/@latest"),
                        "application/json",
                    )
                    .await?
                    .body,
            )?;
            let latest_version = latest["Version"].as_str().ok_or(Error::Invalid)?.to_owned();
            versions.push(Version {
                version: latest_version.clone(),
                published_at: latest["Time"].as_str().map(str::to_owned),
                digest: None,
                deprecated: false,
                yanked: false,
            });
            latest_version
        };
        sort(&mut versions, Ecosystem::Go);
        let known = self.cache.releases("go", package).await?;
        let known: std::collections::HashMap<_, _> = known
            .releases
            .into_iter()
            .map(|v| (v.version, v.published_at))
            .collect();
        // Fetch .info only for new releases; retain publication timestamps for
        // older ones. Never refetch full per-version history on a warm lookup.
        // A cold lookup dates only the newest releases: older ones are outside
        // any release-age wait, and a module can list hundreds of versions.
        // The proxy lists some versions it cannot describe (404); those are
        // not installable releases and are dropped.
        let mut reached_known = false;
        let mut fetched = 0;
        let mut missing = std::collections::HashSet::new();
        for version in &mut versions {
            if let Some(time) = known.get(&version.version) {
                version.published_at.clone_from(time);
                reached_known = true;
            } else if !reached_known && fetched < GO_DATED_RELEASES {
                fetched += 1;
                let response = self
                    .http
                    .get(
                        &format!("{prefix}/{}.info", version.version),
                        "application/json",
                    )
                    .await;
                let response = match response {
                    Ok(response) => response,
                    Err(patchula_http::CacheError::Status(404 | 410)) => {
                        missing.insert(version.version.clone());
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                let info: Value = serde_json::from_slice(&response.body)?;
                version.published_at = info["Time"].as_str().map(str::to_owned);
            }
        }
        versions.retain(|v| !missing.contains(&v.version));
        self.merge("go", package, &versions).await?;
        Ok(Catalog {
            versions,
            dist_tags: BTreeMap::from([("latest".into(), latest_version)]),
        })
    }
    async fn pypi(&self, package: &str) -> Result<Catalog> {
        if !package
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(Error::Invalid);
        }
        let package = registries::normalize_python(package)?;
        let response = self
            .http
            .get_derived(
                &format!("https://pypi.org/pypi/{package}/json"),
                "application/json",
                &compact::PYPI_CATALOG,
            )
            .await;
        let response = match response {
            Ok(v) => v,
            Err(patchula_http::CacheError::Status(404 | 406)) => {
                return self.pypi_simple(&package).await;
            }
            Err(e) => return Err(e.into()),
        };
        Ok(serde_json::from_slice(&response.body)?)
    }
    async fn merge(&self, source: &str, package: &str, versions: &[Version]) -> Result<()> {
        let head = self.cache.release_head(source, package).await?;
        let mut page = Vec::new();
        for v in versions {
            page.push(Release {
                version: v.version.clone(),
                published_at: v.published_at.clone(),
                digest: v.digest.clone(),
            });
            if head.as_deref() == Some(&v.version) {
                break;
            }
        }
        self.cache.merge_releases(source, package, &page).await?;
        Ok(())
    }
}
fn runners(package: &str) -> Result<Catalog> {
    use std::sync::LazyLock;
    static IMAGES: LazyLock<BTreeMap<String, Vec<String>>> = LazyLock::new(|| {
        serde_json::from_str(include_str!("runners.json")).expect("documented runner images")
    });
    let versions = IMAGES
        .get(package)
        .ok_or(Error::Invalid)?
        .iter()
        .map(|v| Version {
            version: v.clone(),
            published_at: None,
            digest: None,
            deprecated: false,
            yanked: false,
        })
        .collect();
    Ok(Catalog {
        versions,
        dist_tags: BTreeMap::new(),
    })
}
fn sort(versions: &mut Vec<Version>, scheme: Ecosystem) {
    // Parse once per release, not once per comparator.
    let mut parsed: Vec<_> = versions
        .drain(..)
        .map(|v| (scheme.parse(&v.version).ok(), v))
        .collect();
    parsed.sort_by(|(a, _), (b, _)| match (a, b) {
        (Some(a), Some(b)) => scheme.order(b, a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    versions.extend(parsed.into_iter().map(|(_, v)| v));
}
/// The longest prefix of `text` within `limit` bytes that ends on a char
/// boundary.
pub fn truncate(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
/// Lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
/// The child manifest an image index offers for `os`/`architecture`/
/// `variant`, when it has an exact platform match.
fn platform_manifest<'a>(
    manifests: &'a [Value],
    os: &str,
    architecture: &str,
    variant: Option<&str>,
) -> Option<&'a Value> {
    manifests.iter().find(|m| {
        m["platform"]["os"] == os
            && m["platform"]["architecture"] == architecture
            && variant.is_none_or(|v| m["platform"]["variant"] == v)
    })
}
pub fn escape_go(package: &str) -> Result<String> {
    let mut out = String::with_capacity(package.len());
    for c in package.chars() {
        if c.is_ascii_uppercase() {
            out.push('!');
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || matches!(c, '.' | '-' | '_' | '~' | '/')
        {
            out.push(c);
        } else {
            return Err(Error::Invalid);
        }
    }
    Ok(out)
}
