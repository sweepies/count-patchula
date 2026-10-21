//! CLI dispatch and persistent cache regression for installation-token receipts.
use super::{App, Services, run};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use patchula_cache::Cache;
use std::ffi::OsStr;

use patchula_http::{Exchange, Response, Transport};

use patchula_update::blob_sha;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

struct Fake {
    files: BTreeMap<String, String>,
    writes: Mutex<Vec<(String, Value)>>,
    prs: Mutex<Vec<Value>>,
    issues: Mutex<Vec<Value>>,
    requests: Mutex<Vec<(String, String, Value)>>,
}
impl Fake {
    fn graphql_author(pr: &Value) -> Value {
        let login = pr["user"]["login"].as_str().unwrap();
        if let Some(bot) = login.strip_suffix("[bot]") {
            json!({"__typename":"Bot", "login":bot})
        } else {
            json!({"__typename":"User", "login":login})
        }
    }
    fn new() -> Self {
        Self {
        files:BTreeMap::from([
            (".github/patchula.pkl".into(),"amends \"../Patchula.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into()),
            ("package.json".into(),"{\n  \"dependencies\": {\"fixture\": \"1.0.0\", \"owner-closed-a\": \"1.0.0\", \"owner-closed-b\": \"1.0.0\"}\n}\n".into()),
        ]),writes:Default::default(),prs:Default::default(),issues:Default::default(),requests:Default::default(),
    }
    }
    fn tree(&self) -> Value {
        let tree: Vec<_> = self
            .files
            .iter()
            .map(|(p, c)| json!({"path":p,"sha":blob_sha(c),"mode":"100644","type":"blob"}))
            .collect();
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
            if body["query"] == patchula_engine::REPOSITORY_HEAD_QUERY {
                (
                    200,
                    json!({"data":{"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base","tree":{"oid":"base-tree"}}}}}}),
                )
            } else if body["query"] == "query{viewer{login}}" {
                (200, json!({"data":{"viewer":{"login":"patchula[bot]"}}}))
            } else if body["query"] == patchula_update::INVENTORY_QUERY {
                let prs = self.prs.lock().unwrap();
                let nodes: Vec<_> = prs.iter().filter(|p|p["state"] == "open").map(|p|json!({
                    "name":p["head"]["ref"],"target":{"oid":p["head"]["sha"],"tree":{"oid":"updated-tree"},"signature":{"isValid":true}},
                    "associatedPullRequests":{"pageInfo":{"hasNextPage":false},"nodes":[{
                        "number":p["number"],"id":p["node_id"],"url":p["html_url"],"title":p["title"],"body":p["body"],
                        "headRefName":p["head"]["ref"],"headRefOid":p["head"]["sha"],"baseRefName":p["base"]["ref"],"author":Self::graphql_author(p),
                        "headRepository":{"nameWithOwner":"demo/sandbox"},"baseRepository":{"nameWithOwner":"demo/sandbox"},
                        "labels":{"nodes":p["labels"],"pageInfo":{"hasNextPage":false}},"autoMergeRequest":p["auto_merge"]
                    }]}
                })).collect();
                let closed: Vec<_> = prs.iter().filter(|p|body["variables"]["closed"].as_array().unwrap().contains(&p["node_id"])).map(|p|json!({
                    "id":p["node_id"],"state":if p["state"] == "closed" {"CLOSED"} else {"OPEN"},"merged":false,"body":p["body"],
                    "author":Self::graphql_author(p),"headRepository":{"nameWithOwner":"demo/sandbox"},"baseRepository":{"nameWithOwner":"demo/sandbox"}
                })).collect();
                (
                    200,
                    json!({"data":{"viewer":{"login":"patchula[bot]"},"nodes":closed,"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base"}},"refs":{"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}},"issues":{"nodes":*self.issues.lock().unwrap(),"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
                )
            } else {
                assert_eq!(body["query"], patchula_update::REF_UPDATE);
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
                let pr = json!({"number":10,"node_id":"pr-id","html_url":"https://github.com/demo/sandbox/pull/10","title":body["title"],"body":body["body"],"state":"open","user":{"login":"patchula[bot]"},"head":{"ref":body["head"],"sha":"new-commit","repo":{"full_name":"demo/sandbox"}},"base":{"ref":"main","repo":{"full_name":"demo/sandbox"}},"labels":[{"name":"dependencies"}],"auto_merge":null,"created_at":chrono::Utc::now().to_rfc3339(),"merged_at":null});
                let mut pr = pr;
                pr["head"]["label"] = json!("demo:branch");
                pr["head"]["repo"]["updated_at"] = json!("2026-10-15T00:00:00Z");
                pr["base"]["sha"] = json!("base");
                self.prs.lock().unwrap().push(pr.clone());
                (201, pr)
            } else if path.ends_with("/issues") {
                let issue = json!({"number":100,"id":"dashboard-id","title":body["title"],"body":body["body"],"state":"OPEN","author":{"__typename":"Bot","login":"patchula"}});
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
        } else if path == "/repos/demo/sandbox/pulls/10" {
            (
                200,
                self.prs
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|p| p["number"] == 10)
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
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
            })
        })
    }
}

fn existing_pr(package: &str, number: u64, closed: bool) -> Value {
    use sha2::{Digest, Sha256};
    // Same target tuple encoded by update_key; these historical PRs existed
    // before this process/cache. The identity is deliberately unrelated.
    let targets = json!([["npm", package, "2.0.0", "2.0.0", null]]);
    let update = Sha256::digest(serde_json::to_vec(&targets).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let marker = json!({"version":1,"identity":"0".repeat(64),"update":update});
    json!({
        "number":number,"node_id":format!("closed-{number}"),
        "state":if closed {"closed"} else {"open"},"merged_at":null,
        "body":format!("<!-- patchula:{marker} -->"),
        "title":"Existing PR", "user":{"login":"patchula[bot]"},
        "head":{"ref":format!("patchula/{package}-2.x"),"sha":"old-commit","repo":{"full_name":"demo/sandbox"}},
        "base":{"ref":"main","repo":{"full_name":"demo/sandbox"}},
        "labels":[],"auto_merge":null
    })
}

#[derive(Clone)]
struct LogWriter(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for LogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn cli_run_reopens_disk_receipts_with_bot_authors_and_mixed_pr_inventory() {
    use tracing_subscriber::{filter::Targets, prelude::*};
    let logs = LogWriter(Default::default());
    let writer = logs.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .with_filter(Targets::new().with_target("patchula", tracing::Level::DEBUG)),
    );
    let _guard = tracing::subscriber::set_default(subscriber);
    let fake = Arc::new(Fake::new());
    let mut foreign = existing_pr("foreign", 3, false);
    foreign["head"]["ref"] = json!("liv/safe-deps");
    foreign["user"]["login"] = json!("liv");
    foreign["body"] = json!("unrelated private PR text");
    *fake.prs.lock().unwrap() = vec![
        existing_pr("owner-closed-a", 1, true),
        existing_pr("owner-closed-b", 2, true),
        foreign,
    ];
    let directory = tempfile::tempdir().unwrap();
    let cache_path = directory.path().join("nested/cache.sqlite");
    let args: Vec<_> = [
        OsStr::new("run"),
        OsStr::new("-vv"),
        OsStr::new("--json"),
        OsStr::new("--cache"),
        cache_path.as_os_str(),
        OsStr::new("--recheck"),
        OsStr::new("next-run"),
        OsStr::new("demo/sandbox"),
    ]
    .into();
    let app = App::parse_from(&args).unwrap();
    assert_eq!(app.verbose, 2);
    for second in [false, true] {
        fake.requests.lock().unwrap().clear();
        fake.writes.lock().unwrap().clear();
        // Exactly the main CLI dispatch: only network I/O and token lookup are
        // substituted. batch opens/closes a fresh disk cache and engine each run.
        let services = Services {
            token: Some("offline-installation".into()),
            transport: Some(fake.clone()),
        };
        assert_eq!(run(&app, &services).await.unwrap(), 0);
        let cache = Cache::open(&cache_path, Default::default()).await.unwrap();
        let bytes = cache
            .extraction_get(&patchula_cache::ExtractionKey {
                repo: "demo/sandbox".into(),
                default_branch_commit: "patchula/".into(),
                config_hash: "verified-write-receipts-v3".into(),
            })
            .await
            .unwrap()
            .expect("CLI must persist receipts before returning");
        let receipts: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(receipts["writes"].as_object().unwrap().len(), 1);
        assert_eq!(receipts["closed"].as_object().unwrap().len(), 2);
        drop(cache);
        if second {
            let requests = fake.requests.lock().unwrap();
            let inventory = requests
                .iter()
                .position(|(_, _, b)| b["query"] == patchula_update::INVENTORY_QUERY)
                .unwrap();
            assert_eq!(requests.len(), 6, "{requests:?}");
            assert!(inventory > 0, "inventory follows repository discovery");
            assert!(
                requests.iter().all(|(method, _, body)| method == "GET"
                    || body["query"] == patchula_update::INVENTORY_QUERY),
                "only the inventory query may use POST: {requests:?}"
            );
            assert!(fake.writes.lock().unwrap().is_empty());
        }
    }
    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("reason=\"missing-receipt\""), "{logs}");
    assert!(
        logs.contains("write receipts saved") && logs.contains("count=3 writes=1 closed=2"),
        "{logs}"
    );
    assert!(logs.contains("restored=3 complete=true"), "{logs}");
    assert!(!logs.contains("offline-installation"));
    assert!(!logs.contains("unrelated private PR text"));
}
