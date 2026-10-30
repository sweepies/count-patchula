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
pub struct Network {
    client: reqwest::Client,
    loopback_http: bool,
}

impl Network {
    /// HTTPS only.
    pub fn new() -> Result<Self, Error> {
        Self::build(false)
    }
    /// HTTPS, plus plain HTTP to loopback addresses, for a local `patchula serve`.
    pub fn with_loopback_http() -> Result<Self, Error> {
        Self::build(true)
    }
    fn build(loopback_http: bool) -> Result<Self, Error> {
        use std::time::Duration;
        reqwest::Client::builder()
            .user_agent(concat!("patchula/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(32)
            // Pagination validates origins; automatic redirects must not bypass it.
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .https_only(!loopback_http)
            .build()
            .map(|client| Self {
                client,
                loopback_http,
            })
            .map_err(|_| Error::Configuration)
    }
    fn allowed(&self, url: &reqwest::Url) -> bool {
        url.scheme() == "https"
            || (self.loopback_http
                && url.scheme() == "http"
                && url.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host
                            .trim_start_matches('[')
                            .trim_end_matches(']')
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                }))
    }
}

impl Transport for Network {
    fn send(&self, request: Request) -> Exchange<'_> {
        Box::pin(async move {
            if !self.allowed(request.url()) {
                return Err(Error::Network);
            }
            let response = self
                .client
                .execute(request)
                .await
                .map_err(|_| Error::Network)?;
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
pub use cached::{Bearer, Cached, Error as CacheError, Options, Stats};
mod trace;
pub use trace::{PhaseTrace, RequestTrace};

#[cfg(test)]
mod tests {
    use super::Network;

    #[test]
    fn only_the_loopback_mode_allows_plain_http_and_only_to_loopback() {
        let url = |u: &str| reqwest::Url::parse(u).unwrap();
        let https = Network::new().unwrap();
        let loopback = Network::with_loopback_http().unwrap();
        for allowed in [
            "http://localhost:8080/",
            "http://127.0.0.1/",
            "http://[::1]:8080/",
        ] {
            assert!(!https.allowed(&url(allowed)), "{allowed}");
            assert!(loopback.allowed(&url(allowed)), "{allowed}");
        }
        for refused in [
            "http://patchula.acme.dev/",
            "http://10.0.0.1/",
            "ftp://localhost/",
        ] {
            assert!(!loopback.allowed(&url(refused)), "{refused}");
        }
        assert!(https.allowed(&url("https://patchula.acme.dev/")));
    }
}
