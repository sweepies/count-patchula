//! Conditional, unauthenticated registry GETs sharing the batch transport pool.
use crate::{RequestTrace, Response, Transport};
use patchula_cache::{Cache, HttpEntry, HttpKey};
use reqwest::{
    Method, Request, StatusCode, Url,
    header::{self, HeaderValue},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Batch HTTP policy, independent of repository parallelism.
#[derive(Clone, Debug)]
pub struct Options {
    pub default_host_concurrency: usize,
    pub host_concurrency: BTreeMap<String, usize>,
    /// Freshness for validator-free OCI tag pages. Zero restores upstream policy.
    pub registry_freshness_secs: u64,
    /// Minimum freshness for public Go proxy lists/latest. Zero disables it.
    pub go_freshness_secs: u64,
    /// Bounded OCI 429 retries; waits above 60 seconds are returned to the caller.
    pub registry_retries: u32,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            default_host_concurrency: 16,
            host_concurrency: BTreeMap::new(),
            registry_freshness_secs: 3600,
            go_freshness_secs: 3600,
            registry_retries: 3,
        }
    }
}
impl Options {
    pub fn host_limit(&self, host: &str) -> usize {
        self.host_concurrency
            .get(host)
            .copied()
            .unwrap_or(self.default_host_concurrency)
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.registry_retries > 5
            || !(1..=100).contains(&self.default_host_concurrency)
            || self.host_concurrency.iter().any(|(host, limit)| {
                !(1..=100).contains(limit)
                    || !Url::parse(&format!("https://{host}/")).is_ok_and(|url| {
                        url.host_str() == Some(host.as_str())
                            && url.as_str() == format!("https://{host}/")
                    })
            })
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid registry document")]
    Document,
    #[error("registry catalog worker failed")]
    Worker,
    #[error("invalid registry URL or header")]
    Configuration,
    #[error("registry HTTP {0}")]
    Status(u16),
    #[error("registry returned 304 without cached body")]
    MissingBody,
    #[error(transparent)]
    Transport(#[from] crate::Error),
    #[error(transparent)]
    Cache(#[from] patchula_cache::Error),
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats {
    pub requests: u64,
    pub not_modified: u64,
    pub cache_hits: u64,
}
struct Inner {
    transport: Arc<dyn Transport>,
    cache: Cache,
    hosts: Mutex<BTreeMap<String, Arc<Host>>>,
    options: Options,
    decoding: Arc<Semaphore>,
    requests: AtomicU64,
    not_modified: AtomicU64,
    cache_hits: AtomicU64,
    derived_reads: tokio::sync::Mutex<BTreeMap<String, Coalesced>>,
}
struct Host {
    semaphore: Arc<Semaphore>,
    cooldown: Mutex<Option<Instant>>,
    pull_cooldown: Mutex<Option<Instant>>,
}
type Coalesced = Arc<tokio::sync::Mutex<Option<(Instant, Response)>>>;
type Derive = fn(&[u8]) -> Result<Vec<u8>, Error>;
/// How long a digest-addressed registry document stays fresh: ten years, so
/// only the cache's size budget evicts it.
const IMMUTABLE_SECS: u64 = 10 * 365 * 24 * 3600;
/// How long a registry's 404 for a digest or a cosign tag is kept.
const MISSING_SECS: i64 = 24 * 3600;

/// A registry bearer token, and whether it was issued to an account rather
/// than anonymously.
#[derive(Clone, Copy)]
pub struct Bearer<'a> {
    pub token: &'a str,
    pub account: bool,
}

/// A manifest or blob addressed by digest, which never changes.
fn immutable(url: &Url) -> bool {
    url.path().contains("/manifests/sha256:") || url.path().contains("/blobs/sha256:")
}
#[derive(Clone)]
pub struct Cached(Arc<Inner>, bool);
impl Cached {
    /// A scoped manual retry revalidates cached documents, preserving validators,
    /// shared governors and quotas without changing ordinary callers' freshness.
    pub fn revalidating(&self) -> Self {
        Self(self.0.clone(), true)
    }
    /// Whether this handle is a scoped manual retry.
    pub fn is_revalidating(&self) -> bool {
        self.1
    }
    pub fn new(
        cache: Cache,
        transport: Arc<dyn Transport>,
        concurrency: usize,
    ) -> Result<Self, Error> {
        if concurrency == 0 {
            return Err(Error::Configuration);
        }
        Self::with_options(
            cache,
            transport,
            Options {
                // Compatibility constructor: the supplied cap now applies per host.
                default_host_concurrency: concurrency,
                ..Default::default()
            },
        )
    }
    pub fn with_options(
        cache: Cache,
        transport: Arc<dyn Transport>,
        options: Options,
    ) -> Result<Self, Error> {
        options.validate()?;
        Ok(Self(
            Arc::new(Inner {
                transport,
                cache,
                hosts: Default::default(),
                options,
                decoding: Arc::new(Semaphore::new(4)),
                requests: AtomicU64::new(0),
                not_modified: AtomicU64::new(0),
                cache_hits: AtomicU64::new(0),
                derived_reads: Default::default(),
            }),
            false,
        ))
    }
    fn host(&self, url: &Url) -> Result<Arc<Host>, Error> {
        let host = url.host_str().ok_or(Error::Configuration)?;
        Ok(self
            .0
            .hosts
            .lock()
            .map_err(|_| Error::Configuration)?
            .entry(host.to_owned())
            .or_insert_with(|| {
                Arc::new(Host {
                    semaphore: Arc::new(Semaphore::new(self.0.options.host_limit(host))),
                    cooldown: Mutex::new(None),
                    pull_cooldown: Mutex::new(None),
                })
            })
            .clone())
    }
    async fn acquire(&self, url: &Url, pull: bool) -> Result<OwnedSemaphorePermit, Error> {
        let host = self.host(url)?;
        loop {
            let permit = host
                .semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| Error::Configuration)?;
            let mut until = *host.cooldown.lock().map_err(|_| Error::Configuration)?;
            if pull {
                until = until.max(
                    *host
                        .pull_cooldown
                        .lock()
                        .map_err(|_| Error::Configuration)?,
                );
            }
            if let Some(delay) =
                until.and_then(|until| until.checked_duration_since(Instant::now()))
            {
                drop(permit);
                if delay > Duration::from_secs(60) {
                    return Err(Error::Status(429));
                }
                tokio::time::sleep(delay).await;
            } else {
                return Ok(permit);
            }
        }
    }
    fn cooldown(&self, url: &Url, delay: Duration, pull: bool) -> Result<(), Error> {
        let host = self.host(url)?;
        let mut until = if pull {
            &host.pull_cooldown
        } else {
            &host.cooldown
        }
        .lock()
        .map_err(|_| Error::Configuration)?;
        let now = Instant::now();
        let next = now
            .checked_add(delay)
            .unwrap_or_else(|| now + Duration::from_secs(3600));
        *until = Some(until.map_or(next, |old| old.max(next)));
        Ok(())
    }
    pub fn stats(&self) -> Stats {
        Stats {
            requests: self.0.requests.load(Ordering::Relaxed),
            not_modified: self.0.not_modified.load(Ordering::Relaxed),
            cache_hits: self.0.cache_hits.load(Ordering::Relaxed),
        }
    }
    pub async fn get(&self, url: &str, accept: &str) -> Result<Response, Error> {
        self.get_representation(url, accept, None, None, Method::GET)
            .await
    }

    /// An uncached JSON request, for APIs whose answers carry no validators.
    /// It shares the host's concurrency limit and the batch's counters.
    pub async fn json(
        &self,
        method: Method,
        url: &str,
        body: Option<Vec<u8>>,
    ) -> Result<Response, Error> {
        let url = Url::parse(url).map_err(|_| Error::Configuration)?;
        if url.scheme() != "https" || url.host_str().is_none() {
            return Err(Error::Configuration);
        }
        let trace = RequestTrace::new(method.as_str(), &url);
        let mut request = Request::new(method, url.clone());
        let headers = request.headers_mut();
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static(concat!("patchula/", env!("CARGO_PKG_VERSION"))),
        );
        if let Some(body) = body {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            *request.body_mut() = Some(body.into());
        }
        let queued = Instant::now();
        let _permit = self.acquire(&url, false).await?;
        self.0.requests.fetch_add(1, Ordering::Relaxed);
        let response = trace
            .send(self.0.transport.as_ref(), request, queued.elapsed(), false)
            .await?;
        if !response.status.is_success() {
            return Err(Error::Status(response.status.as_u16()));
        }
        Ok(response)
    }

    /// Public registry content; bearer tokens never enter the persisted key.
    pub async fn registry_get(
        &self,
        url: &str,
        accept: &str,
        token: Option<Bearer<'_>>,
    ) -> Result<Response, Error> {
        self.registry_representation(url, accept, token, None, Method::GET)
            .await
    }
    /// Manifest version checks need only headers; HEAD has its own cache key.
    pub async fn registry_head(
        &self,
        url: &str,
        accept: &str,
        token: Option<Bearer<'_>>,
    ) -> Result<Response, Error> {
        self.registry_representation(url, accept, token, None, Method::HEAD)
            .await
    }
    pub async fn registry_get_derived(
        &self,
        url: &str,
        accept: &str,
        token: Option<Bearer<'_>>,
        format: &'static str,
        derive: Derive,
    ) -> Result<Response, Error> {
        self.registry_representation(url, accept, token, Some((format, derive)), Method::GET)
            .await
    }
    async fn registry_representation(
        &self,
        url: &str,
        accept: &str,
        token: Option<Bearer<'_>>,
        derived: Option<(&'static str, Derive)>,
        method: Method,
    ) -> Result<Response, Error> {
        let started = Instant::now();
        let key = format!(
            "registry:{method}:{}:{}:{url}\n{accept}\n{}:{}",
            token.is_some(),
            token.is_some_and(|t| t.account),
            derived.map_or("", |(format, _)| format),
            self.1
        );
        let read = self
            .0
            .derived_reads
            .lock()
            .await
            .entry(key)
            .or_default()
            .clone();
        let mut previous = read.lock().await;
        if let Some((finished, response)) = &*previous
            && *finished >= started
        {
            return Ok(response.clone());
        }
        let response = self
            .get_representation(url, accept, derived, Some(token), method)
            .await?;
        if response.status.is_success() {
            *previous = Some((Instant::now(), response.clone()));
        }
        Ok(response)
    }

    /// Fetch an anonymous token without caching the raw response. The registry
    /// caller may persist only its scoped bearer with a validated expiry.
    pub async fn token_get(&self, url: &str) -> Result<Response, Error> {
        self.token_get_with_credentials(url, None).await
    }

    /// Credentials are used only for the validated token realm and never cached.
    pub async fn token_get_with_credentials(
        &self,
        url: &str,
        credentials: Option<(&str, &str)>,
    ) -> Result<Response, Error> {
        let url = Url::parse(url).map_err(|_| Error::Configuration)?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Configuration);
        }
        let trace = RequestTrace::new("GET", &url);
        let mut request = Request::new(Method::GET, url.clone());
        if let Some((username, password)) = credentials {
            use base64::Engine;
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
            let mut value = HeaderValue::from_str(&format!("Basic {encoded}"))
                .map_err(|_| Error::Configuration)?;
            value.set_sensitive(true);
            request.headers_mut().insert(header::AUTHORIZATION, value);
        }
        request
            .headers_mut()
            .insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        let queued = Instant::now();
        let _permit = self.acquire(&url, false).await?;
        self.0.requests.fetch_add(1, Ordering::Relaxed);
        Ok(trace
            .send(self.0.transport.as_ref(), request, queued.elapsed(), false)
            .await?)
    }

    async fn stale_registry(
        &self,
        cached: Option<&HttpEntry>,
        url: &Url,
        accept: &str,
        method: &Method,
    ) -> Result<Option<Response>, Error> {
        // A previous GET also proves a manifest digest if HEAD has not yet been cached.
        let fallback = if cached.is_none() && method == Method::HEAD {
            self.0
                .cache
                .http_get(&HttpKey::new(
                    "GET",
                    url.as_str(),
                    BTreeMap::from([("accept".into(), accept.into())]),
                )?)
                .await?
        } else {
            None
        };
        let Some(entry) = cached.or(fallback.as_ref()) else {
            return Ok(None);
        };
        let mut response = cached_response(entry)?;
        response
            .headers
            .insert("x-patchula-stale", HeaderValue::from_static("rate-limit"));
        response.headers.insert(
            "x-patchula-fetched-at",
            HeaderValue::from_str(&entry.fetched_at.to_string())
                .map_err(|_| Error::Configuration)?,
        );
        self.0.cache_hits.fetch_add(1, Ordering::Relaxed);
        Ok(Some(response))
    }

    /// Derive once on a 200. Fresh hits and 304s contain only the compact body;
    /// validators always refer to the original upstream representation.
    pub async fn get_derived(
        &self,
        url: &str,
        accept: &str,
        format: &'static str,
        derive: Derive,
    ) -> Result<Response, Error> {
        let started = Instant::now();
        let key = format!("{url}\n{accept}\n{format}:{}", self.1);
        let read = self
            .0
            .derived_reads
            .lock()
            .await
            .entry(key)
            .or_default()
            .clone();
        let mut previous = read.lock().await;
        // Coalesce overlapping callers even when upstream requires revalidation.
        // A later sequential call still observes normal HTTP freshness rules.
        if let Some((finished, response)) = &*previous
            && *finished >= started
        {
            return Ok(response.clone());
        }
        let response = self
            .get_representation(url, accept, Some((format, derive)), None, Method::GET)
            .await?;
        *previous = Some((Instant::now(), response.clone()));
        Ok(response)
    }

    async fn get_representation(
        &self,
        url: &str,
        accept: &str,
        derived: Option<(&'static str, Derive)>,
        registry: Option<Option<Bearer<'_>>>,
        method: Method,
    ) -> Result<Response, Error> {
        let url = Url::parse(url).map_err(|_| Error::Configuration)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::Configuration);
        }
        let mut key = HttpKey::new(
            method.as_str(),
            url.as_str(),
            BTreeMap::from([("accept".into(), accept.into())]),
        )?;
        if let Some((format, _)) = derived {
            key = key.representation(format);
        }
        let mut trace = RequestTrace::new(method.as_str(), &url);
        let cached = self.0.cache.http_get(&key).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let no_cache = cached.as_ref().is_some_and(|entry| {
            entry.cache_control.as_deref().is_some_and(|c| {
                c.split(',')
                    .any(|p| p.trim().eq_ignore_ascii_case("no-cache"))
            })
        });
        let ttl = cached
            .as_ref()
            .and_then(|entry| {
                let floor = if registry.is_some()
                    && immutable(&url)
                    && StatusCode::from_u16(entry.status).is_ok_and(|s| s.is_success())
                {
                    IMMUTABLE_SECS
                } else if registry.is_some()
                    && url.path().ends_with("/tags/list")
                    && entry.etag.is_none()
                    && entry.last_modified.is_none()
                {
                    self.0.options.registry_freshness_secs
                } else if url.host_str() == Some("proxy.golang.org")
                    && (url.path().ends_with("/@v/list") || url.path().ends_with("/@latest"))
                {
                    self.0.options.go_freshness_secs
                } else {
                    0
                };
                let local = (floor > 0).then(|| {
                    entry
                        .fetched_at
                        .saturating_add(floor.min(i64::MAX as u64) as i64)
                });
                entry.expires_at.into_iter().chain(local).max()
            })
            .map(|expiry| expiry.saturating_sub(now));
        trace.cache_policy(
            if cached.is_none() {
                "cold"
            } else if no_cache {
                "no-cache"
            } else if ttl.is_none() {
                "no-expiry"
            } else if ttl.is_some_and(|ttl| ttl > 0) {
                "fresh"
            } else {
                "expired"
            },
            ttl,
        );
        if let Some(entry) = &cached
            && ttl.is_some_and(|ttl| ttl > 0)
            && !no_cache
            && !self.1
        {
            self.0.cache_hits.fetch_add(1, Ordering::Relaxed);
            if entry.status == 404 {
                return Err(Error::Status(404));
            }
            let mut headers = header::HeaderMap::new();
            for (name, value) in &entry.headers {
                headers.insert(
                    header::HeaderName::from_bytes(name.as_bytes())
                        .map_err(|_| Error::Configuration)?,
                    HeaderValue::from_str(value).map_err(|_| Error::Configuration)?,
                );
            }
            let response = Response {
                status: StatusCode::from_u16(entry.status).map_err(|_| Error::Configuration)?,
                headers,
                body: entry.body.clone(),
            };
            trace.fresh(&response);
            return Ok(response);
        }
        let mut request = Request::new(method.clone(), url.clone());
        if let Some(Some(Bearer { token, .. })) = registry {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| Error::Configuration)?;
            value.set_sensitive(true);
            request.headers_mut().insert(header::AUTHORIZATION, value);
        }
        request.headers_mut().insert(
            header::ACCEPT,
            HeaderValue::from_str(accept).map_err(|_| Error::Configuration)?,
        );
        request.headers_mut().insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, br, zstd"),
        );
        request.headers_mut().insert(
            header::USER_AGENT,
            HeaderValue::from_static(concat!("patchula/", env!("CARGO_PKG_VERSION"))),
        );
        if let Some(entry) = &cached {
            self.0.cache_hits.fetch_add(1, Ordering::Relaxed);
            for (name, value) in [
                (header::IF_NONE_MATCH, &entry.etag),
                (header::IF_MODIFIED_SINCE, &entry.last_modified),
            ] {
                if let Some(value) = value {
                    request.headers_mut().insert(
                        name,
                        HeaderValue::from_str(value).map_err(|_| Error::Configuration)?,
                    );
                }
            }
        }
        let mut attempt = 0;
        let mut redirects = BTreeSet::from([url.to_string()]);
        let mut response = loop {
            let queued = Instant::now();
            let target = request.url().clone();
            let pull_request = registry.is_some()
                && method == Method::GET
                && target.host_str() == Some("registry-1.docker.io")
                && target.path().contains("/manifests/");
            let permit = match self.acquire(&target, pull_request).await {
                Ok(permit) => permit,
                Err(Error::Status(429)) if registry.is_some() => {
                    if let Some(response) = self
                        .stale_registry(cached.as_ref(), &url, accept, &method)
                        .await?
                    {
                        return Ok(response);
                    }
                    return Err(Error::Status(429));
                }
                Err(error) => return Err(error),
            };
            self.0.requests.fetch_add(1, Ordering::Relaxed);
            let response = trace
                .send(
                    self.0.transport.as_ref(),
                    request.try_clone().ok_or(Error::Configuration)?,
                    queued.elapsed(),
                    cached.is_some(),
                )
                .await?;
            if matches!(response.status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                let next = response
                    .headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| target.join(v).ok())
                    .ok_or(Error::Configuration)?;
                if next.scheme() != "https"
                    || next.host_str().is_none()
                    || !next.username().is_empty()
                    || next.password().is_some()
                    || redirects.len() > 5
                    || !redirects.insert(next.to_string())
                {
                    return Err(Error::Configuration);
                }
                if next.origin() != target.origin() {
                    for name in [
                        header::AUTHORIZATION,
                        header::COOKIE,
                        header::PROXY_AUTHORIZATION,
                    ] {
                        request.headers_mut().remove(name);
                    }
                }
                request.url_mut().clone_from(&next);
                trace = RequestTrace::new(method.as_str(), &next);
                continue;
            }
            if registry.is_some()
                && target.host_str() == Some("registry-1.docker.io")
                && let Some(window) = exhausted_quota(&response)
            {
                // Docker's pull counter is also returned on successful HEADs.
                // Only manifest GETs consume it; blocking HEADs here makes the
                // available digest pins depend on which response finishes first.
                self.cooldown(&target, window, true)?;
            }
            if registry.is_some() && response.status == StatusCode::TOO_MANY_REQUESTS {
                let pull_quota = target.host_str() == Some("registry-1.docker.io")
                    && (response
                        .headers
                        .get("ratelimit-remaining")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.split(';').next().is_some_and(|v| v.trim() == "0"))
                        || response
                            .body
                            .windows(b"pull rate limit".len())
                            .any(|v| v.eq_ignore_ascii_case(b"pull rate limit"))
                        || !response.headers.contains_key("retry-after"));
                let delay = if pull_quota {
                    None
                } else {
                    retry_delay(&response, attempt)
                };
                // Publish even an exhausted/long cooldown before releasing the
                // permit, so unrelated queued requests to this host respect it.
                if let Some(delay) = delay {
                    self.cooldown(&target, delay, false)?;
                } else if pull_quota && exhausted_quota(&response).is_none() {
                    self.cooldown(&target, Duration::from_secs(21600), pull_request)?;
                } else if pull_quota && !pull_request {
                    // An actual HEAD/tag-list 429 is evidence of throttling
                    // those requests too, unlike a successful zero-counter HEAD.
                    self.cooldown(&target, exhausted_quota(&response).unwrap(), false)?;
                }
                drop(permit);
                if let Some(response) = self
                    .stale_registry(cached.as_ref(), &url, accept, &method)
                    .await?
                {
                    return Ok(response);
                }
                if attempt < self.0.options.registry_retries
                    && delay.is_some_and(|delay| delay <= Duration::from_secs(60))
                {
                    attempt += 1;
                    trace = RequestTrace::new(method.as_str(), &target);
                    trace.cache_policy("rate-retry", ttl);
                    continue;
                }
            }
            break response;
        };
        // These are local provenance headers, never trusted from the network.
        response.headers.remove("x-patchula-stale");
        response.headers.remove("x-patchula-fetched-at");
        if registry.is_some()
            && response.status == StatusCode::TOO_MANY_REQUESTS
            && let Some(response) = self
                .stale_registry(cached.as_ref(), &url, accept, &method)
                .await?
        {
            return Ok(response);
        }
        let not_modified = response.status == StatusCode::NOT_MODIFIED;
        if not_modified {
            let entry = cached.ok_or(Error::MissingBody)?;
            self.0.not_modified.fetch_add(1, Ordering::Relaxed);
            response.status =
                StatusCode::from_u16(entry.status).map_err(|_| Error::Configuration)?;
            response.body = entry.body;
            for (name, value) in entry.headers {
                if !response.headers.contains_key(&name) {
                    response.headers.insert(
                        header::HeaderName::from_bytes(name.as_bytes())
                            .map_err(|_| Error::Configuration)?,
                        HeaderValue::from_str(&value).map_err(|_| Error::Configuration)?,
                    );
                }
            }
        }
        if registry.is_some() && matches!(response.status.as_u16(), 401 | 429) {
            return Ok(response);
        }
        if !response.status.is_success() {
            // A digest or cosign tag the registry lacks is asked about again
            // a day later, not on every run.
            if response.status == StatusCode::NOT_FOUND
                && registry.is_some()
                && (immutable(&url) || [".att", ".sbom"].iter().any(|s| url.path().ends_with(s)))
            {
                self.0
                    .cache
                    .http_put(
                        &key,
                        &HttpEntry {
                            status: 404,
                            etag: None,
                            last_modified: None,
                            headers: BTreeMap::new(),
                            body: Vec::new(),
                            fetched_at: now,
                            expires_at: Some(now.saturating_add(MISSING_SECS)),
                            cache_control: None,
                        },
                    )
                    .await?;
            }
            return Err(Error::Status(response.status.as_u16()));
        }
        if !not_modified && let Some((_, derive)) = derived {
            let permit = self
                .0
                .decoding
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| Error::Worker)?;
            let body = std::mem::take(&mut response.body);
            response.body = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                derive(&body)
            })
            .await
            .map_err(|_| Error::Worker)??;
        }
        let text = |name: &str| {
            response
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let control = text("cache-control");
        // Bytes addressed by their digest are the same for everyone, so a
        // registry's no-store or Vary cannot make them private. Those read with
        // an account's token may be private, and the cache key omits the token,
        // so they are stored only as the registry allows.
        let account = registry.flatten().is_some_and(|bearer| bearer.account);
        let uncacheable = !(registry.is_some() && immutable(&url) && !account)
            && (control.as_ref().is_some_and(|c| {
                c.split(',')
                    .any(|v| v.trim().eq_ignore_ascii_case("no-store"))
            }) || text("vary").is_some_and(|v| {
                v.split(',').any(|n| {
                    !["accept", "accept-encoding", "user-agent"]
                        .iter()
                        .any(|known| n.trim().eq_ignore_ascii_case(known))
                })
            }));
        if uncacheable {
            self.0.cache.http_remove(&key).await?;
        } else {
            let entry = HttpEntry {
                status: response.status.as_u16(),
                etag: text("etag"),
                last_modified: text("last-modified"),
                headers: [
                    "etag",
                    "last-modified",
                    "cache-control",
                    "vary",
                    "content-type",
                    "link",
                    "docker-content-digest",
                ]
                .into_iter()
                .filter_map(|name| text(name).map(|v| (name.into(), v)))
                .collect(),
                body: response.body.clone(),
                fetched_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
                expires_at: control
                    .as_deref()
                    .and_then(|c| {
                        c.split(',').find_map(|p| {
                            p.trim()
                                .strip_prefix("max-age=")?
                                .trim_matches('"')
                                .parse::<i64>()
                                .ok()
                        })
                    })
                    .filter(|age| *age >= 0)
                    .and_then(|age| {
                        let elapsed = text("age")
                            .and_then(|v| v.parse::<i64>().ok())
                            .unwrap_or(0)
                            .max(0);
                        now.checked_add(age.saturating_sub(elapsed).max(0))
                    }),
                cache_control: control,
            };
            if not_modified {
                self.0.cache.http_refresh(&key, &entry).await?;
            } else {
                self.0.cache.http_put(&key, &entry).await?;
            }
        }
        Ok(response)
    }
}

fn cached_response(entry: &HttpEntry) -> Result<Response, Error> {
    let mut headers = header::HeaderMap::new();
    for (name, value) in &entry.headers {
        headers.insert(
            header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Configuration)?,
            HeaderValue::from_str(value).map_err(|_| Error::Configuration)?,
        );
    }
    Ok(Response {
        status: StatusCode::from_u16(entry.status).map_err(|_| Error::Configuration)?,
        headers,
        body: entry.body.clone(),
    })
}

// Docker Hub uses `remaining;w=seconds`, not the unrelated Hub REST API's X- headers.
fn exhausted_quota(response: &Response) -> Option<Duration> {
    let remaining = response.headers.get("ratelimit-remaining")?.to_str().ok()?;
    if remaining.split(';').next()?.trim().parse::<u64>().ok()? != 0 {
        return None;
    }
    let window = remaining
        .split(';')
        .chain(
            response
                .headers
                .get("ratelimit-limit")
                .and_then(|v| v.to_str().ok())
                .into_iter()
                .flat_map(|v| v.split(';')),
        )
        .find_map(|v| v.trim().strip_prefix("w=")?.parse::<u64>().ok())
        .unwrap_or(21600);
    Some(Duration::from_secs(window.max(61)))
}

fn retry_delay(response: &Response, attempt: u32) -> Option<Duration> {
    match response.headers.get("retry-after") {
        None => Some(Duration::from_secs(1_u64 << attempt.min(5))),
        Some(value) => {
            let value = value.to_str().ok()?;
            value
                .parse::<u64>()
                .ok()
                .map(Duration::from_secs)
                .or_else(|| {
                    httpdate::parse_http_date(value)
                        .ok()
                        .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Revalidate(std::sync::atomic::AtomicUsize);
    impl Transport for Revalidate {
        fn send(&self, request: Request) -> crate::Exchange<'_> {
            assert_eq!(request.headers()[header::IF_NONE_MATCH], "\"one\"");
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(Response {
                    status: StatusCode::NOT_MODIFIED,
                    headers: header::HeaderMap::from_iter([(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("max-age=3600"),
                    )]),
                    body: Vec::new(),
                })
            })
        }
    }
    #[tokio::test]
    async fn scoped_manual_retry_bypasses_freshness_preserves_validators_and_shares_stats() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let url = "https://registry.example/catalog";
        cached_entry(&cache, url, Some("max-age=60"), 0, Some("\"one\"")).await;
        let transport = Arc::new(Revalidate(Default::default()));
        let client = Cached::new(cache, transport.clone(), 4).unwrap();
        client.get(url, "text/plain").await.unwrap();
        assert_eq!(transport.0.load(Ordering::Relaxed), 0);
        let response = client.revalidating().get(url, "text/plain").await.unwrap();
        assert_eq!(response.body, b"catalog");
        assert_eq!(transport.0.load(Ordering::Relaxed), 1);
        client.get(url, "text/plain").await.unwrap();
        assert_eq!(transport.0.load(Ordering::Relaxed), 1);
        assert_eq!(client.stats().requests, 1);
        assert_eq!(client.stats().not_modified, 1);
    }

    struct Never;
    impl Transport for Never {
        fn send(&self, _: Request) -> crate::Exchange<'_> {
            panic!("unexpected network request")
        }
    }
    fn key(url: &str) -> HttpKey {
        HttpKey::new(
            "GET",
            url,
            BTreeMap::from([("accept".into(), "text/plain".into())]),
        )
        .unwrap()
    }
    async fn cached_entry(
        cache: &Cache,
        url: &str,
        control: Option<&str>,
        age: i64,
        etag: Option<&str>,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        cache
            .http_put(
                &key(url),
                &HttpEntry {
                    status: 200,
                    body: b"catalog".to_vec(),
                    headers: BTreeMap::new(),
                    etag: etag.map(str::to_owned),
                    last_modified: None,
                    fetched_at: now - age,
                    expires_at: Some(now - age + 60),
                    cache_control: control.map(str::to_owned),
                },
            )
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn freshness_floors_use_fetch_time_and_can_be_disabled_on_existing_entries() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let go = "https://proxy.golang.org/example.org/mod/@v/list";
        let tags = "https://ghcr.io/v2/example/image/tags/list?n=1000";
        cached_entry(&cache, go, Some("max-age=60"), 300, None).await;
        cached_entry(&cache, tags, None, 300, None).await;
        let client =
            Cached::with_options(cache.clone(), Arc::new(Never), Default::default()).unwrap();
        assert_eq!(client.get(go, "text/plain").await.unwrap().body, b"catalog");
        assert_eq!(
            client
                .registry_get(tags, "text/plain", None)
                .await
                .unwrap()
                .body,
            b"catalog"
        );
        let transport = Arc::new(Count(std::sync::atomic::AtomicUsize::new(0)));
        let client = Cached::with_options(
            cache.clone(),
            transport.clone(),
            Options {
                go_freshness_secs: 0,
                registry_freshness_secs: 0,
                ..Default::default()
            },
        )
        .unwrap();
        client.get(go, "text/plain").await.unwrap();
        client.registry_get(tags, "text/plain", None).await.unwrap();
        assert_eq!(transport.0.load(Ordering::Relaxed), 2);
        cached_entry(&cache, go, None, 3601, None).await;
        let client = Cached::with_options(cache, transport.clone(), Default::default()).unwrap();
        client.get(go, "text/plain").await.unwrap();
        assert_eq!(
            transport.0.load(Ordering::Relaxed),
            3,
            "floor expires from original fetch time"
        );
    }
    struct Count(std::sync::atomic::AtomicUsize);
    impl Transport for Count {
        fn send(&self, _: Request) -> crate::Exchange<'_> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(Response {
                    status: StatusCode::OK,
                    headers: Default::default(),
                    body: b"new".to_vec(),
                })
            })
        }
    }
    #[tokio::test]
    async fn no_cache_validators_and_unrelated_urls_do_not_get_registry_grace() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let transport = Arc::new(Count(std::sync::atomic::AtomicUsize::new(0)));
        let client =
            Cached::with_options(cache.clone(), transport.clone(), Default::default()).unwrap();
        for (url, control, etag) in [
            ("https://ghcr.io/v2/a/b/tags/list", Some("no-cache"), None),
            ("https://ghcr.io/v2/a/c/tags/list", None, Some("tag")),
            ("https://ghcr.io/v2/a/b/manifests/latest", None, None),
            (
                "https://proxy.golang.org/example.org/@v/v1.0.0.info",
                None,
                None,
            ),
            ("https://other.org/example.org/@v/list", None, None),
        ] {
            cached_entry(&cache, url, control, 300, etag).await;
            client.registry_get(url, "text/plain", None).await.unwrap();
        }
        assert_eq!(transport.0.load(Ordering::Relaxed), 5);
    }
    struct Limited {
        calls: std::sync::atomic::AtomicUsize,
        retry_after: &'static str,
    }
    impl Transport for Limited {
        fn send(&self, _: Request) -> crate::Exchange<'_> {
            let attempt = self.calls.fetch_add(1, Ordering::Relaxed);
            let retry_after = self.retry_after;
            Box::pin(async move {
                Ok(Response {
                    status: if attempt < 2 {
                        StatusCode::TOO_MANY_REQUESTS
                    } else {
                        StatusCode::OK
                    },
                    headers: [(header::RETRY_AFTER, HeaderValue::from_static(retry_after))]
                        .into_iter()
                        .collect(),
                    body: b"new".to_vec(),
                })
            })
        }
    }
    #[tokio::test]
    async fn registry_retries_are_bounded_and_long_cooldowns_do_not_block_other_hosts() {
        for (retry_after, retries, expected_status, expected_calls) in
            [("0", 3, 200, 3), ("0", 1, 429, 2), ("3600", 3, 429, 1)]
        {
            let transport = Arc::new(Limited {
                calls: std::sync::atomic::AtomicUsize::new(0),
                retry_after,
            });
            let cache = Cache::open(":memory:", Default::default()).await.unwrap();
            let client = Cached::with_options(
                cache,
                transport.clone(),
                Options {
                    registry_retries: retries,
                    ..Default::default()
                },
            )
            .unwrap();
            let response = client
                .registry_get("https://a.org/v2/p/tags/list", "text/plain", None)
                .await
                .unwrap();
            assert_eq!(response.status.as_u16(), expected_status);
            assert_eq!(transport.calls.load(Ordering::Relaxed), expected_calls);
            if retry_after == "3600" {
                assert!(matches!(
                    client
                        .registry_get("https://a.org/v2/q/tags/list", "text/plain", None)
                        .await,
                    Err(Error::Status(429))
                ));
                assert_eq!(
                    transport.calls.load(Ordering::Relaxed),
                    1,
                    "queued callers respect the shared cooldown"
                );
                assert_eq!(
                    client
                        .registry_get("https://b.org/v2/q/tags/list", "text/plain", None)
                        .await
                        .unwrap()
                        .status,
                    429
                );
                assert_eq!(
                    transport.calls.load(Ordering::Relaxed),
                    2,
                    "other hosts remain available"
                );
            }
        }
    }
    struct PullQuota {
        calls: std::sync::atomic::AtomicUsize,
        header_only: bool,
    }
    impl Transport for PullQuota {
        fn send(&self, _: Request) -> crate::Exchange<'_> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let header_only = self.header_only;
            Box::pin(async move {
                Ok(Response {
                    status: StatusCode::TOO_MANY_REQUESTS,
                    headers: if header_only {
                        [(
                            header::HeaderName::from_static("ratelimit-remaining"),
                            HeaderValue::from_static("0;w=3600"),
                        )]
                        .into_iter()
                        .collect()
                    } else {
                        Default::default()
                    },
                    body: if header_only {
                        Vec::new()
                    } else {
                        br#"{"errors":[{"code":"TOOMANYREQUESTS","message":"You have reached your unauthenticated pull rate limit"}]}"#.to_vec()
                    },
                })
            })
        }
    }
    #[tokio::test]
    async fn zero_pull_counter_does_not_make_head_digests_depend_on_order_or_cache() {
        struct VersionChecks;
        impl Transport for VersionChecks {
            fn send(&self, request: Request) -> crate::Exchange<'_> {
                assert_ne!(request.method(), Method::GET, "manifest GET is quota gated");
                let zero = request.url().path().contains("/limited/");
                Box::pin(async move {
                    Ok(Response {
                        status: StatusCode::OK,
                        headers: [
                            (
                                header::HeaderName::from_static("ratelimit-remaining"),
                                HeaderValue::from_str(if zero { "0;w=3600" } else { "10;w=3600" })
                                    .unwrap(),
                            ),
                            (
                                header::HeaderName::from_static("docker-content-digest"),
                                HeaderValue::from_static("sha256:fixed"),
                            ),
                            (header::ETAG, HeaderValue::from_static("\"fixed\"")),
                        ]
                        .into_iter()
                        .collect(),
                        body: Vec::new(),
                    })
                })
            }
        }
        for reverse in [false, true] {
            let cache = Cache::open(":memory:", Default::default()).await.unwrap();
            let urls = [
                "https://registry-1.docker.io/v2/limited/image/manifests/latest",
                "https://registry-1.docker.io/v2/other/image/manifests/latest",
            ];
            for _ in 0..3 {
                // Reopening the client resets in-memory state, while HTTP entries
                // exercise cold and warm validation with the same responses.
                let client = Cached::with_options(
                    cache.clone(),
                    Arc::new(VersionChecks),
                    Options {
                        default_host_concurrency: 1,
                        ..Default::default()
                    },
                )
                .unwrap();
                for index in if reverse { [1, 0] } else { [0, 1] } {
                    let response = client
                        .registry_head(urls[index], "text/plain", None)
                        .await
                        .unwrap();
                    assert_eq!(response.headers["docker-content-digest"], "sha256:fixed");
                }
                assert!(matches!(
                    client.registry_get(urls[1], "text/plain", None).await,
                    Err(Error::Status(429))
                ));
            }
        }
    }

    #[tokio::test]
    async fn docker_hub_pull_quota_is_not_a_short_retry() {
        for header_only in [false, true] {
            let transport = Arc::new(PullQuota {
                calls: std::sync::atomic::AtomicUsize::new(0),
                header_only,
            });
            let cache = Cache::open(":memory:", Default::default()).await.unwrap();
            let client =
                Cached::with_options(cache, transport.clone(), Default::default()).unwrap();
            let url = "https://registry-1.docker.io/v2/library/image/manifests/latest";
            let response = if header_only {
                client.registry_head(url, "text/plain", None).await
            } else {
                client.registry_get(url, "text/plain", None).await
            }
            .unwrap();
            assert_eq!(response.status, 429);
            assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
        }
    }
    struct Blocked {
        started: tokio::sync::mpsc::UnboundedSender<String>,
        release: Semaphore,
    }
    impl Transport for Blocked {
        fn send(&self, request: Request) -> crate::Exchange<'_> {
            Box::pin(async move {
                self.started
                    .send(request.url().host_str().unwrap().into())
                    .unwrap();
                self.release.acquire().await.unwrap().forget();
                Ok(Response {
                    status: StatusCode::OK,
                    headers: Default::default(),
                    body: Vec::new(),
                })
            })
        }
    }
    #[tokio::test]
    async fn a_saturated_host_does_not_block_another_host() {
        let (started, mut calls) = tokio::sync::mpsc::unbounded_channel();
        let transport = Arc::new(Blocked {
            started,
            release: Semaphore::new(0),
        });
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let client = Cached::with_options(
            cache,
            transport.clone(),
            Options {
                default_host_concurrency: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let mut jobs = Vec::new();
        for url in ["https://a.org/1", "https://a.org/2", "https://b.org/1"] {
            let client = client.clone();
            jobs.push(tokio::spawn(async move {
                client.get(url, "text/plain").await.unwrap()
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut hosts = vec![calls.recv().await.unwrap(), calls.recv().await.unwrap()];
            hosts.sort();
            assert_eq!(hosts, ["a.org", "b.org"]);
            assert!(
                calls.try_recv().is_err(),
                "same-host second request remains queued"
            );
            transport.release.add_permits(3);
            for job in jobs {
                job.await.unwrap();
            }
        })
        .await
        .expect("another host must start while the first is blocked");
    }
    const BLOB: &str = "https://ghcr.io/v2/example/image/blobs/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    struct NoStore;
    impl Transport for NoStore {
        fn send(&self, _: Request) -> crate::Exchange<'_> {
            Box::pin(async {
                Ok(Response {
                    status: StatusCode::OK,
                    headers: header::HeaderMap::from_iter([(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("no-store"),
                    )]),
                    body: b"blob".to_vec(),
                })
            })
        }
    }
    #[tokio::test]
    async fn a_digest_is_cached_despite_no_store_unless_an_account_read_it() {
        for (account, stored) in [(false, true), (true, false)] {
            let cache = Cache::open(":memory:", Default::default()).await.unwrap();
            let client =
                Cached::with_options(cache.clone(), Arc::new(NoStore), Default::default()).unwrap();
            let bearer = Bearer {
                token: "t",
                account,
            };
            client
                .registry_get(BLOB, "text/plain", Some(bearer))
                .await
                .unwrap();
            assert_eq!(
                cache.http_get(&key(BLOB)).await.unwrap().is_some(),
                stored,
                "account: {account}"
            );
        }
    }
    #[tokio::test]
    async fn a_missing_digest_is_asked_about_again_once_its_day_is_up() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        cache
            .http_put(
                &key(BLOB),
                &HttpEntry {
                    status: 404,
                    body: Vec::new(),
                    headers: BTreeMap::new(),
                    etag: None,
                    last_modified: None,
                    fetched_at: now - 2 * 24 * 3600,
                    expires_at: Some(now - 24 * 3600),
                    cache_control: None,
                },
            )
            .await
            .unwrap();
        let transport = Arc::new(Count(std::sync::atomic::AtomicUsize::new(0)));
        let client = Cached::with_options(cache, transport.clone(), Default::default()).unwrap();
        let response = client.registry_get(BLOB, "text/plain", None).await.unwrap();
        assert_eq!(response.body, b"new");
        assert_eq!(transport.0.load(Ordering::Relaxed), 1);
    }
}
