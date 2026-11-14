//! Conditional, unauthenticated registry GETs sharing the batch transport pool.
use crate::{
    Credential, RequestTrace, Response, Stale, Transport, conditional,
    credentials::{matching, mirrored},
};
use patchula_cache::{Cache, HttpEntry, representation};
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
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// GitHub's in-flight cap when no `api.github.com` host limit is set.
pub const GITHUB_DEFAULT_CONCURRENCY: usize = 16;

/// The longest wait the client sleeps through for a host's cooldown or a
/// retry; a longer one is returned to the caller.
const LONGEST_WAIT: Duration = Duration::from_secs(60);
/// Redirects followed for one read before it fails.
const MAX_REDIRECTS: usize = 5;
/// A cooldown whose end overflows the clock lasts this long instead.
const COOLDOWN_FALLBACK: Duration = Duration::from_secs(3600);

/// Batch HTTP policy, independent of repository parallelism.
#[derive(Clone, Debug)]
pub struct Options {
    pub default_host_concurrency: usize,
    pub host_concurrency: BTreeMap<String, usize>,
    /// Freshness for validator-free OCI tag pages. Zero restores upstream policy.
    pub registry_freshness_secs: u64,
    /// Minimum freshness for public Go proxy lists/latest. Zero disables it.
    pub go_freshness_secs: u64,
    /// Freshness window for the OSV advisory listings kept in cache state.
    /// Zero lists every version on every run.
    pub advisory_freshness_secs: u64,
    /// Bounded OCI 429 retries; a wait longer than [`LONGEST_WAIT`] is
    /// returned to the caller.
    pub registry_retries: u32,
    /// Operator registry entries. A request under an entry's prefix carries
    /// its credentials, and its cache entries are scoped to that entry.
    pub credentials: Vec<Credential>,
    /// The registry protocol knowledge freshness, quotas and special cases
    /// consult. `NoRules` is the default: no floors, nothing immutable, no
    /// quotas. `registry_freshness_secs` and `go_freshness_secs` stay
    /// operator settings here; whoever builds the rules reads them.
    pub rules: Arc<dyn RegistryRules>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            default_host_concurrency: 32,
            host_concurrency: BTreeMap::new(),
            registry_freshness_secs: 3600,
            go_freshness_secs: 3600,
            advisory_freshness_secs: 3600,
            registry_retries: 3,
            credentials: Vec::new(),
            rules: Arc::new(NoRules),
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
    /// The in-flight cap a token run gives GitHub's API: its own host limit
    /// when one is set, otherwise the default host limit capped at
    /// [`GITHUB_DEFAULT_CONCURRENCY`], so raising the registries' default
    /// does not raise GitHub's.
    pub fn github_limit(&self) -> usize {
        self.host_concurrency
            .get("api.github.com")
            .copied()
            .unwrap_or_else(|| {
                self.default_host_concurrency
                    .min(GITHUB_DEFAULT_CONCURRENCY)
            })
    }
    pub fn validate(&self) -> Result<(), Error> {
        if self.registry_retries > 5
            || !(1..=crate::MAX_CONCURRENCY).contains(&self.default_host_concurrency)
            || self.host_concurrency.iter().any(|(host, limit)| {
                !(1..=crate::MAX_CONCURRENCY).contains(limit)
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
    #[error("invalid upstream document")]
    Document,
    #[error("derive worker failed")]
    Worker,
    #[error("invalid HTTP URL or header")]
    Configuration,
    /// A stored entry's status or headers do not parse.
    #[error("invalid cached HTTP metadata")]
    Metadata,
    #[error("HTTP {0}")]
    Status(u16),
    /// The host is cooling down from an earlier 429 for longer than the
    /// client waits; no request was sent.
    #[error("host is rate limited; retry after {0} seconds")]
    Throttled(u64),
    #[error("HTTP 304 without a cached body")]
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
    /// Answers taken from the cache: served fresh, served stale while
    /// rate limited, or confirmed by a 304.
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
    reads: Reads,
}
struct Host {
    semaphore: Arc<Semaphore>,
    cooldown: Mutex<Option<Instant>>,
    pull_cooldown: Mutex<Option<Instant>>,
}
/// Identical reads in flight, keyed by request. Callers that overlap one
/// fetch share its answer. An entry exists only while some caller holds it,
/// so the map is bounded by the reads in flight, not by every URL ever read.
#[derive(Default)]
struct Reads(Mutex<BTreeMap<String, Read>>);
/// The last answer for a read, when it finished, and whether its request
/// carried credentials, which a caller served the same answer records too.
type Read = Arc<tokio::sync::Mutex<Option<(Instant, Response, bool)>>>;
/// A caller's hold on a read. Dropping it, on every path including
/// cancellation, removes the entry once no other caller holds it.
struct Lease<'a> {
    reads: &'a Reads,
    key: String,
    read: Option<Read>,
}
impl Reads {
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Read>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// The answer of a fetch that finished after this call began, or else
    /// `fetch`'s, shared with overlapping callers when `share` accepts it.
    async fn coalesce(
        &self,
        key: String,
        fetch: impl Future<Output = (Result<Response, Error>, bool)>,
        share: fn(&Response) -> bool,
    ) -> Result<Response, Error> {
        let started = Instant::now();
        let read = self.lock().entry(key.clone()).or_default().clone();
        let lease = Lease {
            reads: self,
            key,
            read: Some(read),
        };
        let mut previous = lease.read().lock().await;
        if let Some((finished, response, credentialed)) = &*previous
            && *finished >= started
        {
            if *credentialed {
                crate::tracked::sent();
            }
            return Ok(response.clone());
        }
        let (response, credentialed) = fetch.await;
        let response = response?;
        if share(&response) {
            *previous = Some((Instant::now(), response.clone(), credentialed));
        }
        Ok(response)
    }
}
impl Lease<'_> {
    fn read(&self) -> &Read {
        self.read.as_ref().expect("held until drop")
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        // Clones are only taken under this lock, so a count of one here means
        // no caller holds the read and none can take it before it is removed.
        let mut reads = self.reads.lock();
        self.read = None;
        if reads
            .get(&self.key)
            .is_some_and(|read| Arc::strong_count(read) == 1)
        {
            reads.remove(&self.key);
        }
    }
}
/// Registry protocol knowledge the cached client consults instead of
/// hardcoding one registry family's rules. Every method has a default that
/// knows nothing, which is what `NoRules` supplies.
pub trait RegistryRules: Send + Sync + std::fmt::Debug {
    /// Whether a response at this URL can never change once fetched.
    fn immutable(&self, _url: &Url) -> bool {
        false
    }
    /// How long a stored entry stays fresh beyond its own expiry. `registry`
    /// marks registry reads; a `None` answer leaves freshness to the
    /// response's own headers.
    fn freshness_floor(&self, _url: &Url, _entry: &HttpEntry, _registry: bool) -> Option<Duration> {
        None
    }
    /// How long a registry's 404 at this URL is remembered instead of asked
    /// again on the next run.
    fn remember_missing(&self, _url: &Url) -> Option<Duration> {
        None
    }
    /// Whether this request counts against a pull quota.
    fn pull_quota(&self, _method: &Method, _url: &Url) -> bool {
        false
    }
    /// The window a response reports the pull quota exhausted for.
    fn quota_window(&self, _url: &Url, _response: &Response) -> Option<Duration> {
        None
    }
    /// The window a pull request's 429 reports the pull quota spent for, or
    /// `None` when the 429 is an ordinary throttle.
    fn pull_limit(&self, _url: &Url, _response: &Response) -> Option<Duration> {
        None
    }
}

/// No registry knowledge: every floor, quota and special case is off.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRules;
impl RegistryRules for NoRules {}

impl From<conditional::Error> for Error {
    fn from(error: conditional::Error) -> Self {
        match error {
            conditional::Error::Metadata => Error::Metadata,
            conditional::Error::Worker => Error::Worker,
            conditional::Error::Stream(transport) => Error::Transport(transport),
        }
    }
}

/// The checks every URL the client sends to passes: HTTPS, a host, and no
/// userinfo, which would put credentials in the URL.
fn validate_url(url: &Url) -> Result<(), Error> {
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::Configuration);
    }
    Ok(())
}

/// A body the client derives from a fetched document and caches under its
/// own representation. `version` is the transform's own version: bump it when
/// the compaction code changes what bytes it produces, never for a release;
/// `tests/derived-versions.tsv` guards the pairing. The function reads the
/// document as `io::Read`, so a streamed body never has to be held whole.
pub struct Derived<E> {
    pub name: &'static str,
    pub version: u32,
    pub derive: fn(&mut dyn std::io::Read) -> std::result::Result<Vec<u8>, E>,
}
// A derived spec is a name, a version and a function pointer; `E` need not
// be Clone.
impl<E> Clone for Derived<E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E> Copy for Derived<E> {}
impl<E> Derived<E> {
    /// Names must leave `@` to the version, checked by the same `plain` rule
    /// state namespaces follow.
    pub const fn new(
        name: &'static str,
        version: u32,
        derive: fn(&mut dyn std::io::Read) -> std::result::Result<Vec<u8>, E>,
    ) -> Self {
        assert!(
            patchula_cache::plain(name),
            "a derived name is nonempty and has no @, ~ or +"
        );
        Self {
            name,
            version,
            derive,
        }
    }
    /// The cache `representation` this derived body is stored under.
    pub fn representation(&self) -> String {
        representation(self.name, self.version)
    }
}

/// A registry bearer token, and whether it was issued to an account rather
/// than anonymously.
#[derive(Clone, Copy)]
pub struct Bearer<'a> {
    pub token: &'a str,
    pub account: bool,
}
/// How a read reaches its upstream.
#[derive(Clone, Copy)]
enum Access<'a> {
    /// A plain read: an operator mirror may stand in, and operator
    /// credentials apply.
    Plain,
    /// A container registry read, with the registry's bearer token when it
    /// issued one. Registry rules, throttles and quotas apply.
    Registry(Option<Bearer<'a>>),
}
impl<'a> Access<'a> {
    fn registry(self) -> bool {
        matches!(self, Access::Registry(_))
    }
    fn bearer(self) -> Option<Bearer<'a>> {
        match self {
            Access::Registry(bearer) => bearer,
            Access::Plain => None,
        }
    }
}
#[derive(Clone)]
pub struct Cached {
    inner: Arc<Inner>,
    /// A scoped manual retry: cached documents are revalidated, never served
    /// fresh.
    revalidating: bool,
}
impl Cached {
    /// A scoped manual retry revalidates cached documents, preserving validators,
    /// shared governors and quotas without changing ordinary callers' freshness.
    pub fn revalidating(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            revalidating: true,
        }
    }
    /// Whether this handle is a scoped manual retry.
    pub fn is_revalidating(&self) -> bool {
        self.revalidating
    }
    /// The operator's OSV advisory-listing window, for the datasource's
    /// `osv-listing` state. Zero means a listing is never reused.
    pub fn advisory_freshness(&self) -> Duration {
        Duration::from_secs(self.inner.options.advisory_freshness_secs)
    }
    /// A client with default options and `concurrency` requests in flight
    /// per host.
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
        Ok(Self {
            inner: Arc::new(Inner {
                transport,
                cache,
                hosts: Default::default(),
                options,
                // Bounds buffered decodes; this client derives from streams,
                // which their host permits bound instead.
                decoding: Arc::new(Semaphore::new(4)),
                requests: AtomicU64::new(0),
                not_modified: AtomicU64::new(0),
                cache_hits: AtomicU64::new(0),
                reads: Reads::default(),
            }),
            revalidating: false,
        })
    }
    fn host(&self, url: &Url) -> Result<Arc<Host>, Error> {
        let host = url.host_str().ok_or(Error::Configuration)?;
        Ok(lock(&self.inner.hosts)
            .entry(host.to_owned())
            .or_insert_with(|| {
                Arc::new(Host {
                    semaphore: Arc::new(Semaphore::new(self.inner.options.host_limit(host))),
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
            let mut until = *lock(&host.cooldown);
            if pull {
                until = until.max(*lock(&host.pull_cooldown));
            }
            if let Some(delay) =
                until.and_then(|until| until.checked_duration_since(Instant::now()))
            {
                drop(permit);
                if delay > LONGEST_WAIT {
                    return Err(Error::Throttled(delay.as_secs()));
                }
                tokio::time::sleep(delay).await;
            } else {
                return Ok(permit);
            }
        }
    }
    fn cooldown(&self, url: &Url, delay: Duration, pull: bool) -> Result<(), Error> {
        let host = self.host(url)?;
        let mut until = lock(if pull {
            &host.pull_cooldown
        } else {
            &host.cooldown
        });
        let now = Instant::now();
        let next = now
            .checked_add(delay)
            .unwrap_or_else(|| now + COOLDOWN_FALLBACK);
        *until = Some(until.map_or(next, |old| old.max(next)));
        Ok(())
    }
    /// The operator entry whose credentials requests to `url` carry.
    pub fn credential(&self, url: &str) -> Option<&Credential> {
        matching(&self.inner.options.credentials, &Url::parse(url).ok()?)
    }
    pub fn stats(&self) -> Stats {
        Stats {
            requests: self.inner.requests.load(Ordering::Relaxed),
            not_modified: self.inner.not_modified.load(Ordering::Relaxed),
            cache_hits: self.inner.cache_hits.load(Ordering::Relaxed),
        }
    }
    pub async fn get(&self, url: &str, accept: &str) -> Result<Response, Error> {
        self.get_representation(url, accept, None, Access::Plain, Method::GET)
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
        validate_url(&url)?;
        let url = mirrored(&self.inner.options.credentials, &url).unwrap_or(url);
        let trace = RequestTrace::new(method.as_str(), &url);
        let mut request = Request::new(method, url.clone());
        let headers = request.headers_mut();
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static(crate::USER_AGENT),
        );
        if let Some(value) =
            matching(&self.inner.options.credentials, &url).and_then(|entry| entry.auth.header())
        {
            crate::tracked::sent();
            headers.insert(header::AUTHORIZATION, value);
        }
        if let Some(body) = body {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            *request.body_mut() = Some(body.into());
        }
        let queued = Instant::now();
        let _permit = self.acquire(&url, false).await?;
        self.inner.requests.fetch_add(1, Ordering::Relaxed);
        let response = trace
            .send(
                self.inner.transport.as_ref(),
                request,
                queued.elapsed(),
                false,
            )
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
        derived: &Derived<Error>,
    ) -> Result<Response, Error> {
        self.registry_representation(url, accept, token, Some(derived), Method::GET)
            .await
    }
    async fn registry_representation(
        &self,
        url: &str,
        accept: &str,
        token: Option<Bearer<'_>>,
        derived: Option<&Derived<Error>>,
        method: Method,
    ) -> Result<Response, Error> {
        let key = format!(
            "registry:{method}:{}:{}:{url}\n{accept}\n{}:{}",
            token.is_some(),
            token.is_some_and(|t| t.account),
            derived
                .map(|derived| derived.representation())
                .as_deref()
                .unwrap_or(""),
            self.revalidating
        );
        // Boxed: the fetch future is large and would otherwise sit inline in
        // every caller's future as well as in `coalesce`'s.
        let fetch = Box::pin(crate::tracked::tracked(self.get_representation(
            url,
            accept,
            derived,
            Access::Registry(token),
            method.clone(),
        )));
        self.inner
            .reads
            .coalesce(key, fetch, |response| response.status.is_success())
            .await
    }

    /// Fetch a registry token without caching the raw response; the caller
    /// may persist only its scoped bearer with a validated expiry.
    /// `credentials`, sent as Basic auth, are used only for a validated token
    /// realm and never cached.
    pub async fn token_get(
        &self,
        url: &str,
        credentials: Option<(&str, &str)>,
    ) -> Result<Response, Error> {
        let url = Url::parse(url).map_err(|_| Error::Configuration)?;
        validate_url(&url)?;
        let trace = RequestTrace::new("GET", &url);
        let mut request = Request::new(Method::GET, url.clone());
        if let Some((username, password)) = credentials {
            crate::tracked::sent();
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
        self.inner.requests.fetch_add(1, Ordering::Relaxed);
        Ok(trace
            .send(
                self.inner.transport.as_ref(),
                request,
                queued.elapsed(),
                false,
            )
            .await?)
    }

    async fn stale_registry(
        &self,
        cached: Option<&HttpEntry>,
        url: &Url,
        accept: &str,
        method: &Method,
        scope: Option<&str>,
    ) -> Result<Option<Response>, Error> {
        // A previous GET also proves a manifest digest if HEAD has not yet been cached.
        let fallback = if cached.is_none() && method == Method::HEAD {
            let key = conditional::key("GET", url.as_str(), &[("accept", accept)], None, scope)?;
            self.inner.cache.http_get(&key).await?
        } else {
            None
        };
        let Some(entry) = cached.or(fallback.as_ref()) else {
            return Ok(None);
        };
        let mut response = conditional::response(entry).map_err(Error::from)?;
        response.stale = Some(Stale {
            fetched_at: entry.fetched_at,
        });
        self.inner.cache_hits.fetch_add(1, Ordering::Relaxed);
        Ok(Some(response))
    }

    /// Derive once on a 200. Fresh hits and 304s contain only the compact body;
    /// validators always refer to the original upstream representation.
    pub async fn get_derived(
        &self,
        url: &str,
        accept: &str,
        derived: &Derived<Error>,
    ) -> Result<Response, Error> {
        let key = format!(
            "{url}\n{accept}\n{}:{}",
            derived.representation(),
            self.revalidating
        );
        // Coalesce overlapping callers even when upstream requires revalidation.
        // A later sequential call still observes normal HTTP freshness rules.
        // Boxed: the fetch future is large and would otherwise sit inline in
        // every caller's future as well as in `coalesce`'s.
        let fetch = Box::pin(crate::tracked::tracked(self.get_representation(
            url,
            accept,
            Some(derived),
            Access::Plain,
            Method::GET,
        )));
        self.inner.reads.coalesce(key, fetch, |_| true).await
    }

    async fn get_representation(
        &self,
        url: &str,
        accept: &str,
        derived: Option<&Derived<Error>>,
        access: Access<'_>,
        method: Method,
    ) -> Result<Response, Error> {
        let url = Url::parse(url).map_err(|_| Error::Configuration)?;
        validate_url(&url)?;
        let registry = access.registry();
        // An operator mirror stands in for a public registry; there is no
        // fallback to the public one. Container registries are never mirrored,
        // since their token services belong to the registry's own host.
        let requested = url.clone();
        let url = (!registry)
            .then(|| mirrored(&self.inner.options.credentials, &url))
            .flatten()
            .unwrap_or(url);
        let credential = matching(&self.inner.options.credentials, &url).cloned();
        // Whether the answer comes from the network or the cache, whose
        // entries are kept per credential, it is one these credentials read.
        if credential.as_ref().is_some_and(|entry| entry.auth.sends())
            || access.bearer().is_some_and(|bearer| bearer.account)
        {
            crate::tracked::sent();
        }
        let scope = credential.as_ref().map(Credential::scope);
        let key = conditional::key(
            method.as_str(),
            url.as_str(),
            &[("accept", accept)],
            derived.map(|derived| derived.representation()).as_deref(),
            scope,
        )?;
        let mut trace = RequestTrace::new(method.as_str(), &url);
        let cached = self.inner.cache.http_get(&key).await?;
        let now = conditional::unix_now();
        let no_cache = cached.as_ref().is_some_and(|entry| {
            entry.cache_control.as_deref().is_some_and(|c| {
                c.split(',')
                    .any(|p| p.trim().eq_ignore_ascii_case("no-cache"))
            })
        });
        let ttl = cached
            .as_ref()
            .and_then(|entry| {
                let floor = self
                    .inner
                    .options
                    .rules
                    .freshness_floor(&requested, entry, registry)
                    .filter(|floor| !floor.is_zero())
                    .map(|floor| floor.as_secs().min(i64::MAX as u64) as i64);
                let local = floor.map(|floor| entry.fetched_at.saturating_add(floor));
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
            && !self.revalidating
        {
            self.inner.cache_hits.fetch_add(1, Ordering::Relaxed);
            if entry.status == 404 {
                return Err(Error::Status(404));
            }
            let response = conditional::response(entry).map_err(Error::from)?;
            trace.fresh(&response);
            return Ok(response);
        }
        let mut request = Request::new(method.clone(), url.clone());
        if let Some(Bearer { token, .. }) = access.bearer() {
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| Error::Configuration)?;
            value.set_sensitive(true);
            request.headers_mut().insert(header::AUTHORIZATION, value);
        } else if let Some(value) = credential.as_ref().and_then(|entry| entry.auth.header()) {
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
            HeaderValue::from_static(crate::USER_AGENT),
        );
        if let Some(entry) = &cached {
            conditional::add_validators(&mut request, entry)?;
        }
        let mut attempt = 0;
        let mut redirects = BTreeSet::from([url.to_string()]);
        let mut foreign = false;
        // The host permit of the answered request is held through derivation:
        // a request is in flight until its body is read.
        let (mut response, stream, _permit) = loop {
            let queued = Instant::now();
            let target = request.url().clone();
            let pull_request = registry && self.inner.options.rules.pull_quota(&method, &target);
            let mut permit = match self.acquire(&target, pull_request).await {
                Ok(permit) => Some(permit),
                Err(Error::Throttled(wait)) if registry => {
                    if let Some(response) = self
                        .stale_registry(cached.as_ref(), &url, accept, &method, scope)
                        .await?
                    {
                        return Ok(response);
                    }
                    return Err(Error::Throttled(wait));
                }
                Err(error) => return Err(error),
            };
            self.inner.requests.fetch_add(1, Ordering::Relaxed);
            // A derived answer streams into its derive worker, so a large
            // document is never held whole. Any status the loop handles —
            // redirects, throttles and retries — is buffered; those bodies
            // are small.
            let mut stream = None;
            let response = if derived.is_none() {
                trace
                    .send(
                        self.inner.transport.as_ref(),
                        request.try_clone().ok_or(Error::Configuration)?,
                        queued.elapsed(),
                        cached.is_some(),
                    )
                    .await?
            } else {
                let answer = trace
                    .send_streaming(
                        self.inner.transport.as_ref(),
                        request.try_clone().ok_or(Error::Configuration)?,
                        queued.elapsed(),
                        cached.is_some(),
                    )
                    .await?;
                if answer.status.is_success() {
                    let crate::Streaming {
                        status,
                        headers,
                        body,
                    } = answer;
                    stream = Some(body);
                    Response {
                        status,
                        headers,
                        body: Vec::new(),
                        stale: None,
                    }
                } else {
                    answer.collect().await?
                }
            };
            if matches!(response.status.as_u16(), 301 | 302 | 303 | 307 | 308) {
                let next = response
                    .headers
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| target.join(v).ok())
                    .ok_or(Error::Configuration)?;
                validate_url(&next)?;
                if redirects.len() > MAX_REDIRECTS || !redirects.insert(next.to_string()) {
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
                let next_entry = matching(&self.inner.options.credentials, &next);
                if access.bearer().is_some() {
                    // A registry token stays on its origin; one issued under an
                    // operator entry also stays within that entry's prefix.
                    if scope.is_some() && next_entry.map(Credential::scope) != scope {
                        request.headers_mut().remove(header::AUTHORIZATION);
                    }
                } else {
                    // Operator credentials follow a redirect only to a URL an
                    // entry covers, and only from a request that sent some: a
                    // redirect never lends credentials to a request that had
                    // none. Another entry's answer is never stored
                    // under this request's key.
                    request.headers_mut().remove(header::AUTHORIZATION);
                    if credential.as_ref().is_some_and(|entry| entry.auth.sends())
                        && let Some(entry) = next_entry
                        && let Some(value) = entry.auth.header()
                    {
                        request.headers_mut().insert(header::AUTHORIZATION, value);
                        foreign |= Some(entry.scope()) != scope;
                    }
                }
                request.url_mut().clone_from(&next);
                trace = RequestTrace::new(method.as_str(), &next);
                continue;
            }
            if registry
                && let Some(window) = self.inner.options.rules.quota_window(&target, &response)
            {
                // Docker's pull counter is also returned on successful HEADs.
                // Only manifest GETs consume it; blocking HEADs here makes the
                // available digest pins depend on which response finishes first.
                self.cooldown(&target, window, true)?;
            }
            if registry && response.status == StatusCode::TOO_MANY_REQUESTS {
                // Only manifest GETs consume Docker Hub's pull counter, so only
                // their 429 can mean it is spent. A HEAD or tag-list 429 cools
                // the host for the counter's window when it names one (that is
                // evidence those requests are throttled too), and is otherwise
                // an ordinary throttle.
                let pull_limit = pull_request
                    .then(|| self.inner.options.rules.pull_limit(&target, &response))
                    .flatten();
                let throttle = if let Some(window) = pull_limit {
                    Throttle::Pulls(window)
                } else if let Some(window) =
                    self.inner.options.rules.quota_window(&target, &response)
                {
                    Throttle::Host(window)
                } else {
                    Throttle::Retry(retry_delay(&response, attempt))
                };
                // Publish even an exhausted/long cooldown before releasing the
                // permit, so unrelated queued requests to this host respect it.
                let delay = match throttle {
                    Throttle::Pulls(window) => {
                        self.cooldown(&target, window, true)?;
                        None
                    }
                    Throttle::Host(window) => {
                        self.cooldown(&target, window, false)?;
                        None
                    }
                    Throttle::Retry(delay) => {
                        if let Some(delay) = delay {
                            self.cooldown(&target, delay, false)?;
                        }
                        delay
                    }
                };
                drop(permit.take());
                if let Some(response) = self
                    .stale_registry(cached.as_ref(), &url, accept, &method, scope)
                    .await?
                {
                    return Ok(response);
                }
                if attempt < self.inner.options.registry_retries
                    && delay.is_some_and(|delay| delay <= LONGEST_WAIT)
                {
                    attempt += 1;
                    trace = RequestTrace::new(method.as_str(), &target);
                    trace.cache_policy("rate-retry", ttl);
                    continue;
                }
            }
            break (response, stream, permit);
        };
        if registry
            && response.status == StatusCode::TOO_MANY_REQUESTS
            && let Some(response) = self
                .stale_registry(cached.as_ref(), &url, accept, &method, scope)
                .await?
        {
            return Ok(response);
        }
        let not_modified = response.status == StatusCode::NOT_MODIFIED;
        if not_modified {
            let entry = cached.ok_or(Error::MissingBody)?;
            self.inner.not_modified.fetch_add(1, Ordering::Relaxed);
            self.inner.cache_hits.fetch_add(1, Ordering::Relaxed);
            response = conditional::not_modified(response, entry)?;
        }
        if registry && matches!(response.status.as_u16(), 401 | 429) {
            return Ok(response);
        }
        if !response.status.is_success() {
            // A digest or cosign tag the registry lacks is asked about again
            // a day later, not on every run.
            if response.status == StatusCode::NOT_FOUND
                && registry
                && let Some(window) = self.inner.options.rules.remember_missing(&url)
            {
                self.inner
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
                            expires_at: Some(
                                now.saturating_add(window.as_secs().min(i64::MAX as u64) as i64),
                            ),
                            cache_control: None,
                        },
                    )
                    .await?;
            }
            return Err(Error::Status(response.status.as_u16()));
        }
        if !not_modified && let Some(derived) = derived {
            let body = conditional::Body::Streaming(
                stream.expect("a successful derived read keeps its body stream"),
            );
            response.body = conditional::derive(body, derived.derive, &self.inner.decoding)
                .await
                .map_err(|error| match error {
                    conditional::Error::Stream(transport) => Error::Transport(transport),
                    conditional::Error::Metadata | conditional::Error::Worker => Error::Worker,
                })??;
        }
        // Bytes addressed by their digest are the same for everyone, so a
        // registry's no-store or Vary cannot make them private. Those read with
        // an account's token or an operator entry's credentials may be private,
        // so they are stored only as the registry allows.
        let account = access.bearer().is_some_and(|bearer| bearer.account)
            || credential
                .as_ref()
                .is_some_and(|entry| entry.auth != crate::Auth::Anonymous);
        let public_digest = registry && self.inner.options.rules.immutable(&url) && !account;
        // A registry bearer token is not in the key; operator credentials are,
        // through their scope.
        let authorization_in_key = access.bearer().is_none();
        if foreign || !(public_digest || conditional::storable(&response, authorization_in_key)) {
            self.inner.cache.http_remove(&key).await?;
        } else {
            let entry = conditional::entry(&response, conditional::unix_now());
            if not_modified {
                self.inner.cache.http_refresh(&key, &entry).await?;
            } else {
                self.inner.cache.http_put(&key, &entry).await?;
            }
        }
        Ok(response)
    }
}

/// A cooldown map's lock. The guarded value is a plain instant a panicking
/// holder cannot leave half-written, so a poisoned lock is still read.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a registry's 429 asks of later requests.
enum Throttle {
    /// Docker Hub's pull counter is spent: manifest GETs wait out the window.
    Pulls(Duration),
    /// Every request to the host waits out the window, with no retry.
    Host(Duration),
    /// The host is busy: wait the delay and retry when it is short. `None`
    /// is a `Retry-After` that could not be read, so no retry.
    Retry(Option<Duration>),
}

fn retry_delay(response: &Response, attempt: u32) -> Option<Duration> {
    if response.headers.contains_key("retry-after") {
        conditional::retry_after(response)
    } else {
        Some(Duration::from_secs(1_u64 << attempt.min(5)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GitHub keeps its own cap when the registries' default rises: the
    /// default capped at 16, unless `api.github.com` is set.
    #[test]
    fn github_keeps_its_own_cap() {
        let mut options = Options::default();
        assert_eq!(options.default_host_concurrency, 32);
        assert_eq!(options.github_limit(), 16);
        options.default_host_concurrency = 8;
        assert_eq!(options.github_limit(), 8);
        options.host_concurrency.insert("api.github.com".into(), 40);
        assert_eq!(options.github_limit(), 40);
    }
    use patchula_cache::HttpKey;

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
                    stale: None,
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
        let now = conditional::unix_now();
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
    struct Count(std::sync::atomic::AtomicUsize);
    impl Transport for Count {
        fn send(&self, _: Request) -> crate::Exchange<'_> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(Response {
                    status: StatusCode::OK,
                    headers: Default::default(),
                    body: b"new".to_vec(),
                    stale: None,
                })
            })
        }
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
                    stale: None,
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
                    Err(Error::Throttled(_))
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
                    stale: None,
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
    #[tokio::test]
    async fn a_missing_digest_is_asked_about_again_once_its_day_is_up() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let now = conditional::unix_now();
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
    struct Scripted(
        Mutex<std::collections::VecDeque<(&'static str, Option<&'static str>, Response)>>,
    );
    impl Transport for Scripted {
        fn send(&self, request: Request) -> crate::Exchange<'_> {
            let (url, auth, response) = self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request");
            assert_eq!(request.url().as_str(), url);
            assert_eq!(
                request
                    .headers()
                    .get(header::AUTHORIZATION)
                    .map(|v| v.to_str().unwrap()),
                auth
            );
            Box::pin(async { Ok(response) })
        }
    }
    #[tokio::test]
    async fn operator_credentials_stay_on_their_prefix_and_in_their_own_cache_entries() {
        const INDEX: &str = "https://acme.jfrog.io/artifactory/api/helm/charts/index.yaml";
        const MIRROR: &str = "https://cdn.example.com/charts/index.yaml";
        let reply =
            |status: u16, headers: &[(header::HeaderName, &'static str)], body: &[u8]| Response {
                status: StatusCode::from_u16(status).unwrap(),
                headers: headers
                    .iter()
                    .map(|(name, value)| (name.clone(), HeaderValue::from_static(value)))
                    .collect(),
                body: body.to_vec(),
                stale: None,
            };
        let entry = Credential::new(
            "https://acme.jfrog.io/artifactory/api/helm/charts",
            crate::Auth::Bearer {
                token: "tok".into(),
            },
        )
        .unwrap();
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let transport = Arc::new(Scripted(Mutex::new(
            [
                (
                    INDEX,
                    Some("Bearer tok"),
                    reply(302, &[(header::LOCATION, MIRROR)], b""),
                ),
                // Another origin never sees the entry's token.
                (
                    MIRROR,
                    None,
                    reply(200, &[(header::CACHE_CONTROL, "max-age=600")], b"charts"),
                ),
                // Outside the prefix on the same host, no token either.
                (
                    "https://acme.jfrog.io/artifactory/api/helm/other/index.yaml",
                    None,
                    reply(401, &[], b""),
                ),
            ]
            .into(),
        )));
        let client = Cached::with_options(
            cache.clone(),
            transport.clone(),
            Options {
                credentials: vec![entry.clone()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            client.get(INDEX, "application/yaml").await.unwrap().body,
            b"charts"
        );
        let scoped = || {
            HttpKey::new(
                "GET",
                INDEX,
                BTreeMap::from([("accept".into(), "application/yaml".into())]),
            )
            .unwrap()
        };
        assert!(cache.http_get(&scoped()).await.unwrap().is_none());
        assert!(
            cache
                .http_get(&scoped().credential(entry.scope()))
                .await
                .unwrap()
                .is_some()
        );
        // The fresh entry serves the same credentials without a request.
        assert_eq!(
            client.get(INDEX, "application/yaml").await.unwrap().body,
            b"charts"
        );
        assert!(
            client
                .get(
                    "https://acme.jfrog.io/artifactory/api/helm/other/index.yaml",
                    "application/yaml"
                )
                .await
                .is_err()
        );
        assert!(transport.0.lock().unwrap().is_empty());
        // A client without the entry does not read it from the shared cache.
        let anonymous = Arc::new(Scripted(Mutex::new(
            [(INDEX, None, reply(401, &[], b""))].into(),
        )));
        let client = Cached::with_options(cache, anonymous.clone(), Default::default()).unwrap();
        assert!(client.get(INDEX, "application/yaml").await.is_err());
        assert!(anonymous.0.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn redirects_never_store_or_carry_another_entrys_credentials() {
        const PUBLIC: &str = "https://charts.example.com/index.yaml";
        const PRIVATE: &str = "https://acme.jfrog.io/charts/index.yaml";
        const BLOB: &str = "https://acme.jfrog.io/v2/team/app/blobs/latest";
        const OUTSIDE: &str = "https://acme.jfrog.io/other/blob";
        const OTHER: &str = "https://acme.jfrog.io/charts/other.yaml";
        const MOVED: &str = "https://acme.jfrog.io/v2/moved";
        let reply = |status: u16, location: Option<&'static str>| Response {
            status: StatusCode::from_u16(status).unwrap(),
            headers: location
                .map(|to| (header::LOCATION, HeaderValue::from_static(to)))
                .into_iter()
                .chain([(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("max-age=600"),
                )])
                .collect(),
            body: b"body".to_vec(),
            stale: None,
        };
        let entry = Credential::new(
            "https://acme.jfrog.io/charts",
            crate::Auth::Bearer {
                token: "tok".into(),
            },
        )
        .unwrap();
        let registry = Credential::new(
            "https://acme.jfrog.io/v2",
            crate::Auth::Bearer {
                token: "reg".into(),
            },
        )
        .unwrap();
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let transport = Arc::new(Scripted(Mutex::new(
            [
                // A request without credentials gains none on a redirect, even
                // into an entry's prefix.
                (PUBLIC, None, reply(302, Some(PRIVATE))),
                (PRIVATE, None, reply(200, None)),
                // A credentialed request redirected into another entry's
                // prefix carries that entry's credentials, but its answer is
                // not stored under its own key.
                (OTHER, Some("Bearer tok"), reply(302, Some(MOVED))),
                (MOVED, Some("Bearer reg"), reply(200, None)),
                // A registry token issued under an entry stays in its prefix,
                // even on the same origin.
                (BLOB, Some("Bearer account"), reply(307, Some(OUTSIDE))),
                (OUTSIDE, None, reply(200, None)),
            ]
            .into(),
        )));
        let client = Cached::with_options(
            cache.clone(),
            transport.clone(),
            Options {
                credentials: vec![entry, registry],
                ..Default::default()
            },
        )
        .unwrap();
        let key = |url| {
            HttpKey::new(
                "GET",
                url,
                BTreeMap::from([("accept".into(), "application/yaml".into())]),
            )
            .unwrap()
        };
        client.get(PUBLIC, "application/yaml").await.unwrap();
        assert!(cache.http_get(&key(PUBLIC)).await.unwrap().is_some());
        client.get(OTHER, "application/yaml").await.unwrap();
        let scope = client.credential(OTHER).unwrap().scope().to_owned();
        assert!(
            cache
                .http_get(&key(OTHER).credential(&scope))
                .await
                .unwrap()
                .is_none()
        );
        let bearer = Bearer {
            token: "account",
            account: true,
        };
        client
            .registry_get(BLOB, "text/plain", Some(bearer))
            .await
            .unwrap();
        assert!(transport.0.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn a_mirror_stands_in_for_its_public_registry_without_fallback() {
        const PUBLIC: &str = "https://registry.npmjs.org/react";
        const MIRROR: &str = "https://acme.jfrog.io/api/npm/npm-remote/react";
        let reply = |status: u16| Response {
            status: StatusCode::from_u16(status).unwrap(),
            headers: header::HeaderMap::new(),
            body: b"{}".to_vec(),
            stale: None,
        };
        let entry = Credential::new(
            "https://acme.jfrog.io/api/npm/npm-remote",
            crate::Auth::Bearer {
                token: "tok".into(),
            },
        )
        .unwrap()
        .mirroring("https://registry.npmjs.org")
        .unwrap();
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let transport = Arc::new(Scripted(Mutex::new(
            [
                (MIRROR, Some("Bearer tok"), reply(200)),
                // A failing mirror is an error, never a read of the public registry.
                (MIRROR, Some("Bearer tok"), reply(503)),
            ]
            .into(),
        )));
        let client = Cached::with_options(
            cache,
            transport.clone(),
            Options {
                credentials: vec![entry],
                ..Default::default()
            },
        )
        .unwrap();
        client.get(PUBLIC, "application/json").await.unwrap();
        assert!(
            client
                .revalidating()
                .get(PUBLIC, "application/json")
                .await
                .is_err()
        );
        assert!(transport.0.lock().unwrap().is_empty());
    }
    /// Coalesced reads leave nothing behind once their callers are done,
    /// whether they finish, fail or are cancelled.
    #[tokio::test]
    async fn coalesced_reads_are_dropped_when_no_caller_holds_them() {
        fn same(body: &mut dyn std::io::Read) -> Result<Vec<u8>, Error> {
            let mut bytes = Vec::new();
            body.read_to_end(&mut bytes).unwrap();
            Ok(bytes)
        }
        let (started, mut calls) = tokio::sync::mpsc::unbounded_channel();
        let transport = Arc::new(Blocked {
            started,
            release: Semaphore::new(0),
        });
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let client = Cached::with_options(cache, transport.clone(), Default::default()).unwrap();
        let read = |client: Cached| async move {
            client
                .get_derived(
                    "https://a.org/doc",
                    "text/plain",
                    &Derived::new("same", 1, same),
                )
                .await
        };
        let first = tokio::spawn(read(client.clone()));
        let second = tokio::spawn(read(client.clone()));
        calls.recv().await.unwrap();
        // Both callers hold the one read: the map, the fetching caller and the
        // one waiting on it.
        tokio::time::timeout(Duration::from_secs(2), async {
            while client
                .inner
                .reads
                .lock()
                .values()
                .map(Arc::strong_count)
                .ne([3])
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        transport.release.add_permits(1);
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(
            calls.try_recv().ok(),
            None,
            "the overlapping caller shared one fetch"
        );
        assert!(client.inner.reads.lock().is_empty());

        let cancelled = tokio::spawn(read(client.clone()));
        calls.recv().await.unwrap();
        cancelled.abort();
        assert!(cancelled.await.unwrap_err().is_cancelled());
        assert!(client.inner.reads.lock().is_empty());
    }

    fn derive_same(body: &mut dyn std::io::Read) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        Ok(bytes)
    }
    #[test]
    fn a_derived_spec_clones_like_its_copy() {
        fn cloned<T: Clone>(value: &T) -> T {
            value.clone()
        }
        let derived = Derived::new("same", 1, derive_same);
        let cloned = cloned(&derived);
        assert_eq!(cloned.name, "same");
        assert_eq!(cloned.representation(), "same@v1");
    }

    /// An entry stored under `doc@v1` — as any build writing that version
    /// would — is read by a later build whose transform kept version 1. A
    /// bump misses, and an entry keyed by the retired `name+build` scheme is
    /// never read again.
    #[tokio::test]
    async fn a_derived_body_is_versioned_by_its_code_not_the_release() {
        fn same(body: &mut dyn std::io::Read) -> Result<Vec<u8>, Error> {
            let mut bytes = Vec::new();
            body.read_to_end(&mut bytes).unwrap();
            Ok(bytes)
        }
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let url = "https://a.org/doc";
        let stored = |format: &str| {
            let key = HttpKey::new(
                "GET",
                url,
                BTreeMap::from([("accept".into(), "text/plain".into())]),
            )
            .unwrap()
            .representation(format);
            let now = conditional::unix_now();
            let cache = cache.clone();
            async move {
                cache
                    .http_put(
                        &key,
                        &HttpEntry {
                            status: 200,
                            etag: None,
                            last_modified: None,
                            headers: BTreeMap::new(),
                            body: b"stored".to_vec(),
                            fetched_at: now,
                            expires_at: Some(now + 3600),
                            cache_control: None,
                        },
                    )
                    .await
                    .unwrap();
            }
        };
        // A "release A" entry: the same representation this build computes.
        stored(&representation("doc", 1)).await;
        let transport = Arc::new(Count(std::sync::atomic::AtomicUsize::new(0)));
        let client =
            Cached::with_options(cache.clone(), transport.clone(), Default::default()).unwrap();
        let doc = Derived::new("doc", 1, same);
        assert_eq!(
            client
                .get_derived(url, "text/plain", &doc)
                .await
                .unwrap()
                .body,
            b"stored",
            "a later build reads an unchanged version"
        );
        assert_eq!(transport.0.load(Ordering::Relaxed), 0);
        // A bumped version is another representation: it misses and fetches.
        let bumped = Derived::new("doc", 2, same);
        assert_eq!(
            client
                .get_derived(url, "text/plain", &bumped)
                .await
                .unwrap()
                .body,
            b"new"
        );
        assert_eq!(transport.0.load(Ordering::Relaxed), 1);
        // The retired `name+build` scheme is never looked up again: with
        // only a `doc+0.0.0` entry left, a version-1 read fetches.
        for format in [representation("doc", 1), representation("doc", 2)] {
            cache
                .http_remove(
                    &HttpKey::new(
                        "GET",
                        url,
                        BTreeMap::from([("accept".into(), "text/plain".into())]),
                    )
                    .unwrap()
                    .representation(&format),
                )
                .await
                .unwrap();
        }
        stored("doc+0.0.0").await;
        assert_eq!(
            client
                .get_derived(url, "text/plain", &doc)
                .await
                .unwrap()
                .body,
            b"new"
        );
        assert_eq!(transport.0.load(Ordering::Relaxed), 2);
    }

    struct Always {
        status: u16,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl Transport for Always {
        fn send(&self, _: Request) -> crate::Exchange<'_> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let status = StatusCode::from_u16(self.status).unwrap();
            Box::pin(async move {
                Ok(Response {
                    status,
                    headers: Default::default(),
                    body: Vec::new(),
                    stale: None,
                })
            })
        }
    }

    /// Rules that remember every registry 404 for a day.
    #[derive(Debug)]
    struct RememberMissing;
    impl RegistryRules for RememberMissing {
        fn remember_missing(&self, _: &Url) -> Option<Duration> {
            Some(Duration::from_secs(24 * 3600))
        }
    }
    /// Without registry rules a missing manifest is asked about again on the
    /// next run rather than remembered.
    #[tokio::test]
    async fn a_registry_404_is_remembered_only_when_the_rules_say_so() {
        let url = "https://registry.example/v2/library/image/manifests/latest";
        for (rules, expected_calls) in [
            (Arc::new(NoRules) as Arc<dyn RegistryRules>, 2),
            (Arc::new(RememberMissing) as Arc<dyn RegistryRules>, 1),
        ] {
            let transport = Arc::new(Always {
                status: 404,
                calls: Default::default(),
            });
            let cache = Cache::open(":memory:", Default::default()).await.unwrap();
            let client = Cached::with_options(
                cache,
                transport.clone(),
                Options {
                    rules,
                    ..Default::default()
                },
            )
            .unwrap();
            for _ in 0..2 {
                assert!(matches!(
                    client.registry_get(url, "text/plain", None).await,
                    Err(Error::Status(404))
                ));
            }
            assert_eq!(transport.calls.load(Ordering::Relaxed), expected_calls);
        }
    }

    /// Rules that know the pull quota but not what a pull-limit 429 looks
    /// like get the default answer: it is not one, so the bare 429 is an
    /// ordinary throttle returned inside the retry budget.
    #[derive(Debug)]
    struct PullsOnly;
    impl RegistryRules for PullsOnly {
        fn pull_quota(&self, method: &Method, url: &Url) -> bool {
            *method == Method::GET
                && url.host_str() == Some("registry-1.docker.io")
                && url.path().contains("/manifests/")
        }
    }
    #[tokio::test]
    async fn pull_quota_without_pull_limit_knowledge_is_an_ordinary_throttle() {
        let transport = Arc::new(Always {
            status: 429,
            calls: Default::default(),
        });
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let client = Cached::with_options(
            cache,
            transport.clone(),
            Options {
                registry_retries: 0,
                rules: Arc::new(PullsOnly),
                ..Default::default()
            },
        )
        .unwrap();
        let response = client
            .registry_get(
                "https://registry-1.docker.io/v2/library/image/manifests/latest",
                "text/plain",
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
        // An ordinary throttle cools the host only briefly: the next pull
        // waits it out and is sent. The pull limit would Err at once.
        let response = client
            .registry_get(
                "https://registry-1.docker.io/v2/library/image/manifests/latest",
                "text/plain",
                None,
            )
            .await
            .unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(transport.calls.load(Ordering::Relaxed), 2);
    }
}
