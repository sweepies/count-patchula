#![forbid(unsafe_code)]

use patchula_cache::{Cache, Options as CacheOptions};
use patchula_github::{Client, Error, GraphqlPage, Options, blob_sha, model};
use patchula_http::{Exchange, Response, Transport};
use patchula_testkit::{Options as FixtureOptions, Replay};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
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

/// Answers conditional GETs per path: a `Vary` response revalidates with its
/// stored validators; `Vary: *` is never stored, so every GET is a request.
struct Vary {
    counts: Mutex<BTreeMap<String, usize>>,
}
impl Transport for Vary {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let path = request.url().path().to_owned();
            *self.counts.lock().unwrap().entry(path.clone()).or_default() += 1;
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("etag", "\"v1\"".parse().unwrap());
            headers.insert("cache-control", "no-cache".parse().unwrap());
            let starred = path.ends_with("star");
            headers.insert(
                "vary",
                if starred {
                    "*"
                } else {
                    "Accept, Authorization"
                }
                .parse()
                .unwrap(),
            );
            if !starred && request.headers().contains_key("if-none-match") {
                return Ok(Response {
                    status: reqwest::StatusCode::NOT_MODIFIED,
                    headers,
                    body: Vec::new(),
                    stale: None,
                });
            }
            if !starred {
                headers.insert(
                    "link",
                    "<https://api.github.com/next>; rel=\"next\""
                        .parse()
                        .unwrap(),
                );
            }
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers,
                body: b"{\"ok\":true}".to_vec(),
                stale: None,
            })
        })
    }
}

#[tokio::test]
async fn a_vary_response_is_stored_once_and_revalidated_but_vary_star_is_not() {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await
    .unwrap();
    let transport = Arc::new(Vary {
        counts: Mutex::new(BTreeMap::new()),
    });
    let client =
        Client::with_transport(TOKEN, cache, Options::default(), transport.clone()).unwrap();
    let first = client.get("repos/o/r/vary").await.unwrap();
    let second = client.get("repos/o/r/vary").await.unwrap();
    assert_eq!(first.body, second.body);
    // The 304 kept the cached Link header the response did not repeat.
    assert_eq!(
        second.headers["link"],
        "<https://api.github.com/next>; rel=\"next\""
    );
    assert_eq!(client.stats().await.not_modified, 1);
    // An immutable read answers from any stored entry, so three requests
    // prove nothing was stored.
    for _ in 0..3 {
        client
            .get_immutable_response("repos/o/r/vary-star")
            .await
            .unwrap();
    }
    let counts = transport.counts.lock().unwrap();
    // One store and one revalidation: the Vary response lives under a single key.
    assert_eq!(counts["/repos/o/r/vary"], 2);
    assert_eq!(counts["/repos/o/r/vary-star"], 3);
}

/// Answers every request with the same JSON body, and counts them.
struct Counted(AtomicUsize);
impl Transport for Counted {
    fn send(&self, _: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("etag", "\"blob\"".parse().unwrap());
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers,
                body: b"{\"content\":\"aGk=\"}".to_vec(),
                stale: None,
            })
        })
    }
}

#[tokio::test]
async fn an_immutable_get_is_answered_from_the_cache_without_a_request() {
    let directory = tempfile::tempdir().unwrap();
    let cache = || async {
        Cache::open(
            directory.path().join("cache.sqlite"),
            CacheOptions::default(),
        )
        .await
        .unwrap()
    };
    let transport = Arc::new(Counted(AtomicUsize::new(0)));
    let client =
        Client::with_transport(TOKEN, cache().await, Options::default(), transport.clone())
            .unwrap();
    let path = "repos/acme/app/git/blobs/0123456789abcdef0123456789abcdef01234567";
    let first = client.get_immutable_response(path).await.unwrap();
    let second = client.get_immutable_response(path).await.unwrap();
    assert_eq!(first.body, second.body);
    assert_eq!(second.headers["etag"], "\"blob\"");
    assert_eq!(transport.0.load(Ordering::SeqCst), 1);
    // An ordinary GET still revalidates.
    client.get(path).await.unwrap();
    assert_eq!(transport.0.load(Ordering::SeqCst), 2);
    // Another credential has its own cache entries.
    let other = Client::with_transport(
        "ghp_otherTestCredential456",
        cache().await,
        Options::default(),
        transport.clone(),
    )
    .unwrap();
    other.get_immutable_response(path).await.unwrap();
    assert_eq!(transport.0.load(Ordering::SeqCst), 3);
}

/// GitHub for blobs: GraphQL gives `hi\n` right, a wrong text for the second
/// blob and nothing for a binary third; REST gives each blob as it is.
struct Blobs {
    graphql: AtomicUsize,
    rest: AtomicUsize,
}
const HI: &str = "45b983be36b73c0788dc9cbcb76cbb80fc7bb057";
const HO: &str = "8a3a5f1d1b5e7b8b9f1a1f2fa8ba9c2bb3f2e9f1";
const BIN: &str = "0000000000000000000000000000000000000b1b";
impl Transport for Blobs {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let body = if request.url().path() == "/graphql" {
                self.graphql.fetch_add(1, Ordering::SeqCst);
                let query: Value =
                    serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
                let mut repository = serde_json::Map::new();
                for (key, sha) in query["variables"].as_object().unwrap() {
                    let Some(index) = key.strip_prefix('o').filter(|i| i.parse::<usize>().is_ok())
                    else {
                        continue;
                    };
                    let blob = match sha.as_str().unwrap() {
                        HI => json!({"text":"hi\n","isBinary":false,"isTruncated":false}),
                        HO => json!({"text":"not ho","isBinary":false,"isTruncated":false}),
                        _ => json!({"text":null,"isBinary":true,"isTruncated":false}),
                    };
                    repository.insert(format!("b{index}"), blob);
                }
                json!({"data":{"repository":repository}})
            } else {
                self.rest.fetch_add(1, Ordering::SeqCst);
                let content = match request.url().path().rsplit('/').next().unwrap() {
                    HO => "aG8K",
                    _ => "AAE=",
                };
                json!({"encoding":"base64","content":content})
            };
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: reqwest::header::HeaderMap::new(),
                body: serde_json::to_vec(&body).unwrap(),
                stale: None,
            })
        })
    }
}

#[tokio::test]
async fn blobs_come_from_graphql_only_when_their_text_hashes_to_the_sha() {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await
    .unwrap();
    let transport = Arc::new(Blobs {
        graphql: AtomicUsize::new(0),
        rest: AtomicUsize::new(0),
    });
    let client =
        Client::with_transport(TOKEN, cache, Options::default(), transport.clone()).unwrap();
    let shas = [HI, HO, BIN].map(String::from);
    let blobs = client.blobs("acme/app", &shas).await.unwrap();
    assert_eq!(blobs[HI], b"hi\n");
    // The wrong GraphQL text is not taken; REST gives the blob.
    assert_eq!(blobs[HO], b"ho\n");
    assert_eq!(blobs[BIN], [0, 1]);
    assert_eq!(transport.graphql.load(Ordering::SeqCst), 1);
    assert_eq!(transport.rest.load(Ordering::SeqCst), 2);
    // Every blob is now cached, the GraphQL one as its REST form too.
    let again = client.blobs("acme/app", &shas).await.unwrap();
    assert_eq!(again, blobs);
    assert_eq!(
        client
            .get_immutable_response(&format!("repos/acme/app/git/blobs/{HI}"))
            .await
            .unwrap()
            .status,
        200
    );
    assert_eq!(transport.graphql.load(Ordering::SeqCst), 1);
    assert_eq!(transport.rest.load(Ordering::SeqCst), 2);
}

/// GitHub answering GraphQL blob batches by `mode`: texts, an errors payload,
/// or a refused credential; REST always gives each blob.
struct Batches {
    texts: std::collections::BTreeMap<String, String>,
    mode: &'static str,
    graphql: AtomicUsize,
    rest: AtomicUsize,
}
impl Transport for Batches {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let (status, body) = if request.url().path() == "/graphql" {
                self.graphql.fetch_add(1, Ordering::SeqCst);
                let query: Value =
                    serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
                let mut repository = serde_json::Map::new();
                for (key, sha) in query["variables"].as_object().unwrap() {
                    if let Some(index) =
                        key.strip_prefix('o').filter(|i| i.parse::<usize>().is_ok())
                    {
                        let text = &self.texts[sha.as_str().unwrap()];
                        repository.insert(
                            format!("b{index}"),
                            json!({"text":text,"isBinary":false,"isTruncated":false}),
                        );
                    }
                }
                match self.mode {
                    "errors" => (
                        200,
                        json!({"data":null,"errors":[{"message":"Something went wrong"}]}),
                    ),
                    "refused" => (401, json!({"message":"Bad credentials"})),
                    _ => (200, json!({"data":{"repository":repository}})),
                }
            } else {
                self.rest.fetch_add(1, Ordering::SeqCst);
                let sha = request.url().path().rsplit('/').next().unwrap();
                use base64::Engine;
                let content = base64::engine::general_purpose::STANDARD.encode(&self.texts[sha]);
                (200, json!({"encoding":"base64","content":content}))
            };
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: reqwest::header::HeaderMap::new(),
                body: serde_json::to_vec(&body).unwrap(),
                stale: None,
            })
        })
    }
}

async fn batches(
    mode: &'static str,
    count: usize,
) -> (tempfile::TempDir, Client, Arc<Batches>, Vec<String>) {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await
    .unwrap();
    let texts: std::collections::BTreeMap<_, _> = (0..count)
        .map(|n| {
            let text = format!("{n}\n");
            (blob_sha(text.as_bytes()), text)
        })
        .collect();
    let shas = texts.keys().cloned().collect();
    let transport = Arc::new(Batches {
        texts,
        mode,
        graphql: AtomicUsize::new(0),
        rest: AtomicUsize::new(0),
    });
    let client =
        Client::with_transport(TOKEN, cache, Options::default(), transport.clone()).unwrap();
    (directory, client, transport, shas)
}

#[tokio::test]
async fn blobs_past_fifty_take_one_graphql_query_per_fifty() {
    let (_directory, client, transport, shas) = batches("texts", 60).await;
    let blobs = client.blobs("acme/app", &shas).await.unwrap();
    assert_eq!(blobs.len(), 60);
    assert_eq!(blobs[&blob_sha(b"59\n")], b"59\n");
    assert_eq!(transport.graphql.load(Ordering::SeqCst), 2);
    assert_eq!(transport.rest.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_graphql_refusal_leaves_blobs_to_rest_but_a_refused_credential_stops() {
    let (_directory, client, transport, shas) = batches("errors", 3).await;
    assert_eq!(client.blobs("acme/app", &shas).await.unwrap().len(), 3);
    assert_eq!(transport.graphql.load(Ordering::SeqCst), 1);
    assert_eq!(transport.rest.load(Ordering::SeqCst), 3);
    let (_directory, client, _, shas) = batches("refused", 3).await;
    assert!(matches!(
        client.blobs("acme/app", &shas).await,
        Err(Error::Status(401))
    ));
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
        .graphql::<Value>(QUERY, json!({"cursor":null}))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        Error::Graphql(
            "Push blocked. Changes must be made through a pull request.; \
             Cannot update this protected ref."
                .into()
        )
    );
    assert_eq!(
        error.to_string(),
        "GitHub GraphQL returned errors: Push blocked. Changes must be made through a pull request.; \
         Cannot update this protected ref."
    );
    let text = format!("{error:?}");
    assert!(!text.contains("FORBIDDEN") && !text.contains(TOKEN) && !text.contains("simulated"));
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
            assert_eq!(
                request.headers()["user-agent"],
                concat!("patchula/", env!("CARGO_PKG_VERSION"))
            );
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: reqwest::header::HeaderMap::new(),
                body: b"{\"data\":{\"ok\":true}}".to_vec(),
                stale: None,
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
                    .graphql::<Value>("query { viewer { login } }", json!({}))
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
    // must miss the cache, so its request finds no interaction and fails as an
    // ordinary transport error without reaching any network.
    for error in [
        other.get("conditional").await.unwrap_err(),
        other
            .get("https://evil.invalid/?token=secret")
            .await
            .unwrap_err(),
        other
            .graphql::<Value>("sensitive query body", Value::Null)
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
    // A burst of one keeps every write a second apart, as the budget is
    // after a secondary-rate-limit answer.
    let github = Client::with_transport(
        "offline",
        cache,
        Options {
            write_burst: 1,
            ..Default::default()
        },
        transport,
    )
    .unwrap();
    let started = tokio::time::Instant::now();
    github
        .post::<Value>("repos/demo/app/git/trees", json!({}))
        .await
        .unwrap();
    github
        .post::<Value>("repos/demo/app/git/trees", json!({}))
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
    let started = tokio::time::Instant::now();
    github
        .graphql::<Value>("query{viewer{login}}", json!({}))
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    let (a, b) = tokio::join!(
        github.post::<Value>("a", json!({})),
        github.post::<Value>("b", json!({}))
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(observer.maximum.load(Ordering::SeqCst), 1);
}

/// Answers each request with the body its path maps to.
struct Json(BTreeMap<&'static str, &'static str>);
impl Transport for Json {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let body = self.0[request.url().path()].as_bytes().to_vec();
        Box::pin(async move {
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: reqwest::header::HeaderMap::new(),
                body,
                stale: None,
            })
        })
    }
}

#[derive(Debug, serde::Deserialize)]
struct Data {
    viewer: model::Actor,
}

async fn json_client(bodies: BTreeMap<&'static str, &'static str>) -> (tempfile::TempDir, Client) {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await
    .unwrap();
    let client =
        Client::with_transport(TOKEN, cache, Options::default(), Arc::new(Json(bodies))).unwrap();
    (directory, client)
}

#[tokio::test]
async fn typed_responses_decode_through_the_generic_methods() {
    let (_directory, client) = json_client(BTreeMap::from([
        (
            "/repos/o/r/git/ref/heads/main",
            r#"{"ref":"refs/heads/main","object":{"type":"commit","sha":"abc","url":"u"}}"#,
        ),
        (
            "/trees",
            r#"[{"path":"src/x.rs","mode":"100644","type":"blob","sha":"def"}]"#,
        ),
        (
            "/graphql",
            r#"{"data":{"viewer":{"login":"patchula[bot]"}}}"#,
        ),
        ("/repos/o/r/empty", ""),
    ]))
    .await;
    let reference = client
        .get_fresh::<model::GitRef>("repos/o/r/git/ref/heads/main")
        .await
        .unwrap();
    assert_eq!(reference.object.sha, "abc");
    let entries = client
        .rest_pages::<model::TreeEntry>("trees?per_page=2", |_| false)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, "blob");
    assert_eq!(entries[0].path, "src/x.rs");
    let data = client
        .graphql::<Data>("query{viewer{login}}", json!({}))
        .await
        .unwrap();
    assert_eq!(data.viewer.login, "patchula[bot]");
    // An empty body still decodes as `Value::Null`.
    assert_eq!(
        client
            .post::<Value>("repos/o/r/empty", json!({}))
            .await
            .unwrap(),
        Value::Null
    );
}

#[tokio::test]
async fn a_decode_error_names_operation_and_field_path_but_never_the_value() {
    // `truncated` wants a boolean and `viewer` a struct; each got a string a
    // serde type-mismatch message would quote, and that text must not leak.
    let (_directory, client) = json_client(BTreeMap::from([
        (
            "/repos/o/r/git/trees/abc",
            r#"{"tree":[],"truncated":"sensitive-body-text"}"#,
        ),
        ("/graphql", r#"{"data":{"viewer":"sensitive-body-text"}}"#),
    ]))
    .await;
    let error = client
        .get_fresh::<model::Tree>("repos/o/r/git/trees/abc")
        .await
        .unwrap_err();
    assert_eq!(
        error,
        Error::Decode {
            operation: "GET repos/o/r/git/trees/abc".into(),
            path: "truncated".into()
        }
    );
    let error = client
        .graphql::<Data>("query{viewer{login}}", json!({}))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        Error::Decode {
            operation: "query viewer".into(),
            path: "viewer".into()
        }
    );
    for output in [format!("{error}"), format!("{error:?}")] {
        assert!(output.contains("query viewer"), "{output}");
        assert!(output.contains("viewer"), "{output}");
        assert!(!output.contains("sensitive-body-text"), "{output}");
        assert!(!output.contains("expected struct"), "{output}");
    }
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
        github.post::<Value>("content", json!({})).await.unwrap();
    }
    assert_eq!(
        github
            .post::<Value>("content", json!({}))
            .await
            .unwrap_err(),
        Error::RateLimit
    );
    assert_eq!(github.stats().await.requests, 500);
}
