//! Independent contract fixtures; no network client is constructed.
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::Client;
use patchula_http::{Cached, Exchange, Response, Transport};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Host {
    status: u16,
    empty: bool,
    partial: bool,
    lost: bool,
    missing_tool: bool,
    requests: Mutex<Vec<(String, bool)>>,
}
impl Transport for Host {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let path = request.url().path();
            let conditional = request.headers().contains_key("if-none-match");
            self.requests
                .lock()
                .unwrap()
                .push((path.into(), conditional));
            let github = request.url().host_str() == Some("api.github.com");
            assert_eq!(request.headers().contains_key("authorization"), github);
            if !github && self.lost {
                return Err(patchula_http::Error::Fixture("lost connection".into()));
            }
            let body = if github {
                assert_eq!(path, "/graphql");
                json!({"data":{"r0":{"releases":{"nodes":[{"tagName":"v2.0.0","publishedAt":"2026-01-01T00:00:00Z","isDraft":false,"isPrerelease":false}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}).to_string()
            } else if path.ends_with("index.v1.json") {
                if self.missing_tool {
                    json!({"format":1,"tools":{}}).to_string()
                } else {
                    json!({"format":1,"tools":{
                    "age":{"count":if self.partial {99} else {1},"versions":[["2.0.0",1767225600000i64,0]],"tail":0,"prefixes":{}},
                    "fnox":{"count":1,"versions":[["3.0.0",1767225600000i64,0]],"tail":0,"prefixes":{}}
                }}).to_string()
                }
            } else if path.ends_with("age.toml") {
                "[versions]\n'1.0.0'={created_at=2025-01-01T00:00:00Z}\n'2.0.0'={created_at=2026-01-01T00:00:00Z}\n'3.0.0'={created_at=2026-02-01T00:00:00Z,prerelease=true}\n".into()
            } else if path.ends_with("tags.json") {
                json!({"tags":[{"tag":"v2.0.0","sha":"a".repeat(40),"date":"2099-01-01T00:00:00Z"}],"truncated":false}).to_string()
            } else {
                assert_eq!(path, "/data/github/filosottile/age/releases.json");
                json!({"releases":if self.empty {json!([])} else {json!([{"tag":"v2.0.0","date":"2026-01-01T00:00:00Z","prerelease":false}])},"truncated":false}).to_string()
            };
            Ok(Response {
                status: reqwest::StatusCode::from_u16(if !github && self.status != 0 {
                    self.status
                } else if conditional {
                    304
                } else {
                    200
                })
                .unwrap(),
                headers: [
                    (reqwest::header::ETAG, "\"catalog\"".parse().unwrap()),
                    (
                        reqwest::header::CACHE_CONTROL,
                        "max-age=0, must-revalidate".parse().unwrap(),
                    ),
                ]
                .into_iter()
                .collect(),
                body: body.into_bytes(),
            })
        })
    }
}
async fn client(host: Arc<Host>, cache: Cache) -> Client {
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        host.clone(),
    )
    .unwrap();
    Client::new(github, Cached::new(cache.clone(), host, 4).unwrap(), cache).catalog_batch()
}
async fn memory() -> Cache {
    Cache::open(":memory:", Default::default()).await.unwrap()
}

#[tokio::test]
async fn public_release_and_tag_catalogs_are_conditional_and_keep_age_provenance() {
    let host = Arc::new(Host::default());
    let cache = memory().await;
    for _ in 0..2 {
        let catalog = client(host.clone(), cache.clone())
            .await
            .lookup(Source::GithubTags, "FiloSottile/age")
            .await
            .unwrap();
        assert_eq!(
            catalog.versions[0].published_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            catalog.versions[0].digest.as_deref(),
            Some("a".repeat(40).as_str())
        );
    }
    let reads = host.requests.lock().unwrap();
    assert_eq!(reads.len(), 4);
    assert!(!reads.iter().any(|(path, _)| path == "/graphql"));
    assert!(reads[2..].iter().all(|(_, conditional)| *conditional));
}

#[tokio::test]
async fn mirror_answers_are_authoritative_but_transient_errors_can_use_github() {
    for status in [403, 404, 429, 500, 502, 503] {
        let host = Arc::new(Host {
            status,
            ..Default::default()
        });
        let result = client(host.clone(), memory().await)
            .await
            .lookup(Source::GithubReleases, "FiloSottile/age")
            .await;
        assert_eq!(result.is_ok(), status == 429 || status >= 500);
        assert_eq!(
            host.requests
                .lock()
                .unwrap()
                .iter()
                .any(|(path, _)| path == "/graphql"),
            status == 429 || status >= 500
        );
    }
    let host = Arc::new(Host {
        empty: true,
        ..Default::default()
    });
    assert!(
        client(host.clone(), memory().await)
            .await
            .lookup(Source::GithubReleases, "FiloSottile/age")
            .await
            .unwrap()
            .versions
            .is_empty()
    );
    assert_eq!(host.requests.lock().unwrap().len(), 1);
    let host = Arc::new(Host {
        lost: true,
        ..Default::default()
    });
    assert_eq!(
        client(host.clone(), memory().await)
            .await
            .lookup(Source::GithubReleases, "FiloSottile/age")
            .await
            .unwrap()
            .versions
            .len(),
        1
    );
}

#[tokio::test]
async fn private_unlisted_repositories_and_explicit_opt_out_skip_the_host() {
    for (package, enabled) in [("private-org/internal", true), ("FiloSottile/age", false)] {
        let host = Arc::new(Host::default());
        client(host.clone(), memory().await)
            .await
            .with_versions_host(enabled)
            .lookup(Source::GithubReleases, package)
            .await
            .unwrap();
        assert_eq!(
            host.requests.lock().unwrap().as_slice(),
            [("/graphql".into(), false)]
        );
    }
    let host = Arc::new(Host {
        missing_tool: true,
        ..Default::default()
    });
    let source = client(host.clone(), memory().await).await;
    assert!(
        !source
            .mise_tool_catalog("age")
            .await
            .unwrap()
            .versions
            .is_empty()
    );
    assert_eq!(
        host.requests.lock().unwrap().as_slice(),
        [
            ("/data/index.v1.json".into(), false),
            ("/graphql".into(), false)
        ]
    );
}

#[tokio::test]
async fn complete_index_resolves_many_tools_once_and_partial_index_uses_full_toml() {
    let host = Arc::new(Host::default());
    let cache = memory().await;
    for _ in 0..2 {
        let source = client(host.clone(), cache.clone()).await;
        let (age, fnox) = tokio::join!(
            source.mise_tool_catalog("age"),
            source.mise_tool_catalog("fnox")
        );
        assert_eq!(age.unwrap().versions[0].version, "2.0.0");
        assert_eq!(fnox.unwrap().versions[0].version, "3.0.0");
    }
    assert_eq!(
        host.requests.lock().unwrap().as_slice(),
        [
            ("/data/index.v1.json".into(), false),
            ("/data/index.v1.json".into(), true)
        ]
    );
    let host = Arc::new(Host {
        partial: true,
        ..Default::default()
    });
    let catalog = client(host.clone(), memory().await)
        .await
        .mise_tool_catalog("age")
        .await
        .unwrap();
    assert_eq!(
        catalog
            .versions
            .iter()
            .map(|v| v.version.as_str())
            .collect::<Vec<_>>(),
        ["1.0.0", "2.0.0"]
    );
    assert_eq!(host.requests.lock().unwrap().len(), 2);
    assert!(
        host.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path.ends_with("age.toml"))
    );
}

#[tokio::test]
async fn cached_catalogs_do_not_freeze_mutable_lightweight_or_annotated_tag_targets() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Moving(AtomicUsize);
    impl Transport for Moving {
        fn send(&self, request: reqwest::Request) -> Exchange<'_> {
            Box::pin(async move {
                assert_eq!(request.url().path(), "/graphql");
                let body: serde_json::Value =
                    serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
                assert!(
                    body["query"]
                        .as_str()
                        .unwrap()
                        .starts_with("query FreshTagDigests(")
                );
                assert_eq!(body["variables"]["e0"], "refs/tags/v1");
                assert_eq!(body["variables"]["e1"], "refs/tags/v2");
                let phase = self.0.fetch_add(1, Ordering::SeqCst);
                let sha = if phase == 0 {
                    "a".repeat(40)
                } else {
                    "b".repeat(40)
                };
                Ok(Response {status:reqwest::StatusCode::OK,headers:Default::default(),body:serde_json::to_vec(&json!({"data":{
                    "r0":{"t0":{"__typename":"Commit","oid":sha},
                    "t1":{"__typename":"Tag","oid":"c".repeat(40),"target":{"__typename":"Commit","oid":sha}}}
                }})).unwrap()})
            })
        }
    }
    let cache = memory().await;
    let moving = Arc::new(Moving(AtomicUsize::new(0)));
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        moving.clone(),
    )
    .unwrap();
    let source = Client::new(
        github,
        Cached::new(cache.clone(), moving.clone(), 4).unwrap(),
        cache,
    );
    let keys = vec![
        ("example/tool".into(), "v1".into()),
        ("example/tool".into(), "v2".into()),
    ];
    for (i, expected) in ["a".repeat(40), "b".repeat(40)].iter().enumerate() {
        let digests = source.fresh_tag_digests(&keys).await;
        assert!(
            digests
                .values()
                .all(|value| value.as_ref().unwrap() == expected)
        );
        assert_eq!(
            moving.0.load(Ordering::SeqCst),
            i + 1,
            "one query for both mutable tags per run"
        );
    }
}
