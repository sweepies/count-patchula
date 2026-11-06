use super::*;
use patchula_cache::{HttpEntry, HttpKey};
use reqwest::{Method, Url};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

type Token = Arc<Mutex<Option<(String, Instant)>>>;
pub(super) type Tokens = Arc<Mutex<BTreeMap<String, Token>>>;
pub(crate) const MANIFEST: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

pub(crate) fn validate_digest(digest: &str) -> Result<()> {
    if digest
        .strip_prefix("sha256:")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
/// What release notes read from a config blob: an image's source and version
/// labels, and a Helm chart's `sources` and Artifact Hub changes.
fn compact_config(bytes: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| patchula_http::CacheError::Document)?;
    let labels = &value["config"]["Labels"];
    serde_json::to_vec(&serde_json::json!({
        "org.opencontainers.image.source": labels["org.opencontainers.image.source"],
        "org.opencontainers.image.version": labels["org.opencontainers.image.version"],
        "chart": notes::ChartNotes::from_metadata(&value),
    }))
    .map_err(|_| patchula_http::CacheError::Document)
}

/// The config of one image or chart version, as release notes read it.
struct ImageConfig {
    /// The compact config blob from `compact_config`.
    config: Value,
    /// The manifest's `org.opencontainers.image.source` annotation.
    annotated_source: Option<String>,
    /// Whether the manifest's config is a Helm chart's.
    chart: bool,
}
impl ImageConfig {
    fn chart_notes(&self) -> notes::ChartNotes {
        serde_json::from_value(self.config["chart"].clone()).unwrap_or_default()
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn token_key(host: &str, path: &str) -> Result<HttpKey> {
    Ok(HttpKey::new(
        "GET",
        &format!("https://{host}/v2/{path}/"),
        BTreeMap::new(),
    )?
    .representation("anonymous-registry-pull-token-v1"))
}
fn compact_tags(body: &[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError> {
    #[derive(Deserialize, Serialize)]
    struct Page {
        tags: Option<Vec<String>>,
    }
    let page: Page =
        serde_json::from_slice(body).map_err(|_| patchula_http::CacheError::Document)?;
    serde_json::to_vec(&page).map_err(|_| patchula_http::CacheError::Document)
}

pub(crate) fn identity(package: &str) -> Result<(String, String)> {
    let package = package.strip_prefix("oci://").unwrap_or(package);
    let (first, rest) = package.split_once('/').unwrap_or((package, ""));
    let (host, path) = if first.contains('.') || first.contains(':') {
        (first.to_owned(), rest.to_owned())
    } else {
        (
            "registry-1.docker.io".into(),
            if rest.is_empty() {
                format!("library/{package}")
            } else {
                package.into()
            },
        )
    };
    let (host, path) = if matches!(host.as_str(), "docker.io" | "index.docker.io") {
        // `docker.io/busybox` is the official image `library/busybox`.
        let path = if path.contains('/') {
            path
        } else {
            format!("library/{path}")
        };
        ("registry-1.docker.io".to_string(), path)
    } else {
        (host, path)
    };
    if path.is_empty()
        || !path
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"/._-".contains(&c))
        || path.split('/').any(str::is_empty)
    {
        return Err(Error::Invalid);
    }
    let url = Url::parse(&format!("https://{host}/")).map_err(|_| Error::Invalid)?;
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Invalid);
    }
    Ok((host, path))
}

pub(crate) type Derive = fn(&[u8]) -> std::result::Result<Vec<u8>, patchula_http::CacheError>;

impl Client {
    async fn registry(
        &self,
        host: &str,
        path: &str,
        url: &str,
        accept: &str,
        method: Method,
    ) -> Result<patchula_http::Response> {
        self.registry_with(host, path, url, accept, method, None)
            .await
    }

    /// A registry read whose blob body is derived by "derived" instead of
    /// the default config compaction release notes read.
    pub(crate) async fn registry_with(
        &self,
        host: &str,
        path: &str,
        url: &str,
        accept: &str,
        method: Method,
        derived: Option<(&'static str, Derive)>,
    ) -> Result<patchula_http::Response> {
        // An operator entry covering the registry makes every token an account's:
        // kept in memory only, never in the shared cache, and per entry.
        let entry = self
            .http
            .credential(url)
            .filter(|entry| entry.auth != patchula_http::Auth::Anonymous)
            .cloned();
        let scope = format!(
            "{host}/{path}#{}",
            entry.as_ref().map_or("", patchula_http::Credential::scope)
        );
        let authenticated = entry.is_some();
        // A token is an account's only when the entry sends credentials for
        // it; a withheld entry's is fetched without any.
        let account = entry.as_ref().is_some_and(|entry| entry.auth.sends());
        let token = self.tokens.lock().await.entry(scope).or_default().clone();
        let key = token_key(host, path)?;
        let mut saved = token.lock().await;
        if saved
            .as_ref()
            .is_none_or(|(_, expiry)| *expiry <= Instant::now())
        {
            *saved = None;
            if !authenticated
                && let Some(entry) = self.cache.http_get(&key).await?
                && let Some(expiry) = entry.expires_at.filter(|expiry| *expiry > now())
                && let Ok(bearer) = String::from_utf8(entry.body)
            {
                *saved = Some((
                    bearer,
                    Instant::now() + Duration::from_secs((expiry - now()).max(0) as u64),
                ));
            }
        }
        let bearer = saved
            .as_ref()
            .filter(|(_, expiry)| *expiry > Instant::now())
            .map(|(v, _)| v.clone());
        drop(saved);
        let mut response = self
            .registry_read(
                url,
                accept,
                bearer.as_deref(),
                account,
                method.clone(),
                derived,
            )
            .await?;
        if response.status == 401 {
            let mut saved_token = token.lock().await;
            let refreshed = saved_token
                .as_ref()
                .filter(|(v, expiry)| *expiry > Instant::now() && Some(v) != bearer.as_ref())
                .map(|(v, _)| v.clone());
            let next = if let Some(refreshed) = refreshed {
                refreshed
            } else {
                self.cache.http_remove(&key).await?;
                *saved_token = None;
                let challenge = response
                    .headers
                    .get("www-authenticate")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(Error::Invalid)?;
                let raw = challenge
                    .strip_prefix("Bearer ")
                    .or_else(|| challenge.strip_prefix("bearer "))
                    .ok_or(Error::Invalid)?;
                let fields: BTreeMap<_, _> = raw
                    .split(',')
                    .filter_map(|v| v.trim().split_once('='))
                    .map(|(k, v)| (k, v.trim_matches('"')))
                    .collect();
                let mut realm = Url::parse(fields.get("realm").ok_or(Error::Invalid)?)
                    .map_err(|_| Error::Invalid)?;
                if realm.scheme() != "https"
                    || !realm.username().is_empty()
                    || realm.password().is_some()
                    || !(realm.host_str() == Some(host.split(':').next().unwrap_or(host))
                        || (host == "registry-1.docker.io"
                            && realm.host_str() == Some("auth.docker.io"))
                        // GitLab registries take tokens from their GitLab
                        // instance: registry.gitlab.com from gitlab.com.
                        || (realm.host_str() == host.strip_prefix("registry.")
                            && realm.path() == "/jwt/auth"))
                {
                    return Err(Error::Invalid);
                }
                realm.set_query(None);
                if let Some(service) = fields.get("service") {
                    realm.query_pairs_mut().append_pair("service", service);
                }
                realm
                    .query_pairs_mut()
                    .append_pair("scope", &format!("repository:{path}:pull"));
                let requested_at = now();
                // The registry entry's basic credentials may go to a token service
                // on the registry's own origin, Docker Hub's auth.docker.io, or
                // gitlab.com for its registry; any other needs its own entry.
                let trusted_realm = Url::parse(&format!("https://{host}/"))
                    .is_ok_and(|registry| registry.origin() == realm.origin())
                    || (realm.port_or_known_default() == Some(443)
                        && ((host == "registry-1.docker.io"
                            && realm.host_str() == Some("auth.docker.io")
                            && realm.path() == "/token")
                            || (host == "registry.gitlab.com"
                                && realm.host_str() == Some("gitlab.com")
                                && realm.path() == "/jwt/auth")));
                // Only a registry with its own entry gets an account token, so its
                // answers are always cached under that entry, never anonymously.
                let realm_entry = self.http.credential(realm.as_str()).cloned();
                let source = entry
                    .as_ref()
                    .and_then(|entry| realm_entry.as_ref().or(trusted_realm.then_some(entry)));
                let credentials = match source.map(|entry| &entry.auth) {
                    Some(patchula_http::Auth::Basic { username, password }) => {
                        Some((username.as_str(), password.as_str()))
                    }
                    _ => None,
                };
                let auth = self
                    .http
                    .token_get_with_credentials(realm.as_str(), credentials)
                    .await?;
                if !auth.status.is_success() {
                    return Err(patchula_http::CacheError::Status(auth.status.as_u16()).into());
                }
                let value: Value = serde_json::from_slice(&auth.body)?;
                let bearer = value["token"]
                    .as_str()
                    .or_else(|| value["access_token"].as_str())
                    .ok_or(Error::Invalid)?
                    .to_owned();
                // Anonymous pull-only credentials, scoped to this registry and image.
                // Honor issued_at and HTTP Age so a cached token cannot outlive issuance.
                let issued = value["issued_at"]
                    .as_str()
                    .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                    .map_or(requested_at, |v| v.timestamp().min(requested_at));
                let age = auth
                    .headers
                    .get("age")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0)
                    .max(0);
                let expires_at = issued
                    .saturating_add(value["expires_in"].as_u64().unwrap_or(60).min(3600) as i64)
                    .saturating_sub(age)
                    .saturating_sub(10);
                let expiry = Instant::now()
                    + Duration::from_secs(expires_at.saturating_sub(now()).max(0) as u64);
                if !authenticated
                    && expires_at > now()
                    && !auth
                        .headers
                        .get("cache-control")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| {
                            v.split(',')
                                .any(|v| v.trim().eq_ignore_ascii_case("no-store"))
                        })
                {
                    self.cache
                        .http_put(
                            &key,
                            &HttpEntry {
                                status: 200,
                                etag: None,
                                last_modified: None,
                                headers: BTreeMap::new(),
                                body: bearer.as_bytes().to_vec(),
                                fetched_at: requested_at,
                                expires_at: Some(expires_at),
                                cache_control: None,
                            },
                        )
                        .await?;
                }
                *saved_token = Some((bearer.clone(), expiry));
                bearer
            };
            drop(saved_token);
            response = self
                .registry_read(url, accept, Some(&next), account, method, derived)
                .await?;
            if response.status == 401 {
                *token.lock().await = None;
                self.cache.http_remove(&key).await?;
            }
        }
        if response.status == 429 {
            return Err(Error::RateLimit(
                response
                    .headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(60),
            ));
        }
        if !response.status.is_success() {
            return Err(patchula_http::CacheError::Status(response.status.as_u16()).into());
        }
        let reference = if let Some((_, tag)) = url.split_once("/manifests/") {
            format!("digest:{tag}")
        } else {
            return Ok(response);
        };
        let mut stale = self.stale_registry.lock().await;
        let entries = stale.entry(format!("{host}/{path}")).or_default();
        if response.headers.contains_key("x-patchula-stale") {
            if let Some(time) = response
                .headers
                .get("x-patchula-fetched-at")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
            {
                entries.insert(reference, time);
            }
        } else {
            entries.remove(&reference);
        }
        Ok(response)
    }

    async fn registry_read(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
        account: bool,
        method: Method,
        derived: Option<(&'static str, Derive)>,
    ) -> Result<patchula_http::Response> {
        let token = token.map(|token| patchula_http::Bearer { token, account });
        if method == Method::HEAD {
            return Ok(self.http.registry_head(url, accept, token).await?);
        }
        if let Some((format, derive)) = derived {
            return Ok(self
                .http
                .registry_get_derived(url, accept, token, format, derive)
                .await?);
        }
        if Url::parse(url)
            .map_err(|_| Error::Invalid)?
            .path()
            .ends_with("/tags/list")
        {
            Ok(self
                .http
                .registry_get_derived(url, accept, token, "oci-tags-v1", compact_tags)
                .await?)
        } else if url.contains("/blobs/") {
            Ok(self
                .http
                .registry_get_derived(url, accept, token, "oci-config-notes-v1", compact_config)
                .await?)
        } else {
            Ok(self.http.registry_get(url, accept, token).await?)
        }
    }

    pub(super) async fn image_source(
        &self,
        package: &str,
        version: &str,
    ) -> Result<Option<notes::Repository>> {
        let (host, path) = identity(package)?;
        let fallback = (host == "registry-1.docker.io")
            .then(|| {
                path.strip_prefix("library/")
                    .and_then(|name| notes::mapped("docker-official", name))
            })
            .flatten();
        let image = match self.image_config(&host, &path, version).await {
            Ok(image) => image,
            Err(_) if fallback.is_some() => return Ok(fallback),
            Err(error) => return Err(error),
        };
        if image.chart {
            // `helm push` copies the chart's first `sources` entry into the
            // manifest annotation; the config holds the whole list.
            return Ok(image.chart_notes().repository().or_else(|| {
                image
                    .annotated_source
                    .as_deref()
                    .and_then(notes::Repository::from_url)
                    .map(notes::Repository::chart_sourced)
            }));
        }
        let labels = &image.config;
        // Official images annotate their Dockerfile repository, not the
        // project's; the table of official images comes first.
        let mut source = labels["org.opencontainers.image.source"]
            .as_str()
            .and_then(notes::Repository::from_url)
            .or(fallback)
            .or_else(|| {
                image
                    .annotated_source
                    .as_deref()
                    .and_then(notes::Repository::from_url)
            });
        if let Some(source) = &mut source {
            source.image_version = labels["org.opencontainers.image.version"]
                .as_str()
                .map(str::to_owned);
        }
        Ok(source)
    }

    /// A Helm chart version's notes metadata, or `None` when the artifact at
    /// that tag is not a Helm chart.
    pub(super) async fn oci_chart(
        &self,
        package: &str,
        version: &str,
    ) -> Result<Option<notes::ChartNotes>> {
        let (host, path) = identity(package)?;
        let image = self.image_config(&host, &path, version).await?;
        Ok(image.chart.then(|| image.chart_notes()))
    }

    async fn image_config(&self, host: &str, path: &str, version: &str) -> Result<ImageConfig> {
        if version.is_empty()
            || !version
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || b"._-+:".contains(&v))
        {
            return Err(Error::Invalid);
        }
        // OCI tags cannot hold `+`: `helm push` writes build metadata as `_`,
        // which a catalog of OCI versions reads back as `+`.
        let mut reference = version.replace('+', "_");
        // An index may carry the annotation for all its platforms; a
        // platform's own manifest overrides it.
        let mut annotated_source = None;
        for _ in 0..4 {
            let response = self
                .registry(
                    host,
                    path,
                    &format!("https://{host}/v2/{path}/manifests/{reference}"),
                    MANIFEST,
                    Method::GET,
                )
                .await?;
            let manifest: Value = serde_json::from_slice(&response.body)?;
            if let Some(source) =
                manifest["annotations"]["org.opencontainers.image.source"].as_str()
            {
                annotated_source = Some(source.to_owned());
            }
            if let Some(digest) = manifest["config"]["digest"].as_str() {
                validate_digest(digest)?;
                let response = self
                    .registry(
                        host,
                        path,
                        &format!("https://{host}/v2/{path}/blobs/{digest}"),
                        "application/json",
                        Method::GET,
                    )
                    .await?;
                return Ok(ImageConfig {
                    config: serde_json::from_slice(&response.body)?,
                    annotated_source,
                    chart: manifest["config"]["mediaType"]
                        == "application/vnd.cncf.helm.config.v1+json",
                });
            }
            let manifests = manifest["manifests"].as_array().ok_or(Error::Invalid)?;
            let child = manifests
                .iter()
                .find(|m| {
                    m["platform"]["os"] == "linux" && m["platform"]["architecture"] == "amd64"
                })
                .or_else(|| {
                    manifests.iter().find(|m| {
                        m["platform"]["os"]
                            .as_str()
                            .is_some_and(|os| os != "unknown")
                    })
                })
                .ok_or(Error::Invalid)?;
            reference = child["digest"].as_str().ok_or(Error::Invalid)?.into();
            validate_digest(&reference)?;
        }
        Err(Error::Invalid)
    }

    pub(super) async fn docker(&self, package: &str) -> Result<Catalog> {
        let (host, path) = identity(package)?;
        let mut url = Url::parse(&format!("https://{host}/v2/{path}/tags/list?n=1000"))
            .map_err(|_| Error::Invalid)?;
        let origin = url.origin();
        let mut seen = HashSet::new();
        let mut tags = std::collections::BTreeSet::new();
        let mut stale_at: Option<i64> = None;
        loop {
            if !seen.insert(url.to_string()) || seen.len() > 10000 {
                return Err(Error::Invalid);
            }
            let response = self
                .registry(&host, &path, url.as_str(), "application/json", Method::GET)
                .await?;
            if response.headers.contains_key("x-patchula-stale")
                && let Some(time) = response
                    .headers
                    .get("x-patchula-fetched-at")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<i64>().ok())
            {
                stale_at = Some(stale_at.map_or(time, |old| old.min(time)));
            }
            #[derive(Deserialize)]
            struct Page {
                tags: Option<Vec<String>>,
            }
            let page: Page = serde_json::from_slice(&response.body)?;
            let before = tags.len();
            tags.extend(page.tags.unwrap_or_default());
            let next = response
                .headers
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.split(',')
                        .find(|v| v.contains("rel=\"next\"") || v.contains("rel=next"))
                })
                .and_then(|v| v.split_once('<')?.1.split_once('>').map(|v| v.0));
            let Some(next) = next else {
                break;
            };
            if tags.len() == before {
                return Err(Error::Invalid);
            }
            let next = url.join(next).map_err(|_| Error::Invalid)?;
            if next.origin() != origin
                || next.path() != url.path()
                || !next.username().is_empty()
                || next.password().is_some()
            {
                return Err(Error::Invalid);
            }
            url = next;
        }
        // Lexical pages cannot stop at a known tag: new tags may appear anywhere.
        let mut catalog = Catalog {
            versions: tags
                .into_iter()
                .map(|version| Version {
                    version,
                    published_at: None,
                    digest: None,
                    deprecated: false,
                    yanked: false,
                })
                .collect(),
            dist_tags: BTreeMap::new(),
        };
        sort(&mut catalog.versions, Ecosystem::Docker);
        let mut stale = self.stale_registry.lock().await;
        let entries = stale.entry(format!("{host}/{path}")).or_default();
        if let Some(time) = stale_at {
            entries.insert("catalog".into(), time);
        } else {
            entries.remove("catalog");
        }
        Ok(catalog)
    }

    /// Retain the multi-arch index digest, never a child platform digest.
    pub async fn registry_digest(&self, package: &str, tag: &str) -> Result<String> {
        if tag.is_empty()
            || !tag
                .bytes()
                .all(|v| v.is_ascii_alphanumeric() || b"._-+:".contains(&v))
        {
            return Err(Error::Invalid);
        }
        let tag = tag.replace('+', "_");
        let (host, path) = identity(package)?;
        let url = format!("https://{host}/v2/{path}/manifests/{tag}");
        let response = match self
            .registry(&host, &path, &url, MANIFEST, Method::HEAD)
            .await
        {
            Ok(response) if response.headers.contains_key("docker-content-digest") => response,
            Ok(_) | Err(Error::Http(patchula_http::CacheError::Status(405 | 501))) => {
                self.registry(&host, &path, &url, MANIFEST, Method::GET)
                    .await?
            }
            Err(error) => return Err(error),
        };
        use sha2::{Digest, Sha256};
        let digest = response
            .headers
            .get("docker-content-digest")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "sha256:{}",
                    Sha256::digest(&response.body)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                )
            });
        if !digest
            .strip_prefix("sha256:")
            .is_some_and(|v| v.len() == 64 && v.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(Error::Invalid);
        }
        Ok(digest)
    }
}

#[cfg(test)]
mod identity_tests {
    #[test]
    fn docker_hub_names_resolve_to_one_identity() {
        for name in [
            "busybox",
            "docker.io/busybox",
            "index.docker.io/library/busybox",
        ] {
            assert_eq!(
                super::identity(name).unwrap(),
                (
                    "registry-1.docker.io".to_string(),
                    "library/busybox".to_string()
                ),
                "{name}"
            );
        }
        assert_eq!(
            super::identity("docker.io/bitnami/redis").unwrap().1,
            "bitnami/redis"
        );
    }
}
