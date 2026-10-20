//! The same in-repo Pkl and token-only engine entry point used by the sandbox.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rustovate_cache::Cache;
use rustovate_engine::{Engine, Options};
use rustovate_http::{Exchange, Response, Transport};
use rustovate_report::{ActionStatus, Report, Status};
use rustovate_update::blob_sha;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Fake {
    files: BTreeMap<String, String>,
    lock: bool,
    lock_name: &'static str,
    writes: Mutex<Vec<(String, Value)>>,
    prs: Mutex<Vec<Value>>,
    issues: Mutex<Vec<Value>>,
    requests: Mutex<Vec<(String, String, Value)>>,
    latency: std::time::Duration,
    close_on_read: std::sync::atomic::AtomicBool,
    catalog: Mutex<Value>,
    fail_lookup: std::sync::atomic::AtomicBool,
    merge_settings: Mutex<Value>,
    checks: Mutex<Vec<Value>>,
    change_head_on_checks: std::sync::atomic::AtomicBool,
    merge_refused: std::sync::atomic::AtomicBool,
    deletion_race: std::sync::atomic::AtomicBool,
}

struct Rotating(std::sync::atomic::AtomicUsize);
struct Authenticated(Arc<Fake>);
impl Transport for Authenticated {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        if request.url().host_str() == Some("api.github.com") {
            let auth = &request.headers()["authorization"];
            assert!(auth.is_sensitive());
            assert!(auth.to_str().unwrap().starts_with("Bearer offline-token-"));
        } else {
            assert!(!request.headers().contains_key("authorization"));
        }
        self.0.send(request)
    }
}
impl rustovate_github::Credentials for Rotating {
    fn scope(&self) -> &str {
        "offline-app:1/installation:10"
    }
    fn token(&self) -> rustovate_github::Token<'_> {
        let count = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Box::pin(async move { Ok(format!("offline-token-{count}")) })
    }
}
#[tokio::test]
async fn installation_and_caller_token_batches_match_and_token_rotation_keeps_receipts_warm() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let cli_fake = Arc::new(Fake::new(false));
    let server_fake = Arc::new(Fake::new(false));
    let cli_cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let server_cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let credentials = Arc::new(Rotating(Default::default()));
    let cli = Engine::with_transport("offline-caller-token", cli_cache, 4, cli_fake)
        .unwrap()
        .with_versions_host(false)
        .with_recheck(rustovate_engine::Recheck::Now);
    let server = Engine::with_credentials(
        credentials.clone(),
        server_cache,
        4,
        Arc::new(Authenticated(server_fake.clone())),
    )
    .unwrap()
    .with_recheck(rustovate_engine::Recheck::Now);
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    let mut cli_report = cli.run_at(options(), false, false, now).await.unwrap();
    let mut server_report = server.run_at(options(), false, false, now).await.unwrap();
    cli_report.stats = None;
    server_report.stats = None;
    assert_eq!(cli_report, server_report);
    server_fake.writes.lock().unwrap().clear();
    let repeated = server.run_at(options(), false, false, now).await.unwrap();
    assert_eq!(
        repeated.actions.unwrap().branches[0].status,
        ActionStatus::Unchanged
    );
    assert!(server_fake.writes.lock().unwrap().is_empty());
    assert!(credentials.0.load(std::sync::atomic::Ordering::Relaxed) > 2);
}
impl Fake {
    fn new(lock: bool) -> Self {
        Self {
        files:BTreeMap::from([
            (".github/rustovate.pkl".into(),"amends \"../Rustovate.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into()),
            ("package.json".into(),"{\n  \"dependencies\": {\"fixture\": \"1.0.0\"}\n}\n".into()),
        ]),lock,lock_name:"bun.lockb",writes:Default::default(),prs:Default::default(),issues:Default::default(),requests:Default::default(),latency:Default::default(),close_on_read:Default::default(),catalog:Mutex::new(json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}}})),fail_lookup:Default::default(),
        merge_settings: Mutex::new(json!({"id":"repo-id","autoMergeAllowed":false,"squashMergeAllowed":true,"mergeCommitAllowed":true,"rebaseMergeAllowed":true,"deleteBranchOnMerge":false})),
        checks: Mutex::new(vec![json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"})]),
        change_head_on_checks: Default::default(), merge_refused: Default::default(), deletion_race: Default::default(),
    }
    }
    fn tree(&self) -> Value {
        let mut tree: Vec<_> = self
            .files
            .iter()
            .map(|(p, c)| json!({"path":p,"sha":blob_sha(c),"mode":"100644","type":"blob"}))
            .collect();
        if self.lock {
            tree.push(
                json!({"path":self.lock_name,"sha":"unselected-lock","type":"blob","mode":"100644"}),
            );
        }
        json!({"truncated":false,"tree":tree})
    }
}
impl Transport for Fake {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let path = request.url().path();
        let body: Value = request
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        self.requests.lock().unwrap().push((
            request.method().to_string(),
            path.into(),
            body.clone(),
        ));
        let (status, value) = if request.url().host_str() == Some("registry.npmjs.org") {
            (
                if self.fail_lookup.load(std::sync::atomic::Ordering::Relaxed) {
                    503
                } else {
                    200
                },
                self.catalog.lock().unwrap().clone(),
            )
        } else if path == "/graphql" {
            assert!(request.headers()["authorization"].is_sensitive());
            if body["query"] == rustovate_engine::REPOSITORY_HEAD_QUERY {
                (
                    200,
                    json!({"data":{"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base","tree":{"oid":"base-tree"}}}}}}),
                )
            } else if body["query"] == "query{viewer{login}}" {
                (200, json!({"data":{"viewer":{"login":"rustovate[bot]"}}}))
            } else if body["query"] == rustovate_update::INVENTORY_QUERY {
                let prs = self.prs.lock().unwrap();
                let nodes: Vec<_> = prs.iter().filter(|p|p["state"] == "open").map(|p|json!({
                    "name":p["head"]["ref"],"target":{"oid":p["head"]["sha"],"tree":{"oid":"updated-tree"},"signature":{"isValid":true}},
                    "associatedPullRequests":{"pageInfo":{"hasNextPage":false},"nodes":[{
                        "number":p["number"],"id":p["node_id"],"url":p["html_url"],"title":p["title"],"body":p["body"],
                        "headRefName":p["head"]["ref"],"headRefOid":p["head"]["sha"],"baseRefName":p["base"]["ref"],"author":p["user"],
                        "headRepository":{"nameWithOwner":"demo/sandbox"},"baseRepository":{"nameWithOwner":"demo/sandbox"},
                        "labels":{"nodes":p["labels"],"pageInfo":{"hasNextPage":false}},"autoMergeRequest":p["auto_merge"]
                    }]}
                })).collect();
                let closed: Vec<_> = prs.iter().filter(|p|body["variables"]["closed"].as_array().unwrap().contains(&p["node_id"])).map(|p|json!({
                    "id":p["node_id"],"state":if p["state"] == "closed" {"CLOSED"} else {"OPEN"},"merged":false,"body":p["body"],
                    "author":p["user"],"headRepository":{"nameWithOwner":"demo/sandbox"},"baseRepository":{"nameWithOwner":"demo/sandbox"}
                })).collect();
                let mut repository = self.merge_settings.lock().unwrap().clone();
                repository["defaultBranchRef"] = json!({"name":"main","target":{"oid":"base"}});
                repository["refs"] =
                    json!({"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}});
                repository["issues"] = json!({"nodes":self.issues.lock().unwrap().iter().filter(|i|i["state"] == "OPEN").cloned().collect::<Vec<_>>(),"pageInfo":{"hasNextPage":false,"endCursor":null}});
                (
                    200,
                    json!({"data":{"viewer":{"login":"rustovate[bot]"},"nodes":closed,"repository":repository}}),
                )
            } else if body["query"] == rustovate_update::automerge::CHECKS_QUERY {
                if self
                    .change_head_on_checks
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    self.prs.lock().unwrap()[0]["head"]["sha"] = json!("human-commit");
                }
                let checks = self.checks.lock().unwrap();
                let offset = body["variables"]["cursor"]
                    .as_str()
                    .map(|c| c.parse::<usize>().unwrap())
                    .unwrap_or(0);
                let next = offset + 100;
                (
                    200,
                    json!({"data":{"repository":{"object":{"oid":body["variables"]["oid"],"statusCheckRollup":{"contexts":{"nodes":checks.iter().skip(offset).take(100).cloned().collect::<Vec<_>>(),"pageInfo":{"hasNextPage":next < checks.len(),"endCursor":next.to_string()}}}}}}}),
                )
            } else if body["query"]
                .as_str()
                .is_some_and(|q| q.contains("PullRequestAutoMerge"))
            {
                let enabled = body["query"]
                    .as_str()
                    .unwrap()
                    .contains("enablePullRequestAutoMerge");
                if enabled {
                    assert_eq!(body["variables"]["input"]["expectedHeadOid"], "new-commit");
                }
                self.writes.lock().unwrap().push((path.into(), body));
                for pr in self.prs.lock().unwrap().iter_mut() {
                    pr["auto_merge"] = if enabled {
                        json!({"enabled":true})
                    } else {
                        Value::Null
                    };
                }
                (
                    200,
                    json!({"data":if enabled {json!({"enablePullRequestAutoMerge":{"pullRequest":{"id":"pr-1"}}})} else {json!({"disablePullRequestAutoMerge":{"pullRequest":{"id":"pr-1"}}})}}),
                )
            } else {
                assert_eq!(body["query"], rustovate_update::REF_UPDATE);
                let deleting = body["variables"]["input"]["refUpdates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|r| r["afterOid"] == "0000000000000000000000000000000000000000");
                self.writes.lock().unwrap().push((path.into(), body));
                (
                    200,
                    if deleting
                        && self
                            .deletion_race
                            .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        json!({"errors":[{"message":"head changed"}]})
                    } else {
                        json!({"data":{"updateRefs":{"clientMutationId":null}}})
                    },
                )
            }
        } else if request.method() == reqwest::Method::PUT && path.ends_with("/merge") {
            let mut prs = self.prs.lock().unwrap();
            let pr = &mut prs[0];
            assert_eq!(body["sha"], pr["head"]["sha"]);
            assert_eq!(pr["user"]["login"], "rustovate[bot]");
            assert_eq!(pr["mergeable_state"], "clean");
            self.writes.lock().unwrap().push((path.into(), body));
            let merged = !self
                .merge_refused
                .load(std::sync::atomic::Ordering::Relaxed);
            if merged {
                pr["state"] = json!("closed");
                pr["merged_at"] = json!("2026-10-15T12:00:00Z");
            }
            (200, json!({"merged":merged,"sha":"merged-base"}))
        } else if request.method() == reqwest::Method::PATCH
            && path.starts_with("/repos/demo/sandbox/issues/")
        {
            self.writes
                .lock()
                .unwrap()
                .push((path.into(), body.clone()));
            let number: u64 = path.rsplit('/').next().unwrap().parse().unwrap();
            let mut issues = self.issues.lock().unwrap();
            let issue = issues.iter_mut().find(|i| i["number"] == number).unwrap();
            if let Some(text) = body.get("body") {
                issue["body"] = text.clone();
            }
            if body["state"] == "closed" {
                issue["state"] = json!("CLOSED");
            }
            (200, issue.clone())
        } else if request.method() == reqwest::Method::PATCH
            && path.starts_with("/repos/demo/sandbox/pulls/")
        {
            self.writes
                .lock()
                .unwrap()
                .push((path.into(), body.clone()));
            let number: u64 = path.rsplit('/').next().unwrap().parse().unwrap();
            let mut prs = self.prs.lock().unwrap();
            let pr = prs.iter_mut().find(|p| p["number"] == number).unwrap();
            for (key, value) in body.as_object().unwrap() {
                if key == "base" {
                    pr["base"]["ref"] = value.clone();
                } else {
                    pr[key] = value.clone();
                }
            }
            (200, pr.clone())
        } else if request.method() == reqwest::Method::POST {
            self.writes
                .lock()
                .unwrap()
                .push((path.into(), body.clone()));
            if path.ends_with("/pulls") {
                let number = self.prs.lock().unwrap().len() + 1;
                let pr = json!({"number":number,"node_id":format!("pr-{number}"),"html_url":format!("https://github.com/demo/sandbox/pull/{number}"),"title":body["title"],"body":body["body"],"state":"open","user":{"login":"rustovate[bot]"},"head":{"ref":body["head"],"sha":"new-commit","repo":{"full_name":"demo/sandbox"}},"base":{"ref":"main","repo":{"full_name":"demo/sandbox"}},"labels":[{"name":"dependencies"}],"auto_merge":null,"created_at":chrono::Utc::now().to_rfc3339(),"merged_at":null});
                let mut pr = pr;
                pr["draft"] = json!(false);
                pr["mergeable"] = json!(true);
                pr["mergeable_state"] = json!("clean");
                pr["head"]["label"] = json!("demo:branch");
                pr["head"]["repo"]["updated_at"] = json!("2026-10-15T00:00:00Z");
                pr["base"]["sha"] = json!("base");
                self.prs.lock().unwrap().push(pr.clone());
                (201, pr)
            } else if path.ends_with("/issues") {
                let number = 100 + self.issues.lock().unwrap().len();
                let issue = json!({"number":number,"id":format!("dashboard-{number}"),"title":body["title"],"body":body["body"],"state":"OPEN","author":{"__typename":"Bot","login":"rustovate"}});
                self.issues.lock().unwrap().push(issue.clone());
                (201, issue)
            } else if path.ends_with("/git/blobs") {
                (
                    201,
                    json!({"sha":blob_sha(body["content"].as_str().unwrap())}),
                )
            } else if path.ends_with("/git/trees") {
                (201, json!({"sha":"updated-tree"}))
            } else if path.ends_with("/git/commits") {
                (
                    201,
                    json!({"sha":"new-commit","verification":{"verified":true}}),
                )
            } else {
                panic!("unexpected write {path}");
            }
        } else if path == "/repos/demo/sandbox/pulls" {
            (200, json!(*self.prs.lock().unwrap()))
        } else if path.starts_with("/repos/demo/sandbox/pulls/") {
            let number: u64 = path.rsplit('/').next().unwrap().parse().unwrap();
            if self
                .close_on_read
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                self.prs.lock().unwrap()[0]["state"] = json!("closed");
            }
            (
                200,
                self.prs
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|p| p["number"] == number)
                    .unwrap()
                    .clone(),
            )
        } else if path == "/repos/demo/sandbox/branches" {
            (200, json!([]))
        } else if path == "/repos/demo/sandbox" {
            (200, json!({"default_branch":"main","node_id":"repo-id"}))
        } else if path.ends_with("/git/trees/base-tree") {
            (200, self.tree())
        } else if let Some(sha) = path.strip_prefix("/repos/demo/sandbox/git/blobs/") {
            let content = if sha == "unselected-lock" {
                "{}"
            } else {
                self.files.values().find(|c| blob_sha(c) == sha).unwrap()
            };
            (
                200,
                json!({"encoding":"base64","content":STANDARD.encode(content)}),
            )
        } else if path.ends_with("/git/commits/base") {
            (200, json!({"tree":{"sha":"base-tree"}}))
        } else if path.ends_with("/git/ref/heads/main") {
            (200, json!({"object":{"sha":"base"}}))
        } else if path.contains("/git/ref/heads/") {
            (404, json!({}))
        } else if path.ends_with("/branches/main") {
            (
                200,
                json!({"commit":{"sha":"base","commit":{"tree":{"sha":"base-tree"}}}}),
            )
        } else {
            panic!("unexpected read {path}");
        };
        Box::pin(async move {
            tokio::time::sleep(self.latency).await;
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
            })
        })
    }
}

#[tokio::test(start_paused = true)]
async fn steady_runs_only_lookup_and_one_inventory_with_open_or_closed_updates() {
    for closed in [false, true] {
        let mut fake = Fake::new(false);
        fake.latency = std::time::Duration::from_millis(200);
        let fake = Arc::new(fake);
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let run = || {
            engine.run(
                Options {
                    repositories: vec!["demo/sandbox".into()],
                    ..Default::default()
                },
                false,
                false,
            )
        };
        let first = run().await.unwrap();
        assert_eq!(
            first.actions.as_ref().unwrap().branches[0].status,
            ActionStatus::Created
        );
        if closed {
            fake.prs.lock().unwrap()[0]["state"] = json!("closed");
            let verified = run().await.unwrap();
            assert_eq!(
                verified.actions.as_ref().unwrap().branches[0].status,
                ActionStatus::Skipped
            );
        }
        // Reopen the persistent cache with a new engine: no process-local proof.
        drop(engine);
        let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        fake.requests.lock().unwrap().clear();
        fake.writes.lock().unwrap().clear();
        let started = tokio::time::Instant::now();
        let report = engine
            .run(
                Options {
                    repositories: vec!["demo/sandbox".into()],
                    ..Default::default()
                },
                false,
                false,
            )
            .await
            .unwrap();
        assert_eq!(report.repositories[0].status, Status::Complete);
        assert!(
            fake.writes.lock().unwrap().is_empty(),
            "steady dashboard must not mutate"
        );
        assert_eq!(
            report.actions.as_ref().unwrap().branches[0].status,
            if closed {
                ActionStatus::Skipped
            } else {
                ActionStatus::Unchanged
            }
        );
        let requests = fake.requests.lock().unwrap();
        assert_eq!(requests.len(), 4, "{requests:?}");
        assert_eq!(
            requests
                .iter()
                .filter(|(_, path, body)| path == "/graphql"
                    && body["query"] == rustovate_update::INVENTORY_QUERY)
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(method, path, _)| method == "GET"
                    && matches!(
                        path.as_str(),
                        "/repos/demo/sandbox" | "/repos/demo/sandbox/branches/main"
                    ))
                .count(),
            2
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(method, path, _)| method == "GET" && path == "/fixture")
                .count(),
            1
        );
        eprintln!(
            "steady closed={closed}: {} requests (repository metadata + branch head + npm lookup + inventory), {:?} at 200ms synthetic latency",
            requests.len(),
            started.elapsed()
        );
    }
}

#[tokio::test(start_paused = true)]
async fn dashboard_renames_are_preserved_and_disabling_closes_only_the_owned_issue() {
    let fake = Arc::new(Fake::new(false));
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let run = |config| {
        engine.run(
            Options {
                repositories: vec!["demo/sandbox".into()],
                config,
                ..Default::default()
            },
            false,
            false,
        )
    };
    run(None).await.unwrap();
    fake.issues.lock().unwrap()[0]["title"] = json!("Owner dashboard title");
    fake.writes.lock().unwrap().clear();
    run(None).await.unwrap();
    assert!(fake.writes.lock().unwrap().is_empty());
    assert_eq!(
        fake.issues.lock().unwrap()[0]["title"],
        "Owner dashboard title"
    );
    // Simulate the previous layout while retaining the cached content hash.
    {
        let mut issues = fake.issues.lock().unwrap();
        let body = issues[0]["body"].as_str().unwrap();
        let (actions, details) = body.split_once("\n<details>").unwrap();
        let (intro, prs) = actions.split_once("\n## Open PRs").unwrap();
        issues[0]["body"] = json!(format!("{intro}\n<details>{details}\n## Open PRs{prs}"));
    }
    run(None).await.unwrap();
    {
        let writes = fake.writes.lock().unwrap();
        assert_eq!(
            writes.len(),
            1,
            "layout migration must only patch the dashboard"
        );
        assert!(writes[0].0.contains("/issues/"));
    }
    fake.writes.lock().unwrap().clear();
    run(None).await.unwrap();
    assert!(
        fake.writes.lock().unwrap().is_empty(),
        "unchanged content hash must converge after one patch"
    );
    let mut config = rustovate_config::load(std::path::Path::new("../../Rustovate.pkl")).unwrap();
    config.prs.dashboard = false;
    config.defaults.wait_hours = 0;
    run(Some(config)).await.unwrap();
    assert_eq!(fake.issues.lock().unwrap()[0]["state"], "CLOSED");
    fake.writes.lock().unwrap().clear();
    let mut config = rustovate_config::load(std::path::Path::new("../../Rustovate.pkl")).unwrap();
    config.prs.dashboard = false;
    config.defaults.wait_hours = 0;
    run(Some(config)).await.unwrap();
    assert!(fake.writes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn owner_closure_during_publication_keeps_repository_successful() {
    let fake = Arc::new(Fake::new(false));
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let run = || {
        engine.run(
            Options {
                repositories: vec!["demo/sandbox".into()],
                ..Default::default()
            },
            false,
            false,
        )
    };
    run().await.unwrap();
    // Invalidate the write receipt without changing the ownership marker.
    fake.prs.lock().unwrap()[0]["title"] = json!("operator title");
    fake.close_on_read
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let report = run().await.unwrap();
    assert_eq!(report.repositories[0].status, Status::Complete);
    let pr = &report.actions.as_ref().unwrap().pull_requests[0];
    assert_eq!(pr.status, ActionStatus::Skipped);
    assert!(pr.reason.as_deref().unwrap().contains("closed-by-owner"));
}
#[tokio::test(start_paused = true)]
async fn token_only_run_uses_repository_config_cold_and_cached_and_reports_actions() {
    for (lock, allow, dry_run) in [
        (false, false, true),
        (false, false, false),
        (true, false, false),
        (true, true, true),
        (true, true, false),
    ] {
        let fake = Arc::new(Fake::new(lock));
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        for repeat in 0..2 {
            let report = engine
                .run(
                    Options {
                        repositories: vec!["demo/sandbox".into()],
                        ..Default::default()
                    },
                    dry_run,
                    allow,
                )
                .await
                .unwrap();
            assert_eq!(
                report.repositories[0].status,
                Status::Complete,
                "{report:?}"
            );
            Report::from_json(&serde_json::to_vec(&report).unwrap()).unwrap();
            let actions = report.actions.as_ref().unwrap();
            assert_eq!(actions.schema_version, 1);
            assert_eq!(actions.dry_run, dry_run);
            assert_eq!(actions.branches.len(), 1, "{actions:?}");
            let action = &actions.branches[0];
            assert_eq!(action.lockfile_pending, lock);
            assert_eq!(
                action.status,
                if lock && !allow {
                    ActionStatus::Skipped
                } else if dry_run {
                    ActionStatus::Planned
                } else {
                    if repeat == 0 {
                        ActionStatus::Created
                    } else {
                        ActionStatus::Unchanged
                    }
                },
                "{action:?}"
            );
            assert!(
                action.files[0]
                    .diff
                    .contains("+  \"dependencies\": {\"fixture\": \"2.0.0\"}")
            );
            if dry_run {
                assert!(fake.writes.lock().unwrap().is_empty());
            } else if lock && !allow {
                assert!(
                    fake.writes
                        .lock()
                        .unwrap()
                        .iter()
                        .all(|(p, _)| p.contains("/issues"))
                );
            }
            if repeat == 1 {
                assert_eq!(report.stats.as_ref().unwrap().extraction_hits, 1);
            }
        }
    }
}

struct FailTools;

#[tokio::test(start_paused = true)]
async fn fallback_auto_merge_requires_a_prior_receipt_and_every_current_head_check() {
    for case in [
        "green",
        "neutral-skipped-status",
        "status-only",
        "pending",
        "failure",
        "cancelled",
        "no-checks",
        "pending-status",
        "failed-status",
        "late-page-failure",
        "draft",
        "conflict",
        "protected",
        "behind",
        "unknown",
        "head-race",
        "human-head",
        "human-owner",
        "api-refusal",
        "delete-race",
    ] {
        let fake = Arc::new(Fake::new(false));
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache.clone(), 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let mut config = checkbox_config();
        config.defaults.merge = true;
        let now = "2026-10-15T12:00:00Z".parse().unwrap();
        let first = checkbox_run(&engine, &config, now).await;
        assert_eq!(
            first.actions.unwrap().branches[0].status,
            ActionStatus::Created
        );
        assert!(
            !fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path, body)| path.ends_with("/merge")
                    || body["query"] == rustovate_update::automerge::CHECKS_QUERY),
            "{case}: new PR must wait for another run"
        );
        match case {
            "neutral-skipped-status" => *fake.checks.lock().unwrap() = vec![
                json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"NEUTRAL"}),
                json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"}),
                json!({"__typename":"StatusContext","state":"SUCCESS"}),
            ],
            "status-only" => *fake.checks.lock().unwrap() = vec![json!({"__typename":"StatusContext","state":"SUCCESS"})],
            "pending" => fake.checks.lock().unwrap()[0]["status"] = json!("IN_PROGRESS"),
            "failure" => fake.checks.lock().unwrap()[0]["conclusion"] = json!("FAILURE"),
            "cancelled" => fake.checks.lock().unwrap()[0]["conclusion"] = json!("CANCELLED"),
            "no-checks" => fake.checks.lock().unwrap().clear(),
            "pending-status" | "failed-status" => fake.checks.lock().unwrap().push(json!({"__typename":"StatusContext","state":if case == "pending-status" {"PENDING"} else {"FAILURE"}})),
            "late-page-failure" => {
                let green = fake.checks.lock().unwrap()[0].clone();
                *fake.checks.lock().unwrap() = vec![green; 101];
                fake.checks.lock().unwrap()[100]["conclusion"] = json!("FAILURE");
            }
            "draft" => fake.prs.lock().unwrap()[0]["draft"] = json!(true),
            "conflict" => fake.prs.lock().unwrap()[0]["mergeable"] = json!(false),
            "protected" | "behind" | "unknown" => fake.prs.lock().unwrap()[0]["mergeable_state"] = json!(if case == "protected" {"blocked"} else {case}),
            "head-race" => fake.change_head_on_checks.store(true, std::sync::atomic::Ordering::Relaxed),
            "human-head" => fake.prs.lock().unwrap()[0]["head"]["sha"] = json!("human-commit"),
            "human-owner" => fake.prs.lock().unwrap()[0]["user"]["login"] = json!("human"),
            "api-refusal" => fake.merge_refused.store(true, std::sync::atomic::Ordering::Relaxed),
            "delete-race" => fake.deletion_race.store(true, std::sync::atomic::Ordering::Relaxed),
            _ => {}
        }
        fake.requests.lock().unwrap().clear();
        fake.writes.lock().unwrap().clear();
        let report = engine
            .run_at(
                Options {
                    repositories: vec!["demo/sandbox".into()],
                    config: Some(config.clone()),
                    ..Default::default()
                },
                false,
                false,
                now,
            )
            .await
            .unwrap();
        let merges = matches!(
            case,
            "green" | "neutral-skipped-status" | "status-only" | "delete-race"
        );
        let actions = &report.actions.as_ref().unwrap().pull_requests;
        assert_eq!(
            actions.iter().any(|a| a.status == ActionStatus::Merged),
            merges,
            "{case}: {report:?}"
        );
        let requests = fake.requests.lock().unwrap().clone();
        let merge_requests: Vec<_> = requests
            .iter()
            .filter(|(_, path, _)| path.ends_with("/merge"))
            .collect();
        assert_eq!(
            merge_requests.len(),
            usize::from(merges || case == "api-refusal"),
            "{case}: {requests:?}"
        );
        if let Some((method, _, body)) = merge_requests.first() {
            assert_eq!(method, "PUT");
            assert_eq!(*body, json!({"sha":"new-commit","merge_method":"squash"}));
        }
        let deletes: Vec<_> = requests
            .iter()
            .filter(|(_, _, b)| {
                b["query"] == rustovate_update::REF_UPDATE
                    && b["variables"]["input"]["refUpdates"][1]["afterOid"]
                        == "0000000000000000000000000000000000000000"
            })
            .collect();
        assert_eq!(deletes.len(), usize::from(merges), "{case}");
        if let Some((_, _, body)) = deletes.first() {
            assert_eq!(
                body["variables"]["input"]["refUpdates"][0]["beforeOid"],
                "merged-base"
            );
            assert_eq!(
                body["variables"]["input"]["refUpdates"][1]["beforeOid"],
                "new-commit"
            );
        }
        let dashboard = fake.issues.lock().unwrap()[0]["body"]
            .as_str()
            .unwrap()
            .to_owned();
        if merges {
            assert!(
                dashboard.contains("## Merged PRs") && dashboard.contains("merged by rustovate"),
                "{case}: {dashboard}"
            );
            let bytes = cache
                .extraction_get(&rustovate_cache::ExtractionKey {
                    repo: "demo/sandbox".into(),
                    default_branch_commit: "rustovate/".into(),
                    config_hash: "verified-write-receipts-v3".into(),
                })
                .await
                .unwrap()
                .unwrap();
            assert!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["writes"]
                    .as_object()
                    .unwrap()
                    .is_empty(),
                "merged heads must not remain eligible for another merge"
            );
            let github = rustovate_github::Client::with_transport(
                "offline-installation",
                cache.clone(),
                Default::default(),
                fake.clone(),
            )
            .unwrap();
            let inventory = rustovate_update::Inventory::read(
                &github,
                &cache,
                &report.repositories[0],
                &config,
            )
            .await
            .unwrap();
            let prs = rustovate_update::automerge::dashboard_prs(&inventory, "demo/sandbox", &[]);
            let next_dashboard = rustovate_update::dashboard::render(
                &report.repositories[0],
                &[],
                &prs,
                Default::default(),
            );
            assert!(
                next_dashboard.contains("## Merged PRs")
                    && next_dashboard.contains("merged by rustovate"),
                "merged outcomes must survive the next inventory: {next_dashboard}"
            );
        } else if !matches!(case, "human-head" | "head-race" | "human-owner") {
            assert!(dashboard.contains("auto-merge"), "{case}: {dashboard}");
        }
        if case == "delete-race" {
            assert!(actions.iter().any(|a| {
                a.reason
                    .as_deref()
                    .is_some_and(|r| r.contains("cleanup deferred"))
            }));
        }
        if case == "late-page-failure" {
            assert_eq!(
                requests
                    .iter()
                    .filter(|(_, _, b)| b["query"] == rustovate_update::automerge::CHECKS_QUERY)
                    .count(),
                2
            );
        }
        if matches!(case, "human-head" | "human-owner") {
            assert!(
                !requests
                    .iter()
                    .any(|(_, _, b)| b["query"] == rustovate_update::automerge::CHECKS_QUERY)
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn auto_merge_uses_inventory_methods_and_prefers_native_without_status_reads() {
    for method in ["native", "squash", "merge", "rebase", "none"] {
        let fake = Arc::new(Fake::new(false));
        {
            let mut settings = fake.merge_settings.lock().unwrap();
            settings["autoMergeAllowed"] = json!(method == "native");
            settings["squashMergeAllowed"] = json!(matches!(method, "native" | "squash"));
            settings["mergeCommitAllowed"] = json!(matches!(method, "native" | "squash" | "merge"));
            settings["rebaseMergeAllowed"] = json!(method != "none");
            settings["deleteBranchOnMerge"] = json!(true);
        }
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let mut config = checkbox_config();
        config.defaults.merge = true;
        let now = chrono::Utc::now();
        checkbox_run(&engine, &config, now).await;
        fake.requests.lock().unwrap().clear();
        let report = engine
            .run_at(
                Options {
                    repositories: vec!["demo/sandbox".into()],
                    config: Some(config),
                    ..Default::default()
                },
                false,
                false,
                now,
            )
            .await
            .unwrap();
        let requests = fake.requests.lock().unwrap().clone();
        assert_eq!(
            requests
                .iter()
                .filter(|(_, _, b)| b["query"] == rustovate_update::INVENTORY_QUERY)
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|(_, path, _)| path == "/repos/demo/sandbox")
                .count(),
            1,
            "only the existing extraction metadata read is allowed"
        );
        assert!(
            !requests
                .iter()
                .any(|(_, _, b)| b["query"] == rustovate_update::REF_UPDATE),
            "GitHub deletes branches in this case"
        );
        if method == "native" {
            assert!(!fake.prs.lock().unwrap()[0]["auto_merge"].is_null());
            assert!(!requests.iter().any(|(_, path, b)| path.ends_with("/merge")
                || b["query"] == rustovate_update::automerge::CHECKS_QUERY));
            assert_eq!(
                requests.len(),
                4,
                "native steady run retains one inventory and no merge precondition reads: {requests:?}"
            );
        } else if method == "none" {
            assert_eq!(report.repositories[0].status, Status::Error);
            assert!(!requests.iter().any(|(_, path, _)| path.ends_with("/merge")));
        } else {
            assert!(
                requests
                    .iter()
                    .any(|(_, path, b)| path.ends_with("/merge") && b["merge_method"] == method),
                "{method}: {requests:?}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn manual_create_age_and_schedule_permits_do_not_authorize_fallback_merging() {
    for hold in ["age", "schedule"] {
        let fake = Arc::new(Fake::new(false));
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let mut config = checkbox_config();
        config.defaults.merge = true;
        let now: chrono::DateTime<chrono::Utc> = "2026-10-15T12:00:00Z".parse().unwrap();
        if hold == "age" {
            config.defaults.wait_hours = 24;
            *fake.catalog.lock().unwrap() = json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}},"time":{"2.0.0":now.to_rfc3339()}});
        } else {
            config.defaults.windows = vec![rustovate_config::Window {
                cron: vec!["0 0 1 1 *".into()],
                zone: "UTC".into(),
            }];
        }
        checkbox_run(&engine, &config, now).await;
        tick_dashboard(&fake, "create");
        checkbox_run(&engine, &config, now).await;
        assert_eq!(fake.prs.lock().unwrap().len(), 1);
        fake.requests.lock().unwrap().clear();
        checkbox_run(&engine, &config, now).await;
        assert!(
            !fake
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path, b)| path.ends_with("/merge")
                    || b["query"] == rustovate_update::automerge::CHECKS_QUERY),
            "{hold}"
        );
        if hold == "age" {
            let later = checkbox_run(&engine, &config, now + chrono::Duration::hours(25)).await;
            assert!(
                later
                    .actions
                    .unwrap()
                    .pull_requests
                    .iter()
                    .any(|a| a.status == ActionStatus::Merged)
            );
        }
    }
}

fn checkbox_config() -> rustovate_config::Config {
    let mut config = rustovate_config::load(std::path::Path::new("../../Rustovate.pkl")).unwrap();
    config.prs.dashboard = true;
    config.defaults.wait_hours = 0;
    config
}

#[tokio::test(start_paused = true)]
async fn age_cached_catalogs_refresh_for_checked_pr_and_repository_controls_only() {
    let mut fixture = Fake::new(false);
    fixture.files.insert(
        "package.json".into(),
        json!({"dependencies":{"fixture":"1.0.0","other":"1.0.0"}}).to_string(),
    );
    *fixture.catalog.lock().unwrap() = json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}},"time":{"1.0.0":"2020-01-01T00:00:00Z","2.0.0":"2021-01-01T00:00:00Z"}});
    let fake = Arc::new(fixture);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let mut config = checkbox_config();
    config.defaults.wait_hours = 24;
    let now: chrono::DateTime<chrono::Utc> = "2026-10-15T12:00:00Z".parse().unwrap();
    checkbox_run(&engine, &config, now).await;
    fake.requests.lock().unwrap().clear();
    let warm = checkbox_run(&engine, &config, now + chrono::Duration::minutes(1)).await;
    assert!(
        warm.repositories[0].package_files[0]
            .dependencies
            .iter()
            .all(|dep| dep
                .metadata
                .as_ref()
                .unwrap()
                .lookup
                .as_ref()
                .unwrap()
                .catalog
                .as_ref()
                .unwrap()
                .reused)
    );
    assert!(
        !fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, path, _)| matches!(path.as_str(), "/fixture" | "/other"))
    );
    {
        let mut prs = fake.prs.lock().unwrap();
        let pr = prs
            .iter_mut()
            .find(|pr| pr["head"]["ref"].as_str().unwrap().contains("fixture-"))
            .unwrap();
        pr["body"] = json!(
            pr["body"]
                .as_str()
                .unwrap()
                .replace("- [ ] Created", "- [x] Created")
        );
    }
    *fake.catalog.lock().unwrap() = json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}},"time":{"1.0.0":"2020-01-01T00:00:00Z","2.0.0":"2022-01-01T00:00:00Z"}});
    fake.requests.lock().unwrap().clear();
    let selected = checkbox_run(&engine, &config, now + chrono::Duration::minutes(2)).await;
    for dep in &selected.repositories[0].package_files[0].dependencies {
        let evidence = dep
            .metadata
            .as_ref()
            .unwrap()
            .lookup
            .as_ref()
            .unwrap()
            .catalog
            .as_ref()
            .unwrap();
        assert_eq!(evidence.reused, dep.name != "fixture");
        assert_eq!(
            dep.updates[0].release_age.as_ref().unwrap().published_at,
            if dep.name == "fixture" {
                "2022-01-01T00:00:00Z"
            } else {
                "2021-01-01T00:00:00Z"
            }
        );
        assert!(
            dep.updates
                .iter()
                .any(|update| update.new_version.as_deref() == Some("2.0.0"))
        );
    }
    {
        let reads = fake.requests.lock().unwrap();
        assert_eq!(
            reads
                .iter()
                .filter(|(_, path, _)| path == "/fixture")
                .count(),
            1
        );
        assert!(!reads.iter().any(|(_, path, _)| path == "/other"));
    }
    tick_dashboard(&fake, "recheck");
    fake.requests.lock().unwrap().clear();
    let all = checkbox_run(&engine, &config, now + chrono::Duration::minutes(3)).await;
    assert!(
        all.repositories[0].package_files[0]
            .dependencies
            .iter()
            .all(|dep| !dep
                .metadata
                .as_ref()
                .unwrap()
                .lookup
                .as_ref()
                .unwrap()
                .catalog
                .as_ref()
                .unwrap()
                .reused)
    );
    for package in ["/fixture", "/other"] {
        assert_eq!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, path, _)| path == package)
                .count(),
            1
        );
    }
    assert_unticked(&fake);
}
async fn checkbox_run(
    engine: &Engine,
    config: &rustovate_config::Config,
    now: chrono::DateTime<chrono::Utc>,
) -> Report {
    let report = engine
        .run_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                config: Some(config.clone()),
                ..Default::default()
            },
            false,
            false,
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        report.repositories[0].status,
        Status::Complete,
        "{report:?}"
    );
    report
}
fn tick_dashboard(fake: &Fake, action: &str) {
    let mut issues = fake.issues.lock().unwrap();
    let body = issues[0]["body"].as_str().unwrap();
    let line = body
        .lines()
        .find(|l| l.contains(&format!("\"action\":\"{action}\"")))
        .unwrap();
    // Wording deliberately changes: only the hidden marker may select behavior.
    let marker = line.split_once("<!-- rustovate-action:").unwrap().1;
    issues[0]["body"] = json!(body.replace(
        line,
        &format!("- [X] Operator-edited wording <!-- rustovate-action:{marker}")
    ));
}
fn assert_unticked(fake: &Fake) {
    for issue in fake
        .issues
        .lock()
        .unwrap()
        .iter()
        .filter(|i| i["state"] == "OPEN" && i["author"]["login"] == "rustovate")
    {
        let body = issue["body"].as_str().unwrap();
        assert!(!body.contains("- [x]") && !body.contains("- [X]"), "{body}");
    }
}

#[tokio::test(start_paused = true)]
async fn dashboard_age_click_bypasses_only_the_selected_target_and_survives_unticking() {
    let fake = Arc::new(Fake::new(false));
    let now = chrono::Utc::now();
    *fake.catalog.lock().unwrap() = json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}},"time":{"2.0.0":now.to_rfc3339()}});
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let mut config = checkbox_config();
    config.defaults.wait_hours = 24;
    let first = checkbox_run(&engine, &config, now).await;
    assert!(first.actions.as_ref().unwrap().branches.is_empty());
    assert!(
        fake.issues.lock().unwrap()[0]["body"]
            .as_str()
            .unwrap()
            .contains("eligible at")
    );
    assert!(
        fake.issues.lock().unwrap()[0]["body"]
            .as_str()
            .unwrap()
            .contains("catalog fetched at")
    );
    fake.requests.lock().unwrap().clear();
    fake.writes.lock().unwrap().clear();
    checkbox_run(&engine, &config, now).await;
    assert!(fake.writes.lock().unwrap().is_empty());
    assert!(
        !fake
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, p, _)| p.ends_with("/pulls")),
        "empty steady repo must use inventory only"
    );
    tick_dashboard(&fake, "create");
    let created = checkbox_run(&engine, &config, now).await;
    assert_eq!(
        created.actions.as_ref().unwrap().branches[0].status,
        ActionStatus::Created
    );
    assert_unticked(&fake);
    drop(engine);
    // A successful manual age decision is carried in the bot PR, not just in receipts.
    let cache = Cache::open(
        directory.path().join("fresh-cache.sqlite"),
        Default::default(),
    )
    .await
    .unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let recovered = checkbox_run(&engine, &config, now).await;
    assert!(
        recovered.repositories[0].package_files[0].dependencies[0]
            .updates
            .iter()
            .any(|u| u.new_version.as_deref() == Some("2.0.0"))
    );
    assert_eq!(fake.prs.lock().unwrap().len(), 1);
    fake.writes.lock().unwrap().clear();
    let steady = checkbox_run(&engine, &config, now).await;
    assert_eq!(
        steady.actions.unwrap().branches[0].status,
        ActionStatus::Unchanged
    );
    assert!(fake.writes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn dashboard_create_click_bypasses_concurrent_hourly_and_schedule_holds() {
    for reason in ["concurrent", "hourly", "schedule"] {
        let mut fake = Fake::new(false);
        if reason != "schedule" {
            fake.files.insert(
                "package.json".into(),
                "{\"dependencies\":{\"fixture\":\"1.0.0\",\"other\":\"1.0.0\"}}".into(),
            );
        }
        let fake = Arc::new(fake);
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let mut config = checkbox_config();
        if reason == "concurrent" {
            config.prs.concurrent = Some(1);
        }
        if reason == "hourly" {
            config.prs.per_hour = Some(1);
        }
        if reason == "schedule" {
            config.defaults.windows = vec![rustovate_config::Window {
                cron: vec!["0 0 1 1 *".into()],
                zone: "UTC".into(),
            }];
        }
        let now = "2026-10-15T12:00:00Z".parse().unwrap();
        let first = checkbox_run(&engine, &config, now).await;
        assert!(
            first
                .actions
                .as_ref()
                .unwrap()
                .branches
                .iter()
                .any(|b| b.status == ActionStatus::Skipped),
            "{reason}: {first:?}"
        );
        tick_dashboard(&fake, "create");
        let next = checkbox_run(&engine, &config, now).await;
        assert!(
            next.actions
                .unwrap()
                .branches
                .iter()
                .all(|b| matches!(b.status, ActionStatus::Created | ActionStatus::Unchanged)),
            "{reason}"
        );
        assert_unticked(&fake);
    }
}

#[tokio::test(start_paused = true)]
async fn targeted_clicks_keep_unrelated_open_and_closed_receipts_fast() {
    for origin in ["recreate", "create", "pr", "retry"] {
        let mut request_counts = Vec::new();
        for dependencies in [3, 6] {
            let mut fake = Fake::new(false);
            let deps: BTreeMap<_, _> = (0..dependencies)
                .map(|i| (format!("fixture{i}"), "1.0.0"))
                .collect();
            fake.files.insert(
                "package.json".into(),
                json!({"dependencies":deps}).to_string(),
            );
            let fake = Arc::new(fake);
            let cache = Cache::open(":memory:", Default::default()).await.unwrap();
            let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
                .unwrap()
                .with_versions_host(false);
            let mut config = checkbox_config();
            if origin == "create" {
                config.prs.concurrent = Some(dependencies - 1);
            }
            let now = chrono::Utc::now();
            checkbox_run(&engine, &config, now).await;
            if origin == "recreate" {
                // Include another owner-closed receipt as well as untouched open PRs.
                for pr in fake.prs.lock().unwrap().iter_mut().take(2) {
                    pr["state"] = json!("closed");
                }
                checkbox_run(&engine, &config, now).await;
            }
            if origin == "pr" {
                let mut prs = fake.prs.lock().unwrap();
                prs[0]["body"] = json!(
                    prs[0]["body"]
                        .as_str()
                        .unwrap()
                        .replace("- [ ] Created", "- [x] Created")
                );
            } else if origin == "retry" {
                // An unknown/stale target is consumed without invalidating any write receipt.
                let mut issues = fake.issues.lock().unwrap();
                let body = issues[0]["body"].as_str().unwrap();
                issues[0]["body"] = json!(format!(
                    "{body}\n- [x] Retry <!-- rustovate-action:{{\"action\":\"retry\",\"target\":\"{}\"}} -->",
                    "a".repeat(64)
                ));
            } else {
                tick_dashboard(&fake, origin);
            }
            let untouched: Vec<_> = fake
                .prs
                .lock()
                .unwrap()
                .iter()
                .filter(|p| origin != "pr" || !p["body"].as_str().unwrap().contains("- [x]"))
                .map(|p| {
                    (
                        p["number"].as_u64().unwrap(),
                        p["head"]["ref"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            fake.requests.lock().unwrap().clear();
            let report = checkbox_run(&engine, &config, now).await;
            assert_unticked(&fake);
            let requests = fake.requests.lock().unwrap().clone();
            assert_eq!(
                requests
                    .iter()
                    .filter(|(_, _, body)| body["query"] == rustovate_update::INVENTORY_QUERY)
                    .count(),
                1
            );
            let branch_reads: Vec<_> = requests
                .iter()
                .filter(|(_, path, _)| path.contains("/git/ref/heads/") && !path.ends_with("/main"))
                .collect();
            assert_eq!(
                branch_reads.len(),
                usize::from(origin != "retry"),
                "{origin}: {requests:?}"
            );
            for (number, branch) in &untouched {
                if branch_reads
                    .iter()
                    .any(|(_, path, _)| path.ends_with(&branch.replace("/", "%2F")))
                {
                    continue;
                }
                assert!(
                    !requests
                        .iter()
                        .any(|(_, path, _)| path == &format!("/repos/demo/sandbox/pulls/{number}")),
                    "{origin}: untouched PR #{number} was fetched"
                );
                assert!(
                    report
                        .actions
                        .as_ref()
                        .unwrap()
                        .branches
                        .iter()
                        .any(|b| &b.branch == branch
                            && matches!(b.status, ActionStatus::Unchanged | ActionStatus::Skipped)),
                    "{origin}: {report:?}"
                );
            }
            // Source lookups scale with dependencies; branch verification must not.
            let write_requests = requests
                .iter()
                .filter(|(_, path, _)| !path.starts_with("/fixture"))
                .count();
            eprintln!(
                "targeted {origin}, dependencies={dependencies}: {write_requests} repository requests"
            );
            request_counts.push(write_requests);
            fake.writes.lock().unwrap().clear();
            checkbox_run(&engine, &config, now).await;
            assert!(
                fake.writes.lock().unwrap().is_empty(),
                "{origin}: next run must converge"
            );
        }
        assert_eq!(
            request_counts[0], request_counts[1],
            "{origin}: unrelated branches added requests"
        );
        assert_eq!(request_counts[0], if origin == "retry" { 4 } else { 16 });
    }
}

#[tokio::test(start_paused = true)]
async fn dashboard_recreate_click_opens_a_new_pr_and_does_not_repeat_the_old_owner_hold() {
    let fake = Arc::new(Fake::new(false));
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let config = checkbox_config();
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    fake.prs.lock().unwrap()[0]["state"] = json!("closed");
    checkbox_run(&engine, &config, now).await;
    tick_dashboard(&fake, "recreate");
    checkbox_run(&engine, &config, now).await;
    assert_eq!(fake.prs.lock().unwrap().len(), 2);
    assert_eq!(fake.prs.lock().unwrap()[0]["state"], "closed");
    assert_unticked(&fake);
    let next = checkbox_run(&engine, &config, now).await;
    assert_eq!(
        next.actions.unwrap().branches[0].status,
        ActionStatus::Unchanged
    );
}

#[tokio::test(start_paused = true)]
async fn repository_and_pr_recheck_clicks_force_branch_work_and_clear_the_checkboxes() {
    for origin in ["dashboard", "pr"] {
        let fake = Arc::new(Fake::new(false));
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let config = checkbox_config();
        let now = chrono::Utc::now();
        checkbox_run(&engine, &config, now).await;
        if origin == "dashboard" {
            tick_dashboard(&fake, "recheck");
        } else {
            let mut prs = fake.prs.lock().unwrap();
            prs[0]["body"] = json!(
                prs[0]["body"]
                    .as_str()
                    .unwrap()
                    .replace("- [ ] Created", "- [x] Created")
            );
        }
        fake.writes.lock().unwrap().clear();
        checkbox_run(&engine, &config, now).await;
        assert!(
            fake.writes
                .lock()
                .unwrap()
                .iter()
                .any(|(p, _)| p.ends_with("/git/commits")),
            "{origin}"
        );
        assert!(
            !fake.prs.lock().unwrap()[0]["body"]
                .as_str()
                .unwrap()
                .contains("- [x]")
        );
        assert_unticked(&fake);
        fake.writes.lock().unwrap().clear();
        checkbox_run(&engine, &config, now).await;
        assert!(fake.writes.lock().unwrap().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn failed_lookup_click_retries_the_source_and_unticks_even_when_it_still_fails() {
    let fake = Arc::new(Fake::new(false));
    fake.fail_lookup
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let config = checkbox_config();
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    tick_dashboard(&fake, "retry");
    fake.requests.lock().unwrap().clear();
    let report = checkbox_run(&engine, &config, now).await;
    assert!(!report.repositories[0].warnings.is_empty());
    assert_eq!(
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p, _)| p == "/fixture")
            .count(),
        1
    );
    assert_unticked(&fake);
}

#[tokio::test(start_paused = true)]
async fn human_authored_dashboard_checkboxes_never_request_branch_work() {
    let fake = Arc::new(Fake::new(false));
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let config = checkbox_config();
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    tick_dashboard(&fake, "recheck");
    fake.issues.lock().unwrap()[0]["author"] = json!({"__typename":"User","login":"human"});
    fake.writes.lock().unwrap().clear();
    checkbox_run(&engine, &config, now).await;
    assert!(
        !fake
            .writes
            .lock()
            .unwrap()
            .iter()
            .any(|(p, _)| p.ends_with("/git/commits"))
    );
    assert!(
        fake.issues.lock().unwrap()[0]["body"]
            .as_str()
            .unwrap()
            .contains("- [X]")
    );
    assert_eq!(fake.issues.lock().unwrap().len(), 2);
}
impl rustovate_update::lockfiles::Executor for FailTools {
    fn run(
        &self,
        _: rustovate_update::lockfiles::CommandSpec,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, String>> + Send + '_>>
    {
        Box::pin(async { Err("fixture tool unavailable".into()) })
    }
}
#[tokio::test(start_paused = true)]
async fn lockfile_failure_is_a_branch_error_and_publishes_no_manifest_only_branch() {
    let mut fake = Fake::new(true);
    fake.lock_name = "bun.lock";
    let fake = Arc::new(fake);
    let temp = tempfile::tempdir().unwrap();
    let runner =
        rustovate_update::lockfiles::Runner::new(temp.path().into(), "offline-installation")
            .with_executor(Arc::new(FailTools));
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false)
        .with_lockfile_runner(runner);
    let report = engine
        .run(
            Options {
                repositories: vec!["demo/sandbox".into()],
                ..Default::default()
            },
            false,
            false,
        )
        .await
        .unwrap();
    assert_eq!(report.repositories[0].status, Status::Error, "{report:?}");
    let branch = &report.actions.unwrap().branches[0];
    assert_eq!(branch.status, ActionStatus::Error);
    assert!(
        branch
            .reason
            .as_ref()
            .unwrap()
            .contains("fixture tool unavailable")
    );
    assert!(
        fake.writes
            .lock()
            .unwrap()
            .iter()
            .all(|(p, _)| p.contains("/issues"))
    );
}
