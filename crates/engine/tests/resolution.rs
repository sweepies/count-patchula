//! How the report names each way a current version was resolved: the
//! lockfile's entry for npm and Cargo, the declared value for every other
//! manager a lockfile resolved. The remaining kinds are pinned by the
//! actions tests (`floating-ref` for a branch ref, `pinned-commit` for a
//! commit pin, `release-line` for a tool, `supported-range` for a toolchain
//! constraint).
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_testkit::{
    Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Redactor, Replay,
};
use serde_json::json;
use std::sync::Arc;

async fn report(tree: &std::path::Path, cassette: Cassette) -> patchula_report::Report {
    let dir = tempfile::tempdir().unwrap();
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
                local: Some(tree.to_path_buf()),
                ..Options::default()
            },
            true,
            false,
            "2026-02-01T00:00:00Z".parse().unwrap(),
        )
        .await
        .unwrap();
    replay.assert_finished().unwrap();
    report
}

fn interaction(url: &str, accept: &str, body: serde_json::Value) -> Interaction {
    Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: url.into(),
            headers: [("accept".to_owned(), vec![accept.to_owned()])].into(),
            body: Body::Text("".into()),
        },
        response: RecordedResponse {
            status: 200,
            headers: Default::default(),
            body: Body::Json(body),
        },
    }
}

fn write(tree: &std::path::Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = tree.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// A `package-lock.json` records where npm resolved the declaration, so the
/// report says `lockfile`.
#[tokio::test]
async fn an_npm_version_recorded_by_its_lockfile_reports_lockfile() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\" }\n",
            ),
            ("package.json", r#"{"dependencies":{"left-pad":"^1.0.0"}}"#),
            (
                "package-lock.json",
                r#"{"packages":{"node_modules/left-pad":{"version":"1.3.0","resolved":"https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz"}}}"#,
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the npm registry metadata format".into(),
        interactions: vec![interaction(
            "https://registry.npmjs.org/left-pad",
            "application/json",
            json!({
                "versions": {"1.0.0": {}, "1.3.0": {}, "1.3.1": {}, "1.4.0": {}},
                "dist-tags": {"latest": "1.4.0"},
            }),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Lockfile);
    assert_eq!(resolution.version, "1.3.0");
}

/// Every other lockfile-resolved manager reports the same version as
/// `declared` today: the `lockfile` resolution names npm and Cargo alone.
/// This is today's wire, not necessarily the right one.
#[tokio::test]
async fn a_composer_version_recorded_by_its_lockfile_reports_declared() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    write(
        &tree,
        &[
            (
                "patchula.pkl",
                "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"composer\" }\n",
            ),
            ("composer.json", r#"{"require":{"acme/lib":"^2.0"}}"#),
            (
                "composer.lock",
                r#"{"packages":[{"name":"acme/lib","version":"2.5.0","notification-url":"https://packagist.org/downloads/"}]}"#,
            ),
        ],
    );
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the Packagist p2 metadata format".into(),
        interactions: vec![interaction(
            "https://repo.packagist.org/p2/acme/lib.json",
            "application/json",
            json!({"packages": {"acme/lib": [
                {"version": "2.6.0", "time": "2026-01-01T00:00:00+00:00"},
                {"version": "2.5.0", "time": "2025-12-01T00:00:00+00:00"}
            ]}}),
        )],
    };
    let report = report(&tree, cassette).await;
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Declared);
    assert_eq!(resolution.version, "2.5.0");
}
