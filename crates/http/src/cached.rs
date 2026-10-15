//! Conditional, unauthenticated registry GETs sharing the batch transport pool.
use crate::{Response, Transport};
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
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
    requests: AtomicU64,
    not_modified: AtomicU64,
    cache_hits: AtomicU64,
}
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
        let url = Url::parse(url).map_err(|_| Error::Configuration)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::Configuration);
        }
        let key = HttpKey::new(
            "GET",
            url.as_str(),
            BTreeMap::from([("accept".into(), accept.into())]),
        )?;
        let cached = self.0.cache.http_get(&key).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        if let Some(entry) = &cached
            && entry.etag.is_none()
            && entry.last_modified.is_none()
            && entry.expires_at.is_some_and(|expiry| expiry > now)
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
            return Ok(Response {
                status: StatusCode::from_u16(entry.status).map_err(|_| Error::Configuration)?,
                headers,
                body: entry.body.clone(),
            });
        }
        let mut request = Request::new(Method::GET, url);
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
        let _permit = self
            .0
            .semaphore
            .acquire()
            .await
            .map_err(|_| Error::Configuration)?;
        self.0.requests.fetch_add(1, Ordering::Relaxed);
        let mut response = self.0.transport.send(request).await?;
        if response.status == StatusCode::NOT_MODIFIED {
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
        if !response.status.is_success() {
            return Err(Error::Status(response.status.as_u16()));
        }
        let text = |name: &str| {
            response
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let control = text("cache-control");
        let uncacheable = control
            .as_ref()
            .is_some_and(|c| c.split(',').any(|v| v.trim() == "no-store"))
            || text("vary").is_some_and(|v| {
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
                    .and_then(|age| now.checked_add(age)),
                cache_control: control,
            };
            self.0.cache.http_put(&key, &entry).await?;
        }
        Ok(response)
    }
}
