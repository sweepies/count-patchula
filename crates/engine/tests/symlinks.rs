//! A symlink is never read as a file: on GitHub its tree entry is mode
//! `120000`, and the link target's text must not become the file's content.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_testkit::github::FakeGitHub;
use patchula_update::blob_sha;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

const CONFIG: &str = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\" }\n";

/// Hash this test's recorded requests and compare them with the snapshot;
/// `PATCHULA_UPDATE_TRANSCRIPTS=1` rewrites it instead.
fn check_transcript() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

/// The link's blob is the target's path; the scan must mark the manifest
/// unreadable and never ask for that blob.
#[tokio::test]
async fn a_symlinked_manifest_is_unreadable_on_github() {
    let github = Arc::new(FakeGitHub::new("demo/app"));
    let link = blob_sha("base/package.json");
    github.seed_files(
        "main",
        "base",
        &BTreeMap::from([("patchula.pkl".to_owned(), CONFIG.to_owned())]),
        vec![json!({"path":"package.json","sha":link,"type":"blob","mode":"120000"})],
    );
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline-token", cache, 4, github.clone()).unwrap();
    let report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    assert!(
        repo.warnings
            .iter()
            .any(|w| w == "package.json: skipped: the Count could not read it"),
        "{:?}",
        repo.warnings
    );
    assert!(
        !github
            .log()
            .iter()
            .any(|(_, path, body)| path.contains(&link)
                || body["variables"]
                    .as_object()
                    .is_some_and(|vars| vars.values().any(|v| v.as_str() == Some(&link)))),
        "the link's blob was read"
    );
    check_transcript();
}
