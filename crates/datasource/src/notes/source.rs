use super::*;
use std::{collections::BTreeMap, sync::LazyLock};

static UPSTREAMS: LazyLock<BTreeMap<String, BTreeMap<String, String>>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../upstreams.json")).expect("upstream map")
});

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Repository {
    pub url: String,
    pub github: Option<String>,
    /// A GitLab or Gitea/Forgejo upstream; GitHub upstreams use `github`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<Forge>,
    pub directory: Option<String>,
    pub image_version: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    Gitlab,
    /// Gitea and Forgejo (including Codeberg) share one API.
    Gitea,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forge {
    pub kind: ForgeKind,
    pub host: String,
    /// `group/subgroup/project` on GitLab, `owner/repo` on Gitea.
    pub path: String,
}
impl Forge {
    /// Recognise a forge from its host. Unknown self-hosted instances stay
    /// unsupported rather than being probed.
    fn detect(host: &str, segments: &[&str]) -> Option<Self> {
        let kind = if host == "gitlab.com" || host.split('.').any(|l| l == "gitlab") {
            ForgeKind::Gitlab
        } else if matches!(host, "codeberg.org" | "gitea.com")
            || host.split('.').any(|l| matches!(l, "gitea" | "forgejo"))
        {
            ForgeKind::Gitea
        } else {
            return None;
        };
        // GitLab paths may nest groups; `-` starts a project sub-route.
        let mut parts: Vec<_> = segments
            .iter()
            .take_while(|s| **s != "-")
            .copied()
            .collect();
        if kind == ForgeKind::Gitea {
            parts.truncate(2);
        }
        if let Some(last) = parts.last_mut() {
            *last = last.trim_end_matches(".git");
        }
        let valid = |p: &&str| {
            !p.is_empty()
                && p.len() <= 100
                && !matches!(*p, "." | "..")
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        };
        (parts.len() >= 2 && parts.iter().all(valid)).then(|| Self {
            kind,
            host: host.to_ascii_lowercase(),
            path: parts.join("/"),
        })
    }
    pub fn web(&self) -> String {
        format!("https://{}/{}", self.host, self.path)
    }
}

impl Repository {
    pub fn from_url(raw: &str) -> Option<Self> {
        let raw = raw.trim().trim_start_matches("git+");
        let raw = if let Some(path) = raw.strip_prefix("git@github.com:") {
            format!("https://github.com/{path}")
        } else if let Some((host, path)) = raw
            .strip_prefix("git@")
            .and_then(|rest| rest.split_once(':'))
            .filter(|(host, _)| !host.contains('/'))
        {
            format!("https://{host}/{path}")
        } else if let Some(path) = raw.strip_prefix("github:") {
            format!("https://github.com/{path}")
        } else if !raw.contains("://") && raw.split('/').count() == 2 {
            format!("https://github.com/{raw}")
        } else {
            raw.to_owned()
        };
        let mut url = reqwest::Url::parse(&raw).ok()?;
        url.set_query(None);
        url.set_fragment(None);
        if (!url.username().is_empty() && !(url.scheme() == "ssh" && url.username() == "git"))
            || url.password().is_some()
        {
            return None;
        }
        if url.host_str() == Some("github.com") {
            let parts: Vec<_> = url.path_segments()?.take(2).collect();
            if parts.len() != 2
                || parts.iter().any(|p| {
                    p.is_empty()
                        || p.len() > 100
                        || !p
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                        || matches!(*p, "." | "..")
                })
            {
                return None;
            }
            let repo = format!("{}/{}", parts[0], parts[1].trim_end_matches(".git"));
            Some(Self {
                url: format!("https://github.com/{repo}"),
                github: Some(repo),
                ..Self::default()
            })
        } else if matches!(url.scheme(), "https" | "http" | "git" | "ssh") {
            let segments: Vec<_> = url.path_segments().into_iter().flatten().collect();
            let forge = url
                .host_str()
                .and_then(|host| Forge::detect(host, &segments));
            Some(Self {
                url: forge.as_ref().map_or_else(|| url.to_string(), Forge::web),
                forge,
                ..Self::default()
            })
        } else {
            None
        }
    }
}

fn directory(raw: &str) -> Option<String> {
    (!raw.is_empty()
        && !raw.starts_with('/')
        && raw.split('/').all(|s| !matches!(s, "" | "." | ".."))
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-@".contains(&b)))
    .then(|| raw.into())
}

fn metadata(value: &Value) -> Option<Repository> {
    let repository = &value["repository"];
    let raw = repository.as_str().or_else(|| repository["url"].as_str());
    let mut source = raw
        .and_then(Repository::from_url)
        .or_else(|| value["homepage"].as_str().and_then(Repository::from_url))?;
    source.directory = repository["directory"].as_str().and_then(directory);
    Some(source)
}

fn compact_npm(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    // Deserialize selected fields only: full packuments can be tens of MB.
    #[derive(Deserialize)]
    struct Metadata {
        repository: Option<Value>,
        homepage: Option<String>,
    }
    let value: Metadata =
        serde_json::from_slice(bytes).map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&metadata(
        &serde_json::json!({"repository":value.repository,"homepage":value.homepage}),
    ))
    .map_err(|_| patchula_http::CacheError::Document)
}
fn compact_crate(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&metadata(&value["crate"])).map_err(|_| patchula_http::CacheError::Document)
}
fn compact_pypi(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| patchula_http::CacheError::Document)?;
    let urls = value["info"]["project_urls"].as_object();
    let mut source = None;
    // Case insensitive; prefer a repository over documentation/changelog sites.
    for key in [
        "source",
        "source code",
        "repository",
        "code",
        "github",
        "changelog",
        "changes",
    ] {
        source = urls
            .and_then(|urls| urls.iter().find(|(name, _)| name.eq_ignore_ascii_case(key)))
            .and_then(|(_, v)| v.as_str())
            .and_then(Repository::from_url);
        if source.is_some() {
            break;
        }
    }
    source = source.or_else(|| {
        value["info"]["home_page"]
            .as_str()
            .and_then(Repository::from_url)
    });
    serde_json::to_vec(&source).map_err(|_| patchula_http::CacheError::Document)
}

pub(crate) fn mapped(group: &str, name: &str) -> Option<Repository> {
    Repository::from_url(UPSTREAMS.get(group)?.get(name)?)
}

pub(super) fn vanity(html: &str, package: &str) -> Option<(String, Repository)> {
    static META: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<meta\s+([^>]+)>").unwrap());
    static ATTR: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?is)([\w-]+)\s*=\s*["']([^"']*)["']"#).unwrap());
    for tag in META.captures_iter(html) {
        let attrs: BTreeMap<_, _> = ATTR
            .captures_iter(&tag[1])
            .map(|a| (a[1].to_ascii_lowercase(), a[2].to_owned()))
            .collect();
        if attrs
            .get("name")
            .is_none_or(|v| !v.eq_ignore_ascii_case("go-import"))
        {
            continue;
        }
        let parts: Vec<_> = attrs.get("content")?.split_whitespace().collect();
        if parts.len() >= 3
            && parts[1] == "git"
            && (package == parts[0]
                || package
                    .strip_prefix(parts[0])
                    .is_some_and(|v| v.starts_with('/')))
        {
            return Some((parts[0].into(), Repository::from_url(parts[2])?));
        }
    }
    None
}

impl Client {
    /// The upstream of a mise tool. Core tools have a fixed one. Otherwise the
    /// release URL mise lists for the version names it. Failing that, the
    /// project's registry names it when mise's backend for the tool is npm,
    /// PyPI or crates.io.
    async fn mise_source(&self, tool: &str, version: &str) -> Result<Option<Repository>> {
        if let Some(source) = mapped("mise-core", tool.trim_start_matches("core:")) {
            return Ok(Some(source));
        }
        let releases = self.mise_releases(tool, None).await?;
        let url = |release: &patchula_mise::Release| release.release_url.clone();
        if let Some(source) = releases
            .iter()
            .find(|release| release.version == version)
            .and_then(url)
            .or_else(|| releases.iter().rev().find_map(url))
            .and_then(|url| Repository::from_url(&url))
        {
            return Ok(Some(source));
        }
        let token = self.mise_token().await;
        let backend = self.mise_lookup()?.backend(tool, token.as_deref()).await?;
        let (source, package) = match backend.split_once(':') {
            Some(("npm", package)) => (Source::Npm, package),
            Some(("pypi", package)) => (Source::Pypi, package),
            Some(("cargo", package)) if !package.contains('/') => (Source::Crates, package),
            _ => return Ok(None),
        };
        Box::pin(self.source_repository(source, package, version)).await
    }
    /// Only called while rendering changed PRs. Registry metadata uses independent
    /// compact representations and the batch's conditional public HTTP cache.
    pub async fn source_repository(
        &self,
        source: Source,
        package: &str,
        version: &str,
    ) -> Result<Option<Repository>> {
        if package.is_empty()
            || package.contains(['?', '#', '\\'])
            || package.split('/').any(|p| matches!(p, "." | ".."))
        {
            return Err(Error::Invalid);
        }
        let response = match source {
            Source::GithubReleases | Source::GithubTags => {
                return Ok(Repository::from_url(&format!(
                    "https://github.com/{package}"
                )));
            }
            Source::Mise => return self.mise_source(package, version).await,
            Source::Go => {
                if package.starts_with("github.com/") {
                    let mut repository = Repository::from_url(&format!("https://{package}"));
                    let mut subpath: Vec<_> = package.split('/').skip(3).collect();
                    if subpath.last().is_some_and(|part| {
                        part.strip_prefix('v')
                            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                    }) {
                        subpath.pop();
                    }
                    if let Some(repository) = &mut repository {
                        repository.directory = directory(&subpath.join("/"));
                    }
                    return Ok(repository);
                }
                if let Some(name) = package
                    .strip_prefix("golang.org/x/")
                    .and_then(|s| s.split('/').next())
                {
                    // Official Go project mirrors: https://go.dev/doc/contribute#go-contrib
                    return Ok(Repository::from_url(&format!(
                        "https://github.com/golang/{name}"
                    )));
                }
                let response = self
                    .http
                    .get(&format!("https://{package}?go-get=1"), "text/html")
                    .await?;
                let html = std::str::from_utf8(&response.body).map_err(|_| Error::Invalid)?;
                let found = vanity(html, package);
                // Verify the authoritative prefix as required by the Go protocol.
                if let Some((prefix, source)) = &found
                    && prefix != package
                {
                    let response = self
                        .http
                        .get(&format!("https://{prefix}?go-get=1"), "text/html")
                        .await?;
                    if vanity(
                        std::str::from_utf8(&response.body).map_err(|_| Error::Invalid)?,
                        prefix,
                    )
                    .as_ref()
                        != Some(&(prefix.clone(), source.clone()))
                    {
                        return Err(Error::Invalid);
                    }
                }
                return Ok(found.map(|(_, source)| source));
            }
            Source::Npm => {
                self.http
                    .get_derived(
                        &format!(
                            "https://registry.npmjs.org/{}/{}",
                            package.replace('/', "%2f"),
                            version
                        ),
                        "application/json",
                        "notes-source-npm-v1",
                        compact_npm,
                    )
                    .await?
            }
            Source::Crates => {
                self.http
                    .get_derived(
                        &format!("https://crates.io/api/v1/crates/{package}"),
                        "application/json",
                        "notes-source-crates-v1",
                        compact_crate,
                    )
                    .await?
            }
            Source::Pypi => {
                self.http
                    .get_derived(
                        &format!("https://pypi.org/pypi/{package}/json"),
                        "application/json",
                        "notes-source-pypi-v1",
                        compact_pypi,
                    )
                    .await?
            }
            Source::Docker | Source::Oci => return self.image_source(package, version).await,
            _ => return Ok(None),
        };
        Ok(serde_json::from_slice(&response.body)?)
    }
}
