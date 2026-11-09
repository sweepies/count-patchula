//! Batch-shared GitHub REST/GraphQL requests with conditional reads and rate limits.
#![forbid(unsafe_code)]

pub mod app;
mod blobs;
mod credentials;
mod governor;
mod pagination;
pub use credentials::{Credentials, Token};
pub use governor::{PrimaryLimit, Stats};
pub use pagination::GraphqlPage;
pub use patchula_http::{Network, Response, Transport};

use governor::Governor;
use patchula_cache::Cache;
use patchula_http::{Derived, conditional};
use reqwest::{
    Method, Request, StatusCode, Url,
    header::{self, HeaderValue},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fmt, sync::Arc, time::Duration};

const ACCEPT: &str = "application/vnd.github+json";
const API_VERSION: &str = "2022-11-28";
const USER_AGENT: &str = concat!("patchula/", env!("CARGO_PKG_VERSION"));
/// The representation-selecting headers every GitHub GET sends; they are the
/// cache key's `sent` headers.
pub(crate) const SENT: &[(&str, &str)] =
    &[("accept", ACCEPT), ("x-github-api-version", API_VERSION)];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid GitHub client options or token")]
    Configuration,
    #[error("invalid GitHub API URL or origin")]
    Url,
    #[error("GitHub HTTP transport failed")]
    Transport,
    #[error("GitHub cache operation failed")]
    Cache,
    #[error("GitHub returned HTTP {0}")]
    Status(u16),
    #[error("GitHub rate limit retry/wait budget exhausted")]
    RateLimit,
    #[error("GitHub response is invalid JSON")]
    Json,
    /// Carries GitHub's `errors[].message` values, joined and truncated; empty
    /// when GitHub sent none.
    #[error("GitHub GraphQL returned errors{}", graphql_detail(.0))]
    Graphql(String),
    #[error("GitHub pagination cursor/link is invalid, repeated or exceeds page budget")]
    Pagination,
    #[error("GitHub returned 304 without a cached body")]
    MissingBody,
}

pub type Result<T> = std::result::Result<T, Error>;

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
    /// `None` for refreshable credentials: `exchange` swaps in a fresh token
    /// per request, so a path that skipped the swap sends no Authorization at
    /// all rather than a placeholder.
    authorization: Option<HeaderValue>,
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
        Self::check_options(&options)?;
        // `exchange` swaps in a fresh token per request, so requests carry no
        // Authorization until then; a path that skipped the swap gets a 401.
        let credential_scope = Sha256::digest(credentials.scope().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Self::assemble(
            Some(credentials),
            None,
            credential_scope,
            options,
            cache,
            transport,
        ))
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
        if token.is_empty() || token.chars().any(char::is_whitespace) {
            return Err(Error::Configuration);
        }
        Self::check_options(&options)?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::Configuration)?;
        authorization.set_sensitive(true);
        let credential_scope = Sha256::digest(authorization.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self::assemble(
            None,
            Some(authorization),
            credential_scope,
            options,
            cache,
            transport,
        ))
    }

    fn check_options(options: &Options) -> Result<()> {
        if options.concurrency == 0
            || options.concurrency > patchula_http::MAX_CONCURRENCY
            || options.max_pages == 0
            || options.max_retries > 10
            || !valid_api_url(&options.api_url)
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }

    fn assemble(
        credentials: Option<Arc<dyn Credentials>>,
        authorization: Option<HeaderValue>,
        credential_scope: String,
        options: Options,
        cache: Cache,
        transport: Arc<dyn Transport>,
    ) -> Self {
        let mut options = options;
        options.api_url = api_root(&options.api_url);
        let governor = Governor::new(options.concurrency, options.max_rate_wait);
        Self(Arc::new(Inner {
            credentials,
            decoding: Arc::new(tokio::sync::Semaphore::new(4)),
            transport,
            cache,
            authorization,
            credential_scope,
            options,
            governor,
        }))
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
            .as_ref()
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::to_owned)
            .ok_or(Error::Configuration)
    }

    /// Public metadata reads share the transport pool but never GitHub auth.
    pub fn public_http(&self, options: patchula_http::Options) -> patchula_http::Cached {
        patchula_http::Cached::with_options(
            self.0.cache.clone(),
            self.0.transport.clone(),
            patchula_http::Options {
                default_host_concurrency: self.concurrency(),
                ..options
            },
        )
        .expect("validated options")
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

    /// The configured API root every request path resolves against.
    pub fn api_url(&self) -> &Url {
        &self.0.options.api_url
    }

    fn url(&self, path: &str) -> Result<Url> {
        join_api(&self.0.options.api_url, path)
    }

    fn request(&self, method: Method, url: Url) -> Request {
        let mut request = Request::new(method, url);
        let headers = request.headers_mut();
        if let Some(authorization) = &self.0.authorization {
            headers.insert(header::AUTHORIZATION, authorization.clone());
        }
        headers.insert(header::ACCEPT, HeaderValue::from_static(ACCEPT));
        headers.insert(
            "x-github-api-version",
            HeaderValue::from_static(API_VERSION),
        );
        headers.insert(header::USER_AGENT, HeaderValue::from_static(USER_AGENT));
        request
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
        self.get_representation(path, None, Read::Revalidate)
            .await?
            .ok_or(Error::MissingBody)
    }

    /// A GET whose answer never changes, such as a Git blob by its SHA: a
    /// cached success is returned without asking GitHub again. The cache key
    /// carries the credential scope, so another credential never reads it.
    pub async fn get_immutable(&self, path: &str) -> Result<Response> {
        self.get_representation(path, None, Read::Immutable)
            .await?
            .ok_or(Error::MissingBody)
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
        derived: &Derived<serde_json::Error>,
    ) -> Result<Response> {
        self.get_representation(path, Some(derived), Read::Revalidate)
            .await?
            .ok_or(Error::MissingBody)
    }

    async fn get_representation(
        &self,
        path: &str,
        derived: Option<&Derived<serde_json::Error>>,
        read: Read,
    ) -> Result<Option<Response>> {
        let mut url = self.url(path)?;
        let mut visited = HashSet::new();
        for _ in 0..10 {
            if !visited.insert(url.clone()) {
                return Err(Error::Pagination);
            }
            let mut request = self.request(Method::GET, url.clone());
            // One key per request: the client sends a fixed header set, so any
            // Vary the server names selects the same values every time.
            let key = conditional::key(
                "GET",
                url.as_str(),
                SENT,
                derived.map(|derived| derived.representation()).as_deref(),
                Some(&self.0.credential_scope),
            )
            .map_err(|_| Error::Cache)?;
            let cached = self
                .0
                .cache
                .http_get(&key)
                .await
                .map_err(|_| Error::Cache)?;
            if read != Read::Revalidate
                && let Some(entry) = &cached
                && (200..300).contains(&entry.status)
            {
                return Ok(Some(
                    conditional::response(entry).map_err(|_| Error::Cache)?,
                ));
            }
            if read == Read::Cached {
                return Ok(None);
            }
            if let Some(entry) = &cached {
                conditional::add_validators(&mut request, entry).map_err(|_| Error::Cache)?;
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
                response = conditional::not_modified(response, cached).map_err(|_| Error::Cache)?;
            }
            if !response.status.is_success() {
                return Err(Error::Status(response.status.as_u16()));
            }
            if !not_modified && let Some(derived) = derived {
                response.body = conditional::derive(
                    std::mem::take(&mut response.body),
                    derived.derive,
                    &self.0.decoding,
                )
                .await
                .map_err(|_| Error::Json)?
                .map_err(|_| Error::Json)?;
            }
            if conditional::storable(&response, true) {
                let entry = conditional::entry(&response, conditional::unix_now());
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
            } else {
                self.0
                    .cache
                    .http_remove(&key)
                    .await
                    .map_err(|_| Error::Cache)?;
            }
            return Ok(Some(response));
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
            return Err(Error::Graphql(graphql_messages(&json)));
        }
        json.get("data")
            .filter(|data| !data.is_null())
            .cloned()
            .ok_or_else(|| Error::Graphql(String::new()))
    }
}

/// How a GET treats what the cache holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Read {
    /// Ask GitHub, with the cached validators.
    Revalidate,
    /// Return a cached success without asking: the answer cannot change.
    Immutable,
    /// Return a cached success, or nothing, without asking.
    Cached,
}

/// Longest GraphQL error text kept in `Error::Graphql`, in characters.
const GRAPHQL_MESSAGE_LIMIT: usize = 300;

/// GitHub's `errors[].message` values joined with "; ", cut to the limit.
fn graphql_messages(response: &Value) -> String {
    let messages: Vec<&str> = response["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|error| error["message"].as_str())
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .collect();
    let joined = messages.join("; ");
    match joined.char_indices().nth(GRAPHQL_MESSAGE_LIMIT) {
        Some((end, _)) => format!("{}…", &joined[..end]),
        None => joined,
    }
}

fn graphql_detail(messages: &str) -> String {
    if messages.is_empty() {
        String::new()
    } else {
        format!(": {messages}")
    }
}

pub(crate) fn text<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers.get(name)?.to_str().ok()
}

/// Whether `url` is a usable API root: HTTPS, a host, no credentials, query
/// or fragment.
/// An API base as a directory URL, so relative paths join under it:
/// `https://ghe.example/api/v3` and `https://ghe.example/api/v3/` are the same root.
pub(crate) fn api_root(url: &Url) -> Url {
    let mut root = url.clone();
    if !root.path().ends_with('/') {
        root.set_path(&format!("{}/", root.path()));
    }
    root
}

pub(crate) fn valid_api_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}

/// Resolve `path` against an API base, refusing credentials, fragments and
/// cross-origin escapes.
pub(crate) fn join_api(base: &Url, path: &str) -> Result<Url> {
    let url = base.join(path).map_err(|_| Error::Url)?;
    if url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Url);
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn graphql_messages_are_joined_and_truncated() {
        let response = json!({"errors":[{"message":"one"},{"type":"X"},{"message":" two "}]});
        assert_eq!(graphql_messages(&response), "one; two");
        let long = "é".repeat(GRAPHQL_MESSAGE_LIMIT + 50);
        let cut = graphql_messages(&json!({"errors":[{"message":long}]}));
        assert_eq!(cut.chars().count(), GRAPHQL_MESSAGE_LIMIT + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(graphql_messages(&json!({"errors":[{}]})), "");
        assert_eq!(
            Error::Graphql(String::new()).to_string(),
            "GitHub GraphQL returned errors"
        );
    }
}
