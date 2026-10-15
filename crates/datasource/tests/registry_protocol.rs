//! Independently simulated protocol edge cases, never live HTTP.
use rustovate_cache::Cache;
use rustovate_config::Source;
use rustovate_datasource::Client;
use rustovate_http::{Cached, Exchange, Response, Transport};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

struct Steps(Mutex<VecDeque<(String, Response)>>);
impl Transport for Steps {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let (url, response) = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected network read");
        assert_eq!(request.url().as_str(), url);
        if response.status == 304 {
            assert_eq!(request.headers()["if-none-match"], "\"tags\"");
        }
        Box::pin(async {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok(response)
        })
    }
}
fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> Response {
    Response {
        status: reqwest::StatusCode::from_u16(status).unwrap(),
        body: body.as_bytes().to_vec(),
        headers: headers
            .iter()
            .map(|(k, v)| {
                (
                    reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    v.parse().unwrap(),
                )
            })
            .collect(),
    }
}
async fn client(transport: Arc<Steps>, dir: &std::path::Path) -> Client {
    let cache = Cache::open(dir.join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let github = rustovate_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        transport.clone(),
    )
    .unwrap();
    Client::new(
        github,
        Cached::with_options(
            cache.clone(),
            transport,
            rustovate_http::Options {
                registry_retries: 0,
                ..Default::default()
            },
        )
        .unwrap(),
        cache,
    )
}
#[tokio::test]
async fn lexical_pagination_is_complete_and_conditional_links_survive_304() {
    let a = "https://ghcr.io/v2/spec/image/tags/list?n=1000";
    let b = "https://ghcr.io/v2/spec/image/tags/list?n=1000&last=1.0";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            a.into(),
            response(
                200,
                r#"{"tags":["1.0"]}"#,
                &[
                    ("etag", "\"tags\""),
                    (
                        "link",
                        "</v2/spec/image/tags/list?n=1000&last=1.0>; rel=\"next\"",
                    ),
                ],
            ),
        ),
        (
            b.into(),
            response(
                200,
                r#"{"tags":["2.0-alpine","2.0-bookworm"]}"#,
                &[("etag", "\"tags\"")],
            ),
        ),
        (a.into(), response(304, "", &[])),
        (b.into(), response(304, "", &[])),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let client = client(steps.clone(), dir.path()).await;
    for _ in 0..2 {
        assert_eq!(
            client
                .lookup(Source::Docker, "ghcr.io/spec/image")
                .await
                .unwrap()
                .versions
                .len(),
            3
        );
    }
    assert!(steps.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn rate_limit_and_foreign_pagination_are_explicit() {
    for (response, expected) in [
        (
            response(429, "{}", &[("retry-after", "42")]),
            "retry after 42",
        ),
        (
            response(
                200,
                r#"{"tags":["1.0"]}"#,
                &[(
                    "link",
                    "<https://other.invalid/v2/spec/image/tags/list>; rel=\"next\"",
                )],
            ),
            "invalid package",
        ),
    ] {
        let steps = Arc::new(Steps(Mutex::new(VecDeque::from([(
            "https://ghcr.io/v2/spec/image/tags/list?n=1000".into(),
            response,
        )]))));
        let dir = tempfile::tempdir().unwrap();
        assert!(
            client(steps, dir.path())
                .await
                .lookup(Source::Docker, "ghcr.io/spec/image")
                .await
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
}

#[tokio::test]
async fn concurrent_charts_share_one_compact_index_but_later_calls_revalidate() {
    let url = "https://charts.example.org/index.yaml";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            url.into(),
            response(
                200,
                "apiVersion: v1\nentries:\n  first:\n    - version: 1.0.0\n  second:\n    - version: 2.0.0\n",
                &[("etag", "\"tags\""), ("cache-control", "no-cache")],
            ),
        ),
        (url.into(), response(304, "", &[])),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let client = client(steps.clone(), dir.path()).await;
    let (a, b) = tokio::join!(
        client.lookup(Source::Helm, "https://charts.example.org|first"),
        client.lookup(Source::Helm, "https://charts.example.org|second")
    );
    assert_eq!(a.unwrap().versions[0].version, "1.0.0");
    assert_eq!(b.unwrap().versions[0].version, "2.0.0");
    assert_eq!(
        steps.0.lock().unwrap().len(),
        1,
        "overlapping callers share the original document"
    );
    assert!(
        client
            .lookup(Source::Helm, "https://charts.example.org|first")
            .await
            .is_ok()
    );
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn validator_free_tags_are_compact_and_survive_a_new_client() {
    let url = "https://ghcr.io/v2/spec/image/tags/list?n=1000";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([(
        url.into(),
        response(200, r#"{"tags":["2.0","1.0"],"unused":"discarded"}"#, &[]),
    )]))));
    let dir = tempfile::tempdir().unwrap();
    let first = client(steps.clone(), dir.path())
        .await
        .lookup(Source::Docker, "ghcr.io/spec/image")
        .await
        .unwrap();
    let second = client(steps.clone(), dir.path())
        .await
        .lookup(Source::Docker, "ghcr.io/spec/image")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(second).unwrap()
    );
    assert!(steps.0.lock().unwrap().is_empty());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let key = rustovate_cache::HttpKey::new(
        "GET",
        url,
        std::collections::BTreeMap::from([("accept".into(), "application/json".into())]),
    )
    .unwrap()
    .representation("oci-tags-v1");
    let entry = cache.http_get(&key).await.unwrap().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&entry.body).unwrap(),
        serde_json::json!({"tags":["2.0","1.0"]})
    );
}

#[tokio::test]
async fn go_skips_latest_for_tagged_versions_and_preserves_pseudo_version_fallback() {
    for (list, latest, expected) in [
        ("v1.0.0\nv2.0.0-beta.1\n", None, "v1.0.0"),
        ("v2.0.0-beta.1\n", None, "v2.0.0-beta.1"),
        (
            "",
            Some("v0.0.0-20261001000000-abcdefabcdef"),
            "v0.0.0-20261001000000-abcdefabcdef",
        ),
    ] {
        let mut reads = VecDeque::from([(
            "https://proxy.golang.org/example.org/mod/@v/list".into(),
            response(200, list, &[]),
        )]);
        if let Some(latest) = latest {
            reads.push_back((
                "https://proxy.golang.org/example.org/mod/@latest".into(),
                response(
                    200,
                    &format!(r#"{{"Version":"{latest}","Time":"2026-10-01T00:00:00Z"}}"#),
                    &[],
                ),
            ));
        }
        let mut versions: Vec<_> = list.lines().collect();
        if let Some(latest) = latest {
            versions.push(latest);
        }
        // The source sorts descending and hydrates every new tagged version.
        versions.sort_by(|a, b| b.cmp(a));
        for version in versions {
            reads.push_back((
                format!("https://proxy.golang.org/example.org/mod/@v/{version}.info"),
                response(200, r#"{"Time":"2026-10-01T00:00:00Z"}"#, &[]),
            ));
        }
        let steps = Arc::new(Steps(Mutex::new(reads)));
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            let catalog = client(steps.clone(), dir.path())
                .await
                .lookup(Source::Go, "example.org/mod")
                .await
                .unwrap();
            assert_eq!(catalog.dist_tags["latest"], expected);
            assert!(catalog.versions.iter().all(|v| v.published_at.is_some()));
        }
        assert!(steps.0.lock().unwrap().is_empty());
    }
}

struct AuthSteps(Mutex<VecDeque<(String, Option<String>, Response)>>);
impl Transport for AuthSteps {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let (url, bearer, response) = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        assert_eq!(request.url().as_str(), url);
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .map(|v| v.to_str().unwrap().to_owned()),
            bearer
        );
        Box::pin(async { Ok(response) })
    }
}
async fn auth_client(steps: Arc<AuthSteps>, dir: &std::path::Path) -> Client {
    let cache = Cache::open(dir.join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    Client::new(
        rustovate_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            steps.clone(),
        )
        .unwrap(),
        Cached::new(cache.clone(), steps, 4).unwrap(),
        cache,
    )
}
#[tokio::test]
async fn anonymous_pull_tokens_survive_reopen_and_rejected_tokens_are_replaced() {
    let manifest = "https://ghcr.io/v2/spec/image/manifests/latest";
    let auth = "https://ghcr.io/token?service=ghcr.io&scope=repository%3Aspec%2Fimage%3Apull";
    let challenge = || {
        response(
            401,
            "",
            &[(
                "www-authenticate",
                r#"Bearer realm="https://ghcr.io/token",service="ghcr.io",scope="ignored:push""#,
            )],
        )
    };
    let ok = || {
        response(
            200,
            "{}",
            &[(
                "docker-content-digest",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )],
        )
    };
    let steps = Arc::new(AuthSteps(Mutex::new(VecDeque::from([
        (manifest.into(), None, challenge()),
        (
            auth.into(),
            None,
            response(200, r#"{"token":"anonymous-one","expires_in":300}"#, &[]),
        ),
        (manifest.into(), Some("Bearer anonymous-one".into()), ok()),
        (
            manifest.into(),
            Some("Bearer anonymous-one".into()),
            challenge(),
        ),
        (
            auth.into(),
            None,
            response(
                200,
                r#"{"access_token":"anonymous-two","expires_in":300}"#,
                &[],
            ),
        ),
        (manifest.into(), Some("Bearer anonymous-two".into()), ok()),
        (manifest.into(), Some("Bearer anonymous-two".into()), ok()),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..3 {
        auth_client(steps.clone(), dir.path())
            .await
            .registry_digest("ghcr.io/spec/image", "latest")
            .await
            .unwrap();
    }
    assert!(steps.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn expired_anonymous_tokens_are_not_reused_between_clients() {
    let manifest = "https://ghcr.io/v2/spec/image/manifests/latest";
    for headers in [vec![], vec![("cache-control", "no-store")]] {
        let mut reads = VecDeque::new();
        for _ in 0..2 {
            reads.extend([
                (
                    manifest.into(),
                    None,
                    response(
                        401,
                        "",
                        &[(
                            "www-authenticate",
                            r#"Bearer realm="https://ghcr.io/token""#,
                        )],
                    ),
                ),
                (
                    "https://ghcr.io/token?scope=repository%3Aspec%2Fimage%3Apull".into(),
                    None,
                    response(
                        200,
                        if headers.is_empty() {
                            r#"{"token":"anonymous-expired","expires_in":1}"#
                        } else {
                            r#"{"token":"anonymous-expired","expires_in":300}"#
                        },
                        &headers,
                    ),
                ),
                (
                    manifest.into(),
                    Some("Bearer anonymous-expired".into()),
                    response(200, "{}", &[("docker-content-digest", "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")]),
                ),
            ]);
        }
        let steps = Arc::new(AuthSteps(Mutex::new(reads)));
        let dir = tempfile::tempdir().unwrap();
        for _ in 0..2 {
            auth_client(steps.clone(), dir.path())
                .await
                .registry_digest("ghcr.io/spec/image", "latest")
                .await
                .unwrap();
        }
        assert!(steps.0.lock().unwrap().is_empty());
    }
}

struct DigestSteps(Mutex<VecDeque<(reqwest::Method, Response)>>);
impl Transport for DigestSteps {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let (method, response) = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected manifest request");
        assert_eq!(request.method(), method);
        if method == reqwest::Method::HEAD {
            if response.status == 304 {
                assert_eq!(request.headers()["if-none-match"], "head");
            } else {
                assert!(!request.headers().contains_key("if-none-match"));
            }
        }
        Box::pin(async { Ok(response) })
    }
}
#[tokio::test]
async fn manifest_heads_have_separate_validators_and_fall_back_only_when_needed() {
    let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    for first in [
        response(200, "", &[]),
        response(405, "", &[]),
        response(501, "", &[]),
    ] {
        let steps = Arc::new(DigestSteps(Mutex::new(VecDeque::from([
            (reqwest::Method::HEAD, first),
            (
                reqwest::Method::GET,
                response(
                    200,
                    "{}",
                    &[("docker-content-digest", digest), ("etag", "get")],
                ),
            ),
            (
                reqwest::Method::HEAD,
                response(
                    200,
                    "",
                    &[("docker-content-digest", digest), ("etag", "head")],
                ),
            ),
            (reqwest::Method::HEAD, response(304, "", &[])),
        ]))));
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        let client = Client::new(
            rustovate_github::Client::with_transport(
                "offline",
                cache.clone(),
                Default::default(),
                steps.clone(),
            )
            .unwrap(),
            Cached::new(cache.clone(), steps.clone(), 4).unwrap(),
            cache,
        );
        for _ in 0..3 {
            assert_eq!(
                client
                    .registry_digest("ghcr.io/spec/image", "latest")
                    .await
                    .unwrap(),
                digest
            );
        }
        assert!(steps.0.lock().unwrap().is_empty());
    }
}
