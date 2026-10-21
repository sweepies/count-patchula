#![forbid(unsafe_code)]

use patchula_cache::{Cache, Options as CacheOptions};
use patchula_github::{Client, Error, GraphqlPage, Options};
use patchula_http::{Exchange, Response, Transport};
use patchula_testkit::{Options as FixtureOptions, Replay};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

const QUERY: &str = "query($cursor: String) { repository(owner: \"patchula\", name: \"simulated\") { releases(first: 2, after: $cursor) { nodes { name } pageInfo { hasNextPage endCursor } } } }";
const TOKEN: &str = "ghp_privateTestCredential123";

async fn client(name: &str, options: Options) -> (tempfile::TempDir, Client, Arc<Replay>) {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await
    .unwrap();
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/github/{name}.json"));
    let replay = Arc::new(Replay::open(&path, FixtureOptions::default()).unwrap());
    let client = Client::with_transport(TOKEN, cache, options, replay.clone()).unwrap();
    (directory, client, replay)
}

#[tokio::test]
async fn every_get_uses_validators_and_304_preserves_body_and_link_metadata() {
    let (_directory, client, replay) = client("conditional", Options::default()).await;
    let first = client.get("conditional").await.unwrap();
    let second = client.get("conditional").await.unwrap();
    assert_eq!(first.body, second.body);
    assert_eq!(second.status, 200);
    let original_links: Vec<_> = first
        .headers
        .get_all("link")
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    assert_eq!(original_links.join(", "), second.headers["link"]);
    assert_eq!(second.headers["cache-control"], "private, max-age=120");
    assert_eq!(client.stats().await.not_modified, 1);
    replay.assert_finished().unwrap();
}

#[tokio::test(start_paused = true)]
async fn secondary_limit_has_exponential_backoff_and_report_stats() {
    let (_directory, client, replay) = client("secondary", Options::default()).await;
    client.get("secondary").await.unwrap();
    let stats = client.stats().await;
    assert_eq!(stats.requests, 3);
    assert_eq!(stats.retries, 2);
    assert!(stats.wait_millis >= 180_000, "{stats:?}");
    assert_eq!(stats.primary["core"].limit, Some(5000));
    assert_eq!(stats.primary["core"].used, Some(3));
    assert_eq!(stats.primary["core"].remaining, Some(4997));
    replay.assert_finished().unwrap();
}

#[tokio::test(start_paused = true)]
async fn retry_after_is_honored_and_primary_zero_delays_the_next_request() {
    let (_directory, client, replay) = client("retry-after", Options::default()).await;
    client.get("retry-after").await.unwrap();
    assert!(client.stats().await.wait_millis >= 2_000);
    replay.assert_finished().unwrap();
    let (_directory, client, replay) = self::client("primary", Options::default()).await;
    client.get("primary").await.unwrap();
    client.get("primary-next").await.unwrap();
    assert!(client.stats().await.wait_millis >= 1_000);
    replay.assert_finished().unwrap();
}

#[tokio::test(start_paused = true)]
async fn retry_budget_is_finite_and_permission_errors_are_not_retried() {
    let (_directory, client, replay) = client("retry-exhausted", Options::default()).await;
    assert_eq!(client.get("exhausted").await.unwrap_err(), Error::RateLimit);
    assert_eq!(client.stats().await.requests, 4);
    assert_eq!(client.stats().await.retries, 3);
    replay.assert_finished().unwrap();
    let (_directory, client, replay) = self::client("forbidden", Options::default()).await;
    let error = client.get("forbidden").await.unwrap_err();
    assert_eq!(error, Error::Status(403));
    assert_eq!(client.stats().await.requests, 1);
    for output in [
        format!("{client:?}"),
        format!("{client}"),
        format!("{error:?}"),
        format!("{error}"),
    ] {
        assert!(!output.contains(TOKEN));
        assert!(!output.contains("Permission denied"));
    }
    replay.assert_finished().unwrap();
}

#[tokio::test]
async fn rest_link_pagination_stops_at_known_history_before_requesting_another_page() {
    let (_directory, client, replay) = client("rest-pages", Options::default()).await;
    let nodes = client
        .rest_pages::<Value>("pages?per_page=2", |page| {
            page.iter().any(|item| item["name"] == "v2")
        })
        .await
        .unwrap();
    assert_eq!(nodes.len(), 4);
    assert_eq!(client.stats().await.requests, 2);
    replay.assert_finished().unwrap();
}

#[tokio::test]
async fn graphql_matches_post_cursors_and_stops_before_the_next_page() {
    let (_directory, client, replay) = client("graphql-pages", Options::default()).await;
    let nodes = client
        .graphql_pages(
            QUERY,
            json!({}),
            |data| GraphqlPage::<Value>::from_connection(&data, "/repository/releases"),
            |nodes| nodes.iter().any(|node| node["name"] == "v2"),
        )
        .await
        .unwrap();
    assert_eq!(nodes.len(), 4);
    assert_eq!(client.stats().await.requests, 2);
    replay.assert_finished().unwrap();
}

#[tokio::test]
async fn no_store_does_not_supply_validators_on_the_next_get() {
    let (_directory, client, replay) = client("no-store", Options::default()).await;
    client.get("no-store").await.unwrap();
    client.get("no-store").await.unwrap();
    replay.assert_finished().unwrap();
}

#[tokio::test]
async fn pagination_rejects_loops_and_cross_origin_links() {
    for (name, path, expected) in [
        ("rest-loop", "loop", Error::Pagination),
        ("unsafe-link", "unsafe", Error::Url),
    ] {
        let (_directory, client, replay) = client(name, Options::default()).await;
        assert_eq!(
            client
                .rest_pages::<Value>(path, |_| false)
                .await
                .unwrap_err(),
            expected
        );
        assert_eq!(client.stats().await.requests, 1);
        replay.assert_finished().unwrap();
    }
    let (_directory, client, replay) = client("graphql-loop", Options::default()).await;
    let error = client
        .graphql_pages(
            QUERY,
            json!({}),
            |data| GraphqlPage::<Value>::from_connection(&data, "/repository/releases"),
            |_| false,
        )
        .await
        .unwrap_err();
    assert_eq!(error, Error::Pagination);
    assert_eq!(client.stats().await.requests, 2);
    replay.assert_finished().unwrap();
}

#[tokio::test]
async fn graphql_errors_and_uncached_304s_fail_safely_and_rest_redirects_are_governed() {
    let (_directory, client, replay) = client("graphql-errors", Options::default()).await;
    let error = client
        .graphql(QUERY, json!({"cursor":null}))
        .await
        .unwrap_err();
    assert_eq!(error, Error::Graphql);
    assert!(!format!("{error:?}").contains("private response body"));
    replay.assert_finished().unwrap();
    let (_directory, client, replay) = self::client("missing-body", Options::default()).await;
    assert_eq!(
        client.get("missing-body").await.unwrap_err(),
        Error::MissingBody
    );
    replay.assert_finished().unwrap();
    let (_directory, client, replay) = self::client("redirect", Options::default()).await;
    assert_eq!(client.get("redirect").await.unwrap().status, 200);
    assert_eq!(client.stats().await.requests, 2);
    replay.assert_finished().unwrap();
}

struct Concurrent {
    active: AtomicUsize,
    maximum: AtomicUsize,
}
impl Transport for Concurrent {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            assert!(request.headers()["authorization"].is_sensitive());
            assert_eq!(request.headers()["user-agent"], "patchula/0.1.0");
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: reqwest::header::HeaderMap::new(),
                body: b"{\"data\":{\"ok\":true}}".to_vec(),
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clones_share_one_concurrency_cap_for_rest_and_graphql() {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await
    .unwrap();
    let transport = Arc::new(Concurrent {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
    });
    let client = Client::with_transport(
        TOKEN,
        cache,
        Options {
            concurrency: 2,
            ..Options::default()
        },
        transport.clone(),
    )
    .unwrap();
    let mut tasks = Vec::new();
    for i in 0..20 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            if i % 2 == 0 {
                client.get(&format!("repos/test/{i}")).await.unwrap();
            } else {
                client
                    .graphql("query { viewer { login } }", json!({}))
                    .await
                    .unwrap();
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(transport.maximum.load(Ordering::SeqCst), 2);
    assert_eq!(client.stats().await.requests, 20);
    assert!(client.stats().await.wait_millis > 0);
}

#[tokio::test]
async fn credentials_are_partitioned_in_cache_and_errors_hide_credentials_and_bodies() {
    let (_directory, client, replay) = client("conditional", Options::default()).await;
    client.get("conditional").await.unwrap();
    let other = Client::with_transport(
        "different-private-token",
        {
            // Use the same on-disk cache through another handle to test partitioning.
            Cache::open(
                _directory.path().join("cache.sqlite"),
                CacheOptions::default(),
            )
            .await
            .unwrap()
        },
        Options::default(),
        replay,
    )
    .unwrap();
    // The remaining cassette expects validators. A different credential scope
    // must miss the cache, so it fails matching without reaching any network.
    let error = other.get("conditional").await.unwrap_err();
    assert!(matches!(error, Error::Fixture(_)));
    assert!(error.to_string().contains("unmatched GET"));
    for error in [
        error,
        other
            .get("https://evil.invalid/?token=secret")
            .await
            .unwrap_err(),
        other
            .graphql("sensitive query body", Value::Null)
            .await
            .unwrap_err(),
    ] {
        let output = format!("{error:?} {error}");
        for secret in [
            TOKEN,
            "different-private-token",
            "sensitive query body",
            "token=secret",
        ] {
            assert!(!output.contains(secret));
        }
    }
    let cache = Cache::open(
        _directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await
    .unwrap();
    assert!(matches!(
        Client::with_transport(
            "bad\ntoken",
            cache,
            Options::default(),
            Arc::new(Concurrent {
                active: AtomicUsize::new(0),
                maximum: AtomicUsize::new(0)
            })
        ),
        Err(Error::Configuration)
    ));
}

#[tokio::test(start_paused = true)]
async fn mutations_share_serial_cooldown_but_graphql_queries_do_not() {
    let transport = Arc::new(Concurrent {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
    });
    let observer = transport.clone();
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let github = Client::with_transport("offline", cache, Default::default(), transport).unwrap();
    let started = tokio::time::Instant::now();
    github
        .post("repos/demo/app/git/trees", json!({}))
        .await
        .unwrap();
    github
        .post("repos/demo/app/git/trees", json!({}))
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    let started = tokio::time::Instant::now();
    github
        .graphql("query{viewer{login}}", json!({}))
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    let (a, b) = tokio::join!(github.post("a", json!({})), github.post("b", json!({})));
    a.unwrap();
    b.unwrap();
    assert_eq!(observer.maximum.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn content_hour_budget_fails_before_sending_when_wait_exceeds_budget() {
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let transport = Arc::new(Concurrent {
        active: AtomicUsize::new(0),
        maximum: AtomicUsize::new(0),
    });
    let github = Client::with_transport(
        "offline",
        cache,
        Options {
            max_rate_wait: Duration::from_secs(5),
            ..Default::default()
        },
        transport,
    )
    .unwrap();
    for _ in 0..500 {
        github.post("content", json!({})).await.unwrap();
    }
    assert_eq!(
        github.post("content", json!({})).await.unwrap_err(),
        Error::RateLimit
    );
    assert_eq!(github.stats().await.requests, 500);
}
