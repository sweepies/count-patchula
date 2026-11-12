//! Transport boundary: requests can be built without constructing a network client.
#![forbid(unsafe_code)]

use reqwest::{Request, StatusCode, header::HeaderMap};
use std::{future::Future, pin::Pin, task::Poll};
use tracing::Instrument;

/// Provenance for an answer served from the cache while the registry was
/// rate limited: when the entry was fetched upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stale {
    pub fetched_at: i64,
}

/// Buffered response. Debug deliberately excludes headers and body.
#[derive(Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    /// Set when the cache served the entry because the registry was rate
    /// limited; `None` for answers that came from the network or a fresh hit.
    pub stale: Option<Stale>,
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
            // Bodies can be hundreds of megabytes: run the exchange on a
            // runtime worker so reading and inflating them does not block the
            // engine task. `Span::current` is the request's trace span, so
            // `protocol` and the transport's own events keep landing on it.
            // A spawned task gets a copy of the caller's task-locals, so nothing
            // in here may record or read `tracked`: credential use is recorded
            // in the caller before `send`, and a record made here would be lost.
            let client = self.client.clone();
            let exchange = AbortOnDrop(tokio::spawn(
                async move {
                    let response = client.execute(request).await.map_err(|_| Error::Network)?;
                    tracing::Span::current()
                        .record("protocol", tracing::field::debug(response.version()));
                    let status = response.status();
                    let headers = response.headers().clone();
                    let body = Vec::from(response.bytes().await.map_err(|_| Error::Network)?);
                    Ok(Response {
                        status,
                        headers,
                        body,
                        stale: None,
                    })
                }
                .instrument(tracing::Span::current()),
            ));
            match exchange.await {
                Ok(result) => result,
                Err(join) if join.is_panic() => std::panic::resume_unwind(join.into_panic()),
                Err(_) => Err(Error::Network),
            }
        })
    }
}

/// Abort a spawned task if the future owning it is dropped, so a cancelled
/// send does not leave its exchange running on a worker nobody observes.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, tokio::task::JoinError>;
    fn poll(mut self: Pin<&mut Self>, context: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(context)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The upper bound every client concurrency setting shares: registry host
/// limits, the GitHub client's in-flight cap and repository batch parallelism.
pub const MAX_CONCURRENCY: usize = 100;

mod cached;
pub mod conditional;
mod credentials;
pub use cached::{
    Bearer, Cached, Derived, Error as CacheError, NoRules, Options, RegistryRules, Stats,
    representation,
};
pub use credentials::{Auth, Credential, entry_for, prefix as url_prefix};
mod trace;
pub use trace::{PhaseTrace, RequestTrace, TOKEN_SHAPES};
mod tracked;
pub use tracked::tracked;

#[cfg(test)]
mod tests {
    use super::{Network, Transport};

    #[tokio::test]
    async fn loopback_http_serves_a_real_response() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nhi")
                .unwrap();
        });
        let network = Network::with_loopback_http().unwrap();
        let url = reqwest::Url::parse(&format!("http://{address}/")).unwrap();
        let request = reqwest::Request::new(reqwest::Method::GET, url);
        let response = network.send(request).await.unwrap();
        assert_eq!(response.status, reqwest::StatusCode::OK);
        assert_eq!(response.body, b"hi");
        assert_eq!(response.stale, None);
    }

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
