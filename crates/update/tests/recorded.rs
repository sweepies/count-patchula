//! Recorded against `renovatebot/github-action`, a public repository whose
//! dependency dashboard `renovate[bot]` opened (issue #31, open). Replays by
//! default; recording needs `PATCHULA_RECORD=1` and a token.
#![forbid(unsafe_code)]

use patchula_cache::{Cache, Options as CacheOptions};
use patchula_github::{Client, Network, Options};
use patchula_report::Repository;
use patchula_testkit::{Options as FixtureOptions, Redactor};
use patchula_update::Inventory;
use serde_json::json;
use std::{path::Path, sync::Arc};

async fn client(name: &str) -> (tempfile::TempDir, Cache, Client) {
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
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../fixtures/update/{name}.json"));
    let options = FixtureOptions {
        redactor: Redactor::default().with_literals([token.clone()]),
        ..FixtureOptions::default()
    };
    let transport =
        patchula_testkit::from_env(&path, options, || Ok(Arc::new(Network::new()?))).unwrap();
    (
        directory,
        cache.clone(),
        Client::with_transport(&token, cache, Options::default(), transport).unwrap(),
    )
}

/// `renovatebot/github-action`'s default branch and the head the cassette
/// recorded; a live head check compares against these, so they freeze when
/// the cassette does.
fn repository() -> Repository {
    serde_json::from_value(json!({"repository":"renovatebot/github-action",
        "status":"complete","reason":null,"packageFiles":[],
        "defaultBranch":"main",
        "commit":"9e4cad21de6d039b080dc3bd7c651788b7dd72a9"}))
    .unwrap()
}

/// The first inventory read has no stored actor hint, so it asks the new
/// first-page document unfiltered; the answer's `viewer.login` lands in the
/// test's own cache, so the second read is the same filtered document every
/// time — in the recording and in replay alike.
#[tokio::test]
async fn a_public_inventory_read_replays_the_split_documents() {
    let (_directory, cache, client) = client("inventory").await;
    let config = patchula_config::load(Path::new("../../PatchulaConfig.pkl")).unwrap();
    let repository = repository();
    let first = Inventory::read(&client, &cache, &repository, &config)
        .await
        .unwrap();
    assert!(first.issues.complete);
    // renovate[bot]'s open dependency dashboard is in the unfiltered read.
    assert!(
        first
            .issues
            .items
            .iter()
            .flatten()
            .any(|issue| issue.number == 31)
    );
    // Ref nodes never decode inside a cassette: the redactor replaces every
    // `signature` value, so the `Commit` fragment fails and the items stay
    // `None` — the walk still gathered both refs' slots.
    assert_eq!(first.refs.items.len(), 2);
    let second = Inventory::read(&client, &cache, &repository, &config)
        .await
        .unwrap();
    assert_eq!(second.actor, first.actor);
    // The second read filtered `issues` by `createdBy` for the recorded
    // actor: everything left is that actor's own.
    assert!(
        second
            .issues
            .items
            .iter()
            .flatten()
            .all(|issue| issue.author.as_ref().is_some_and(|author| {
                let login = author.login.strip_suffix("[bot]").unwrap_or(&author.login);
                let actor = second.actor.strip_suffix("[bot]").unwrap_or(&second.actor);
                login == actor
            }))
    );
    // The read the filter skipped stays: every non-issue surface is equal.
    assert_eq!(format!("{:?}", first.refs), format!("{:?}", second.refs));
    assert_eq!(
        format!("{:?}", first.closed),
        format!("{:?}", second.closed)
    );
}
