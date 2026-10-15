//! Specification-written offline API responses for discovery, skips and errors.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rustovate_cache::Cache;
use rustovate_engine::{Engine, Options};
use rustovate_testkit::{Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Replay};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
fn get(
    path: &str,
    value: Value,
    status: u16,
    etag: Option<&str>,
    conditional: Option<&str>,
    registry: bool,
) -> Interaction {
    let mut headers = BTreeMap::from([(
        "accept".into(),
        vec![
            if registry {
                "application/vnd.npm.install-v1+json"
            } else {
                "application/vnd.github+json"
            }
            .into(),
        ],
    )]);
    if !registry {
        headers.insert("x-github-api-version".into(), vec!["2022-11-28".into()]);
    }
    if let Some(etag) = conditional {
        headers.insert("if-none-match".into(), vec![etag.into()]);
    }
    Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: format!(
                "https://{}/{path}",
                if registry {
                    "registry.npmjs.org"
                } else {
                    "api.github.com"
                }
            ),
            headers,
            body: Body::Text(String::new()),
        },
        response: RecordedResponse {
            status,
            headers: etag
                .map(|e| BTreeMap::from([("etag".into(), vec![e.into()])]))
                .unwrap_or_default(),
            body: Body::Json(value),
        },
    }
}
#[tokio::test]
async fn discovered_config_precedence_skips_errors_and_cached_extraction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spec.json");
    let mut interactions = Vec::new();
    let listing = json!([{"full_name":"demo/configured"},{"full_name":"demo/skipped"},{"full_name":"demo/error"}]);
    interactions.push(get(
        "user/repos?per_page=100",
        listing.clone(),
        200,
        Some("list"),
        None,
        false,
    ));
    for repo in ["configured", "skipped"] {
        interactions.push(get(
            &format!("repos/demo/{repo}"),
            json!({"default_branch":"main"}),
            200,
            Some("meta"),
            None,
            false,
        ));
        interactions.push(get(
            &format!("repos/demo/{repo}/branches/main"),
            json!({"commit":{"sha":"head","commit":{"tree":{"sha":"tree"}}}}),
            200,
            Some("head"),
            None,
            false,
        ));
        let tree = if repo == "configured" {
            json!([{"path":".github/rustovate.pkl","type":"blob","sha":"config"},{"path":"rustovate.pkl","type":"blob","sha":"bad-config"},{"path":"package.json","type":"blob","sha":"manifest"}])
        } else {
            json!([])
        };
        interactions.push(get(
            &format!("repos/demo/{repo}/git/trees/tree?recursive=1"),
            json!({"truncated":false,"tree":tree}),
            200,
            Some("tree"),
            None,
            false,
        ));
    }
    interactions.push(get(
        "repos/demo/error",
        json!({"message":"Not Found"}),
        404,
        None,
        None,
        false,
    ));
    for (sha, text) in [
        (
            "config",
            "amends \"../Rustovate.pkl\"\nmanagers = new Listing { \"npm\" }\n",
        ),
        (
            "manifest",
            "{\"dependencies\":{\"fixture-spec\":\"1.0.0\"}}",
        ),
    ] {
        interactions.push(get(
            &format!("repos/demo/configured/git/blobs/{sha}"),
            json!({"encoding":"base64","content":STANDARD.encode(text)}),
            200,
            Some(sha),
            None,
            false,
        ));
    }
    interactions.push(get(
        "fixture-spec",
        json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}}}),
        200,
        Some("npm"),
        None,
        true,
    ));
    interactions.push(get(
        "user/repos?per_page=100",
        json!(null),
        304,
        Some("list"),
        Some("list"),
        false,
    ));
    for repo in ["configured", "skipped"] {
        for (suffix, etag) in [("", "meta"), ("/branches/main", "head")] {
            interactions.push(get(
                &format!("repos/demo/{repo}{suffix}"),
                json!(null),
                304,
                Some(etag),
                Some(etag),
                false,
            ));
        }
    }
    interactions.push(get(
        "repos/demo/error",
        json!({"message":"Not Found"}),
        404,
        None,
        None,
        false,
    ));
    interactions.push(get(
        "fixture-spec",
        json!(null),
        304,
        Some("npm"),
        Some("npm"),
        true,
    ));
    let cassette=Cassette{schema_version:1,provenance:"Simulated independently from GitHub repository/tree/blob API and npm metadata specifications".into(),interactions};
    rustovate_testkit::save(&path, &cassette, &Default::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone()).unwrap();
    let cold = engine.lookup(Options::default()).await.unwrap();
    let warm = engine.lookup(Options::default()).await.unwrap();
    assert_eq!(cold.repositories, warm.repositories);
    use rustovate_report::Status;
    assert_eq!(cold.repositories[0].status, Status::Complete);
    assert_eq!(cold.repositories[1].status, Status::Error);
    assert_eq!(cold.repositories[2].status, Status::Skipped);
    assert_eq!(
        cold.repositories[0].package_files[0].dependencies[0].updates[0]
            .new_version
            .as_deref(),
        Some("2.0.0")
    );
    assert_eq!(warm.stats.as_ref().unwrap().extraction_hits, 1);
    replay.assert_finished().unwrap();
}

#[tokio::test]
async fn installation_tokens_use_installation_repository_listing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("installation.json");
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from GitHub installation repository API specification".into(),
        interactions: vec![
            get(
                "user/repos?per_page=100",
                json!({"message":"Resource not accessible by integration"}),
                403,
                None,
                None,
                false,
            ),
            get(
                "installation/repositories?per_page=100&page=1",
                json!({"repositories":[],"total_count":0}),
                200,
                None,
                None,
                false,
            ),
        ],
    };
    rustovate_testkit::save(&path, &cassette, &Default::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone()).unwrap();
    assert!(
        engine
            .lookup(Options::default())
            .await
            .unwrap()
            .repositories
            .is_empty()
    );
    replay.assert_finished().unwrap();
}
