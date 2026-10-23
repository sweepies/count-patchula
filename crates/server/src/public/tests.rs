use super::*;
use crate::settings::Settings;
use axum::body::Body;
use patchula_cache::{ExtractionKey, queue::Work};
use patchula_http::Transport;
use std::sync::Arc;
use tower::ServiceExt;

/// GitHub's view of each repository's visibility.
struct Visibility;
impl Transport for Visibility {
    fn send(&self, request: reqwest::Request) -> patchula_http::Exchange<'_> {
        let (status, body) = match request.url().path() {
            "/repos/acme/app" => (200, json!({"private": false})),
            "/repos/acme/secret" => (200, json!({"private": true})),
            _ => (500, json!({"message": "Server Error"})),
        };
        Box::pin(async move {
            Ok(patchula_http::Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    }
}

fn settings(extra: &str) -> Settings {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.pkl");
    std::fs::write(&path, format!("amends \"PatchulaServer.pkl\"\n{extra}\n")).unwrap();
    Settings::load(&path).unwrap()
}

fn report() -> Report {
    let repository = |name: &str| {
        json!({"repository": name, "status": "complete", "reason": null, "packageFiles": [],
            "defaultBranch": "main", "commit": "c".repeat(40)})
    };
    let dependency = |name: &str, updates: Value| {
        json!({"manager": "cargo", "name": name, "package": name,
            "current": {"value": "1", "version": "1.0.0", "digest": null}, "updates": updates})
    };
    let mut app = repository("acme/app");
    app["packageFiles"] = json!([{"path": "Cargo.toml", "dependencies": [
        dependency("serde", json!([{"newValue": "2", "newVersion": "2.0.0", "newDigest": null,
            "updateType": "major", "group": null}])),
        dependency("tokio", json!([])),
    ]}]);
    serde_json::from_value(json!({
        "schemaVersion": 2,
        "repositories": [app, repository("acme/secret"), repository("acme/broken")],
        "actions": {"schemaVersion": 1, "dryRun": false,
            "branches": [{"repository": "acme/app", "branch": "patchula/serde-2.x",
                "baseCommit": "c".repeat(40), "status": "created", "reason": null,
                "message": "Update serde to v2", "previousCommit": null, "commit": "d".repeat(40),
                "verified": true, "lockfilePending": false,
                "files": [{"path": "Cargo.toml", "blobSha": "e".repeat(40), "diff": "-1\n+2\n"}]}],
            "pullRequests": [
                {"repository": "acme/app", "branch": "patchula/serde-2.x", "number": 5,
                    "url": "https://github.com/acme/app/pull/5", "status": "created", "reason": null},
                {"repository": "acme/app", "branch": "patchula/old", "number": 3,
                    "url": "https://github.com/acme/app/pull/3", "status": "closed", "reason": null},
                {"repository": "acme/secret", "branch": "patchula/x", "number": 1,
                    "url": "https://github.com/acme/secret/pull/1", "status": "created", "reason": null}]}
    }))
    .unwrap()
}

async fn get(router: &Router, path: &str) -> (StatusCode, Option<HeaderValue>, Value) {
    let response = router
        .clone()
        .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let origin = response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .cloned();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, origin, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn anyone_reads_what_the_last_run_left_of_public_repositories() {
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let dashboard = ExtractionKey {
        repo: "acme/app".into(),
        default_branch_commit: String::new(),
        config_hash: "dashboard-receipt-v1".into(),
    };
    cache
        .extraction_put(&dashboard, br#"{"number":7,"actor":"patchula","hash":"h"}"#)
        .await
        .unwrap();
    let client = Client::with_transport(
        "offline-test-token",
        cache.clone(),
        Default::default(),
        Arc::new(Visibility),
    )
    .unwrap();
    record(&cache, &client, 1, &report()).await;
    cache
        .enqueue(None, vec![Work::repository(1, "acme/app")], 0, 0)
        .await
        .unwrap();
    let router = routes(Public {
        cache: cache.clone(),
        settings: Reload::new(settings("")),
    });

    let (status, origin, list) = get(&router, "/api/v1/repositories").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(origin.unwrap(), "*");
    let repositories = list["repositories"].as_array().unwrap();
    assert_eq!(
        repositories.len(),
        1,
        "private and unreadable ones are hidden"
    );
    let app = &repositories[0];
    assert_eq!(app["repository"], "acme/app");
    assert_eq!(app["status"], "complete");
    assert_eq!(app["defaultBranch"], "main");
    assert_eq!(
        (&app["dependencies"], &app["updates"], &app["pullRequests"]),
        (&json!(2), &json!(1), &json!(1))
    );
    assert_eq!(app["dashboard"], 7);
    assert_eq!(app["queued"]["running"], false);
    assert!(app["ranAt"].is_string());
    assert!(app.get("packageFiles").is_none());

    let (status, origin, detail) = get(&router, "/api/v1/repositories/Acme/App").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(origin.unwrap(), "*");
    assert_eq!(detail["repository"], "acme/app");
    assert_eq!(
        detail["packageFiles"][0]["dependencies"][0]["updates"][0]["newVersion"],
        "2.0.0"
    );
    assert_eq!(detail["branches"][0]["files"], json!(["Cargo.toml"]));
    assert_eq!(
        detail["pullRequests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pr| pr["number"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [5, 3]
    );
    assert_eq!(detail["dashboard"], 7);
    assert_eq!(detail["merged"], json!([]));

    for path in [
        "/api/v1/repositories/acme/secret",
        "/api/v1/repositories/acme/broken",
        "/api/v1/repositories/acme/missing",
    ] {
        let (status, origin, error) = get(&router, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(origin.unwrap(), "*");
        assert_eq!(error["errorKind"], "unknown-repository");
    }
}

#[tokio::test]
async fn settings_hide_repositories_or_the_whole_api() {
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let client = Client::with_transport(
        "offline-test-token",
        cache.clone(),
        Default::default(),
        Arc::new(Visibility),
    )
    .unwrap();
    record(&cache, &client, 1, &report()).await;
    let only = routes(Public {
        cache: cache.clone(),
        settings: Reload::new(settings("allowedRepositories { \"acme/other\" }")),
    });
    let (_, _, list) = get(&only, "/api/v1/repositories").await;
    assert_eq!(list["repositories"], json!([]));
    let (status, _, _) = get(&only, "/api/v1/repositories/acme/app").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let off = routes(Public {
        cache,
        settings: Reload::new(settings("publicApi = false")),
    });
    for path in ["/api/v1/repositories", "/api/v1/repositories/acme/app"] {
        let (status, _, error) = get(&off, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error["errorKind"], "disabled");
    }
}
