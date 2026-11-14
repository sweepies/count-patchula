//! The registry rules the Count's lookups follow, supplied to the generic
//! HTTP client: OCI digest immutability, tag-page and Go proxy floors,
//! remembered 404s and Docker Hub's pull quota.
use patchula_cache::HttpEntry;
use patchula_http::{Options, RegistryRules, Response};
use reqwest::{Method, Url};
use std::{sync::Arc, time::Duration};

/// How long a digest-addressed registry document stays fresh: ten years, so
/// only the cache's size budget evicts it.
const IMMUTABLE: Duration = Duration::from_secs(10 * 365 * 24 * 3600);
/// How long a registry's 404 for a digest or a cosign tag is kept.
const MISSING: Duration = Duration::from_secs(24 * 3600);
const DOCKER_HUB: &str = "registry-1.docker.io";
/// Docker Hub's pull quota window when a response does not name one: six
/// hours.
const PULL_WINDOW: Duration = Duration::from_secs(6 * 3600);
const GO_PROXY: &str = "proxy.golang.org";

/// The rules for the operator freshness floors in `options`.
pub fn registry_rules(options: &Options) -> Arc<dyn RegistryRules> {
    Arc::new(Rules {
        registry_freshness: Duration::from_secs(options.registry_freshness_secs),
        go_freshness: Duration::from_secs(options.go_freshness_secs),
    })
}

/// `Options` for a `Cached` that serves registry reads: the datasource rules
/// installed over the defaults.
pub fn http_options() -> Options {
    let mut options = Options::default();
    options.rules = registry_rules(&options);
    options
}

/// A `Cached` for registry reads at `concurrency` per host, outside the
/// engine's shared client.
pub fn cached(
    cache: patchula_cache::Cache,
    transport: std::sync::Arc<dyn patchula_http::Transport>,
    concurrency: usize,
) -> std::result::Result<patchula_http::Cached, patchula_http::CacheError> {
    patchula_http::Cached::with_options(
        cache,
        transport,
        Options {
            default_host_concurrency: concurrency,
            ..http_options()
        },
    )
}

#[derive(Debug)]
struct Rules {
    registry_freshness: Duration,
    go_freshness: Duration,
}

/// A manifest or blob addressed by digest, which never changes.
fn immutable(url: &Url) -> bool {
    url.path().contains("/manifests/sha256:") || url.path().contains("/blobs/sha256:")
}

impl RegistryRules for Rules {
    fn immutable(&self, url: &Url) -> bool {
        immutable(url)
    }
    fn freshness_floor(&self, url: &Url, entry: &HttpEntry, registry: bool) -> Option<Duration> {
        if registry && immutable(url) && (200..300).contains(&entry.status) {
            return Some(IMMUTABLE);
        }
        if registry
            && url.path().ends_with("/tags/list")
            && entry.etag.is_none()
            && entry.last_modified.is_none()
        {
            return Some(self.registry_freshness);
        }
        if url.host_str() == Some(GO_PROXY)
            && (url.path().ends_with("/@v/list") || url.path().ends_with("/@latest"))
        {
            return Some(self.go_freshness);
        }
        None
    }
    fn remember_missing(&self, url: &Url) -> Option<Duration> {
        (immutable(url) || [".att", ".sbom"].iter().any(|s| url.path().ends_with(s)))
            .then_some(MISSING)
    }
    fn pull_quota(&self, method: &Method, url: &Url) -> bool {
        *method == Method::GET
            && url.host_str() == Some(DOCKER_HUB)
            && url.path().contains("/manifests/")
    }
    fn quota_window(&self, url: &Url, response: &Response) -> Option<Duration> {
        (url.host_str() == Some(DOCKER_HUB))
            .then(|| exhausted_quota(response))
            .flatten()
    }
    fn pull_limit(&self, _url: &Url, response: &Response) -> Option<Duration> {
        pull_limited(response).then(|| exhausted_quota(response).unwrap_or(PULL_WINDOW))
    }
}

/// A manifest GET's 429 is Docker Hub's pull limit: its counter reads zero,
/// its body says so, or it gives no `Retry-After`, which the pull limit omits.
fn pull_limited(response: &Response) -> bool {
    response
        .headers
        .get("ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().parse::<u64>() == Ok(0))
        })
        || response
            .body
            .windows(b"pull rate limit".len())
            .any(|v| v.eq_ignore_ascii_case(b"pull rate limit"))
        || !response.headers.contains_key("retry-after")
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
        .map_or(PULL_WINDOW, Duration::from_secs);
    Some(window.max(Duration::from_secs(61)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use patchula_cache::{Cache, HttpKey};
    use patchula_http::{Bearer, CacheError, Cached, Transport, conditional};
    use reqwest::{
        Request, StatusCode,
        header::{self, HeaderMap, HeaderValue},
    };
    use std::{
        collections::BTreeMap,
        sync::atomic::{AtomicUsize, Ordering},
    };

    /// The datasource rules with these operator freshness floors.
    fn rules(registry_freshness_secs: u64, go_freshness_secs: u64) -> Arc<dyn RegistryRules> {
        registry_rules(&Options {
            registry_freshness_secs,
            go_freshness_secs,
            ..Default::default()
        })
    }
    struct Never;
    impl Transport for Never {
        fn send(&self, _: Request) -> patchula_http::Exchange<'_> {
            panic!("unexpected network request")
        }
    }
    struct Count(AtomicUsize);
    impl Transport for Count {
        fn send(&self, _: Request) -> patchula_http::Exchange<'_> {
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
    struct Always {
        status: u16,
        calls: AtomicUsize,
    }
    impl Transport for Always {
        fn send(&self, _: Request) -> patchula_http::Exchange<'_> {
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
    const BLOB: &str = "https://ghcr.io/v2/example/image/blobs/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// A missing attestation or SBOM is remembered for a day; a missing tag
    /// is asked about again.
    #[tokio::test]
    async fn a_missing_attestation_is_remembered_and_a_missing_tag_is_not() {
        for (url, expected_calls) in [
            (
                "https://registry-1.docker.io/v2/library/image/manifests/latest.att",
                1,
            ),
            (
                "https://registry-1.docker.io/v2/library/image/manifests/latest",
                2,
            ),
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
                    rules: rules(3600, 3600),
                    ..Default::default()
                },
            )
            .unwrap();
            for _ in 0..2 {
                assert!(matches!(
                    client.registry_get(url, "text/plain", None).await,
                    Err(CacheError::Status(404))
                ));
            }
            assert_eq!(transport.calls.load(Ordering::Relaxed), expected_calls);
        }
    }

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        Response {
            status: StatusCode::from_u16(status).unwrap(),
            headers: headers
                .iter()
                .map(|(k, v)| {
                    (
                        reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                        v.parse().unwrap(),
                    )
                })
                .collect::<HeaderMap>(),
            body: body.as_bytes().to_vec(),
            stale: None,
        }
    }

    /// A manifest GET's 429 is Docker Hub's pull limit when its counter
    /// reads zero, its body says so, or it omits `Retry-After` — the pull
    /// limit is the one throttle that does not ask to be retried.
    #[test]
    fn a_manifest_429_is_a_pull_limit_only_on_the_evidence() {
        let rules = registry_rules(&Options::default());
        let url =
            Url::parse("https://registry-1.docker.io/v2/library/image/manifests/latest").unwrap();
        let pull_limited = |response: Response| rules.pull_limit(&url, &response).is_some();
        // The counter spent: `remaining` parses to zero.
        assert!(pull_limited(response(
            429,
            &[("ratelimit-remaining", "0;w=21600"), ("retry-after", "30")],
            "{}",
        )));
        // Docker Hub's own wording, in any case.
        assert!(pull_limited(response(
            429,
            &[("retry-after", "30")],
            "You have reached your pull rate limit.",
        )));
        // No `Retry-After` at all: the pull limit omits it.
        assert!(pull_limited(response(429, &[], "{}")));
        // An ordinary throttle names its wait and keeps its counter.
        assert!(!pull_limited(response(
            429,
            &[
                ("ratelimit-remaining", "137;w=21600"),
                ("retry-after", "30"),
            ],
            "{}",
        )));
        // A spent counter on a non-pull-limit body still reads as limited:
        // the header alone is the evidence.
        assert!(pull_limited(response(
            429,
            &[("ratelimit-remaining", "0")],
            "{}",
        )));
    }

    #[tokio::test]
    async fn freshness_floors_use_fetch_time_and_can_be_disabled_on_existing_entries() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let go = "https://proxy.golang.org/example.org/mod/@v/list";
        let tags = "https://ghcr.io/v2/example/image/tags/list?n=1000";
        cached_entry(&cache, go, Some("max-age=60"), 300, None).await;
        cached_entry(&cache, tags, None, 300, None).await;
        let client = Cached::with_options(
            cache.clone(),
            Arc::new(Never),
            Options {
                rules: rules(3600, 3600),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(client.get(go, "text/plain").await.unwrap().body, b"catalog");
        assert_eq!(
            client
                .registry_get(tags, "text/plain", None)
                .await
                .unwrap()
                .body,
            b"catalog"
        );
        let transport = Arc::new(Count(AtomicUsize::new(0)));
        let client = Cached::with_options(
            cache.clone(),
            transport.clone(),
            Options {
                rules: rules(0, 0),
                ..Default::default()
            },
        )
        .unwrap();
        client.get(go, "text/plain").await.unwrap();
        client.registry_get(tags, "text/plain", None).await.unwrap();
        assert_eq!(transport.0.load(Ordering::Relaxed), 2);
        cached_entry(&cache, go, None, 3601, None).await;
        let client = Cached::with_options(
            cache,
            transport.clone(),
            Options {
                rules: rules(3600, 3600),
                ..Default::default()
            },
        )
        .unwrap();
        client.get(go, "text/plain").await.unwrap();
        assert_eq!(
            transport.0.load(Ordering::Relaxed),
            3,
            "floor expires from original fetch time"
        );
    }

    #[tokio::test]
    async fn no_cache_validators_and_unrelated_urls_do_not_get_registry_grace() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let transport = Arc::new(Count(AtomicUsize::new(0)));
        let client = Cached::with_options(
            cache.clone(),
            transport.clone(),
            Options {
                rules: rules(3600, 3600),
                ..Default::default()
            },
        )
        .unwrap();
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

    struct PullQuota {
        calls: AtomicUsize,
        header_only: bool,
    }
    impl Transport for PullQuota {
        fn send(&self, _: Request) -> patchula_http::Exchange<'_> {
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
                    stale: None,
                })
            })
        }
    }
    #[tokio::test]
    async fn zero_pull_counter_does_not_make_head_digests_depend_on_order_or_cache() {
        struct VersionChecks;
        impl Transport for VersionChecks {
            fn send(&self, request: Request) -> patchula_http::Exchange<'_> {
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
                        stale: None,
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
                        rules: rules(3600, 3600),
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
                    Err(CacheError::Throttled(_))
                ));
            }
        }
    }

    #[tokio::test]
    async fn docker_hub_pull_quota_is_not_a_short_retry() {
        for header_only in [false, true] {
            let transport = Arc::new(PullQuota {
                calls: AtomicUsize::new(0),
                header_only,
            });
            let cache = Cache::open(":memory:", Default::default()).await.unwrap();
            let client = Cached::with_options(
                cache,
                transport.clone(),
                Options {
                    rules: rules(3600, 3600),
                    ..Default::default()
                },
            )
            .unwrap();
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

    struct NoStore;
    impl Transport for NoStore {
        fn send(&self, _: Request) -> patchula_http::Exchange<'_> {
            Box::pin(async {
                Ok(Response {
                    status: StatusCode::OK,
                    headers: header::HeaderMap::from_iter([(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("no-store"),
                    )]),
                    body: b"blob".to_vec(),
                    stale: None,
                })
            })
        }
    }
    #[tokio::test]
    async fn a_digest_is_cached_despite_no_store_unless_an_account_read_it() {
        for (account, stored) in [(false, true), (true, false)] {
            let cache = Cache::open(":memory:", Default::default()).await.unwrap();
            let client = Cached::with_options(
                cache.clone(),
                Arc::new(NoStore),
                Options {
                    rules: rules(3600, 3600),
                    ..Default::default()
                },
            )
            .unwrap();
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

    struct Throttled(AtomicUsize);
    impl Transport for Throttled {
        fn send(&self, _: Request) -> patchula_http::Exchange<'_> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(Response {
                    status: StatusCode::TOO_MANY_REQUESTS,
                    headers: Default::default(),
                    body: Vec::new(),
                    stale: None,
                })
            })
        }
    }
    /// A HEAD does not consume Docker Hub's pull counter, so its bare 429 is an
    /// ordinary throttle, not hours without the registry.
    #[tokio::test]
    async fn a_bare_docker_hub_head_429_is_a_short_throttle() {
        let transport = Arc::new(Throttled(Default::default()));
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let client = Cached::with_options(
            cache,
            transport.clone(),
            Options {
                registry_retries: 0,
                rules: rules(3600, 3600),
                ..Default::default()
            },
        )
        .unwrap();
        let url = "https://registry-1.docker.io/v2/library/image/manifests/latest";
        for calls in [1, 2] {
            let response = client.registry_head(url, "text/plain", None).await.unwrap();
            assert_eq!(response.status, 429);
            assert_eq!(transport.0.load(Ordering::Relaxed), calls);
        }
    }

    /// A digest manifest is immutable, so its registry floor keeps an expired
    /// entry fresh without asking again.
    #[tokio::test]
    async fn an_immutable_manifest_outlives_its_expiry() {
        let url = "https://registry-1.docker.io/v2/library/image/manifests/sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        cached_entry(&cache, url, None, 300, None).await;
        let client = Cached::with_options(
            cache,
            Arc::new(Never),
            Options {
                rules: rules(3600, 3600),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            client
                .registry_get(url, "text/plain", None)
                .await
                .unwrap()
                .body,
            b"catalog"
        );
    }

    /// A nonzero pull counter and a clean body leave only the missing
    /// `Retry-After` to mark the 429 as the pull limit being spent.
    #[tokio::test]
    async fn a_remaining_pull_counter_without_retry_after_is_the_pull_limit() {
        struct Remaining(AtomicUsize);
        impl Transport for Remaining {
            fn send(&self, _: Request) -> patchula_http::Exchange<'_> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Box::pin(async {
                    Ok(Response {
                        status: StatusCode::TOO_MANY_REQUESTS,
                        headers: header::HeaderMap::from_iter([(
                            header::HeaderName::from_static("ratelimit-remaining"),
                            HeaderValue::from_static("5;w=3600"),
                        )]),
                        body: b"slow down".to_vec(),
                        stale: None,
                    })
                })
            }
        }
        let transport = Arc::new(Remaining(Default::default()));
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let client = Cached::with_options(
            cache,
            transport.clone(),
            Options {
                rules: rules(3600, 3600),
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
        // The pull limit is spent: no retry spent part of the budget, and the
        // next pull fails fast inside the six-hour window.
        assert_eq!(transport.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            client
                .registry_get(
                    "https://registry-1.docker.io/v2/library/image/manifests/latest",
                    "text/plain",
                    None,
                )
                .await,
            Err(CacheError::Throttled(_))
        ));
        assert_eq!(transport.0.load(Ordering::Relaxed), 1);
    }
}
