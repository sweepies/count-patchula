//! Independent, offline source payloads; no installed mise or network in tests.
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::Client;
use patchula_http::Cached;
use patchula_testkit::{Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Replay};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

#[tokio::test]
async fn setup_node_uses_node_lts_membership_without_replacing_github_release_dates() {
    use patchula_http::{Exchange, Response, Transport};
    struct Node;
    impl Transport for Node {
        fn send(&self, request: reqwest::Request) -> Exchange<'_> {
            Box::pin(async move {
                let body = match request.url().path() {
                    "/graphql" => json!({"data":{"r0":{"releases":{"nodes":[
                        {"tagName":"24.2.0","publishedAt":"2026-10-08T12:00:00Z","isDraft":false,"isPrerelease":false},
                        {"tagName":"26.1.0","publishedAt":"2026-10-09T12:00:00Z","isDraft":false,"isPrerelease":false}
                    ],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
                    "/dist/index.json" => json!([
                        {"version":"v26.1.0","date":"2026-10-08","lts":false},
                        {"version":"v24.1.0","date":"2026-10-01","lts":"Example"}
                    ]),
                    "/repos/actions/node-versions/releases" => {
                        json!([{"tag_name":"24.2.0","published_at":"2026-10-08T12:00:00Z","draft":false,"prerelease":false}])
                    }
                    path => panic!("unexpected offline request {path}"),
                };
                Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers: reqwest::header::HeaderMap::from_iter([(
                        reqwest::header::CACHE_CONTROL,
                        "max-age=300".parse().unwrap(),
                    )]),
                    body: serde_json::to_vec(&body).unwrap(),
                })
            })
        }
    }
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let transport = Arc::new(Node);
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        transport.clone(),
    )
    .unwrap();
    let client = Client::new(
        github,
        Cached::new(cache.clone(), transport, 2).unwrap(),
        cache,
    )
    .with_versions_host(false);
    for _ in 0..2 {
        let catalog = client
            .lookup(Source::GithubReleases, "actions/node-versions")
            .await
            .unwrap();
        assert_eq!(catalog.dist_tags["lts"], "24.1.0");
        assert_eq!(catalog.versions.len(), 2);
        assert_eq!(
            catalog
                .versions
                .iter()
                .find(|v| v.version == "24.2.0")
                .unwrap()
                .published_at
                .as_deref(),
            Some("2026-10-08T12:00:00Z")
        );
    }
}

#[tokio::test]
async fn core_node_and_pypi_use_real_metadata_shapes_and_fresh_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mise.json");
    let interactions = [
        ("https://nodejs.org/dist/index.json", json!([
            {"version":"v24.2.0","date":"2026-10-14","lts":false},
            {"version":"v24.1.0","date":"2026-10-01","lts":"Example"},
            {"version":"v22.9.0","date":"2026-10-02","lts":"Previous"}
        ])),
        ("https://pypi.org/pypi/spec-cli/json", json!({"releases":{
            "1.0.0":[{"upload_time_iso_8601":"2026-10-01T09:00:00Z","yanked":false}],
            "1.1.0":[{"upload_time_iso_8601":"2026-10-14T11:00:00Z","yanked":true}, {"upload_time_iso_8601":"2026-10-14T10:00:00Z","yanked":true}],
            "1.2.0":[]
        }})),
    ].into_iter().map(|(url,value)| Interaction {
        request: RecordedRequest { method: "GET".into(), url: url.into(), headers: BTreeMap::from([("accept".into(), vec!["application/json".into()])]), body: Body::Text(String::new()) },
        response: RecordedResponse { status: 200, headers: BTreeMap::from([("etag".into(), vec!["metadata".into()]), ("cache-control".into(), vec!["public,max-age=300".into()])]), body: Body::Json(value) },
    }).collect();
    patchula_testkit::save(&path, &Cassette { schema_version:1, provenance:"Independently simulated from Node distribution index and PyPI JSON API documentation".into(), interactions }, &Default::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        replay.clone(),
    )
    .unwrap();
    let http = Cached::new(cache.clone(), replay.clone(), 4).unwrap();
    let client = Client::new(github, http.clone(), cache).with_versions_host(false);
    let node = client.lookup(Source::Mise, "node").await.unwrap();
    assert_eq!(node.versions[0].version, "24.2.0");
    assert_eq!(node.dist_tags["lts"], "24.1.0");
    assert_eq!(
        node.versions[0].published_at.as_deref(),
        Some("2026-10-14T00:00:00Z")
    );
    client.lookup(Source::Mise, "core:node").await.unwrap();
    let pypi = client.lookup(Source::Pypi, "spec-cli").await.unwrap();
    assert_eq!(pypi.versions.len(), 2);
    assert!(pypi.versions[0].yanked);
    assert_eq!(
        pypi.versions[0].published_at.as_deref(),
        Some("2026-10-14T10:00:00Z")
    );
    assert_eq!(http.stats().requests, 2);
    assert_eq!(http.stats().cache_hits, 1);
    replay.assert_finished().unwrap();
    assert!(
        client
            .lookup(Source::Mise, "unknown-spec-tool")
            .await
            .is_err()
    );
    assert!(client.lookup(Source::Pypi, "../unsafe").await.is_err());
    let runners = client
        .lookup(Source::GithubRunners, "ubuntu-arm")
        .await
        .unwrap();
    assert!(runners.versions.iter().any(|v| v.version == "24.04"));
    assert!(
        runners
            .versions
            .iter()
            .all(|v| v.digest.is_none() && v.published_at.is_none())
    );
}

#[tokio::test]
async fn no_cache_revalidates_fresh_validators_and_304_retains_body() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conditional.json");
    let url = "https://registry.npmjs.org/spec-metadata";
    let interactions = [None, Some("version")]
        .into_iter()
        .map(|etag| {
            let mut headers = BTreeMap::from([("accept".into(), vec!["application/json".into()])]);
            if let Some(etag) = etag {
                headers.insert("if-none-match".into(), vec![etag.into()]);
            }
            Interaction {
                request: RecordedRequest {
                    method: "GET".into(),
                    url: url.into(),
                    headers,
                    body: Body::Text(String::new()),
                },
                response: RecordedResponse {
                    status: if etag.is_some() { 304 } else { 200 },
                    headers: BTreeMap::from([
                        ("etag".into(), vec!["version".into()]),
                        ("cache-control".into(), vec!["max-age=300,no-cache".into()]),
                    ]),
                    body: if etag.is_some() {
                        Body::Text(String::new())
                    } else {
                        Body::Json(json!({"value":"retained"}))
                    },
                },
            }
        })
        .collect();
    patchula_testkit::save(
        &path,
        &Cassette {
            schema_version: 1,
            provenance: "Independently simulated HTTP no-cache revalidation".into(),
            interactions,
        },
        &Default::default(),
    )
    .unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let http = Cached::new(cache, replay.clone(), 4).unwrap();
    let first = http.get(url, "application/json").await.unwrap();
    let next = http.get(url, "application/json").await.unwrap();
    assert_eq!(first.body, next.body);
    assert_eq!(http.stats().not_modified, 1);
    replay.assert_finished().unwrap();
}
