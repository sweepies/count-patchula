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
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

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
    semaphore: Semaphore,
    decoding: Arc<Semaphore>,
    requests: AtomicU64,
    not_modified: AtomicU64,
    cache_hits: AtomicU64,
}
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
        Ok(Self(Arc::new(Inner {
            transport,
            cache,
            semaphore: Semaphore::new(concurrency),
            decoding: Arc::new(Semaphore::new(4)),
            requests: AtomicU64::new(0),
            not_modified: AtomicU64::new(0),
            cache_hits: AtomicU64::new(0),
        })))
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
        self.get_representation(url, accept, None, Some(token))
            .await
    }

    /// Token responses must never enter SQLite.
    pub async fn token_get(&self, url: &str) -> Result<Response, Error> {
        let url = Url::parse(url).map_err(|_| Error::Configuration)?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err(Error::Configuration);
        }
        let trace = RequestTrace::new("GET", &url);
        let mut request = Request::new(Method::GET, url);
        request
            .headers_mut()
            .insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        let queued = Instant::now();
        let _permit = self
            .0
            .semaphore
            .acquire()
            .await
            .map_err(|_| Error::Configuration)?;
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
        self.get_representation(url, accept, Some((format, derive)), None)
            .await
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
            .and_then(|entry| entry.expires_at)
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
            && entry.expires_at.is_some_and(|expiry| expiry > now)
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
        let mut request = Request::new(Method::GET, url);
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
        let permit = self
            .0
            .semaphore
            .acquire()
            .await
            .map_err(|_| Error::Configuration)?;
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
