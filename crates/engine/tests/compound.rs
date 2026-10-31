//! A constraint the Count cannot rewrite is a notice, not a failed lookup.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_testkit::{
    Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Redactor, Replay,
};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn a_compound_constraint_that_cannot_allow_the_release_is_a_notice() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(
        tree.join("patchula.pkl"),
        "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"composer\" }\n",
    )
    .unwrap();
    std::fs::write(
        tree.join("composer.json"),
        r#"{"require":{"acme/lib":"^1 | ^2"}}"#,
    )
    .unwrap();
    std::fs::write(
        tree.join("composer.lock"),
        r#"{"packages":[{"name":"acme/lib","version":"2.5.0"}]}"#,
    )
    .unwrap();
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the Packagist p2 metadata format".into(),
        interactions: vec![Interaction {
            request: RecordedRequest {
                method: "GET".into(),
                url: "https://repo.packagist.org/p2/acme/lib.json".into(),
                headers: [("accept".to_owned(), vec!["application/json".to_owned()])].into(),
                body: Body::Text("".into()),
            },
            response: RecordedResponse {
                status: 200,
                headers: Default::default(),
                body: Body::Json(json!({"packages": {"acme/lib": [
                    {"version": "3.0.0", "time": "2026-01-02T00:00:00+00:00"},
                    {"version": "2.6.0", "time": "2026-01-01T00:00:00+00:00"},
                    {"version": "2.5.0", "time": "2025-12-01T00:00:00+00:00"}
                ]}})),
            },
        }],
    };
    let path = dir.path().join("spec.json");
    patchula_testkit::save(&path, &cassette, &Redactor::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, patchula_testkit::Options::default()).unwrap());
    let cache = Cache::open(
        dir.path().join("cache.sqlite"),
        patchula_cache::Options::default(),
    )
    .await
    .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone())
        .unwrap()
        .with_versions_host(false);
    let report = engine
        .run_at(
            Options {
                local: Some(tree),
                ..Options::default()
            },
            true,
            false,
            "2026-02-01T00:00:00Z".parse().unwrap(),
        )
        .await
        .unwrap();
    replay.assert_finished().unwrap();
    let repo = &report.repositories[0];
    assert!(repo.warnings.is_empty(), "{:?}", repo.warnings);
    assert!(
        repo.lookup_failures.is_empty(),
        "{:?}",
        repo.lookup_failures
    );
    // 2.6.0 is inside the range, so it moves the lock. 3.0.0 is not, and the
    // constraint has two alternatives the Count will not guess between.
    let dep = &repo.package_files[0].dependencies[0];
    assert_eq!(dep.updates.len(), 1, "{:?}", dep.updates);
    assert_eq!(dep.updates[0].new_value, None);
    assert_eq!(dep.updates[0].new_version.as_deref(), Some("2.6.0"));
    assert_eq!(repo.notices.len(), 1, "{:?}", repo.notices);
    let notice = &repo.notices[0];
    assert!(!notice.is_template());
    assert_eq!(
        (
            notice.name.as_str(),
            notice.current.as_str(),
            notice.available.as_str()
        ),
        ("acme/lib", "^1 | ^2", "3.0.0")
    );
    assert!(
        notice
            .reason
            .as_deref()
            .unwrap()
            .contains("without guessing"),
        "{notice:?}"
    );
}
