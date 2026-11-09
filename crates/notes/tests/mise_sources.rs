//! Mise tools whose release notes resolve a repository: the replayed
//! listings live in `fixtures/mise/listings.json`, shared with the
//! datasource tests. Nothing here starts mise or reaches the network.
use patchula_cache::Cache;
use patchula_datasource::Client;
use patchula_http::{Exchange, Response, Transport};
use patchula_mise::Lookup;
use patchula_testkit::MiseReplay;
use patchula_words::Source;
use std::{path::Path, sync::Arc};

/// Answers the one public registry read the notes tests need.
struct Npm;
impl Transport for Npm {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            assert_eq!(
                request.url().as_str(),
                "https://registry.npmjs.org/prettier/3.9.10"
            );
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: reqwest::header::HeaderMap::default(),
                body: br#"{"repository":{"type":"git","url":"git+https://github.com/prettier/prettier.git"}}"#.to_vec(),
                stale: None,
            })
        })
    }
}

fn replay() -> Arc<MiseReplay> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mise/listings.json");
    Arc::new(MiseReplay::open(&path).unwrap())
}
async fn memory() -> Cache {
    Cache::open(":memory:", patchula_cache::Options::default())
        .await
        .unwrap()
}
fn client(replay: Option<&Arc<MiseReplay>>, cache: Cache, token: &str) -> Client {
    let transport = Arc::new(Npm);
    let github = patchula_github::Client::with_transport(
        token,
        cache.clone(),
        patchula_github::Options::default(),
        transport.clone(),
    )
    .unwrap();
    let client = Client::new(
        github,
        patchula_datasource::cached(cache.clone(), transport, 2).unwrap(),
        cache,
    );
    match replay {
        Some(replay) => client.with_mise(Arc::new(Lookup::new(
            replay.clone(),
            "mise".into(),
            "/var/lib/patchula/tools".into(),
            "2026.10.11".into(),
        ))),
        None => client,
    }
}

#[tokio::test]
async fn asdf_and_vfox_tools_are_refused_before_mise_starts() {
    let replay = replay();
    let source = client(Some(&replay), memory().await, "a-token");
    for tool in ["asdf:mise-plugins/asdf-tiny", "vfox:version-fox/vfox-cmake"] {
        let error = source
            .lookup(&patchula_config::PackageId::public(Source::Mise, tool))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("runs plugin code"),
            "{tool}: {error}"
        );
        assert!(
            patchula_notes::Client::new(source.clone())
                .source_repository(
                    &patchula_config::PackageId::public(Source::Mise, tool),
                    "1.0.0"
                )
                .await
                .is_err()
        );
    }
    assert!(replay.calls().is_empty());
}

#[tokio::test]
async fn release_notes_find_their_repository_from_the_release_url() {
    let replay = replay();
    let source = client(Some(&replay), memory().await, "a-token");
    let notes = patchula_notes::Client::new(source.clone());
    let jq = notes
        .source_repository(
            &patchula_config::PackageId::public(Source::Mise, "jq"),
            "1.8.2",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(jq.github.as_deref(), Some("jqlang/jq"));
    let hk = notes
        .source_repository(
            &patchula_config::PackageId::public(Source::Mise, "packslip:jdx/hk"),
            "2.8.0",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hk.github.as_deref(), Some("jdx/hk"));
    // Core tools keep their fixed upstream and need no listing.
    let before = replay.lookups().len();
    for (tool, repo) in [("node", "nodejs/node"), ("core:node", "nodejs/node")] {
        let found = notes
            .source_repository(
                &patchula_config::PackageId::public(Source::Mise, tool),
                "26.12.1",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.github.as_deref(), Some(repo));
    }
    assert_eq!(replay.lookups().len(), before);
}

#[tokio::test]
async fn release_notes_follow_an_npm_backend_when_the_listing_has_no_release_url() {
    let replay = replay();
    let found = patchula_notes::Client::new(client(Some(&replay), memory().await, "a-token"))
        .source_repository(
            &patchula_config::PackageId::public(Source::Mise, "prettier"),
            "3.9.10",
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.github.as_deref(), Some("prettier/prettier"));
    assert_eq!(
        replay.lookups(),
        [
            vec!["ls-remote", "--json", "--prerelease", "--", "prettier"],
            vec!["tool", "--json", "--", "prettier"],
        ]
    );
}
