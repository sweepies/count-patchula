use super::*;
use axum::body::Body;
use axum::http::HeaderMap;
use patchula_cache::{Options as CacheOptions, queue::Work};
use patchula_config::settings::Settings;
use patchula_http::Transport;
use std::sync::Arc;
use tower::ServiceExt;

/// The dashboard receipt patchula_update stores, written through the same
/// namespace and scope.
#[derive(serde::Serialize, serde::Deserialize)]
struct DashboardReceipt(Value);
#[derive(serde::Serialize)]
struct DashboardScope {
    repository: String,
}
impl patchula_cache::State for DashboardReceipt {
    const NAMESPACE: &'static str = "dashboard-receipt";
    const VERSION: u32 = 1;
    type Scope = DashboardScope;
}

fn owned(number: u64, branch: &str) -> Value {
    let marker = json!({"version": 1, "identity": "a".repeat(64), "update": "b".repeat(64)});
    json!({"number": number, "html_url": format!("https://github.com/acme/app/pull/{number}"),
        "title": "Update serde to v2", "body": format!("| serde | 1 → 2 |\n<!-- patchula:{marker} -->"),
        "state": "open", "draft": false, "labels": [{"name": "dependencies"}],
        "user": {"login": "patchula[bot]"},
        "head": {"ref": branch, "sha": "d".repeat(40), "repo": {"full_name": "acme/app"}},
        "base": {"ref": "main", "repo": {"full_name": "acme/app"}},
        "created_at": "2026-10-23T08:00:00Z", "updated_at": "2026-10-23T09:00:00Z",
        "auto_merge": null})
}

/// GitHub's view of each repository and the PRs open in it.
struct GitHub;
impl Transport for GitHub {
    fn send(&self, request: reqwest::Request) -> patchula_http::Exchange<'_> {
        let (status, body) = match request.url().path() {
            "/repos/acme/app" => (200, json!({"private": false})),
            "/repos/acme/secret" => (200, json!({"private": true})),
            "/repos/acme/late" | "/repos/acme/stranger" => (200, json!({"private": false})),
            "/repos/acme/app/pulls" => {
                let mut human = owned(9, "feature");
                human["user"]["login"] = json!("someone");
                (
                    200,
                    json!([
                        owned(5, "patchula/serde-2.x"),
                        owned(6, "patchula/held"),
                        human
                    ]),
                )
            }
            "/repos/acme/app/pulls/5" => {
                let mut pr = owned(5, "patchula/serde-2.x");
                pr["mergeable"] = json!(true);
                pr["mergeable_state"] = json!("clean");
                (200, pr)
            }
            _ => (500, json!({"message": "Server Error"})),
        };
        Box::pin(async move {
            Ok(patchula_http::Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: HeaderMap::default(),
                body: serde_json::to_vec(&body).unwrap(),
                stale: None,
            })
        })
    }
}

async fn recorded() -> Cache {
    let cache = Cache::open(":memory:", CacheOptions::default())
        .await
        .unwrap();
    let known = ["acme/app", "acme/secret", "acme/broken", "acme/fresh"]
        .map(|repo| (repo.to_owned(), false))
        .to_vec();
    cache.repositories_added(1, known).await.unwrap();
    let client = Client::with_transport(
        "offline-test-token",
        cache.clone(),
        patchula_github::Options::default(),
        Arc::new(GitHub),
    )
    .unwrap();
    record(
        &cache,
        &client,
        "patchula[bot]",
        1,
        &Recorded {
            report: &report(),
            withheld: report(),
        },
        Utc::now(),
        // Discovery has not listed acme/late yet; acme/stranger is not ours.
        &|repo: String| Box::pin(async move { repo == "acme/late" }),
    )
    .await;
    cache
}

fn settings(extra: &str) -> Settings {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.pkl");
    std::fs::write(&path, format!("amends \"PatchulaSettings.pkl\"\n{extra}\n")).unwrap();
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
        "schemaVersion": 6,
        "repositories": [app, repository("acme/secret"), repository("acme/broken"),
            repository("acme/late"), repository("acme/stranger")],
        "actions": {"schemaVersion": 2, "dryRun": false,
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
    let cache = recorded().await;
    cache
        .set_state(
            &DashboardScope {
                repository: "acme/app".into(),
            },
            &DashboardReceipt(json!({"number":7,"actor":"patchula","hash":"h"})),
        )
        .await
        .unwrap();
    let client = Client::with_transport(
        "offline-test-token",
        cache.clone(),
        patchula_github::Options::default(),
        Arc::new(GitHub),
    )
    .unwrap();
    record(
        &cache,
        &client,
        "patchula[bot]",
        1,
        &Recorded {
            report: &report(),
            withheld: report(),
        },
        Utc::now(),
        // Discovery has not listed acme/late yet; acme/stranger is not ours.
        &|repo: String| Box::pin(async move { repo == "acme/late" }),
    )
    .await;
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
    let names: Vec<_> = repositories.iter().map(|r| &r["repository"]).collect();
    assert_eq!(
        names,
        ["acme/app", "acme/fresh", "acme/late"],
        "private, unreadable and other installations' ones are hidden"
    );
    assert_eq!(
        repositories[1],
        json!({"repository": "acme/fresh", "ranAt": null, "queued": null}),
        "a repository that has not run yet"
    );
    let app = &repositories[0];
    assert_eq!(app["repository"], "acme/app");
    assert_eq!(app["status"], "complete");
    assert_eq!(app["defaultBranch"], "main");
    assert_eq!(
        (&app["dependencies"], &app["updates"], &app["pullRequests"]),
        (&json!(2), &json!(1), &json!(2))
    );
    assert_eq!(app["dashboard"], 7);
    assert_eq!(app["queued"]["running"].as_bool(), Some(false));
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
    let pulls = detail["pullRequests"].as_array().unwrap();
    assert_eq!(pulls.len(), 2, "only the Count's open PRs");
    let serde = &pulls[0];
    assert_eq!(serde["number"], 5);
    assert_eq!(serde["url"], "https://github.com/acme/app/pull/5");
    assert_eq!(serde["branch"], "patchula/serde-2.x");
    assert_eq!(serde["title"], "Update serde to v2");
    assert!(
        serde["body"]
            .as_str()
            .unwrap()
            .starts_with("| serde | 1 → 2 |")
    );
    assert_eq!(serde["labels"], json!(["dependencies"]));
    assert_eq!(serde["headSha"], "d".repeat(40));
    assert_eq!(serde["base"], "main");
    assert_eq!(
        (&serde["mergeable"], &serde["mergeableState"]),
        (&json!(true), &json!("clean"))
    );
    assert_eq!(serde["autoMerge"].as_bool(), Some(false));
    assert_eq!(
        serde["action"],
        json!({"status": "created", "reason": null})
    );
    let held = &pulls[1];
    assert_eq!(held["number"], 6);
    assert_eq!(
        (&held["mergeable"], &held["action"]),
        (&Value::Null, &Value::Null),
        "a PR the run left alone, whose own read failed"
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
    let cache = recorded().await;
    let only = routes(Public {
        cache: cache.clone(),
        settings: Reload::new(settings(
            "server { allowedRepositories { \"acme/other\" } }",
        )),
    });
    let (.., list) = get(&only, "/api/v1/repositories").await;
    assert_eq!(list["repositories"], json!([]));
    let (status, ..) = get(&only, "/api/v1/repositories/acme/app").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let off = routes(Public {
        cache,
        settings: Reload::new(settings("server { publicApi = false }")),
    });
    for path in ["/api/v1/repositories", "/api/v1/repositories/acme/app"] {
        let (status, _, error) = get(&off, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error["errorKind"], "disabled");
    }
}

#[test]
fn credentialed_lookups_leave_only_the_declared_dependency_in_public() {
    let dependency = |package: &str, source: &str, credentialed: bool| {
        json!({"manager": "regex", "name": package, "package": package,
            "current": {"value": "1.0.0", "version": "1.0.0", "digest": null},
            "updates": [{"newValue": "1.1.0", "newVersion": "1.1.0", "newDigest": null,
                "updateType": "minor", "group": null}],
            "metadata": {"source": source, "kinds": [], "locations": [],
                "credentialed": credentialed,
                "heldBack": [{"version": "2.0.0", "updateType": "major",
                    "publishedAt": "2026-11-01T00:00:00Z", "eligibleAt": "2026-11-02T00:00:00Z"}]}})
    };
    let private = "https://acme.jfrog.io/artifactory/api/helm/charts|app";
    let report: Report = serde_json::from_value(json!({"schemaVersion": 6, "repositories": [{
        "repository": "acme/app", "status": "complete", "reason": null,
        "defaultBranch": "main", "commit": "c".repeat(40),
        "warnings": [format!("digest of {private}:2.0.0: not found"), "a config warning".to_owned()],
        "packageFiles": [{"path": "deploy.yaml",
            "dependencies": [dependency(private, "helm", true), dependency("nginx", "docker", false)]}]}]}))
    .unwrap();
    let shown = withheld(&report);
    let files = &report.repositories[0].package_files;
    let [hidden, public] = &shown.repositories[0].package_files[0].dependencies[..] else {
        panic!("two dependencies")
    };
    assert_eq!(hidden.package, private);
    assert_eq!(hidden.current.value.as_deref(), Some("1.0.0"));
    assert_eq!(hidden.current.version, None);
    assert!(hidden.updates.is_empty());
    assert!(hidden.metadata.as_ref().unwrap().held_back.is_empty());
    assert_eq!(public, &files[0].dependencies[1]);
    // A warning that names it could quote what the registry answered.
    assert_eq!(shown.repositories[0].warnings, ["a config warning"]);
}

#[test]
fn withheld_classifies_every_metadata_field() {
    let dependency = json!({"manager": "npm", "name": "left-pad", "package": "left-pad",
        "current": {"value": "^1.0.0", "version": "1.0.0", "digest": "sha256:abc"},
        "updates": [],
        "metadata": {"source": "npm", "kinds": ["production"],
            "locations": [{"start": 0, "end": 9, "line": 1, "column": 1, "pointer": "/a"}],
            "credentialed": true,
            "tagDigests": {"v1.0.0": "d".repeat(40)},
            "heldBack": [{"version": "2.0.0", "updateType": "major",
                "publishedAt": "2026-11-01T00:00:00Z", "eligibleAt": "2026-11-02T00:00:00Z"}],
            "stale": {"npm": 1},
            "lookup": {"evaluatedAt": "2026-10-01T00:00:00Z", "disabledUpdates": [], "rejected": []},
            "resolution": {"kind": "lockfile", "version": "1.0.0"},
            "advisories": [{"id": "GHSA-aaaa-bbbb-cccc", "url": "https://osv.dev/vulnerability/GHSA-aaaa-bbbb-cccc"}]}});
    let report: Report = serde_json::from_value(json!({"schemaVersion": 6, "repositories": [{
        "repository": "acme/app", "status": "complete", "reason": null,
        "packageFiles": [{"path": "package.json", "dependencies": [dependency]}]}]}))
    .unwrap();
    let shown = withheld(&report);
    let dep = &shown.repositories[0].package_files[0].dependencies[0];
    // What the declaration itself says stays.
    assert_eq!(dep.current.value.as_deref(), Some("^1.0.0"));
    assert_eq!(dep.current.digest.as_deref(), Some("sha256:abc"));
    assert_eq!(dep.current.version, None);
    let meta = dep.metadata.as_ref().unwrap();
    assert_eq!(meta.source, patchula_words::Source::Npm);
    assert_eq!(meta.kinds, [patchula_words::DependencyKind::Production]);
    assert_eq!(meta.locations.len(), 1);
    assert!(meta.credentialed);
    // Everything a credentialed lookup could have found is gone.
    assert!(meta.tag_digests.is_empty());
    assert!(meta.held_back.is_empty());
    assert!(meta.stale.is_empty());
    assert!(meta.lookup.is_none());
    assert!(meta.resolution.is_none());
    assert!(meta.advisories.is_empty());
    assert!(meta.image.is_none());
    assert!(dep.updates.is_empty());
}

#[tokio::test]
async fn only_a_private_repository_is_stored_without_what_credentials_found() {
    let chart = "https://acme.jfrog.io/artifactory/api/helm/charts|app";
    let repository = |name: &str| {
        json!({"repository": name, "status": "complete", "reason": null,
            "defaultBranch": "main", "commit": "c".repeat(40),
            "packageFiles": [{"path": "deploy.yaml", "dependencies": [{
                "manager": "regex", "name": chart, "package": chart,
                "current": {"value": "1.0.0", "version": "1.0.0", "digest": null},
                "updates": [{"newValue": "1.1.0", "newVersion": "1.1.0", "newDigest": null,
                    "updateType": "minor", "group": null}],
                "metadata": {"source": "helm", "kinds": [], "locations": [], "credentialed": true}}]}]})
    };
    let report: Report = serde_json::from_value(json!({"schemaVersion": 6,
        "repositories": [repository("acme/app"), repository("acme/secret")]}))
    .unwrap();
    let cache = Cache::open(":memory:", CacheOptions::default())
        .await
        .unwrap();
    let known = ["acme/app", "acme/secret"]
        .map(|repo| (repo.to_owned(), false))
        .to_vec();
    cache.repositories_added(1, known).await.unwrap();
    let client = Client::with_transport(
        "offline-test-token",
        cache.clone(),
        patchula_github::Options::default(),
        Arc::new(GitHub),
    )
    .unwrap();
    record(
        &cache,
        &client,
        "patchula[bot]",
        1,
        &Recorded {
            report: &report,
            withheld: withheld(&report),
        },
        Utc::now(),
        &|_: String| Box::pin(async { false }),
    )
    .await;
    let snapshot = |repo: &'static str| {
        let cache = cache.clone();
        async move {
            let row = cache.repository(repo).await.unwrap().unwrap();
            String::from_utf8(row.snapshot.unwrap()).unwrap()
        }
    };
    // The public repository shows the update, as its dashboard does.
    assert!(snapshot("acme/app").await.contains("1.1.0"));
    // The private one is stored without it, in case it later becomes public.
    let secret = snapshot("acme/secret").await;
    assert!(
        secret.contains(chart) && !secret.contains("1.1.0"),
        "{secret}"
    );
}
