//! Batch-shared GitHub REST/GraphQL requests with conditional reads and rate limits.
#![forbid(unsafe_code)]

pub mod app;
mod credentials;
mod governor;
mod pagination;
pub use credentials::{Credentials, Token};
pub use governor::{PrimaryLimit, Stats};
pub use pagination::GraphqlPage;
pub use patchula_http::{Network, Response, Transport};

use governor::Governor;
use patchula_cache::{Cache, HttpEntry, HttpKey};
use reqwest::{
    Method, Request, StatusCode, Url,
    header::{self, HeaderValue},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid GitHub client options or token")]
    Configuration,
    #[error("invalid GitHub API URL or origin")]
    Url,
    #[error("GitHub HTTP transport failed")]
    Transport,
    #[error("GitHub offline fixture failed: {0}")]
    Fixture(String),
    #[error("GitHub cache operation failed")]
    Cache,
    #[error("GitHub returned HTTP {0}")]
    Status(u16),
    #[error("GitHub rate limit retry/wait budget exhausted")]
    RateLimit,
    #[error("GitHub response is invalid JSON")]
    Json,
    #[error("GitHub GraphQL returned errors")]
    Graphql,
    #[error("GitHub pagination cursor/link is invalid, repeated or exceeds page budget")]
    Pagination,
    #[error("GitHub returned 304 without a cached body")]
    MissingBody,
}

pub type Result<T> = std::result::Result<T, Error>;
type Derive = fn(&[u8]) -> std::result::Result<Vec<u8>, serde_json::Error>;

#[derive(Clone, Debug)]
pub struct Options {
    /// Shared REST+GraphQL in-flight cap, default 4.
    pub concurrency: usize,
    /// Retries after the initial attempt; applies only to documented rate limits.
    pub max_retries: u32,
    pub max_pages: usize,
    /// Fail instead of retrying early if a server-requested wait exceeds this.
    pub max_rate_wait: Duration,
    /// Supports GitHub Enterprise; HTTPS required, links must retain this origin.
    pub api_url: Url,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            concurrency: 4,
            max_retries: 3,
            max_pages: 1000,
            max_rate_wait: Duration::from_secs(3600),
            api_url: Url::parse("https://api.github.com/").expect("static GitHub API URL"),
        }
    }
}

struct Inner {
    credentials: Option<Arc<dyn Credentials>>,
    transport: Arc<dyn Transport>,
    cache: Cache,
    authorization: HeaderValue,
    credential_scope: String,
    options: Options,
    governor: Governor,
    decoding: Arc<tokio::sync::Semaphore>,
}

/// Clone this handle across the batch: token, connection pool, governor and stats
/// are shared. Never constructs a token or reads the process environment.
#[derive(Clone)]
pub struct Client(Arc<Inner>);

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitHubClient")
            .field("concurrency", &self.0.options.concurrency)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("patchula GitHub client")
    }
}

impl Client {
    /// Keep the connection pool, governor and cache namespace across token renewals.
    pub fn with_credentials(
        credentials: Arc<dyn Credentials>,
        cache: Cache,
        options: Options,
        transport: Arc<dyn Transport>,
    ) -> Result<Self> {
        if credentials.scope().is_empty() {
            return Err(Error::Configuration);
        }
        let mut client = Self::with_transport("installation", cache, options, transport)?;
        let inner = Arc::get_mut(&mut client.0).expect("new client");
        inner.credential_scope = Sha256::digest(credentials.scope().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        inner.credentials = Some(credentials);
        Ok(client)
    }
    pub fn new(token: &str, cache: Cache, options: Options) -> Result<Self> {
        Self::with_transport(
            token,
            cache,
            options,
            Arc::new(Network::new().map_err(|_| Error::Configuration)?),
        )
    }

    /// Injection point for offline replay. Does not construct a real Client.
    pub fn with_transport(
        token: &str,
        cache: Cache,
        options: Options,
        transport: Arc<dyn Transport>,
    ) -> Result<Self> {
        if token.is_empty()
            || token.chars().any(char::is_whitespace)
            || options.concurrency == 0
            || options.concurrency > 100
            || options.max_pages == 0
            || options.max_retries > 10
            || options.api_url.scheme() != "https"
            || options.api_url.host_str().is_none()
            || !options.api_url.username().is_empty()
            || options.api_url.password().is_some()
            || options.api_url.query().is_some()
            || options.api_url.fragment().is_some()
        {
            return Err(Error::Configuration);
        }
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::Configuration)?;
        authorization.set_sensitive(true);
        let credential_scope = Sha256::digest(token.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let governor = Governor::new(options.concurrency, options.max_rate_wait);
        Ok(Self(Arc::new(Inner {
            credentials: None,
            decoding: Arc::new(tokio::sync::Semaphore::new(4)),
            transport,
            cache,
            authorization,
            credential_scope,
            options,
            governor,
        })))
    }

    pub async fn stats(&self) -> Stats {
        self.0.governor.stats().await
    }

    pub fn concurrency(&self) -> usize {
        self.0.options.concurrency
    }

    /// The current token, for a tool that makes its own GitHub requests and is
    /// handed it explicitly. Refreshable credentials renew it.
    pub async fn token(&self) -> Result<String> {
        if let Some(credentials) = &self.0.credentials {
            return credentials.token().await;
        }
        self.0
            .authorization
            .to_str()
            .ok()
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_owned)
            .ok_or(Error::Configuration)
    }

    /// Public metadata reads share the transport pool but never GitHub auth.
    pub fn public_http(&self) -> patchula_http::Cached {
        patchula_http::Cached::new(
            self.0.cache.clone(),
            self.0.transport.clone(),
            self.concurrency(),
        )
        .expect("validated concurrency")
    }
    pub fn cache(&self) -> Cache {
        self.0.cache.clone()
    }
    pub fn versions_host_eligible(&self) -> bool {
        self.0.options.api_url.host_str() == Some("api.github.com")
    }

    /// One-way namespace for derived read hints; never the token itself.
    pub fn credential_scope(&self) -> &str {
        &self.0.credential_scope
    }

    fn url(&self, path: &str) -> Result<Url> {
        let url = self.0.options.api_url.join(path).map_err(|_| Error::Url)?;
        if url.origin() != self.0.options.api_url.origin()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Url);
        }
        Ok(url)
    }

    fn request(&self, method: Method, url: Url) -> Request {
        let mut request = Request::new(method, url);
        let headers = request.headers_mut();
        headers.insert(header::AUTHORIZATION, self.0.authorization.clone());
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static("2022-11-28"),
        );
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static(concat!("patchula/", env!("CARGO_PKG_VERSION"))),
        );
        request
    }

    fn key(&self, request: &Request, vary: impl IntoIterator<Item = String>) -> Result<HttpKey> {
        let mut headers = BTreeMap::new();
        for name in ["accept".to_owned(), "x-github-api-version".into()]
            .into_iter()
            .chain(vary)
        {
            if name == "*" {
                return Err(Error::Cache);
            }
            // Namespace handles authorization; never persist its plaintext.
            if matches!(name.as_str(), "authorization" | "cookie") {
                continue;
            }
            headers.insert(
                name.clone(),
                request
                    .headers()
                    .get(&name)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("")
                    .to_owned(),
            );
        }
        headers.insert(
            "x-patchula-credential-scope".into(),
            self.0.credential_scope.clone(),
        );
        HttpKey::new(request.method().as_str(), request.url().as_str(), headers)
            .map_err(|_| Error::Cache)
    }

    async fn exchange(&self, request: &Request, cached: bool) -> Result<Response> {
        let resource = if request.url().path().ends_with("/graphql") {
            "graphql"
        } else if request.url().path().starts_with("/search/") {
            "search"
        } else {
            "core"
        };
        let content = if resource == "graphql" {
            request
                .body()
                .and_then(|b| b.as_bytes())
                .and_then(|b| serde_json::from_slice::<Value>(b).ok())
                .and_then(|b| {
                    b["query"]
                        .as_str()
                        .map(|q| q.trim_start().starts_with("mutation"))
                })
                .unwrap_or(true)
        } else {
            !matches!(
                *request.method(),
                Method::GET | Method::HEAD | Method::OPTIONS
            )
        };
        for attempt in 0..=self.0.options.max_retries {
            let trace = patchula_http::RequestTrace::new(request.method().as_str(), request.url());
            trace.cache_policy(if cached { "always-revalidate" } else { "cold" }, None);
            let queued = std::time::Instant::now();
            let content_permit = if content {
                Some(self.0.governor.content().await?)
            } else {
                None
            };
            let permit = self.0.governor.acquire(resource).await?;
            self.0.governor.request();
            let mut outgoing = request.try_clone().ok_or(Error::Configuration)?;
            if let Some(credentials) = &self.0.credentials {
                let mut value =
                    HeaderValue::from_str(&format!("Bearer {}", credentials.token().await?))
                        .map_err(|_| Error::Configuration)?;
                value.set_sensitive(true);
                outgoing.headers_mut().insert(header::AUTHORIZATION, value);
            }
            let result = trace
                .send(
                    self.0.transport.as_ref(),
                    outgoing,
                    queued.elapsed(),
                    cached,
                )
                .await;
            let response = match result {
                Ok(response) => response,
                Err(patchula_http::Error::Fixture(message)) => {
                    return Err(Error::Fixture(message));
                }
                Err(_) => return Err(Error::Transport),
            };
            self.0.governor.observe(resource, &response).await;
            // Update the shared cooldown before releasing the in-flight permit.
            let retry = self
                .0
                .governor
                .rate_limited(resource, &response, attempt)
                .await?;
            drop(permit);
            drop(content_permit);
            if !retry {
                return Ok(response);
            }
            if attempt == self.0.options.max_retries {
                return Err(Error::RateLimit);
            }
            self.0.governor.retry();
        }
        Err(Error::RateLimit)
    }

    /// Every REST GET goes to the transport with cached validators when present.
    /// A 304 returns the original status/body with refreshed response metadata.
    pub async fn get(&self, path: &str) -> Result<Response> {
        self.get_representation(path, None).await
    }

    /// Authoritative read used for write preconditions. Always revalidates with
    /// the server; a 304 reuses only the representation the server validated.
    pub async fn get_fresh(&self, path: &str) -> Result<Value> {
        let response = self.get(path).await?;
        serde_json::from_slice(&response.body).map_err(|_| Error::Json)
    }

    /// Git Data writes do not enter the HTTP cache. Only documented rate-limit
    /// refusals are retried; transport/5xx failures may have written and must be
    /// reconciled by the next run rather than blindly repeated.
    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.mutate(Method::POST, path, body).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Result<Value> {
        self.mutate(Method::PATCH, path, body).await
    }
    pub async fn put(&self, path: &str, body: Value) -> Result<Value> {
        self.mutate(Method::PUT, path, body).await
    }
    async fn mutate(&self, method: Method, path: &str, body: Value) -> Result<Value> {
        let mut request = self.request(method, self.url(path)?);
        request.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        *request.body_mut() = Some(serde_json::to_vec(&body).map_err(|_| Error::Json)?.into());
        let response = self.exchange(&request, false).await?;
        if !response.status.is_success() {
            return Err(Error::Status(response.status.as_u16()));
        }
        if response.body.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&response.body).map_err(|_| Error::Json)
    }

    /// Conditional GET whose cached body contains only caller-selected fields.
    pub async fn get_derived(
        &self,
        path: &str,
        format: &'static str,
        derive: Derive,
    ) -> Result<Response> {
        self.get_representation(path, Some((format, derive))).await
    }

    async fn get_representation(
        &self,
        path: &str,
        derived: Option<(&'static str, Derive)>,
    ) -> Result<Response> {
        let mut url = self.url(path)?;
        let mut visited = HashSet::new();
        for _ in 0..10 {
            if !visited.insert(url.clone()) {
                return Err(Error::Pagination);
            }
            let mut request = self.request(Method::GET, url.clone());
            // Vary metadata is kept separately from representations so GET can
            // discover its vary key before applying stored validators.
            let representation = |key: HttpKey| match derived {
                Some((format, _)) => key.representation(format),
                None => key,
            };
            let base_key = representation(self.key(&request, [])?);
            let metadata = self
                .0
                .cache
                .http_get(&base_key)
                .await
                .map_err(|_| Error::Cache)?;
            let vary = metadata
                .as_ref()
                .and_then(|entry| entry.headers.get("vary"))
                .map(|vary| parse_vary(vary))
                .unwrap_or_default();
            let cache_key = representation(self.key(&request, vary)?);
            let cached = if cache_key == base_key {
                metadata
            } else {
                self.0
                    .cache
                    .http_get(&cache_key)
                    .await
                    .map_err(|_| Error::Cache)?
            };
            if let Some(entry) = &cached {
                for (name, value) in [
                    (header::IF_NONE_MATCH, &entry.etag),
                    (header::IF_MODIFIED_SINCE, &entry.last_modified),
                ] {
                    if let Some(value) = value {
                        request.headers_mut().insert(
                            name,
                            HeaderValue::from_str(value).map_err(|_| Error::Cache)?,
                        );
                    }
                }
            }
            let mut response = self.exchange(&request, cached.is_some()).await?;
            if matches!(response.status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                let location = response
                    .headers
                    .get(header::LOCATION)
                    .and_then(|header| header.to_str().ok())
                    .ok_or(Error::Url)?;
                let target = url.join(location).map_err(|_| Error::Url)?;
                url = self.url(target.as_str())?;
                continue;
            }
            let not_modified = response.status == StatusCode::NOT_MODIFIED;
            if not_modified {
                let cached = cached.ok_or(Error::MissingBody)?;
                self.0.governor.not_modified();
                let mut headers = response.headers;
                for (name, value) in cached.headers {
                    if let (Ok(name), Ok(value)) = (
                        header::HeaderName::from_bytes(name.as_bytes()),
                        HeaderValue::from_str(&value),
                    ) && !headers.contains_key(&name)
                    {
                        headers.insert(name, value);
                    }
                }
                response = Response {
                    status: StatusCode::from_u16(cached.status).map_err(|_| Error::Cache)?,
                    headers,
                    body: cached.body,
                };
            }
            if !response.status.is_success() {
                return Err(Error::Status(response.status.as_u16()));
            }
            if !not_modified && let Some((_, derive)) = derived {
                let permit = self
                    .0
                    .decoding
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| Error::Json)?;
                let body = std::mem::take(&mut response.body);
                response.body = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    derive(&body)
                })
                .await
                .map_err(|_| Error::Json)?
                .map_err(|_| Error::Json)?;
            }
            let control = comma_header(&response, "cache-control")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let vary = comma_header(&response, "vary")
                .as_deref()
                .map(parse_vary)
                .unwrap_or_default();
            if control.split(',').any(|part| part.trim() == "no-store")
                || vary.iter().any(|name| name == "*")
            {
                self.0
                    .cache
                    .http_remove(&cache_key)
                    .await
                    .map_err(|_| Error::Cache)?;
                self.0
                    .cache
                    .http_remove(&base_key)
                    .await
                    .map_err(|_| Error::Cache)?;
            } else {
                let entry = cache_entry(&response);
                let key = representation(self.key(&request, vary)?);
                if not_modified {
                    self.0
                        .cache
                        .http_refresh(&key, &entry)
                        .await
                        .map_err(|_| Error::Cache)?;
                } else {
                    self.0
                        .cache
                        .http_put(&key, &entry)
                        .await
                        .map_err(|_| Error::Cache)?;
                }
                // The client sends a fixed request-header set. All dynamic Vary
                // fields other than auth are absent/stable; the base copy keeps
                // discovery and paginated Link headers intact.
                if key != base_key {
                    if not_modified {
                        self.0
                            .cache
                            .http_refresh(&base_key, &entry)
                            .await
                            .map_err(|_| Error::Cache)?;
                    } else {
                        self.0
                            .cache
                            .http_put(&base_key, &entry)
                            .await
                            .map_err(|_| Error::Cache)?;
                    }
                }
            }
            return Ok(response);
        }
        Err(Error::Pagination)
    }

    pub async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let url = self.url("graphql")?;
        let mut request = self.request(Method::POST, url);
        request.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        *request.body_mut() = Some(
            serde_json::to_vec(&serde_json::json!({"query": query, "variables": variables}))
                .map_err(|_| Error::Json)?
                .into(),
        );
        let response = self.exchange(&request, false).await?;
        if !response.status.is_success() {
            return Err(Error::Status(response.status.as_u16()));
        }
        let json: Value = serde_json::from_slice(&response.body).map_err(|_| Error::Json)?;
        if json
            .get("errors")
            .is_some_and(|errors| !errors.as_array().is_some_and(Vec::is_empty))
        {
            return Err(Error::Graphql);
        }
        json.get("data")
            .filter(|data| !data.is_null())
            .cloned()
            .ok_or(Error::Graphql)
    }
}

fn parse_vary(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

pub(crate) fn text<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers.get(name)?.to_str().ok()
}

fn comma_header(response: &Response, name: &str) -> Option<String> {
    let values: Vec<_> = response
        .headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

fn cache_entry(response: &Response) -> HttpEntry {
    let fetched_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let cache_control = comma_header(response, "cache-control");
    let max_age = cache_control
        .as_deref()
        .into_iter()
        .flat_map(|value| value.split(','))
        .find_map(|part| {
            part.trim()
                .strip_prefix("max-age=")?
                .trim_matches('"')
                .parse::<i64>()
                .ok()
        })
        .filter(|age| *age >= 0);
    let expires_at = max_age
        .and_then(|age| fetched_at.checked_add(age))
        .or_else(|| {
            httpdate::parse_http_date(text(response, "expires")?)
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|value| value.as_secs() as i64)
        });
    // Store only metadata needed to reconstitute a GET, never cookies or bodies in logs.
    let headers = [
        "etag",
        "last-modified",
        "link",
        "cache-control",
        "expires",
        "vary",
        "content-type",
    ]
    .into_iter()
    .filter_map(|name| comma_header(response, name).map(|value| (name.into(), value)))
    .collect();
    HttpEntry {
        status: response.status.as_u16(),
        etag: text(response, "etag").map(str::to_owned),
        last_modified: text(response, "last-modified").map(str::to_owned),
        headers,
        body: response.body.clone(),
        fetched_at,
        expires_at,
        cache_control,
    }
}
