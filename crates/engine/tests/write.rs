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
    writes: Mutex<Vec<(String, Value)>>,
}
impl Fake {
    fn new(lock: bool) -> Self {
        Self {
        files:BTreeMap::from([
            (".github/rustovate.pkl".into(),"amends \"../Rustovate.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into()),
            ("package.json".into(),"{\n  \"dependencies\": {\"fixture\": \"1.0.0\"}\n}\n".into()),
        ]),lock,writes:Default::default(),
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
                json!({"path":"bun.lock","sha":"unselected-lock","type":"blob","mode":"100644"}),
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
            if path.ends_with("/git/blobs") {
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
#[tokio::test]
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
