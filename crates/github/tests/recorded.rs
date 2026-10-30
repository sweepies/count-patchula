//! These scenarios are recorded by patchula against public GitHub endpoints.
//! They replay by default, including in CI. Recording requires an existing token.
#![forbid(unsafe_code)]

use patchula_cache::{Cache, Options as CacheOptions};
use patchula_github::{Client, GraphqlPage, Network, Options};
use patchula_testkit::{Options as FixtureOptions, Redactor};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

async fn client(name: &str) -> (tempfile::TempDir, Client) {
    let recording = patchula_testkit::recording();
    let token = if recording {
        std::env::var(patchula_testkit::GITHUB_TOKEN_VAR)
            .or_else(|_| std::env::var(patchula_testkit::GH_TOKEN_VAR))
            .expect("fixture recording requires an existing GITHUB_TOKEN or GH_TOKEN")
    } else {
        "offline-test-credential".into()
    };
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await
    .unwrap();
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/github/{name}.json"));
    let options = FixtureOptions {
        redactor: Redactor::default().with_literals([token.clone()]),
        ..FixtureOptions::default()
    };
    let transport =
        patchula_testkit::from_env(&path, options, || Ok(Arc::new(Network::new()?))).unwrap();
    (
        directory,
        Client::with_transport(&token, cache, Options::default(), transport).unwrap(),
    )
}

#[tokio::test]
async fn public_releases_conditional_get_reuses_304_body() {
    let (_directory, client) = client("public-releases").await;
    let path = "repos/jdx/mr-boxington/releases?per_page=2";
    let first = client.get(path).await.unwrap();
    let releases: Vec<Value> = serde_json::from_slice(&first.body).unwrap();
    assert!(!releases.is_empty());
    assert!(
        !releases[0]["tag_name"]
            .as_str()
            .unwrap()
            .contains("REDACTED")
    );
    let second = client.get(path).await.unwrap();
    assert_eq!(first.body, second.body);
    assert_eq!(client.stats().await.requests, 2);
    assert_eq!(client.stats().await.not_modified, 1);
}

#[tokio::test]
async fn public_tags_rest_pagination_stops_on_first_page() {
    let (_directory, client) = client("public-tags").await;
    let tags = client
        .rest_pages::<Value>("repos/tokio-rs/bytes/tags?per_page=2", |tags| {
            !tags.is_empty()
        })
        .await
        .unwrap();
    assert_eq!(tags.len(), 2);
    assert!(tags[0]["name"].is_string());
    assert!(!tags[0]["name"].as_str().unwrap().contains("REDACTED"));
    assert_eq!(client.stats().await.requests, 1);
}

#[tokio::test]
async fn public_graphql_releases_stops_at_known_history() {
    let (_directory, client) = client("public-graphql").await;
    let query = "query($cursor: String) { repository(owner: \"jdx\", name: \"mr-boxington\") { releases(first: 2, after: $cursor, orderBy: {field: CREATED_AT, direction: DESC}) { nodes { tagName publishedAt } pageInfo { hasNextPage endCursor } } } }";
    let releases = client
        .graphql_pages(
            query,
            json!({}),
            |data| GraphqlPage::<Value>::from_connection(&data, "/repository/releases"),
            |nodes| !nodes.is_empty(),
        )
        .await
        .unwrap();
    assert_eq!(releases.len(), 2);
    assert!(releases[0]["tagName"].is_string());
    assert!(
        !releases[0]["tagName"]
            .as_str()
            .unwrap()
            .contains("REDACTED")
    );
    assert_eq!(client.stats().await.requests, 1);
}
