//! Conditional, unauthenticated registry GETs sharing the batch transport pool.
use crate::{RequestTrace, Response, Transport};
use reqwest::{
    Method, Request, StatusCode, Url,
    header::{self, HeaderValue},
};
use rustovate_cache::{Cache, HttpEntry, HttpKey};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
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
}
impl Default for Options {
    fn default() -> Self {
        Self {
            default_host_concurrency: 16,
            host_concurrency: BTreeMap::new(),
            registry_freshness_secs: 3600,
            go_freshness_secs: 3600,
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
        if !(1..=100).contains(&self.default_host_concurrency)
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
    Cache(#[from] rustovate_cache::Error),
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
    hosts: Mutex<BTreeMap<String, Arc<Semaphore>>>,
    options: Options,
    decoding: Arc<Semaphore>,
    requests: AtomicU64,
    not_modified: AtomicU64,
    cache_hits: AtomicU64,
    derived_reads: tokio::sync::Mutex<BTreeMap<String, Coalesced>>,
}
type Coalesced = Arc<tokio::sync::Mutex<Option<(Instant, Response)>>>;
type Derive = fn(&[u8]) -> Result<Vec<u8>, Error>;
#[derive(Clone)]
pub struct Cached(Arc<Inner>);
impl Cached {
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
        Ok(Self(Arc::new(Inner {
            transport,
            cache,
            hosts: Default::default(),
            options,
            decoding: Arc::new(Semaphore::new(4)),
            requests: AtomicU64::new(0),
            not_modified: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
            derived_reads: Default::default(),
        })))
    }
    async fn acquire(&self, url: &Url) -> Result<OwnedSemaphorePermit, Error> {
        let host = url.host_str().ok_or(Error::Configuration)?;
        let semaphore = self
            .0
            .hosts
            .lock()
            .map_err(|_| Error::Configuration)?
            .entry(host.to_owned())
            .or_insert_with(|| Arc::new(Semaphore::new(self.0.options.host_limit(host))))
            .clone();
        semaphore
            .acquire_owned()
            .await
            .map_err(|_| Error::Configuration)
    }
    pub fn stats(&self) -> Stats {
        Stats {
            requests: self.0.requests.load(Ordering::Relaxed),
            not_modified: self.0.not_modified.load(Ordering::Relaxed),
            cache_hits: self.0.cache_hits.load(Ordering::Relaxed),
        }
    }
    pub async fn get(&self, url: &str, accept: &str) -> Result<Response, Error> {
        self.get_representation(url, accept, None, None).await
    }

    /// Public registry content; bearer tokens never enter the persisted key.
    pub async fn registry_get(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
    ) -> Result<Response, Error> {
        self.registry_representation(url, accept, token, None).await
    }
    pub async fn registry_get_derived(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
        format: &'static str,
        derive: Derive,
    ) -> Result<Response, Error> {
        self.registry_representation(url, accept, token, Some((format, derive)))
            .await
    }
    async fn registry_representation(
        &self,
        url: &str,
        accept: &str,
        token: Option<&str>,
        derived: Option<(&'static str, Derive)>,
    ) -> Result<Response, Error> {
        let started = Instant::now();
        let key = format!(
            "registry:{}:{url}\n{accept}\n{}",
            token.is_some(),
            derived.map_or("", |(format, _)| format)
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
            .get_representation(url, accept, derived, Some(token))
            .await?;
        if response.status.is_success() {
            *previous = Some((Instant::now(), response.clone()));
        }
        Ok(response)
    }

    /// Fetch an anonymous token without caching the raw response. The registry
    /// caller may persist only its scoped bearer with a validated expiry.
    pub async fn token_get(&self, url: &str) -> Result<Response, Error> {
        let url = Url::parse(url).map_err(|_| Error::Configuration)?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Configuration);
        }
        let trace = RequestTrace::new("GET", &url);
        let mut request = Request::new(Method::GET, url.clone());
        request
            .headers_mut()
            .insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        let queued = Instant::now();
        let _permit = self.acquire(&url).await?;
        self.0.requests.fetch_add(1, Ordering::Relaxed);
        Ok(trace
            .send(self.0.transport.as_ref(), request, queued.elapsed(), false)
            .await?)
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
        let key = format!("{url}\n{accept}\n{format}");
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
            .get_representation(url, accept, Some((format, derive)), None)
            .await?;
        *previous = Some((Instant::now(), response.clone()));
        Ok(response)
    }

    async fn get_representation(
        &self,
        url: &str,
        accept: &str,
        derived: Option<(&'static str, Derive)>,
        registry: Option<Option<&str>>,
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
            "GET",
            url.as_str(),
            BTreeMap::from([("accept".into(), accept.into())]),
        )?;
        if let Some((format, _)) = derived {
            key = key.representation(format);
        }
        let trace = RequestTrace::new("GET", &url);
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
        {
            self.0.cache_hits.fetch_add(1, Ordering::Relaxed);
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
        let mut request = Request::new(Method::GET, url.clone());
        if let Some(Some(token)) = registry {
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
            HeaderValue::from_static(concat!("rustovate/", env!("CARGO_PKG_VERSION"))),
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
        let queued = Instant::now();
        let permit = self.acquire(&url).await?;
        self.0.requests.fetch_add(1, Ordering::Relaxed);
        let mut response = trace
            .send(
                self.0.transport.as_ref(),
                request,
                queued.elapsed(),
                cached.is_some(),
            )
            .await?;
        drop(permit);
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
        let uncacheable = control.as_ref().is_some_and(|c| {
            c.split(',')
                .any(|v| v.trim().eq_ignore_ascii_case("no-store"))
        }) || text("vary").is_some_and(|v| {
            v.split(',').any(|n| {
                !["accept", "accept-encoding", "user-agent"]
                    .iter()
                    .any(|known| n.trim().eq_ignore_ascii_case(known))
            })
        });
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
