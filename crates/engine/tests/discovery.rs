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
                "application/json"
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
fn head(repo: &str, branch: Option<&str>) -> Interaction {
    let (owner, name) = repo.split_once('/').unwrap();
    Interaction {
        request: RecordedRequest {
            method: "POST".into(),
            url: "https://api.github.com/graphql".into(),
            headers: BTreeMap::from([
                ("accept".into(), vec!["application/vnd.github+json".into()]),
                ("x-github-api-version".into(), vec!["2022-11-28".into()]),
                ("content-type".into(), vec!["application/json".into()]),
            ]),
            body: Body::Json(
                json!({"query":rustovate_engine::REPOSITORY_HEAD_QUERY,"variables":{"owner":owner,"name":name}}),
            ),
        },
        response: RecordedResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Body::Json(
                json!({"data":{"repository": branch.map(|branch| json!({"isPrivate":false,"defaultBranchRef":{"name":branch,"target":{"oid":"head","tree":{"oid":"tree"}}}}))}}),
            ),
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
        interactions.push(head(&format!("demo/{repo}"), Some("main")));
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
    interactions.push(head("demo/error", None));
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
    }
    interactions.push(head("demo/error", None));
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

#[tokio::test]
async fn cold_snapshot_and_warm_branch_rename_reuse_identical_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("branch.json");
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Independently simulated GitHub default-branch rename".into(),
        interactions: vec![
            head("demo/renamed", Some("main")),
            get(
                "repos/demo/renamed/git/trees/tree?recursive=1",
                json!({"truncated":false,"tree":[]}),
                200,
                None,
                None,
                false,
            ),
            get(
                "repos/demo/renamed",
                json!({"default_branch":"next"}),
                200,
                None,
                None,
                false,
            ),
            get(
                "repos/demo/renamed/branches/main",
                json!(null),
                404,
                None,
                None,
                false,
            ),
            get(
                "repos/demo/renamed/branches/next",
                json!({"commit":{"sha":"head","commit":{"tree":{"sha":"tree"}}}}),
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
    let config = rustovate_config::load_files(
        "/repo/config.pkl",
        BTreeMap::from([
            (
                "/repo/config.pkl".into(),
                "amends \"Rustovate.pkl\"\nmanagers = new Listing {}".into(),
            ),
            (
                "/repo/Rustovate.pkl".into(),
                rustovate_config::SCHEMA.into(),
            ),
        ]),
    )
    .unwrap();
    let options = || Options {
        repositories: vec!["demo/renamed".into()],
        config: Some(config.clone()),
    };
    engine.lookup(options()).await.unwrap();
    let warm = engine.lookup(options()).await.unwrap();
    assert_eq!(
        warm.repositories[0].status,
        rustovate_report::Status::Complete
    );
    assert_eq!(warm.repositories[0].default_branch.as_deref(), Some("next"));
    assert_eq!(warm.stats.unwrap().extraction_hits, 1);
    replay.assert_finished().unwrap();
}

#[tokio::test]
async fn warm_sources_start_before_head_finishes_but_new_extraction_is_authoritative() {
    use rustovate_http::{Exchange, Transport};
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Gate {
        replay: Arc<Replay>,
        warm: AtomicBool,
        prefetched: AtomicBool,
        release: tokio::sync::Notify,
    }
    impl Transport for Gate {
        fn send(&self, request: reqwest::Request) -> Exchange<'_> {
            Box::pin(async move {
                if self.warm.load(Ordering::SeqCst) {
                    if request.url().path() == "/repos/demo/pipeline/branches/main" {
                        self.release.notified().await;
                    } else if request.url().host_str() == Some("registry.npmjs.org")
                        && request.url().path() == "/old-dep"
                    {
                        self.prefetched.store(true, Ordering::SeqCst);
                        self.release.notify_one();
                    }
                }
                self.replay.send(request).await
            })
        }
    }
    // No elapsed-time performance assertion: the blocked head cannot complete
    // until the source read starts. New files must still replace the hint.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pipeline.json");
    let mut interactions = vec![head("demo/pipeline", Some("main"))];
    for (tree, blob, dependency) in [("tree", "blob", "old-dep"), ("tree2", "blob2", "new-dep")] {
        interactions.push(get(
            &format!("repos/demo/pipeline/git/trees/{tree}?recursive=1"),
            json!({"truncated":false,"tree":[{"path":"package.json","type":"blob","sha":blob}]}),
            200,
            None,
            None,
            false,
        ));
        interactions.push(get(&format!("repos/demo/pipeline/git/blobs/{blob}"), json!({"encoding":"base64","content":STANDARD.encode(format!(r#"{{"dependencies":{{"{dependency}":"1.0.0"}}}}"#))}), 200, None, None, false));
        interactions.push(get(
            dependency,
            json!({"dist-tags":{"latest":"1.0.1"},"versions":{"1.0.0":{},"1.0.1":{}}}),
            200,
            Some(dependency),
            None,
            true,
        ));
    }
    interactions.push(get(
        "repos/demo/pipeline",
        json!({"default_branch":"main"}),
        200,
        None,
        None,
        false,
    ));
    interactions.push(get(
        "repos/demo/pipeline/branches/main",
        json!({"commit":{"sha":"head2","commit":{"tree":{"sha":"tree2"}}}}),
        200,
        None,
        None,
        false,
    ));
    interactions.push(get(
        "old-dep",
        json!(null),
        304,
        Some("old-dep"),
        Some("old-dep"),
        true,
    ));
    rustovate_testkit::save(
        &path,
        &Cassette {
            schema_version: 1,
            provenance: "Independently simulated extraction/source overlap and changed dependency"
                .into(),
            interactions,
        },
        &Default::default(),
    )
    .unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let gate = Arc::new(Gate {
        replay: replay.clone(),
        warm: AtomicBool::new(false),
        prefetched: AtomicBool::new(false),
        release: Default::default(),
    });
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, gate.clone()).unwrap();
    let config = rustovate_config::load_files(
        "/repo/config.pkl",
        BTreeMap::from([
            (
                "/repo/config.pkl".into(),
                "amends \"Rustovate.pkl\"\nmanagers = new Listing { \"npm\" }".into(),
            ),
            (
                "/repo/Rustovate.pkl".into(),
                rustovate_config::SCHEMA.into(),
            ),
        ]),
    )
    .unwrap();
    let options = || Options {
        repositories: vec!["demo/pipeline".into()],
        config: Some(config.clone()),
    };
    engine.lookup(options()).await.unwrap();
    gate.warm.store(true, Ordering::SeqCst);
    let warm = tokio::time::timeout(std::time::Duration::from_secs(5), engine.lookup(options()))
        .await
        .expect("head waited for a source read that never started")
        .unwrap();
    assert!(gate.prefetched.load(Ordering::SeqCst));
    let repo = &warm.repositories[0];
    assert_eq!(repo.status, rustovate_report::Status::Complete);
    assert_eq!(repo.commit.as_deref(), Some("head2"));
    assert_eq!(repo.package_files[0].dependencies.len(), 1);
    assert_eq!(repo.package_files[0].dependencies[0].name, "new-dep");
    assert_eq!(
        warm.stats.unwrap().lookups,
        1,
        "removed hints must not enter the report"
    );
    replay.assert_finished().unwrap();
}
