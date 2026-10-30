//! Independently simulated protocol edge cases, never live HTTP.
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::Client;
use patchula_http::{Cached, Exchange, Response, Transport};
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
    let github = patchula_github::Client::with_transport(
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
            patchula_http::Options {
                registry_retries: 0,
                ..Default::default()
            },
        )
        .unwrap(),
        cache,
    )
    .with_versions_host(false)
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
async fn rate_limited_catalogs_and_digests_reuse_cache_without_refreshing_it() {
    let tags = "https://registry-1.docker.io/v2/library/spec/tags/list?n=1000";
    let manifest = "https://registry-1.docker.io/v2/library/spec/manifests/1.0";
    let digest = format!("sha256:{}", "a".repeat(64));
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            tags.into(),
            response(200, r#"{"tags":["1.0"]}"#, &[("etag", "\"tags\"")]),
        ),
        (
            manifest.into(),
            response(
                200,
                "",
                &[("etag", "\"tags\""), ("docker-content-digest", &digest)],
            ),
        ),
        (manifest.into(), response(304, "", &[])),
        (
            tags.into(),
            response(429, "", &[("ratelimit-remaining", "0;w=21600")]),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let sources = client(steps.clone(), dir.path()).await;
    sources.lookup(Source::Docker, "spec").await.unwrap();
    assert_eq!(
        sources.registry_digest("spec", "1.0").await.unwrap(),
        digest
    );
    // An unchanged HEAD validator resolves the pinned digest without downloading a manifest.
    assert_eq!(
        sources.registry_digest("spec", "1.0").await.unwrap(),
        digest
    );
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let key = patchula_cache::HttpKey::new(
        "GET",
        tags,
        std::collections::BTreeMap::from([("accept".into(), "application/json".into())]),
    )
    .unwrap()
    .representation("oci-tags-v1");
    let fetched = cache.http_get(&key).await.unwrap().unwrap().fetched_at;
    assert_eq!(
        sources
            .lookup(Source::Docker, "spec")
            .await
            .unwrap()
            .versions
            .len(),
        1
    );
    // Shared quota blocks subsequent network reads, but not cached results.
    assert_eq!(
        sources.registry_digest("spec", "1.0").await.unwrap(),
        digest
    );
    let stale = sources.registry_stale("docker.io/library/spec").await;
    assert_eq!(stale["catalog"], fetched);
    assert!(stale.contains_key("digest:1.0"));
    assert_eq!(
        cache.http_get(&key).await.unwrap().unwrap().fetched_at,
        fetched
    );
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn zero_remaining_on_success_keeps_digest_heads_and_other_hosts_available() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            "https://registry-1.docker.io/v2/library/spec/manifests/1.0".into(),
            response(
                200,
                "",
                &[
                    ("docker-content-digest", &digest),
                    ("ratelimit-remaining", "0;w=21600"),
                ],
            ),
        ),
        (
            "https://registry-1.docker.io/v2/library/uncached/manifests/1.0".into(),
            response(200, "", &[("docker-content-digest", &digest)]),
        ),
        (
            "https://ghcr.io/v2/spec/image/manifests/1.0".into(),
            response(200, "", &[("docker-content-digest", &digest)]),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let sources = client(steps.clone(), dir.path()).await;
    sources.registry_digest("spec", "1.0").await.unwrap();
    assert_eq!(
        sources.registry_digest("uncached", "1.0").await.unwrap(),
        digest
    );
    sources
        .registry_digest("ghcr.io/spec/image", "1.0")
        .await
        .unwrap();
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rate_limited_heads_can_reuse_legacy_get_digests_but_no_store_is_authoritative() {
    let manifest = "https://registry-1.docker.io/v2/library/spec/manifests/1.0";
    let digest = format!("sha256:{}", "a".repeat(64));
    for no_store in [false, true] {
        let mut headers = vec![("docker-content-digest", digest.as_str())];
        if no_store {
            headers.push(("cache-control", "no-store"));
        }
        let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
            (manifest.into(), response(405, "", &[])),
            (manifest.into(), response(200, "{}", &headers)),
            (
                manifest.into(),
                response(429, "", &[("ratelimit-remaining", "0;w=21600")]),
            ),
        ]))));
        let dir = tempfile::tempdir().unwrap();
        let sources = client(steps.clone(), dir.path()).await;
        sources.registry_digest("spec", "1.0").await.unwrap();
        assert_eq!(
            sources.registry_digest("spec", "1.0").await.is_ok(),
            !no_store
        );
        assert_eq!(sources.registry_stale("spec").await.is_empty(), no_store);
        assert!(steps.0.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn public_helm_index_redirects_use_the_same_https_policy() {
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            "https://charts.example.org/index.yaml".into(),
            response(
                302,
                "",
                &[("location", "https://mirror.example.org/index.yaml")],
            ),
        ),
        (
            "https://mirror.example.org/index.yaml".into(),
            response(
                200,
                "apiVersion: v1\nentries:\n  chart:\n    - version: 1.0.0\n      created: 2026-01-01T00:00:00Z\n",
                &[],
            ),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let catalog = client(steps.clone(), dir.path())
        .await
        .lookup(Source::Helm, "https://charts.example.org|chart")
        .await
        .unwrap();
    assert_eq!(catalog.versions[0].version, "1.0.0");
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_fresh_catalog_clears_stale_provenance_after_a_short_limit() {
    let tags = "https://ghcr.io/v2/spec/image/tags/list?n=1000";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            tags.into(),
            response(200, r#"{"tags":["1.0"]}"#, &[("etag", "\"tags\"")]),
        ),
        (tags.into(), response(429, "", &[("retry-after", "0")])),
        (tags.into(), response(304, "", &[])),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let sources = client(steps.clone(), dir.path()).await;
    sources
        .lookup(Source::Docker, "ghcr.io/spec/image")
        .await
        .unwrap();
    sources
        .lookup(Source::Docker, "ghcr.io/spec/image")
        .await
        .unwrap();
    assert!(
        sources
            .registry_stale("ghcr.io/spec/image")
            .await
            .contains_key("catalog")
    );
    sources
        .lookup(Source::Docker, "ghcr.io/spec/image")
        .await
        .unwrap();
    assert!(
        sources
            .registry_stale("ghcr.io/spec/image")
            .await
            .is_empty()
    );
    assert!(steps.0.lock().unwrap().is_empty());
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
    let key = patchula_cache::HttpKey::new(
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
        patchula_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            steps.clone(),
        )
        .unwrap(),
        Cached::new(cache.clone(), steps, 4).unwrap(),
        cache,
    )
    .with_versions_host(false)
}

#[tokio::test]
async fn registry_redirects_strip_auth_and_reject_downgrades_userinfo_loops_and_long_chains() {
    let manifest = "https://registry.k8s.io/v2/spec/image/manifests/1.0";
    let auth = "https://registry.k8s.io/token?scope=repository%3Aspec%2Fimage%3Apull";
    let digest = format!("sha256:{}", "a".repeat(64));
    let steps = Arc::new(AuthSteps(Mutex::new(VecDeque::from([
        (
            manifest.into(),
            None,
            response(
                401,
                "",
                &[(
                    "www-authenticate",
                    r#"Bearer realm="https://registry.k8s.io/token""#,
                )],
            ),
        ),
        (
            auth.into(),
            None,
            response(200, r#"{"token":"fixture-token","expires_in":300}"#, &[]),
        ),
        (
            manifest.into(),
            Some("Bearer fixture-token".into()),
            response(307, "", &[("location", "/mirror")]),
        ),
        (
            "https://registry.k8s.io/mirror".into(),
            Some("Bearer fixture-token".into()),
            response(
                307,
                "",
                &[("location", "https://mirror.example.org/manifest")],
            ),
        ),
        (
            "https://mirror.example.org/manifest".into(),
            None,
            response(200, "", &[("docker-content-digest", &digest)]),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        auth_client(steps.clone(), dir.path())
            .await
            .registry_digest("registry.k8s.io/spec/image", "1.0")
            .await
            .unwrap(),
        digest
    );
    assert!(steps.0.lock().unwrap().is_empty());
    for target in [
        "http://mirror.example.org/manifest",
        "https://user:pass@mirror.example.org/manifest",
        manifest,
    ] {
        let steps = Arc::new(AuthSteps(Mutex::new(VecDeque::from([(
            manifest.into(),
            None,
            response(307, "", &[("location", target)]),
        )]))));
        let dir = tempfile::tempdir().unwrap();
        assert!(
            auth_client(steps.clone(), dir.path())
                .await
                .registry_digest("registry.k8s.io/spec/image", "1.0")
                .await
                .is_err()
        );
        assert!(steps.0.lock().unwrap().is_empty());
    }
    let steps = Arc::new(AuthSteps(Mutex::new(
        (0..6)
            .map(|n| {
                (
                    if n == 0 {
                        manifest.into()
                    } else {
                        format!("https://registry.k8s.io/hop/{n}")
                    },
                    None,
                    response(307, "", &[("location", &format!("/hop/{}", n + 1))]),
                )
            })
            .collect(),
    )));
    let dir = tempfile::tempdir().unwrap();
    assert!(
        auth_client(steps.clone(), dir.path())
            .await
            .registry_digest("registry.k8s.io/spec/image", "1.0")
            .await
            .is_err()
    );
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn dockerhub_login_is_realm_scoped_and_authenticated_tokens_are_not_persisted() {
    let manifest = "https://registry-1.docker.io/v2/library/spec/manifests/latest";
    let token_url = "https://auth.docker.io/token?service=registry.docker.io&scope=repository%3Alibrary%2Fspec%3Apull";
    let steps = Arc::new(AuthSteps(Mutex::new(VecDeque::from([
        (
            manifest.into(),
            None,
            response(
                401,
                "",
                &[(
                    "www-authenticate",
                    r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io""#,
                )],
            ),
        ),
        (
            token_url.into(),
            Some("Basic dXNlcjpwYXNz".into()),
            response(
                200,
                r#"{"token":"authenticated-fixture","expires_in":300}"#,
                &[],
            ),
        ),
        (
            manifest.into(),
            Some("Bearer authenticated-fixture".into()),
            response(
                200,
                "",
                &[(
                    "docker-content-digest",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )],
            ),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    auth_client(steps.clone(), dir.path())
        .await
        .with_dockerhub_credentials("user".into(), "pass".into())
        .registry_digest("spec", "latest")
        .await
        .unwrap();
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let key = patchula_cache::HttpKey::new(
        "GET",
        "https://registry-1.docker.io/v2/library/spec/",
        Default::default(),
    )
    .unwrap()
    .representation("anonymous-registry-pull-token-v1");
    assert!(cache.http_get(&key).await.unwrap().is_none());
    assert!(steps.0.lock().unwrap().is_empty());
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
            patchula_github::Client::with_transport(
                "offline",
                cache.clone(),
                Default::default(),
                steps.clone(),
            )
            .unwrap(),
            Cached::new(cache.clone(), steps.clone(), 4).unwrap(),
            cache,
        )
        .with_versions_host(false);
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

fn sha(text: &str) -> String {
    use sha2::Digest as _;
    let hex: String = sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

#[tokio::test]
async fn a_cosign_attestation_for_another_subject_is_ignored_for_the_sbom_tag() {
    let child = format!("sha256:{}", "c".repeat(64));
    let index = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child}","size":1,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#
    );
    let digest = sha(&index);
    let statement = serde_json::json!({
        "_type": "https://in-toto.io/Statement/v0.1",
        "predicateType": "https://spdx.dev/Document",
        "subject": [{"name": "other", "digest": {"sha256": "f".repeat(64)}}],
        "predicate": {"spdxVersion": "SPDX-2.3", "creationInfo": {"creators": ["Tool: syft-v1.0.0"]}, "packages": []}
    });
    use base64::Engine as _;
    let envelope = serde_json::json!({
        "payloadType": "application/vnd.in-toto+json",
        "payload": base64::engine::general_purpose::STANDARD.encode(statement.to_string()),
        "signatures": []
    })
    .to_string();
    let att = format!(
        r#"{{"schemaVersion":2,"layers":[{{"mediaType":"application/vnd.dsse.envelope.v1+json","digest":"{}","size":1,"annotations":{{"predicateType":"https://spdx.dev/Document"}}}}]}}"#,
        sha(&envelope)
    );
    let spdx = r#"{"spdxVersion":"SPDX-2.3","creationInfo":{"creators":["Tool: syft-v1.0.0"]},"packages":[{"name":"libssl3","externalRefs":[{"referenceType":"purl","referenceLocator":"pkg:deb/debian/libssl3@3.0.17-1~deb12u2?arch=amd64&distro=debian-12&upstream=openssl"}]}]}"#;
    let sbom = format!(
        r#"{{"schemaVersion":2,"layers":[{{"mediaType":"text/spdx+json","digest":"{}","size":1}}]}}"#,
        sha(spdx)
    );
    let base = "https://ghcr.io/v2/spec/image";
    let tag = |d: &str, suffix: &str| format!("{base}/manifests/{}{suffix}", d.replace(':', "-"));
    let blob = |d: String| format!("{base}/blobs/{d}");
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            format!("{base}/manifests/{digest}"),
            response(200, &index, &[]),
        ),
        (tag(&child, ".att"), response(200, "", &[])),
        (tag(&child, ".att"), response(200, &att, &[])),
        (blob(sha(&envelope)), response(200, &envelope, &[])),
        (tag(&digest, ".att"), response(404, "", &[])),
        (tag(&child, ".sbom"), response(200, "", &[])),
        (tag(&child, ".sbom"), response(200, &sbom, &[])),
        (blob(sha(spdx)), response(200, spdx, &[])),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let client = client(steps.clone(), dir.path()).await;
    let found = client
        .image_sbom("ghcr.io/spec/image", &digest, "linux/amd64")
        .await
        .unwrap();
    let patchula_datasource::ImageSbom::Found(sbom) = found else {
        panic!("{found:?}");
    };
    assert_eq!(sbom.source, patchula_datasource::SbomSource::CosignSbom);
    assert_eq!(sbom.ecosystem.as_deref(), Some("Debian:12"));
    assert!(
        sbom.sources
            .contains(&("openssl".into(), "3.0.17-1~deb12u2".into()))
    );
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_sbom_whose_bytes_do_not_match_its_digest_is_rejected() {
    let child = format!("sha256:{}", "c".repeat(64));
    let attestation = format!("sha256:{}", "d".repeat(64));
    let index = format!(
        r#"{{"manifests":[{{"digest":"{child}","platform":{{"os":"linux","architecture":"amd64"}}}},{{"digest":"{attestation}","platform":{{"os":"unknown","architecture":"unknown"}},"annotations":{{"vnd.docker.reference.type":"attestation-manifest","vnd.docker.reference.digest":"{child}"}}}}]}}"#
    );
    let digest = sha(&index);
    let base = "https://ghcr.io/v2/spec/image";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            format!("{base}/manifests/{digest}"),
            response(200, &index, &[]),
        ),
        (
            format!("{base}/manifests/{attestation}"),
            response(200, "{}", &[]),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let client = client(steps, dir.path()).await;
    let error = client
        .image_sbom("ghcr.io/spec/image", &digest, "linux/amd64")
        .await
        .unwrap_err();
    assert!(
        matches!(error, patchula_datasource::Error::Invalid),
        "{error}"
    );
}

#[tokio::test]
async fn an_official_image_the_mirror_lacks_is_read_from_docker_hub() {
    let index = r#"{"manifests":[{"digest":"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","platform":{"os":"linux","architecture":"s390x"}}]}"#;
    let digest = sha(index);
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            format!("https://mirror.gcr.io/v2/library/debian/manifests/{digest}"),
            response(404, "", &[]),
        ),
        (
            format!("https://registry-1.docker.io/v2/library/debian/manifests/{digest}"),
            response(200, index, &[]),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let client = client(steps.clone(), dir.path()).await;
    let found = client
        .image_sbom("library/debian", &digest, "linux/amd64")
        .await
        .unwrap();
    assert_eq!(
        found,
        patchula_datasource::ImageSbom::NoPlatform(Some("linux/s390x".into()))
    );
    assert!(steps.0.lock().unwrap().is_empty());
}
