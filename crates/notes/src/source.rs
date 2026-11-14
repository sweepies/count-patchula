use super::*;
use std::{collections::BTreeMap, sync::LazyLock};

fn metadata(value: &Value) -> Option<Repository> {
    let repository = &value["repository"];
    let raw = repository.as_str().or_else(|| repository["url"].as_str());
    let mut source = raw
        .and_then(Repository::from_url)
        .or_else(|| value["homepage"].as_str().and_then(Repository::from_url))?;
    if let Some(directory) = repository["directory"].as_str().and_then(directory) {
        source.directory = Some(directory);
    }
    Some(source)
}

fn compact_npm(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    // Deserialize selected fields only: full packuments can be tens of MB.
    #[derive(Deserialize)]
    struct Metadata {
        repository: Option<Value>,
        homepage: Option<String>,
    }
    let value: Metadata = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&metadata(
        &serde_json::json!({"repository":value.repository,"homepage":value.homepage}),
    ))
    .map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NOTES_SOURCE_NPM: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-source-npm", 1, compact_npm);
fn compact_crate(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let value: Value = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&metadata(&value["crate"])).map_err(|_| patchula_http::CacheError::Document)
}
pub(crate) const NOTES_SOURCE_CRATES: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-source-crates", 1, compact_crate);
fn compact_pypi(
    body: &mut dyn std::io::Read,
) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let value: Value = patchula_http::conditional::json_document(body)
        .map_err(|_| patchula_http::CacheError::Document)?;
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
pub(crate) const NOTES_SOURCE_PYPI: patchula_http::Derived<patchula_http::CacheError> =
    patchula_http::Derived::new("notes-source-pypi", 1, compact_pypi);

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
        let releases = self.sources.mise_releases(tool, None).await?;
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
        let token = self.sources.mise_token().await;
        let backend = self
            .sources
            .mise_lookup()?
            .backend(tool, token.as_deref())
            .await?;
        let (source, package) = match backend.split_once(':') {
            Some(("npm", package)) => (Source::Npm, package),
            Some(("pypi", package)) => (Source::Pypi, package),
            Some(("cargo", package)) if !package.contains('/') => (Source::Crates, package),
            _ => return Ok(None),
        };
        Box::pin(self.source_repository(&PackageId::public(source, package), version)).await
    }
    /// Only called while rendering changed PRs. Registry metadata uses independent
    /// compact representations and the batch's conditional public HTTP cache.
    pub async fn source_repository(
        &self,
        id: &PackageId,
        version: &str,
    ) -> Result<Option<Repository>> {
        // Registry metadata is read by name from the public registry; a
        // package from its own is not the public package of its name.
        if !id.is_public() {
            return Ok(None);
        }
        let (source, package) = (id.source, id.name.as_str());
        if !patchula_datasource::package_name_safe(package) {
            return Err(Error::Source(patchula_datasource::Error::Invalid));
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
                    .sources
                    .http
                    .get(&format!("https://{package}?go-get=1"), "text/html")
                    .await?;
                let html = std::str::from_utf8(&response.body)
                    .map_err(|_| Error::Source(patchula_datasource::Error::Invalid))?;
                let found = vanity(html, package);
                // Verify the authoritative prefix as required by the Go protocol.
                if let Some((prefix, source)) = &found
                    && prefix != package
                {
                    let response = self
                        .sources
                        .http
                        .get(&format!("https://{prefix}?go-get=1"), "text/html")
                        .await?;
                    if vanity(
                        std::str::from_utf8(&response.body)
                            .map_err(|_| Error::Source(patchula_datasource::Error::Invalid))?,
                        prefix,
                    )
                    .as_ref()
                        != Some(&(prefix.clone(), source.clone()))
                    {
                        return Err(Error::Source(patchula_datasource::Error::Invalid));
                    }
                }
                return Ok(found.map(|(_, source)| source));
            }
            Source::Npm => {
                self.sources
                    .http
                    .get_derived(
                        &format!(
                            "https://registry.npmjs.org/{}/{}",
                            package.replace('/', "%2f"),
                            version
                        ),
                        "application/json",
                        &NOTES_SOURCE_NPM,
                    )
                    .await?
            }
            Source::Crates => {
                self.sources
                    .http
                    .get_derived(
                        &format!("https://crates.io/api/v1/crates/{package}"),
                        "application/json",
                        &NOTES_SOURCE_CRATES,
                    )
                    .await?
            }
            Source::Pypi => {
                self.sources
                    .http
                    .get_derived(
                        &format!("https://pypi.org/pypi/{package}/json"),
                        "application/json",
                        &NOTES_SOURCE_PYPI,
                    )
                    .await?
            }
            Source::Docker | Source::Oci => {
                return Ok(self.sources.image_source(package, version).await?);
            }
            Source::Helm => {
                let chart = self.sources.helm_chart(package).await?;
                return Ok(chart.notes.get(version).and_then(ChartNotes::repository));
            }
            _ => return Ok(None),
        };
        Ok(serde_json::from_slice(&response.body)?)
    }
}
