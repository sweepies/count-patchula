//! Transport boundary: requests can be built without constructing a network client.
#![forbid(unsafe_code)]

use reqwest::{Request, StatusCode, header::HeaderMap};
use std::{future::Future, pin::Pin};

/// Buffered response. Debug deliberately excludes headers and body.
#[derive(Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// Network errors intentionally discard reqwest's URL-bearing error details.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP transport failed")]
    Network,
    #[error("HTTP client could not be configured")]
    Configuration,
    #[error("fixture: {0}")]
    Fixture(String),
}

pub type Exchange<'a> = Pin<Box<dyn Future<Output = Result<Response, Error>> + Send + 'a>>;

/// Implementations must not log credentials or response bodies.
pub trait Transport: Send + Sync {
    fn send(&self, request: Request) -> Exchange<'_>;
}

/// One shared reqwest connection pool per batch; clones share its pool.
#[derive(Clone)]
pub struct Network(reqwest::Client);

impl Network {
    pub fn new() -> Result<Self, Error> {
        use std::time::Duration;
        reqwest::Client::builder()
            .user_agent(concat!("rustovate/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(8)
            // Pagination validates origins; automatic redirects must not bypass it.
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .https_only(true)
            .build()
            .map(Self)
            .map_err(|_| Error::Configuration)
    }
}

impl Transport for Network {
    fn send(&self, request: Request) -> Exchange<'_> {
        Box::pin(async move {
            let response = self.0.execute(request).await.map_err(|_| Error::Network)?;
            tracing::Span::current().record("protocol", tracing::field::debug(response.version()));
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.bytes().await.map_err(|_| Error::Network)?.to_vec();
            Ok(Response {
                status,
                headers,
                body,
            })
        })
    }
}

mod cached;
pub use cached::{Cached, Error as CacheError, Stats};
mod trace;
pub use trace::{PhaseTrace, RequestTrace};
