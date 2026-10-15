use super::*;
use reqwest::{Method, Url};
use rustovate_cache::{HttpEntry, HttpKey};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

type Token = Arc<Mutex<Option<(String, Instant)>>>;
pub(super) type Tokens = Arc<Mutex<BTreeMap<String, Token>>>;
const MANIFEST: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";

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
fn compact_tags(body: &[u8]) -> std::result::Result<Vec<u8>, rustovate_http::CacheError> {
    #[derive(Deserialize, Serialize)]
    struct Page {
        tags: Option<Vec<String>>,
    }
    let page: Page =
        serde_json::from_slice(body).map_err(|_| rustovate_http::CacheError::Document)?;
    serde_json::to_vec(&page).map_err(|_| rustovate_http::CacheError::Document)
}

pub(super) fn identity(package: &str) -> Result<(String, String)> {
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
    let host = if matches!(host.as_str(), "docker.io" | "index.docker.io") {
        "registry-1.docker.io".into()
    } else {
        host
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

impl Client {
    async fn registry(
        &self,
        host: &str,
        path: &str,
        url: &str,
        accept: &str,
        method: Method,
    ) -> Result<rustovate_http::Response> {
        let scope = format!("{host}/{path}");
        let authenticated = host == "registry-1.docker.io" && self.dockerhub_credentials.is_some();
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
            .registry_read(url, accept, bearer.as_deref(), method.clone())
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
                            && realm.host_str() == Some("auth.docker.io")))
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
                let credentials = self
                    .dockerhub_credentials
                    .as_deref()
                    .filter(|_| {
                        authenticated
                            && realm.host_str() == Some("auth.docker.io")
                            && realm.port_or_known_default() == Some(443)
                            && realm.path() == "/token"
                    })
                    .map(|(username, token)| (username.as_str(), token.as_str()));
                let auth = self
                    .http
                    .token_get_with_credentials(realm.as_str(), credentials)
                    .await?;
                if !auth.status.is_success() {
                    return Err(rustovate_http::CacheError::Status(auth.status.as_u16()).into());
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
            response = self.registry_read(url, accept, Some(&next), method).await?;
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
            return Err(rustovate_http::CacheError::Status(response.status.as_u16()).into());
        }
        let reference = if let Some((_, tag)) = url.split_once("/manifests/") {
            format!("digest:{tag}")
        } else {
            return Ok(response);
        };
        let mut stale = self.stale_registry.lock().await;
        let entries = stale.entry(format!("{host}/{path}")).or_default();
        if response.headers.contains_key("x-rustovate-stale") {
            if let Some(time) = response
                .headers
                .get("x-rustovate-fetched-at")
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
        method: Method,
    ) -> Result<rustovate_http::Response> {
        if method == Method::HEAD {
            return Ok(self.http.registry_head(url, accept, token).await?);
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
        } else {
            Ok(self.http.registry_get(url, accept, token).await?)
        }
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
            if response.headers.contains_key("x-rustovate-stale")
                && let Some(time) = response
                    .headers
                    .get("x-rustovate-fetched-at")
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
            Ok(_) | Err(Error::Http(rustovate_http::CacheError::Status(405 | 501))) => {
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
