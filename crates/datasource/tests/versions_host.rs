//! Independent contract fixtures; no network client is constructed.
use patchula_cache::Cache;
use patchula_datasource::Client;
use patchula_http::{Exchange, Response, Transport};
use patchula_words::Source;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Host {
    status: u16,
    empty: bool,
    lost: bool,
    truncated_releases: bool,
    truncated_tags: bool,
    requests: Mutex<Vec<(String, bool)>>,
    /// Both sources also list v3.0.0, which GitHub marks a prerelease.
    prerelease: bool,
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
                return Err(patchula_http::Error::Network);
            }
            let body = if github {
                assert_eq!(path, "/graphql");
                let mut nodes = vec![
                    json!({"tagName":"v2.0.0","publishedAt":"2026-01-01T00:00:00Z","isDraft":false,"isPrerelease":false}),
                ];
                if self.prerelease {
                    nodes.push(json!({"tagName":"v3.0.0","publishedAt":"2026-02-01T00:00:00Z","isDraft":false,"isPrerelease":true}));
                }
                json!({"data":{"r0":{"releases":{"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}).to_string()
            } else if path.ends_with("tags.json") {
                json!({"tags":[{"tag":"v2.0.0","sha":"a".repeat(40),"date":"2099-01-01T00:00:00Z"}],"truncated":self.truncated_tags}).to_string()
            } else {
                assert_eq!(path, "/data/github/filosottile/age/releases.json");
                let mut releases =
                    vec![json!({"tag":"v2.0.0","date":"2026-01-01T00:00:00Z","prerelease":false})];
                if self.prerelease {
                    releases.push(
                        json!({"tag":"v3.0.0","date":"2026-02-01T00:00:00Z","prerelease":true}),
                    );
                }
                json!({"releases":if self.empty {json!([])} else {json!(releases)},"truncated":self.truncated_releases}).to_string()
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
                stale: None,
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
    Client::new(
        github,
        patchula_datasource::cached(cache.clone(), host, 4).unwrap(),
        cache,
    )
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
            .lookup(&patchula_config::PackageId::public(
                Source::GithubTags,
                "FiloSottile/age",
            ))
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
            .lookup(&patchula_config::PackageId::public(
                Source::GithubReleases,
                "FiloSottile/age",
            ))
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
            .lookup(&patchula_config::PackageId::public(
                Source::GithubReleases,
                "FiloSottile/age"
            ))
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
            .lookup(&patchula_config::PackageId::public(
                Source::GithubReleases,
                "FiloSottile/age"
            ))
            .await
            .unwrap()
            .versions
            .len(),
        1
    );
}

#[tokio::test]
async fn truncated_mirror_documents_are_registry_errors() {
    for (source, truncated_releases, truncated_tags, message) in [
        (
            Source::GithubReleases,
            true,
            false,
            "mise-versions release catalog is truncated; full range unavailable",
        ),
        (
            Source::GithubTags,
            false,
            true,
            "mise-versions tag catalog is truncated; full range unavailable",
        ),
    ] {
        let host = Arc::new(Host {
            truncated_releases,
            truncated_tags,
            ..Default::default()
        });
        let error = client(host.clone(), memory().await)
            .await
            .lookup(&patchula_config::PackageId::public(
                source,
                "FiloSottile/age",
            ))
            .await
            .err()
            .unwrap();
        assert!(
            matches!(error, patchula_datasource::Error::Registry(_)),
            "{error}"
        );
        assert_eq!(error.to_string(), message);
    }
}

#[tokio::test]
async fn private_unlisted_repositories_and_explicit_opt_out_skip_the_host() {
    for (package, enabled) in [("private-org/internal", true), ("FiloSottile/age", false)] {
        let host = Arc::new(Host::default());
        client(host.clone(), memory().await)
            .await
            .with_versions_host(enabled)
            .lookup(&patchula_config::PackageId::public(
                Source::GithubReleases,
                package,
            ))
            .await
            .unwrap();
        assert_eq!(
            host.requests.lock().unwrap().as_slice(),
            [("/graphql".into(), false)]
        );
    }
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
                }})).unwrap(),stale:None})
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
        patchula_datasource::cached(cache.clone(), moving.clone(), 4).unwrap(),
        cache,
    );
    let keys = std::collections::BTreeSet::from([
        ("example/tool".into(), "v1".into()),
        ("example/tool".into(), "v2".into()),
    ]);
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

#[tokio::test]
async fn releases_github_marks_as_prereleases_stay_out_whichever_source_answers() {
    for lost in [false, true] {
        let host = Arc::new(Host {
            prerelease: true,
            lost,
            ..Host::default()
        });
        let catalog = client(host.clone(), memory().await)
            .await
            .lookup(&patchula_config::PackageId::public(
                Source::GithubReleases,
                "FiloSottile/age",
            ))
            .await
            .unwrap();
        let versions: Vec<_> = catalog
            .versions
            .iter()
            .map(|v| v.version.as_str())
            .collect();
        assert_eq!(versions, ["v2.0.0"], "mirror unreachable: {lost}");
        // Releases carry no commit; only tags do.
        assert!(catalog.versions.iter().all(|v| v.digest.is_none()));
    }
}

/// The mirror dates tags only by their releases. A tag it leaves undated may
/// be annotated, so GitHub's own tag catalog answers instead, and dates it
/// by its tagger.
#[tokio::test]
async fn a_tag_the_mirror_leaves_undated_is_dated_through_github() {
    struct Annotated(Mutex<Vec<String>>);
    impl Transport for Annotated {
        fn send(&self, request: reqwest::Request) -> Exchange<'_> {
            Box::pin(async move {
                let path = request.url().path().to_owned();
                self.0.lock().unwrap().push(path.clone());
                let commit = "a".repeat(40);
                let annotated = "f".repeat(40);
                let body = if request.url().host_str() != Some("api.github.com") {
                    if path.ends_with("tags.json") {
                        json!({"tags":[{"tag":"v2.0.0","sha":commit},{"tag":"v2.1.0","sha":"b".repeat(40)}],"truncated":false})
                    } else {
                        json!({"releases":[{"tag":"v2.0.0","date":"2026-01-01T00:00:00Z","prerelease":false}],"truncated":false})
                    }
                } else if path == "/graphql" {
                    let body: serde_json::Value =
                        serde_json::from_slice(request.body().unwrap().as_bytes().unwrap())
                            .unwrap();
                    if body["query"].as_str().unwrap().contains("releases(") {
                        json!({"data":{"r0":{"releases":{"nodes":[
                            {"tagName":"v2.0.0","publishedAt":"2026-01-01T00:00:00Z","isDraft":false,"isPrerelease":false}
                        ],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}})
                    } else {
                        let mut objects = serde_json::Map::new();
                        for i in 0..2 {
                            let sha = body["variables"][format!("s{i}")].as_str().unwrap();
                            let node = if sha == commit {
                                json!({"oid":sha,"__typename":"Commit"})
                            } else {
                                json!({"oid":sha,"__typename":"Tag","tagger":{"date":"2026-03-01T00:00:00Z"},"target":{"__typename":"Commit","oid":"b".repeat(40)}})
                            };
                            objects.insert(format!("t{i}"), node);
                        }
                        json!({"data":{"repository":objects}})
                    }
                } else {
                    assert_eq!(path, "/repos/FiloSottile/age/git/matching-refs/tags/");
                    json!([
                        {"ref":"refs/tags/v2.0.0","object":{"sha":commit}},
                        {"ref":"refs/tags/v2.1.0","object":{"sha":annotated}}
                    ])
                };
                Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers: Default::default(),
                    body: body.to_string().into_bytes(),
                    stale: None,
                })
            })
        }
    }
    let cache = memory().await;
    let transport = Arc::new(Annotated(Mutex::new(Vec::new())));
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        transport.clone(),
    )
    .unwrap();
    let catalog = Client::new(
        github,
        patchula_datasource::cached(cache.clone(), transport.clone(), 4).unwrap(),
        cache,
    )
    .lookup(&patchula_config::PackageId::public(
        Source::GithubTags,
        "FiloSottile/age",
    ))
    .await
    .unwrap();
    let date = |tag: &str| {
        catalog
            .versions
            .iter()
            .find(|v| v.version == tag)
            .and_then(|v| v.published_at.clone())
    };
    assert_eq!(date("v2.1.0").as_deref(), Some("2026-03-01T00:00:00Z"));
    assert_eq!(date("v2.0.0").as_deref(), Some("2026-01-01T00:00:00Z"));
    let requests = transport.0.lock().unwrap();
    assert!(requests.iter().any(|path| path.ends_with("tags.json")));
    assert!(
        requests
            .iter()
            .any(|path| path.ends_with("/matching-refs/tags/"))
    );
}
