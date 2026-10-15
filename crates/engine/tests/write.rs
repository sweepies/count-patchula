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
    requests: Mutex<Vec<(String, String, Value)>>,
    latency: std::time::Duration,
    close_on_read: std::sync::atomic::AtomicBool,
}
impl Fake {
    fn new(lock: bool) -> Self {
        Self {
        files:BTreeMap::from([
            (".github/rustovate.pkl".into(),"amends \"../Rustovate.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into()),
            ("package.json".into(),"{\n  \"dependencies\": {\"fixture\": \"1.0.0\"}\n}\n".into()),
        ]),lock,lock_name:"yarn.lock",writes:Default::default(),prs:Default::default(),requests:Default::default(),latency:Default::default(),close_on_read:Default::default(),
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
                200,
                json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}}}),
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
                    json!({"data":{"viewer":{"login":"rustovate[bot]"},"nodes":closed,"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base"}},"refs":{"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
                )
            } else {
                assert_eq!(body["query"], rustovate_update::REF_UPDATE);
                self.writes.lock().unwrap().push((path.into(), body));
                (
                    200,
                    json!({"data":{"updateRefs":{"clientMutationId":null}}}),
                )
            }
        } else if request.method() == reqwest::Method::POST {
            self.writes
                .lock()
                .unwrap()
                .push((path.into(), body.clone()));
            if path.ends_with("/pulls") {
                let pr = json!({"number":1,"node_id":"pr-id","html_url":"https://github.com/demo/sandbox/pull/1","title":body["title"],"body":body["body"],"state":"open","user":{"login":"rustovate[bot]"},"head":{"ref":body["head"],"sha":"new-commit","repo":{"full_name":"demo/sandbox"}},"base":{"ref":"main","repo":{"full_name":"demo/sandbox"}},"labels":[{"name":"dependencies"}],"auto_merge":null,"created_at":chrono::Utc::now().to_rfc3339(),"merged_at":null});
                let mut pr = pr;
                pr["head"]["label"] = json!("demo:branch");
                pr["head"]["repo"]["updated_at"] = json!("2026-10-15T00:00:00Z");
                pr["base"]["sha"] = json!("base");
                self.prs.lock().unwrap().push(pr.clone());
                (201, pr)
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
        } else if path == "/repos/demo/sandbox/pulls/1" {
            if self
                .close_on_read
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                self.prs.lock().unwrap()[0]["state"] = json!("closed");
            }
            (200, self.prs.lock().unwrap()[0].clone())
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
            if dry_run || (lock && !allow) {
                assert!(fake.writes.lock().unwrap().is_empty());
            }
            if repeat == 1 {
                assert_eq!(report.stats.as_ref().unwrap().extraction_hits, 1);
            }
        }
    }
}

struct FailTools;
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
    assert!(fake.writes.lock().unwrap().is_empty());
}
