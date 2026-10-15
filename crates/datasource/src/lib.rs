//! Shared registry lookups with conditional GETs and incremental GitHub history.
#![forbid(unsafe_code)]
mod compact;
mod docker;
mod github;
mod mise;
mod registries;
use rustovate_cache::{Cache, Release};
use rustovate_config::Source;
use rustovate_http::Cached;
use rustovate_versioning::{Ecosystem, Versioning};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Http(#[from] rustovate_http::CacheError),
    #[error(transparent)]
    Github(#[from] rustovate_github::Error),
    #[error(transparent)]
    Cache(#[from] rustovate_cache::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid package identity or upstream payload")]
    Invalid,
    #[error("data source {0:?} is not implemented in this wave")]
    Unsupported(Source),
    #[error("registry rate limit reached; retry after {0} seconds")]
    RateLimit(u64),
}
pub type Result<T> = std::result::Result<T, Error>;
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
#[derive(Clone)]
pub struct Client {
    pub(crate) github: rustovate_github::Client,
    pub(crate) http: Cached,
    pub(crate) cache: Cache,
    tokens: docker::Tokens,
}
impl Client {
    pub fn new(github: rustovate_github::Client, http: Cached, cache: Cache) -> Self {
        Self {
            github,
            http,
            cache,
            tokens: Default::default(),
        }
    }
    pub async fn lookup(&self, source: Source, package: &str) -> Result<Catalog> {
        if package.is_empty()
            || package.contains(['?', '#', '\\'])
            || package.split('/').any(|s| s == ".." || s == ".")
        {
            return Err(Error::Invalid);
        }
        match source {
            Source::Npm => self.npm(package).await,
            Source::GithubReleases | Source::GithubTags => {
                self.github_versions(source, package).await
            }
            Source::Crates => self.crates(package).await,
            Source::Go => self.go(package).await,
            Source::Pypi => self.pypi(package).await,
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
        }
    }
    async fn npm(&self, package: &str) -> Result<Catalog> {
        let encoded = package.replace('/', "%2f");
        let response = self
            .http
            .get_derived(
                &format!("https://registry.npmjs.org/{encoded}"),
                "application/json",
                "npm-catalog-v1",
                compact::npm,
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
                "crates-catalog-v1",
                compact::crates,
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
        // @latest can include a pseudo-version absent from @v/list.
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
        let latest_version = latest["Version"].as_str().ok_or(Error::Invalid)?;
        if !versions.iter().any(|v| v.version == latest_version) {
            versions.push(Version {
                version: latest_version.into(),
                published_at: latest["Time"].as_str().map(str::to_owned),
                digest: None,
                deprecated: false,
                yanked: false,
            });
        }
        sort(&mut versions, Ecosystem::Go);
        let known = self.cache.releases("go", package).await?;
        let known: std::collections::HashMap<_, _> = known
            .releases
            .into_iter()
            .map(|v| (v.version, v.published_at))
            .collect();
        // Fetch .info only for new releases; retain publication timestamps for
        // older ones. Never refetch full per-version history on a warm lookup.
        let mut reached_known = false;
        for version in &mut versions {
            if let Some(time) = known.get(&version.version) {
                version.published_at = time.clone();
                reached_known = true;
            } else if !reached_known {
                let info: Value = serde_json::from_slice(
                    &self
                        .http
                        .get(
                            &format!("{prefix}/{}.info", version.version),
                            "application/json",
                        )
                        .await?
                        .body,
                )?;
                version.published_at = info["Time"].as_str().map(str::to_owned);
            }
        }
        self.merge("go", package, &versions).await?;
        Ok(Catalog {
            versions,
            dist_tags: BTreeMap::from([("latest".into(), latest_version.into())]),
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
                "pypi-catalog-v1",
                compact::pypi,
            )
            .await;
        let response = match response {
            Ok(v) => v,
            Err(rustovate_http::CacheError::Status(404 | 406)) => {
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
