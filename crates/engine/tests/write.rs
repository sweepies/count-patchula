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
}
impl Fake {
    fn new(lock: bool) -> Self {
        Self {
        files:BTreeMap::from([
            (".github/rustovate.pkl".into(),"amends \"../Rustovate.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into()),
            ("package.json".into(),"{\n  \"dependencies\": {\"fixture\": \"1.0.0\"}\n}\n".into()),
        ]),lock,lock_name:"yarn.lock",writes:Default::default(),prs:Default::default(),
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
                (
                    200,
                    json!({"data":{"viewer":{"login":"rustovate[bot]"},"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base"}},"refs":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
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
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
            })
        })
    }
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
                    ActionStatus::Created
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
