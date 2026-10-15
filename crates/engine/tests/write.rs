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
}
impl Fake {
    fn new(lock: bool) -> Self {
        Self {
        files:BTreeMap::from([
            (".github/rustovate.pkl".into(),"amends \"../Rustovate.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into()),
            ("package.json".into(),"{\n  \"dependencies\": {\"fixture\": \"1.0.0\"}\n}\n".into()),
        ]),lock,lock_name:"yarn.lock",writes:Default::default(),prs:Default::default(),issues:Default::default(),requests:Default::default(),latency:Default::default(),close_on_read:Default::default(),catalog:Mutex::new(json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}}})),fail_lookup:Default::default(),
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
                (
                    200,
                    json!({"data":{"viewer":{"login":"rustovate[bot]"},"nodes":closed,"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base"}},"refs":{"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}},"issues":{"nodes":self.issues.lock().unwrap().iter().filter(|i|i["state"] == "OPEN").cloned().collect::<Vec<_>>(),"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
                )
            } else {
                assert_eq!(body["query"], rustovate_update::REF_UPDATE);
                self.writes.lock().unwrap().push((path.into(), body));
                (
                    200,
                    json!({"data":{"updateRefs":{"clientMutationId":null}}}),
                )
            }
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
        let engine =
            Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
        let engine =
            Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
        let engine =
            Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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

fn checkbox_config() -> rustovate_config::Config {
    let mut config = rustovate_config::load(std::path::Path::new("../../Rustovate.pkl")).unwrap();
    config.prs.dashboard = true;
    config.defaults.wait_hours = 0;
    config
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
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
        let engine =
            Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
async fn dashboard_recreate_click_opens_a_new_pr_and_does_not_repeat_the_old_owner_hold() {
    let fake = Arc::new(Fake::new(false));
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
        let engine =
            Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
        2
    );
    assert_unticked(&fake);
}

#[tokio::test(start_paused = true)]
async fn human_authored_dashboard_checkboxes_never_request_branch_work() {
    let fake = Arc::new(Fake::new(false));
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone()).unwrap();
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
