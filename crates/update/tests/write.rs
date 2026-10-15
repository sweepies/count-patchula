//! Stateful synthetic GitHub: no sockets, tokens or network client.
use rustovate_cache::Cache;
use rustovate_config::{Config, Manager, Rebase};
use rustovate_github::Client;
use rustovate_http::{Exchange, Response, Transport};
use rustovate_manager::{Files, Scanner, word};
use rustovate_report::{
    ActionStatus, Current, Dependency as ReportDependency, PackageFile, Repository, Status, Update,
};
use rustovate_update::{Blob, Plan, Snapshot, apply, blob_sha, branch_name, plan};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct State {
    base: String,
    requests: Vec<(String, String)>,
    prs: BTreeMap<u64, Value>,
    pr_create_error: bool,
    pr_page_size: Option<usize>,
    ref_page_size: Option<usize>,
    latency: std::time::Duration,
    refs: BTreeMap<String, String>,
    trees: BTreeMap<String, Value>,
    commits: BTreeMap<String, Value>,
    writes: Vec<(String, Value)>,
    unsigned: bool,
    race: bool,
    truncated: bool,
    history: Vec<String>,
    merged_heads: BTreeSet<String>,
    pr_lag_reads: usize,
    pending_heads: BTreeMap<u64, Value>,
    pr_head_override: Option<String>,
    close_on_read: bool,
    close_after_reads: Option<usize>,
}
struct Fake(Mutex<State>);
fn response(status: u16, body: Value) -> Response {
    Response {
        status: reqwest::StatusCode::from_u16(status).unwrap(),
        headers: Default::default(),
        body: serde_json::to_vec(&body).unwrap(),
    }
}
impl Transport for Fake {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        assert!(request.headers()["authorization"].is_sensitive());
        let path = request.url().path().to_owned();
        let body: Value = request
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        let mut state = self.0.lock().unwrap();
        let latency = state.latency;
        state
            .requests
            .push((request.method().to_string(), path.clone()));
        let reply = if path == "/graphql" {
            if body["query"] == rustovate_update::INVENTORY_QUERY {
                let nodes: Vec<_> = state.refs.iter().map(|(name, oid)| {
                    let name = name.strip_prefix("refs/heads/").unwrap();
                    let commit = &state.commits[oid];
                    let prs: Vec<_> = state.prs.values().filter(|p|p["state"] == "open" && p["head"]["ref"] == name).map(|p|json!({
                        "number":p["number"],"id":p["node_id"],"url":p["html_url"],"title":p["title"],"body":p["body"],
                        "headRefName":p["head"]["ref"],"headRefOid":p["head"]["sha"],"baseRefName":p["base"]["ref"],"author":p["user"],
                        "headRepository":{"nameWithOwner":p["head"]["repo"]["full_name"]},"baseRepository":{"nameWithOwner":p["base"]["repo"]["full_name"]},
                        "labels":{"nodes":p["labels"],"pageInfo":{"hasNextPage":false}},"autoMergeRequest":p["auto_merge"]
                    })).collect();
                    json!({"name":name,"target":{"oid":oid,"tree":{"oid":commit["tree"]["sha"]},"message":commit["message"],
                        "author":{"user":{"login":commit.get("actor").and_then(Value::as_str).unwrap_or("rustovate[bot]")}},
                        "committer":{"user":{"login":"web-flow"}},"signature":{"isValid":true}},
                        "associatedPullRequests":{"nodes":prs,"pageInfo":{"hasNextPage":false}}})
                }).collect();
                let offset = body["variables"]["cursor"]
                    .as_str()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                let size = state.ref_page_size.unwrap_or(100);
                let more = offset + size < nodes.len();
                let nodes: Vec<_> = nodes.into_iter().skip(offset).take(size).collect();
                response(
                    200,
                    json!({"data":{"viewer":{"login":"rustovate[bot]"},"nodes":state.prs.values().filter(|p|body["variables"]["closed"].as_array().unwrap().contains(&p["node_id"])).map(|p|json!({
                        "id":p["node_id"],"state":if p["state"] == "open" {"OPEN"} else {"CLOSED"},"merged":!p["merged_at"].is_null(),
                        "body":p["body"],"author":p["user"],"headRepository":{"nameWithOwner":p["head"]["repo"]["full_name"]},"baseRepository":{"nameWithOwner":p["base"]["repo"]["full_name"]}
                    })).collect::<Vec<_>>(),"repository":{
                        "defaultBranchRef":{"name":"main","target":{"oid":state.base}},
                        "refs":{"nodes":nodes,"pageInfo":{"hasNextPage":more,"endCursor":(offset+size).to_string()}}
                    }}}),
                )
            } else if body["query"] == "query{viewer{login}}" {
                response(200, json!({"data":{"viewer":{"login":"rustovate[bot]"}}}))
            } else if body["query"]
                .as_str()
                .is_some_and(|q| q.contains("PullRequestAutoMerge"))
            {
                state.writes.push((path.clone(), body.clone()));
                let enabled = body["query"]
                    .as_str()
                    .unwrap()
                    .contains("enablePullRequestAutoMerge");
                for pr in state.prs.values_mut() {
                    pr["auto_merge"] = if enabled {
                        json!({"enabled":true})
                    } else {
                        Value::Null
                    };
                }
                response(
                    200,
                    json!({"data":if enabled { json!({"enablePullRequestAutoMerge":{"pullRequest":{"id":"pr-id"}}}) } else {json!({"disablePullRequestAutoMerge":{"pullRequest":{"id":"pr-id"}}})}}),
                )
            } else {
                assert_eq!(body["query"], rustovate_update::REF_UPDATE);
                state.writes.push((path.clone(), body.clone()));
                let input = &body["variables"]["input"];
                assert_eq!(input["repositoryId"], "repo-id");
                let updates = input["refUpdates"].as_array().unwrap();
                let zero = "0000000000000000000000000000000000000000";
                let valid = !state.race
                    && updates.iter().all(|r| {
                        state
                            .refs
                            .get(r["name"].as_str().unwrap())
                            .map(String::as_str)
                            .unwrap_or(zero)
                            == r["beforeOid"].as_str().unwrap()
                    });
                if valid {
                    for r in updates {
                        state.refs.insert(
                            r["name"].as_str().unwrap().into(),
                            r["afterOid"].as_str().unwrap().into(),
                        );
                    }
                    let lag = state.pr_lag_reads > 0;
                    let mut pending = BTreeMap::new();
                    for pr in state.prs.values_mut() {
                        for r in updates {
                            if r["name"]
                                == format!("refs/heads/{}", pr["head"]["ref"].as_str().unwrap())
                            {
                                if lag {
                                    pending.insert(
                                        pr["number"].as_u64().unwrap(),
                                        r["afterOid"].clone(),
                                    );
                                } else {
                                    pr["head"]["sha"] = r["afterOid"].clone();
                                }
                            }
                        }
                    }
                    state.pending_heads.extend(pending);
                    response(
                        200,
                        json!({"data":{"updateRefs":{"clientMutationId":null}}}),
                    )
                } else {
                    response(200, json!({"errors":[{"message":"reference changed"}]}))
                }
            }
        } else if request.method() == reqwest::Method::POST {
            state.writes.push((path.clone(), body.clone()));
            if path == "/repos/demo/app/pulls" {
                let number = state.prs.len() as u64 + 1;
                let sha =
                    state.refs[&format!("refs/heads/{}", body["head"].as_str().unwrap())].clone();
                let pr = json!({"number":number,"node_id":format!("pr-{number}"),"html_url":format!("https://github.com/demo/app/pull/{number}"),"state":"open","title":body["title"],"body":body["body"],"head":{"ref":body["head"],"sha":sha,"repo":{"full_name":"demo/app"}},"base":{"ref":body["base"],"repo":{"full_name":"demo/app"}},"user":{"login":"rustovate[bot]"},"labels":[],"auto_merge":null,"merged_at":null,"created_at":chrono::Utc::now().to_rfc3339()});
                let mut pr = pr;
                pr["head"]["label"] = json!("demo:branch");
                pr["head"]["repo"]["updated_at"] = json!("2026-10-15T00:00:00Z");
                pr["base"]["sha"] = json!(state.base);
                pr["base"]["repo"]["size"] = json!(42);
                state.prs.insert(number, pr.clone());
                response(
                    if std::mem::take(&mut state.pr_create_error) {
                        500
                    } else {
                        201
                    },
                    pr,
                )
            } else if path.ends_with("/git/blobs") {
                assert_eq!(body["encoding"], "utf-8");
                response(
                    201,
                    json!({"sha":blob_sha(body["content"].as_str().unwrap())}),
                )
            } else if path.ends_with("/git/trees") {
                let mut tree = state.trees[body["base_tree"].as_str().unwrap()].clone();
                for original in body["tree"].as_array().unwrap() {
                    let mut entry = original.clone();
                    if let Some(content) = entry["content"].as_str() {
                        entry["sha"] = json!(blob_sha(content));
                        entry.as_object_mut().unwrap().remove("content");
                    }
                    tree.as_array_mut()
                        .unwrap()
                        .retain(|e| e["path"] != entry["path"]);
                    if !(entry.get("sha").is_some_and(Value::is_null)
                        && entry.get("content").is_none())
                    {
                        tree.as_array_mut().unwrap().push(entry.clone());
                    }
                }
                let sha = format!("tree-{}", state.trees.len());
                state.trees.insert(sha.clone(), tree);
                response(201, json!({"sha":sha}))
            } else if path.ends_with("/git/commits") {
                assert!(
                    body.get("author").is_none()
                        && body.get("committer").is_none()
                        && body.get("signature").is_none()
                );
                assert_eq!(body["parents"].as_array().unwrap().len(), 1);
                assert_eq!(body["parents"][0], state.base);
                let sha = format!("commit-{}", state.commits.len());
                let commit = json!({"sha":sha,"tree":{"sha":body["tree"]},"message":body["message"],"parents":[{"sha":body["parents"][0]}],"verification":{"verified":!state.unsigned}});
                state.commits.insert(sha.clone(), commit.clone());
                response(201, commit)
            } else {
                panic!("unexpected write {path}");
            }
        } else if request.method() == reqwest::Method::PATCH
            && path.starts_with("/repos/demo/app/pulls/")
        {
            state.writes.push((path.clone(), body.clone()));
            let n: u64 = path.rsplit('/').next().unwrap().parse().unwrap();
            let pr = state.prs.get_mut(&n).unwrap();
            for (key, value) in body.as_object().unwrap() {
                if key == "base" {
                    pr["base"]["ref"] = value.clone();
                } else {
                    pr[key] = value.clone();
                }
            }
            response(200, pr.clone())
        } else if request.method() == reqwest::Method::PUT && path.ends_with("/labels") {
            state.writes.push((path.clone(), body.clone()));
            let n: u64 = path.split('/').nth(5).unwrap().parse().unwrap();
            let labels: Vec<_> = body["labels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| json!({"name":l}))
                .collect();
            state.prs.get_mut(&n).unwrap()["labels"] = json!(labels);
            response(200, json!(labels))
        } else if path == "/repos/demo/app/pulls" {
            let page = request
                .url()
                .query_pairs()
                .find(|(k, _)| k == "page")
                .and_then(|(_, p)| p.parse::<usize>().ok())
                .unwrap_or(1);
            let size = state.pr_page_size.unwrap_or(100);
            let prs: Vec<_> = state
                .prs
                .values()
                .rev()
                .skip((page - 1) * size)
                .take(size)
                .cloned()
                .collect();
            let mut reply = response(200, json!(prs));
            if page * size < state.prs.len() {
                reply.headers.insert("link", format!("<https://api.github.com/repos/demo/app/pulls?state=all&page={}>; rel=\"next\"",page+1).parse().unwrap());
            }
            reply
        } else if let Some(n) = path.strip_prefix("/repos/demo/app/pulls/") {
            let n = n.parse::<u64>().unwrap();
            if state.pr_lag_reads > 0 {
                state.pr_lag_reads -= 1;
            } else if let Some(head) = state.pending_heads.remove(&n) {
                state.prs.get_mut(&n).unwrap()["head"]["sha"] = head;
            }
            if let Some(head) = state.pr_head_override.clone() {
                state.prs.get_mut(&n).unwrap()["head"]["sha"] = json!(head);
            }
            if state.close_on_read {
                state.prs.get_mut(&n).unwrap()["state"] = json!("closed");
            }
            if let Some(remaining) = &mut state.close_after_reads {
                if *remaining == 0 {
                    state.prs.get_mut(&n).unwrap()["state"] = json!("closed");
                } else {
                    *remaining -= 1;
                }
            }
            response(200, state.prs[&n].clone())
        } else if path.starts_with("/repos/actions/checkout/releases/tags/") {
            let mut reply = if request.headers().contains_key("if-none-match") {
                response(304, Value::Null)
            } else {
                response(200, json!({"body":"Release details 🦀\n".repeat(10_000)}))
            };
            reply
                .headers
                .insert("etag", "\"release-one\"".parse().unwrap());
            reply
        } else if path == "/repos/demo/app/branches/main" {
            response(200, json!({"commit":{"sha":state.base}}))
        } else if path == "/repos/demo/app/branches" {
            response(200, json!(state.refs.iter().map(|(n, sha)| json!({"name":n.strip_prefix("refs/heads/").unwrap(),"commit":{"sha":sha}})).collect::<Vec<_>>()))
        } else if let Some(sha) = path.strip_prefix("/repos/demo/app/commits/") {
            response(200, json!({"commit":state.commits[sha]}))
        } else if path == "/repos/demo/app" {
            response(
                200,
                json!({"node_id":"repo-id","default_branch":"main","allow_auto_merge":true,"allow_squash_merge":true}),
            )
        } else if let Some(branch) = path.strip_prefix("/repos/demo/app/git/ref/heads/") {
            // url::Url keeps percent-encoded slashes in a path segment.
            let branch = branch.replace("%2F", "/");
            match state.refs.get(&format!("refs/heads/{branch}")) {
                Some(sha) => response(200, json!({"object":{"sha":sha}})),
                None => response(404, json!({})),
            }
        } else if let Some(sha) = path.strip_prefix("/repos/demo/app/git/commits/") {
            response(200, state.commits[sha].clone())
        } else if let Some(sha) = path.strip_prefix("/repos/demo/app/git/trees/") {
            response(
                200,
                json!({"tree":state.trees[sha],"truncated":state.truncated}),
            )
        } else if let Some(spec) = path.strip_prefix("/repos/demo/app/compare/") {
            let sha = spec.split("...").nth(1).unwrap();
            let history: Vec<_> = state
                .history
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(sha))
                .collect();
            let commits: Vec<_> = if spec.starts_with(&format!("{}...", state.base))
                && state.merged_heads.contains(sha)
            {
                Vec::new()
            } else {
                history.iter().map(|sha| {
                let commit = &state.commits[*sha];
                json!({"sha":sha,"author":{"login":commit.get("actor").and_then(Value::as_str).unwrap_or("rustovate[bot]")},"committer":{"login":commit.get("committer_login").and_then(Value::as_str).unwrap_or("web-flow")},"commit":commit})
            }).collect()
            };
            response(
                200,
                json!({"total_commits":commits.len(),"commits":commits}),
            )
        } else {
            panic!("unexpected read {path}");
        };
        Box::pin(async move {
            if !latency.is_zero() {
                tokio::time::sleep(latency).await;
            }
            Ok(reply)
        })
    }
}
fn setup(
    target: &str,
    lock: bool,
) -> (
    Config,
    Repository,
    Vec<rustovate_manager::Dependency>,
    Snapshot,
) {
    let mut config = rustovate_config::load_files(
        "/repo/rustovate.pkl",
        BTreeMap::from([(
            "/repo/rustovate.pkl".into(),
            "amends \"Rustovate.pkl\"".into(),
        )]),
    )
    .unwrap();
    config.managers = vec![Manager::Mise];
    let text = "# tools\n[tools]\nnode = '20.0.0' # keep\n";
    let deps = Scanner::new(&config)
        .unwrap()
        .extract(&Files::from([("mise.toml".into(), text.into())]))
        .unwrap()
        .dependencies;
    let d = &deps[0];
    let snapshot = Snapshot {
        paths: BTreeSet::from(["mise.toml".into()]),
        files: BTreeMap::from([(
            "mise.toml".into(),
            Blob {
                sha: blob_sha(text),
                mode: "100644".into(),
                content: text.into(),
            },
        )]),
    };
    let report = Repository {
        repository: "demo/app".into(),
        status: Status::Complete,
        reason: None,
        warnings: vec![],
        default_branch: Some("main".into()),
        commit: Some("base".into()),
        package_files: vec![PackageFile {
            path: "mise.toml".into(),
            dependencies: vec![ReportDependency {
                manager: word(&d.manager),
                name: d.name.clone(),
                occurrence: None,
                package: d.package.clone(),
                current: Current {
                    value: Some(d.current.clone()),
                    version: d.resolved.clone(),
                    digest: None,
                },
                metadata: None,
                updates: vec![Update {
                    new_value: Some(target.into()),
                    new_version: Some(target.into()),
                    new_digest: None,
                    update_type: "major".into(),
                    group: None,
                    policy: None,
                    release_age: None,
                }],
            }],
        }],
    };
    let mut snapshot = snapshot;
    if lock {
        snapshot.paths.insert("bun.lock".into());
    }
    (config, report, deps, snapshot)
}
fn make_plan(target: &str) -> (Plan, Snapshot) {
    let (config, repo, deps, snapshot) = setup(target, false);
    let plan = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now())
        .unwrap()
        .pop()
        .unwrap();
    (plan, snapshot)
}
fn fake(snapshot: &Snapshot) -> Arc<Fake> {
    let tree: Vec<_> = snapshot
        .files
        .iter()
        .filter(|(_, b)| !b.sha.is_empty())
        .map(|(p, b)| json!({"path":p,"sha":b.sha,"mode":b.mode,"type":"blob"}))
        .collect();
    Arc::new(Fake(Mutex::new(State {
        base: "base".into(),
        refs: BTreeMap::from([("refs/heads/main".into(), "base".into())]),
        trees: BTreeMap::from([("base-tree".into(), json!(tree))]),
        commits: BTreeMap::from([(
            "base".into(),
            json!({"sha":"base","tree":{"sha":"base-tree"}}),
        )]),
        ..Default::default()
    })))
}
async fn client(fake: Arc<Fake>) -> Client {
    Client::with_transport(
        "offline",
        Cache::open(":memory:", Default::default()).await.unwrap(),
        Default::default(),
        fake,
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn create_update_and_repeat_use_one_verified_commit_on_base_and_no_prs() {
    let (mut first, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    apply(
        &github,
        &mut first,
        &snapshot,
        "main",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        first.action.status,
        ActionStatus::Created,
        "{:?}",
        first.action
    );
    assert_eq!(first.action.verified, Some(true));
    let (mut second, _) = make_plan("22.1.0");
    assert_eq!(first.action.branch, second.action.branch);
    apply(
        &github,
        &mut second,
        &snapshot,
        "main",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        second.action.status,
        ActionStatus::Updated,
        "{:?}",
        second.action
    );
    assert_ne!(first.action.commit, second.action.commit);
    let count = server.0.lock().unwrap().writes.len();
    let (mut repeat, _) = make_plan("22.1.0");
    apply(
        &github,
        &mut repeat,
        &snapshot,
        "main",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        repeat.action.status,
        ActionStatus::Unchanged,
        "{:?}",
        repeat.action
    );
    assert_eq!(server.0.lock().unwrap().writes.len(), count);
}
#[tokio::test(start_paused = true)]
async fn dry_run_plans_diffs_and_never_sends_mutations() {
    let (mut plan, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    apply(
        &client(server.clone()).await,
        &mut plan,
        &snapshot,
        "main",
        Rebase::Behind,
        true,
    )
    .await;
    assert_eq!(
        plan.action.status,
        ActionStatus::Planned,
        "{:?}",
        plan.action
    );
    assert!(
        plan.action.files[0]
            .diff
            .contains("--- a/mise.toml\n+++ b/mise.toml\n@@")
    );
    assert!(
        plan.action.files[0]
            .diff
            .contains("+node = '22.0.0' # keep")
    );
    assert!(server.0.lock().unwrap().writes.is_empty());
    assert!(server.0.lock().unwrap().requests.is_empty());
}
#[tokio::test(start_paused = true)]
async fn behind_rebases_and_conflicts_keeps_an_identical_manifest_on_old_base() {
    for mode in [Rebase::Behind, Rebase::Conflicts] {
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        let github = client(server.clone()).await;
        apply(&github, &mut first, &snapshot, "main", mode, false).await;
        {
            let mut s = server.0.lock().unwrap();
            s.base = "base2".into();
            s.refs.insert("refs/heads/main".into(), "base2".into());
            s.commits.insert(
                "base2".into(),
                json!({"sha":"base2","tree":{"sha":"base-tree"}}),
            );
        }
        let (mut next, _) = make_plan("22.0.0");
        next.action.base_commit = "base2".into();
        apply(&github, &mut next, &snapshot, "main", mode, false).await;
        assert_eq!(
            next.action.status,
            if mode == Rebase::Behind {
                ActionStatus::Rebased
            } else {
                ActionStatus::Unchanged
            },
            "{:?}",
            next.action
        );
    }
}
#[tokio::test(start_paused = true)]
async fn human_commit_is_protected_even_with_copied_rustovate_markers() {
    let (mut first, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    apply(
        &github,
        &mut first,
        &snapshot,
        "main",
        Rebase::Behind,
        false,
    )
    .await;
    server
        .0
        .lock()
        .unwrap()
        .commits
        .get_mut(first.action.commit.as_ref().unwrap())
        .unwrap()["actor"] = json!("human");
    let count = server.0.lock().unwrap().writes.len();
    let (mut next, _) = make_plan("22.1.0");
    apply(&github, &mut next, &snapshot, "main", Rebase::Behind, false).await;
    assert_eq!(
        next.action.status,
        ActionStatus::Protected,
        "{:?}",
        next.action
    );
    assert_eq!(server.0.lock().unwrap().writes.len(), count);
}

#[tokio::test(start_paused = true)]
async fn a_human_below_the_bot_head_and_a_foreign_committer_are_protected() {
    for earlier in [false, true] {
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        let github = client(server.clone()).await;
        apply(
            &github,
            &mut first,
            &snapshot,
            "main",
            Rebase::Behind,
            false,
        )
        .await;
        {
            let mut state = server.0.lock().unwrap();
            let sha = first.action.commit.as_ref().unwrap();
            if earlier {
                let mut human = state.commits[sha].clone();
                human["actor"] = json!("human");
                state.commits.insert("human".into(), human);
                state.history.push("human".into());
            } else {
                state.commits.get_mut(sha).unwrap()["committer_login"] = json!("human");
            }
        }
        let count = server.0.lock().unwrap().writes.len();
        let (mut next, _) = make_plan("22.1.0");
        apply(&github, &mut next, &snapshot, "main", Rebase::Behind, false).await;
        assert_eq!(
            next.action.status,
            ActionStatus::Protected,
            "{:?}",
            next.action
        );
        assert_eq!(server.0.lock().unwrap().writes.len(), count);
    }
}

#[tokio::test(start_paused = true)]
async fn conflicts_mode_recreates_when_the_default_changes_a_touched_file() {
    let (mut first, mut snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    apply(
        &github,
        &mut first,
        &snapshot,
        "main",
        Rebase::Conflicts,
        false,
    )
    .await;
    let blob = snapshot.files.get_mut("mise.toml").unwrap();
    blob.content.push_str("# new base comment\n");
    blob.sha = blob_sha(&blob.content);
    {
        let mut state = server.0.lock().unwrap();
        state.base = "base2".into();
        state.refs.insert("refs/heads/main".into(), "base2".into());
        let mut tree = state.trees["base-tree"].clone();
        tree[0]["sha"] = json!(blob.sha);
        state.trees.insert("base-tree2".into(), tree);
        state.commits.insert(
            "base2".into(),
            json!({"sha":"base2","tree":{"sha":"base-tree2"}}),
        );
    }
    let (config, mut repo, deps, _) = setup("22.0.0", false);
    repo.commit = Some("base2".into());
    let mut next = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now())
        .unwrap()
        .pop()
        .unwrap();
    apply(
        &github,
        &mut next,
        &snapshot,
        "main",
        Rebase::Conflicts,
        false,
    )
    .await;
    assert_eq!(
        next.action.status,
        ActionStatus::Rebased,
        "{:?}",
        next.action
    );
    assert!(next.contents["mise.toml"].contains("# new base comment"));
}
#[tokio::test(start_paused = true)]
async fn stale_blob_content_tree_moved_base_unsigned_and_lease_race_fail_closed() {
    for case in 0..6 {
        let (mut plan, mut snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        {
            let mut s = server.0.lock().unwrap();
            match case {
                0 => snapshot
                    .files
                    .get_mut("mise.toml")
                    .unwrap()
                    .content
                    .push_str("# changed"),
                1 => s.trees.get_mut("base-tree").unwrap()[0]["sha"] = json!("changed"),
                2 => {
                    s.refs.insert("refs/heads/main".into(), "new-base".into());
                }
                3 => s.unsigned = true,
                4 => s.race = true,
                _ => s.truncated = true,
            }
        }
        apply(
            &client(server.clone()).await,
            &mut plan,
            &snapshot,
            "main",
            Rebase::Behind,
            false,
        )
        .await;
        assert_eq!(
            plan.action.status,
            ActionStatus::Error,
            "case={case}: {:?}",
            plan.action
        );
        assert!(
            !server
                .0
                .lock()
                .unwrap()
                .refs
                .contains_key(&format!("refs/heads/{}", plan.action.branch))
        );
        if matches!(case, 0..=2 | 5) {
            assert!(server.0.lock().unwrap().writes.is_empty());
        }
    }
}
#[test]
fn names_are_readable_and_validate_git_refs() {
    assert_eq!(
        branch_name("rustovate/", "scope-a-2.x", "one").unwrap(),
        "rustovate/scope-a-2.x"
    );
    assert!(branch_name("../bad/", "a", "one").is_err());
    assert!(branch_name("custom/deps/", "a", "one").unwrap() == "custom/deps/a");
}

#[test]
fn grouped_files_and_prefixes_are_stable_when_occurrence_order_changes() {
    let (mut config, mut repo, _, mut snapshot) = setup("22.0.0", false);
    config.prs.branch = "deps/".into();
    config.defaults.commit_prefix = "build(deps):".into();
    let text = "node 20.0.0 # preserve\n";
    snapshot.files.insert(
        ".tool-versions".into(),
        Blob {
            sha: blob_sha(text),
            mode: "100644".into(),
            content: text.into(),
        },
    );
    snapshot.paths.insert(".tool-versions".into());
    repo.package_files.push(PackageFile {
        path: ".tool-versions".into(),
        dependencies: repo.package_files[0].dependencies.clone(),
    });
    for file in &mut repo.package_files {
        file.dependencies[0].updates[0].group = Some("Node & runtime".into());
    }
    let files = snapshot
        .files
        .iter()
        .map(|(p, b)| (p.clone(), b.content.clone()))
        .collect();
    let mut deps = Scanner::new(&config)
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    let first = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now())
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first.action.status, ActionStatus::Planned);
    assert_eq!(first.action.files.len(), 2);
    assert!(first.action.branch.starts_with("deps/major-node-runtime"));
    assert!(first.action.message.starts_with("build(deps):"));
    repo.package_files.reverse();
    deps.reverse();
    let second = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now())
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first.action, second.action);
    assert_eq!(first.fingerprint, second.fingerprint);
}
#[test]
fn unsupported_lockfile_plans_skip_and_opt_in_marks_manifest_only_updates() {
    let (config, mut repo, _, mut snapshot) = setup("22.0.0", true);
    // Use a JSON manifest to exercise actual npm lock ownership gating.
    let text = "{\"dependencies\":{\"fixture\":\"1.0.0\"}}\n";
    let mut config = config;
    config.managers = vec![Manager::Npm];
    let deps = Scanner::new(&config)
        .unwrap()
        .extract(&Files::from([("package.json".into(), text.into())]))
        .unwrap()
        .dependencies;
    let d = &deps[0];
    repo.package_files[0].path = "package.json".into();
    let r = &mut repo.package_files[0].dependencies[0];
    r.manager = "npm".into();
    r.name = d.name.clone();
    r.package = d.package.clone();
    r.current.value = Some(d.current.clone());
    r.updates[0].new_value = Some("2.0.0".into());
    snapshot.files = BTreeMap::from([(
        "package.json".into(),
        Blob {
            sha: blob_sha(text),
            mode: "100644".into(),
            content: text.into(),
        },
    )]);
    snapshot.paths.remove("bun.lock");
    snapshot.paths.insert("yarn.lock".into());
    for allow in [false, true] {
        let p = plan(&repo, &config, &deps, &snapshot, allow, chrono::Utc::now())
            .unwrap()
            .pop()
            .unwrap();
        assert!(p.action.lockfile_pending);
        assert_eq!(
            p.action.status,
            if allow {
                ActionStatus::Planned
            } else {
                ActionStatus::Skipped
            }
        );
        assert!(!p.action.files.is_empty());
    }
}

#[test]
fn consolidates_minor_patch_digest_pins_and_keeps_only_latest_major() {
    let (config, mut repo, deps, snapshot) = setup("22.0.0", false);
    let original = repo.package_files[0].dependencies[0].updates[0].clone();
    let updates = &mut repo.package_files[0].dependencies[0].updates;
    updates.clear();
    for (kind, version) in [
        ("major", "21.0.0"),
        ("major", "24.0.0"),
        ("minor", "20.9.0"),
        ("patch", "20.0.1"),
        ("digest", "20.0.0"),
        ("pin", "20.0.0"),
    ] {
        let mut u = original.clone();
        u.update_type = kind.into();
        u.new_value = Some(version.into());
        u.new_version = Some(version.into());
        updates.push(u);
    }
    let plans = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now()).unwrap();
    let names: BTreeSet<_> = plans.iter().map(|p| p.action.branch.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "rustovate/node-20.x",
            "rustovate/node-24.x",
            "rustovate/pin-dependencies"
        ])
    );
    let nonmajor = plans
        .iter()
        .find(|p| p.action.branch.ends_with("20.x"))
        .unwrap();
    assert!(nonmajor.contents["mise.toml"].contains("20.9.0"));
    assert_eq!(nonmajor.action.status, ActionStatus::Planned);
}

#[tokio::test(start_paused = true)]
async fn migration_deletes_owned_hash_branch_with_a_lease_and_preserves_human_work() {
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    apply(&github, &mut p, &snapshot, "main", Rebase::Behind, false).await;
    let old = "rustovate/node-major-0123456789abcdef";
    {
        let mut s = server.0.lock().unwrap();
        let sha = s
            .refs
            .remove(&format!("refs/heads/{}", p.action.branch))
            .unwrap();
        let commit = s.commits.get_mut(&sha).unwrap();
        commit["message"] = json!(commit["message"].as_str().unwrap().replace(
            "Rustovate-Base: base",
            "Rustovate-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        let mut human = commit.clone();
        human["actor"] = json!("human");
        s.commits.insert("human".into(), human);
        s.refs.insert(format!("refs/heads/{old}"), sha);
        s.refs
            .insert("refs/heads/rustovate/human".into(), "human".into());
    }
    let actions = rustovate_update::cleanup(&github, "demo/app", "base", "main", &[p], false)
        .await
        .unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].branch, old);
    assert_eq!(actions[0].status, ActionStatus::Deleted);
    let s = server.0.lock().unwrap();
    assert_eq!(
        s.refs[&format!("refs/heads/{old}")],
        "0000000000000000000000000000000000000000"
    );
    assert!(s.refs.contains_key("refs/heads/rustovate/human"));
    assert_eq!(s.refs["refs/heads/main"], "base");
}

#[test]
fn container_occurrences_across_managers_share_target_and_slug_collisions_gain_manager() {
    let (mut config, mut repo, mut deps, mut snapshot) = setup("22.0.0", false);
    let text = "FROM node:20.0.0\n";
    config.managers.push(Manager::Dockerfile);
    let docker = Scanner::new(&config)
        .unwrap()
        .extract(&Files::from([("Dockerfile".into(), text.into())]))
        .unwrap()
        .dependencies
        .remove(0);
    let mut r = repo.package_files[0].dependencies[0].clone();
    r.manager = "dockerfile".into();
    r.current.value = Some(docker.current.clone());
    repo.package_files.push(PackageFile {
        path: "Dockerfile".into(),
        dependencies: vec![r],
    });
    snapshot.files.insert(
        "Dockerfile".into(),
        Blob {
            sha: blob_sha(text),
            mode: "100644".into(),
            content: text.into(),
        },
    );
    deps.push(docker);
    let plans = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now()).unwrap();
    let names: BTreeSet<_> = plans.iter().map(|p| p.action.branch.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from(["rustovate/docker-node-22.x", "rustovate/mise-node-22.x"])
    );
    // Same package/source/target in a second file shares the existing branch.
    let text = "node 20.0.0\n";
    snapshot.files.insert(
        ".tool-versions".into(),
        Blob {
            sha: blob_sha(text),
            mode: "100644".into(),
            content: text.into(),
        },
    );
    let second = Scanner::new(&config)
        .unwrap()
        .extract(&Files::from([(".tool-versions".into(), text.into())]))
        .unwrap()
        .dependencies
        .remove(0);
    deps.push(second);
    repo.package_files.push(PackageFile {
        path: ".tool-versions".into(),
        dependencies: repo.package_files[0].dependencies.clone(),
    });
    let plans = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now()).unwrap();
    assert_eq!(plans.len(), 2);
    assert_eq!(
        plans
            .iter()
            .find(|p| p.action.branch.contains("mise-node"))
            .unwrap()
            .action
            .files
            .len(),
        2
    );
}

#[tokio::test(start_paused = true)]
async fn shared_base_write_trace_and_request_budget() {
    let (p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    server.0.lock().unwrap().latency = std::time::Duration::from_millis(150);
    let github = client(server.clone()).await;
    let mut plans: Vec<_> = (0..12)
        .map(|n| {
            let (mut p, _) = make_plan("22.0.0");
            p.action.branch = format!("rustovate/tool-{n}-22.x");
            p
        })
        .collect();
    let started = tokio::time::Instant::now();
    rustovate_update::apply_all(&github, &mut plans, &snapshot, "main", Rebase::Behind, true).await;
    assert_eq!(github.stats().await.requests, 0);
    eprintln!(
        "dry-run: {} requests, simulated elapsed {:?}",
        github.stats().await.requests,
        started.elapsed()
    );
    let started = tokio::time::Instant::now();
    rustovate_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        Rebase::Behind,
        false,
    )
    .await;
    assert!(
        plans
            .iter()
            .all(|p| p.action.status == ActionStatus::Created)
    );
    let state = server.0.lock().unwrap();
    for (method, path) in &state.requests {
        eprintln!("{method} api.github.com{path}");
    }
    eprintln!(
        "real: {} requests, simulated elapsed {:?}; 12 branches, 150ms/request, 1s mutation cooldown",
        state.requests.len(),
        started.elapsed()
    );
    assert_eq!(state.requests.len(), 4 + 12 * 3 + 1);
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|(_, p)| p.ends_with("/git/trees/base-tree"))
            .count(),
        1
    );
    assert!(
        !state
            .requests
            .iter()
            .any(|(_, p)| p.ends_with("/git/blobs"))
    );
    assert!(
        state
            .writes
            .iter()
            .filter(|(p, _)| p.ends_with("/git/trees"))
            .all(|(_, b)| b["tree"][0]["content"] == p.contents["mise.toml"])
    );
}

fn pr_config() -> (Config, Repository) {
    let (mut config, repo, _, _) = setup("22.0.0", false);
    config.prs.concurrent = None;
    config.prs.per_hour = None;
    (config, repo)
}
async fn pr_cycle(
    client: &Client,
    repo: &Repository,
    config: &Config,
    plans: &mut [Plan],
    snapshot: &Snapshot,
) -> Vec<rustovate_report::PullRequestAction> {
    let mut state = rustovate_update::Lifecycle::prepare(
        client,
        repo,
        plans,
        config,
        chrono::Utc::now(),
        rustovate_update::Recheck::NextRun,
    )
    .await
    .unwrap();
    rustovate_update::apply_all(client, plans, snapshot, "main", Rebase::Behind, false).await;
    state.publish(client, plans).await;
    state.actions
}

#[tokio::test(start_paused = true)]
async fn verified_receipts_reduce_warm_write_to_one_inventory_and_detect_changes() {
    for case in [
        "same",
        "adopted",
        "generated",
        "conflicts",
        "pages",
        "human",
        "body",
        "title",
        "labels",
        "merge",
        "closed",
        "base",
        "plan",
        "obsolete",
        "foreign-obsolete",
    ] {
        let (mut config, mut repo) = pr_config();
        config.prs.rebase = Rebase::Behind;
        if case == "conflicts" {
            config.prs.rebase = Rebase::Conflicts;
        }
        let (mut p, snapshot) = make_plan("22.0.0");
        p.changes[0].labels = vec!["dependencies".into()];
        if case == "generated" {
            p.fingerprint = "d".repeat(64);
            p.lock_jobs.push(rustovate_update::lockfiles::Job {
                ecosystem: rustovate_update::lockfiles::Ecosystem::Cargo,
                directory: String::new(),
                package: None,
                current: None,
                target: None,
            });
            p.lockfiles_ready = true;
        }
        let server = fake(&snapshot);
        let github = client(server.clone()).await;
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        if case == "adopted" {
            pr_cycle(
                &github,
                &repo,
                &config,
                std::slice::from_mut(&mut p),
                &snapshot,
            )
            .await;
            let (mut next, _) = make_plan("22.0.0");
            next.changes = p.changes.clone();
            p = next;
        }
        let mut lifecycle = rustovate_update::Lifecycle::prepare(
            &github,
            &repo,
            std::slice::from_mut(&mut p),
            &config,
            chrono::Utc::now(),
            Default::default(),
        )
        .await
        .unwrap();
        rustovate_update::apply_all(
            &github,
            std::slice::from_mut(&mut p),
            &snapshot,
            "main",
            config.prs.rebase,
            false,
        )
        .await;
        lifecycle
            .publish(&github, std::slice::from_mut(&mut p))
            .await;
        rustovate_update::save_receipts(
            &cache,
            &repo,
            &config,
            std::slice::from_ref(&p),
            &lifecycle,
        )
        .await
        .unwrap();
        let (mut next, _) = make_plan(if case == "plan" { "22.1.0" } else { "22.0.0" });
        next.changes[0].labels = p.changes[0].labels.clone();
        next.lock_jobs = p.lock_jobs.clone();
        {
            let mut s = server.0.lock().unwrap();
            match case {
                "pages" => s.ref_page_size = Some(1),
                "human" => {
                    let mut commit = s.commits[p.action.commit.as_ref().unwrap()].clone();
                    commit["message"] = json!("Human work");
                    s.commits.insert("human".into(), commit);
                    s.refs
                        .insert(format!("refs/heads/{}", p.action.branch), "human".into());
                }
                "body" => s.prs.get_mut(&1).unwrap()["body"] = json!("Human edited body"),
                "title" => s.prs.get_mut(&1).unwrap()["title"] = json!("Human title"),
                "labels" => s.prs.get_mut(&1).unwrap()["labels"] = json!([]),
                "merge" => s.prs.get_mut(&1).unwrap()["auto_merge"] = json!({"enabled":true}),
                "closed" => s.prs.get_mut(&1).unwrap()["state"] = json!("closed"),
                "base" | "conflicts" => {
                    s.base = "base2".into();
                    repo.commit = Some("base2".into());
                    next.action.base_commit = "base2".into();
                }
                "obsolete" | "foreign-obsolete" => {
                    let mut commit = s.commits[p.action.commit.as_ref().unwrap()].clone();
                    if case == "foreign-obsolete" {
                        commit["actor"] = json!("liv-app[bot]");
                    }
                    s.commits.insert("obsolete".into(), commit);
                    s.refs
                        .insert("refs/heads/rustovate/obsolete".into(), "obsolete".into());
                }
                _ => {}
            }
            s.requests.clear();
            s.latency = std::time::Duration::from_millis(200);
        }
        let started = tokio::time::Instant::now();
        let all = rustovate_update::restore_unchanged(
            &github,
            &cache,
            &repo,
            &config,
            std::slice::from_mut(&mut next),
            Default::default(),
        )
        .await
        .unwrap();
        if case == "same" {
            eprintln!(
                "warm no-op: POST api.github.com/graphql; 1 query, 0 mutations, elapsed {:?} (200ms synthetic latency)",
                started.elapsed()
            );
        }
        assert_eq!(
            all,
            matches!(
                case,
                "same" | "adopted" | "generated" | "conflicts" | "pages" | "foreign-obsolete"
            ),
            "{case}"
        );
        assert_eq!(
            next.action.status == ActionStatus::Unchanged,
            matches!(
                case,
                "same"
                    | "adopted"
                    | "generated"
                    | "conflicts"
                    | "pages"
                    | "obsolete"
                    | "foreign-obsolete"
            ),
            "{case}"
        );
        let s = server.0.lock().unwrap();
        assert_eq!(
            s.requests.len(),
            if case == "pages" { 2 } else { 1 },
            "{case}"
        );
        assert!(
            s.requests
                .iter()
                .all(|(m, p)| m == "POST" && p == "/graphql")
        );
    }
}
#[tokio::test(start_paused = true)]
async fn published_pr_heads_wait_for_our_old_head_but_reject_foreign_commits() {
    for case in [
        "lag",
        "timeout",
        "foreign",
        "closed",
        "closed-while-waiting",
    ] {
        let (config, mut repo) = pr_config();
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        let github = client(server.clone()).await;
        pr_cycle(
            &github,
            &repo,
            &config,
            std::slice::from_mut(&mut first),
            &snapshot,
        )
        .await;
        let (mut next, _) = make_plan("22.1.0");
        repo.commit = Some("base2".into());
        next.action.base_commit = "base2".into();
        {
            let mut s = server.0.lock().unwrap();
            let mut base = s.commits["base"].clone();
            base["sha"] = json!("base2");
            s.commits.insert("base2".into(), base);
            s.base = "base2".into();
            s.refs.insert("refs/heads/main".into(), "base2".into());
        }
        let mut lifecycle = rustovate_update::Lifecycle::prepare(
            &github,
            &repo,
            std::slice::from_mut(&mut next),
            &config,
            chrono::Utc::now(),
            Default::default(),
        )
        .await
        .unwrap();
        {
            let mut s = server.0.lock().unwrap();
            s.pr_lag_reads = if case == "timeout" { usize::MAX } else { 2 };
            s.pr_head_override = (case == "foreign").then(|| "human".into());
            s.close_on_read = case == "closed";
        }
        rustovate_update::apply_all(
            &github,
            std::slice::from_mut(&mut next),
            &snapshot,
            "main",
            Rebase::Behind,
            false,
        )
        .await;
        assert_eq!(next.action.status, ActionStatus::Rebased);
        server.0.lock().unwrap().requests.clear();
        if case == "closed-while-waiting" {
            server.0.lock().unwrap().close_after_reads = Some(1);
        }
        let started = tokio::time::Instant::now();
        lifecycle
            .publish(&github, std::slice::from_mut(&mut next))
            .await;
        let result = &lifecycle.actions[0];
        let s = server.0.lock().unwrap();
        let reads = s
            .requests
            .iter()
            .filter(|(m, p)| m == "GET" && p.ends_with("/pulls/1"))
            .count();
        match case {
            "lag" => {
                assert_ne!(result.status, ActionStatus::Error, "{result:?}");
                assert_eq!(reads, 3);
                assert!(started.elapsed() >= std::time::Duration::from_millis(300));
            }
            "timeout" => {
                assert_eq!(result.status, ActionStatus::Error);
                assert_eq!(reads, 7);
                assert!(
                    result
                        .reason
                        .as_deref()
                        .unwrap()
                        .contains("bounded retries")
                );
                assert!(!result.reason.as_deref().unwrap().contains("moved"));
            }
            "foreign" => {
                assert_eq!(result.status, ActionStatus::Error);
                assert_eq!(reads, 1);
                assert!(
                    result
                        .reason
                        .as_deref()
                        .unwrap()
                        .contains("unexpected commit")
                );
            }
            _ => {
                assert_eq!(result.status, ActionStatus::Skipped);
                assert_eq!(result.number, Some(1));
                assert!(
                    result
                        .reason
                        .as_deref()
                        .unwrap()
                        .contains("closed-by-owner")
                );
                assert_eq!(reads, if case == "closed-while-waiting" { 2 } else { 1 });
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn closed_update_receipts_are_freshly_verified_and_invalidated() {
    for case in [
        "same", "reopened", "merged", "body", "actor", "target", "deleted",
    ] {
        let (config, repo) = pr_config();
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        let github = client(server.clone()).await;
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        pr_cycle(
            &github,
            &repo,
            &config,
            std::slice::from_mut(&mut first),
            &snapshot,
        )
        .await;
        server.0.lock().unwrap().prs.get_mut(&1).unwrap()["state"] = json!("closed");
        let (mut next, _) = make_plan("22.0.0");
        let lifecycle = rustovate_update::Lifecycle::prepare(
            &github,
            &repo,
            std::slice::from_mut(&mut next),
            &config,
            chrono::Utc::now(),
            Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(next.action.status, ActionStatus::Skipped);
        rustovate_update::save_receipts(&cache, &repo, &config, &[next], &lifecycle)
            .await
            .unwrap();
        {
            let mut s = server.0.lock().unwrap();
            match case {
                "reopened" => s.prs.get_mut(&1).unwrap()["state"] = json!("open"),
                "merged" => s.prs.get_mut(&1).unwrap()["merged_at"] = json!("2026-10-15T00:00:00Z"),
                "body" => s.prs.get_mut(&1).unwrap()["body"] = json!("changed"),
                "actor" => s.prs.get_mut(&1).unwrap()["user"]["login"] = json!("liv-app[bot]"),
                "deleted" => {
                    s.prs.remove(&1);
                }
                _ => {}
            }
            s.requests.clear();
        }
        let (mut warm, _) = make_plan(if case == "target" { "22.1.0" } else { "22.0.0" });
        let all = rustovate_update::restore_unchanged(
            &github,
            &cache,
            &repo,
            &config,
            std::slice::from_mut(&mut warm),
            Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(all, case == "same", "{case}");
        assert_eq!(
            warm.action.status == ActionStatus::Skipped,
            case == "same",
            "{case}"
        );
        assert_eq!(
            server.0.lock().unwrap().requests,
            [("POST".into(), "/graphql".into())]
        );
    }
}

#[tokio::test(start_paused = true)]
async fn pr_create_repeat_update_labels_notes_and_checked_retry() {
    let (config, repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    p.changes[0].labels = vec!["dependencies".into()];
    p.changes[0].body_notes = vec!["Review our deployment first.".into()];
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    assert_eq!(actions[0].status, ActionStatus::Created);
    {
        let state = server.0.lock().unwrap();
        let pr = &state.prs[&1];
        assert_eq!(pr["title"], p.action.message);
        let body = pr["body"].as_str().unwrap();
        assert!(body.contains("| Package | Change | Type | File |"));
        assert!(body.contains("mise.toml") && body.contains("22.0.0"));
        assert!(body.contains("Review our deployment first."));
        assert!(body.contains("Created by rustovate CLI. Click to re-check on the next run."));
        assert!(body.contains("<!-- rustovate:") && body.contains("<!-- rustovate-recheck -->"));
        assert_eq!(pr["labels"][0]["name"], "dependencies");
    }
    let count = server.0.lock().unwrap().writes.len();
    let (mut repeat, _) = make_plan("22.0.0");
    repeat.changes = p.changes.clone();
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(actions[0].status, ActionStatus::Unchanged);
    assert_eq!(server.0.lock().unwrap().writes.len(), count);
    {
        let mut s = server.0.lock().unwrap();
        let body = s.prs[&1]["body"].as_str().unwrap().replace(
            "- [ ] Created by rustovate CLI. Click to re-check on the next run.",
            "- [x] Any operator wording",
        );
        s.prs.get_mut(&1).unwrap()["body"] = json!(body);
        s.prs.get_mut(&1).unwrap()["labels"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"human-label"}));
    }
    let (mut retry, _) = make_plan("22.0.0");
    retry.changes = p.changes.clone();
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut retry),
        &snapshot,
    )
    .await;
    assert!(retry.retry);
    assert_eq!(retry.action.status, ActionStatus::Updated);
    assert_eq!(actions[0].status, ActionStatus::Updated);
    let (mut next, _) = make_plan("22.1.0");
    next.changes[0].labels = vec!["new-policy".into()];
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        &snapshot,
    )
    .await;
    let state = server.0.lock().unwrap();
    assert_eq!(state.prs.len(), 1);
    assert!(state.prs[&1]["body"].as_str().unwrap().contains("22.1.0"));
    assert!(
        state.prs[&1]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "human-label")
    );
}

#[tokio::test(start_paused = true)]
async fn manually_closed_update_stays_closed_across_files_and_wording_but_newer_target_is_allowed()
{
    let (config, repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    {
        let mut s = server.0.lock().unwrap();
        s.prs.get_mut(&1).unwrap()["state"] = json!("closed");
        let mut decoy = s.prs[&1].clone();
        decoy["number"] = json!(2);
        decoy["user"]["login"] = json!("human");
        s.prs.insert(2, decoy);
        s.pr_page_size = Some(1);
    }
    let count = server.0.lock().unwrap().writes.len();
    let (mut repeat, _) = make_plan("22.0.0");
    repeat.action.message = "build: different wording".into();
    let mut row = repeat.changes[0].clone();
    row.file = "other.toml".into();
    repeat.changes.push(row);
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(repeat.action.status, ActionStatus::Skipped);
    assert!(repeat.action.reason.unwrap().contains("manually closed"));
    assert_eq!(server.0.lock().unwrap().writes.len(), count);
    let (mut next, _) = make_plan("22.1.0");
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        &snapshot,
    )
    .await;
    assert_eq!(actions[0].status, ActionStatus::Created);
    assert_eq!(server.0.lock().unwrap().prs.len(), 3);
}

#[tokio::test(start_paused = true)]
async fn concurrent_and_hourly_limits_include_closed_creates_and_null_is_unlimited() {
    let (mut config, repo) = pr_config();
    config.prs.concurrent = Some(1);
    let (p, snapshot) = make_plan("22.0.0");
    let (mut other, _) = make_plan("24.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let mut plans = vec![p, other];
    pr_cycle(&github, &repo, &config, &mut plans, &snapshot).await;
    assert_eq!(plans[1].action.status, ActionStatus::Skipped);
    assert!(
        plans[1]
            .action
            .reason
            .as_ref()
            .unwrap()
            .contains("concurrent")
    );
    server.0.lock().unwrap().prs.get_mut(&1).unwrap()["state"] = json!("closed");
    config.prs.concurrent = None;
    config.prs.per_hour = Some(1);
    (other, _) = make_plan("24.0.0");
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut other),
        &snapshot,
    )
    .await;
    assert_eq!(other.action.status, ActionStatus::Skipped);
    assert!(other.action.reason.as_ref().unwrap().contains("perHour"));
    config.prs.per_hour = None;
    (other, _) = make_plan("24.0.0");
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut other),
        &snapshot,
    )
    .await;
    assert_eq!(actions[0].status, ActionStatus::Created);
}

#[tokio::test(start_paused = true)]
async fn obsolete_pr_closes_then_branch_deletes_and_human_history_is_protected() {
    for case in ["bot", "human", "closed-by-owner"] {
        let human = case == "human";
        let (config, repo) = pr_config();
        let (mut p, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        let github = client(server.clone()).await;
        pr_cycle(
            &github,
            &repo,
            &config,
            std::slice::from_mut(&mut p),
            &snapshot,
        )
        .await;
        {
            let mut s = server.0.lock().unwrap();
            let commit = s
                .commits
                .get_mut(p.action.commit.as_ref().unwrap())
                .unwrap();
            commit["message"] = json!(commit["message"].as_str().unwrap().replace(
                "Rustovate-Base: base",
                "Rustovate-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ));
            if human {
                commit["actor"] = json!("human");
            }
            s.close_on_read = case == "closed-by-owner";
        }
        let state = rustovate_update::Lifecycle::prepare(
            &github,
            &repo,
            &mut [],
            &config,
            chrono::Utc::now(),
            Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            state.actions[0].status,
            if human {
                ActionStatus::Protected
            } else if case == "closed-by-owner" {
                ActionStatus::Skipped
            } else {
                ActionStatus::Closed
            }
        );
        let cleanup = rustovate_update::cleanup(&github, "demo/app", "base", "main", &[], false)
            .await
            .unwrap();
        assert_eq!(cleanup.len(), usize::from(!human));
        let s = server.0.lock().unwrap();
        assert_eq!(s.prs[&1]["state"], if human { "open" } else { "closed" });
        if !human && case != "closed-by-owner" {
            let close = s
                .writes
                .iter()
                .position(|(_, b)| b["state"] == "closed")
                .unwrap();
            let delete = s
                .writes
                .iter()
                .rposition(|(_, b)| b["query"] == rustovate_update::REF_UPDATE)
                .unwrap();
            assert!(close < delete);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn release_notes_are_bounded_deduplicated_cached_and_policy_controlled() {
    let (config, repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    p.changes[0].source = "github-tags".into();
    p.changes[0].package = "actions/checkout".into();
    p.changes[0].release_notes = true;
    let mut row = p.changes[0].clone();
    row.file = "other.yml".into();
    p.changes.push(row);
    let mut lifecycle = rustovate_update::Lifecycle::prepare(
        &github,
        &repo,
        std::slice::from_mut(&mut p),
        &config,
        chrono::Utc::now(),
        rustovate_update::Recheck::Now,
    )
    .await
    .unwrap();
    apply(&github, &mut p, &snapshot, "main", Rebase::Behind, false).await;
    lifecycle
        .publish(&github, std::slice::from_mut(&mut p))
        .await;
    {
        let s = server.0.lock().unwrap();
        let body = s.prs[&1]["body"].as_str().unwrap();
        assert!(body.len() <= rustovate_update::BODY_LIMIT);
        assert!(body.contains("Release details 🦀") && body.contains("Click to re-check now."));
        assert_eq!(
            s.requests
                .iter()
                .filter(|(_, p)| p.contains("/releases/tags/"))
                .count(),
            1
        );
    }
    let (mut repeat, _) = make_plan("22.0.0");
    repeat.changes = p.changes.clone();
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(github.stats().await.not_modified, 1);
    repeat.action.status = ActionStatus::Unchanged;
    repeat.changes.iter_mut().for_each(|c| {
        c.release_notes = false;
        c.body_notes = vec!["huge 🦀 <!-- rustovate:fake -->".repeat(5000)];
    });
    let count = server
        .0
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|(_, p)| p.contains("/releases/tags/"))
        .count();
    lifecycle
        .publish(&github, std::slice::from_mut(&mut repeat))
        .await;
    let s = server.0.lock().unwrap();
    let body = s.prs[&1]["body"].as_str().unwrap();
    assert!(body.len() <= rustovate_update::BODY_LIMIT);
    assert_eq!(body.matches("<!-- rustovate:").count(), 1);
    assert_eq!(
        s.requests
            .iter()
            .filter(|(_, p)| p.contains("/releases/tags/"))
            .count(),
        count
    );
}

#[tokio::test(start_paused = true)]
async fn merge_policy_uses_only_native_auto_merge_and_lockfile_pending_disables_it() {
    let (config, repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    p.changes[0].merge = true;
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    assert_eq!(actions[0].status, ActionStatus::Created);
    {
        let s = server.0.lock().unwrap();
        assert!(s.writes.iter().any(|(_, b)| {
            b["query"]
                .as_str()
                .is_some_and(|q| q.contains("enablePullRequestAutoMerge"))
                && b["variables"]["input"]["mergeMethod"] == "SQUASH"
        }));
        assert!(!s.requests.iter().any(|(_, p)| p.ends_with("/merge")));
    }
    let (mut next, _) = make_plan("22.1.0");
    next.changes[0].merge = true;
    next.action.lockfile_pending = true;
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        &snapshot,
    )
    .await;
    let s = server.0.lock().unwrap();
    assert!(s.prs[&1]["auto_merge"].is_null());
}

#[tokio::test(start_paused = true)]
async fn uncertain_pr_creation_is_reconciled_next_run_and_never_blindly_retried() {
    let (config, repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    server.0.lock().unwrap().pr_create_error = true;
    let github = client(server.clone()).await;
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    assert_eq!(actions[0].status, ActionStatus::Error);
    assert_eq!(server.0.lock().unwrap().prs.len(), 1);
    let (mut repeat, _) = make_plan("22.0.0");
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(actions[0].status, ActionStatus::Unchanged);
    assert_eq!(server.0.lock().unwrap().prs.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn lookup_warnings_defer_obsolete_pr_closure() {
    let (config, mut repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    repo.warnings
        .push("registry temporarily unavailable".into());
    let state = rustovate_update::Lifecycle::prepare(
        &github,
        &repo,
        &mut [],
        &config,
        chrono::Utc::now(),
        Default::default(),
    )
    .await
    .unwrap();
    assert!(state.actions.is_empty());
    assert_eq!(server.0.lock().unwrap().prs[&1]["state"], "open");
}

#[test]
fn supplied_sandbox_targets_converge_to_sixteen_plans_with_supported_pins() {
    // Self-authored manifests and target facts from the owner's rustovate report;
    // no Renovate fixture, source or preset data is used.
    let (config, mut repo, _, _) = setup("22.0.0", false);
    let mut config = config;
    config.managers = vec![
        Manager::GithubActions,
        Manager::Dockerfile,
        Manager::Compose,
        Manager::Mise,
        Manager::Npm,
    ];
    let files = Files::from([
        (
            ".github/workflows/ci.yml".into(),
            format!(
                "jobs:\n  ci:\n    runs-on: ubuntu-22.04\n    steps:\n      - uses: actions/checkout@{} # v4.1.7\n      - uses: jdx/mise-action@v2\n",
                "a".repeat(40)
            ),
        ),
        (
            "Dockerfile".into(),
            "FROM nginx:1.27.1-alpine\nFROM node:20.17.0-alpine\n".into(),
        ),
        (
            "compose.yml".into(),
            "services:\n  db:\n    image: postgres:16.4\n  cache:\n    image: redis:7.2.5-alpine\n"
                .into(),
        ),
        (
            "mise.toml".into(),
            "[tools]\nnode = '22.9.0'\nbun = '1.1.30'\njq = '1.7.1'\n".into(),
        ),
        (
            "package.json".into(),
            "{\"devDependencies\":{\"prettier\":\"^3.3.0\"}}\n".into(),
        ),
    ]);
    let deps = Scanner::new(&config)
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    let mut snapshot = Snapshot {
        paths: files.keys().cloned().collect(),
        files: files
            .iter()
            .map(|(p, c)| {
                (
                    p.clone(),
                    Blob {
                        sha: blob_sha(c),
                        mode: "100644".into(),
                        content: c.clone(),
                    },
                )
            })
            .collect(),
    };
    snapshot.paths.insert("bun.lock".into());
    repo.package_files.clear();
    for dep in &deps {
        let targets: &[(&str, &str)] = match (dep.package.as_str(), word(&dep.source).as_str()) {
            ("actions/checkout", _) => &[("minor", "v4.4.0"), ("major", "v7.0.1")],
            ("jdx/mise-action", _) => &[("pin", "v2"), ("major", "v5")],
            ("ubuntu", _) => &[("major", "26.04")],
            ("nginx", _) => &[
                ("pin", "1.27.1-alpine"),
                ("patch", "1.27.5-alpine"),
                ("minor", "1.31.7-alpine"),
            ],
            ("node", "docker") => &[
                ("pin", "20.17.0-alpine"),
                ("minor", "20.20.2-alpine"),
                ("major", "26.12.0-alpine"),
            ],
            ("node", _) => &[("minor", "22.23.3"), ("major", "26.12.0")],
            ("postgres", _) => &[("pin", "16.4"), ("minor", "16.15"), ("major", "18.6")],
            ("redis", _) => &[
                ("pin", "7.2.5-alpine"),
                ("patch", "7.2.16-alpine"),
                ("minor", "7.4.11-alpine"),
                ("major", "8.10.2-alpine"),
            ],
            ("oven-sh/bun", _) => &[("patch", "1.1.45"), ("minor", "1.4.3")],
            ("jqlang/jq", _) => &[("minor", "1.8.2")],
            ("prettier", _) => &[("pin", "3.9.10")],
            other => panic!("unexpected synthetic sandbox dependency {other:?}"),
        };
        let updates = targets
            .iter()
            .map(|(kind, target)| Update {
                new_value: Some((*target).into()),
                new_version: Some((*target).into()),
                new_digest: if dep.source == rustovate_config::Source::Docker {
                    Some(format!("sha256:{}", "b".repeat(64)))
                } else if dep.manager == Manager::GithubActions
                    && dep.source == rustovate_config::Source::GithubTags
                {
                    Some("b".repeat(40))
                } else {
                    None
                },
                update_type: (*kind).into(),
                group: None,
                policy: None,
                release_age: None,
            })
            .collect();
        let r = ReportDependency {
            manager: word(&dep.manager),
            name: dep.name.clone(),
            package: dep.package.clone(),
            occurrence: None,
            current: Current {
                value: Some(dep.current.clone()),
                version: dep.resolved.clone(),
                digest: dep.digest.clone(),
            },
            metadata: None,
            updates,
        };
        if let Some(file) = repo.package_files.iter_mut().find(|f| f.path == dep.file) {
            file.dependencies.push(r);
        } else {
            repo.package_files.push(PackageFile {
                path: dep.file.clone(),
                dependencies: vec![r],
            });
        }
    }
    let plans = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now()).unwrap();
    let names: BTreeSet<_> = plans
        .iter()
        .filter(|p| p.action.status == ActionStatus::Planned)
        .map(|p| p.action.branch.as_str())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "rustovate/actions-checkout-4.x",
            "rustovate/actions-checkout-7.x",
            "rustovate/docker-node-20.x",
            "rustovate/docker-node-26.x",
            "rustovate/jdx-mise-action-5.x",
            "rustovate/jqlang-jq-1.x",
            "rustovate/mise-node-22.x",
            "rustovate/mise-node-26.x",
            "rustovate/nginx-1.x",
            "rustovate/oven-sh-bun-1.x",
            "rustovate/pin-dependencies",
            "rustovate/postgres-16.x",
            "rustovate/postgres-18.x",
            "rustovate/redis-7.x",
            "rustovate/redis-8.x",
            "rustovate/ubuntu-26.x",
        ])
    );
    let pins = plans
        .iter()
        .find(|p| p.action.branch == "rustovate/pin-dependencies")
        .unwrap();
    assert_eq!(pins.changes.len(), 6);
    assert_eq!(pins.action.message, "chore(deps): pin dependencies");
    assert!(pins.contents.contains_key("package.json"));
    assert!(!pins.lockfiles_ready);
    assert!(!pins.action.lockfile_pending);
    assert_eq!(
        plans
            .iter()
            .filter(|p| p.action.status == ActionStatus::Skipped)
            .count(),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn pin_title_respects_prefix_and_pin_rows_never_request_release_notes() {
    let (mut config, mut repo, deps, snapshot) = setup("22.0.0", false);
    config.defaults.commit_prefix = "build(deps):".into();
    repo.package_files[0].dependencies[0].updates[0].update_type = "pin".into();
    let mut plans = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now()).unwrap();
    assert_eq!(plans[0].action.message, "build(deps): pin dependencies");
    plans[0].changes[0].source = "github-tags".into();
    plans[0].changes[0].package = "actions/checkout".into();
    plans[0].changes[0].release_notes = true;
    let mut other = plans[0].changes[0].clone();
    other.source = "docker".into();
    plans[0].changes.push(other);
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    pr_cycle(&github, &repo, &config, &mut plans, &snapshot).await;
    let s = server.0.lock().unwrap();
    assert_eq!(s.prs[&1]["title"], "build(deps): pin dependencies");
    let text = s.prs[&1]["body"].as_str().unwrap();
    assert!(text.contains("| Package | Change | Type | File |"));
    assert!(text.contains("| pin |"));
    assert!(!text.to_lowercase().contains("release notes"));
    assert!(!s.requests.iter().any(|(_, p)| p.contains("/releases/")));
}

#[tokio::test(start_paused = true)]
async fn batch_ref_race_keeps_every_prepared_branch_unpublished() {
    let (p, snapshot) = make_plan("22.0.0");
    let (other, _) = make_plan("24.0.0");
    let mut plans = vec![p, other];
    let server = fake(&snapshot);
    server.0.lock().unwrap().race = true;
    let github = client(server.clone()).await;
    rustovate_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        Rebase::Behind,
        false,
    )
    .await;
    assert!(
        plans
            .iter()
            .all(|p| p.action.status == ActionStatus::Error && p.action.commit.is_none())
    );
    let state = server.0.lock().unwrap();
    assert_eq!(state.refs.len(), 1);
    let mutations: Vec<_> = state
        .writes
        .iter()
        .filter(|(_, b)| b["query"] == rustovate_update::REF_UPDATE)
        .collect();
    assert_eq!(mutations.len(), 1);
    assert_eq!(
        mutations[0].1["variables"]["input"]["refUpdates"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test(start_paused = true)]
async fn generated_lockfile_additions_and_deletions_share_the_manifest_commit() {
    let (mut p, mut snapshot) = make_plan("22.0.0");
    let old = "obsolete checksum\n";
    snapshot.files.insert(
        "go.sum".into(),
        Blob {
            sha: blob_sha(old),
            mode: "100644".into(),
            content: old.into(),
        },
    );
    snapshot.files.insert(
        "Cargo.lock".into(),
        Blob {
            sha: String::new(),
            mode: "100644".into(),
            content: String::new(),
        },
    );
    for (path, sha, content) in [
        ("go.sum", blob_sha(old), ""),
        ("Cargo.lock", String::new(), "version = 4\n"),
    ] {
        p.contents.insert(path.into(), content.into());
        p.action.files.push(rustovate_report::FileAction {
            path: path.into(),
            blob_sha: sha,
            diff: String::new(),
        });
    }
    p.deleted.insert("go.sum".into());
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    apply(&github, &mut p, &snapshot, "main", Rebase::Behind, false).await;
    assert_eq!(p.action.status, ActionStatus::Created, "{:?}", p.action);
    let s = server.0.lock().unwrap();
    let commit = &s.commits[p.action.commit.as_ref().unwrap()];
    let tree = s.trees[commit["tree"]["sha"].as_str().unwrap()]
        .as_array()
        .unwrap();
    assert!(!tree.iter().any(|f| f["path"] == "go.sum"));
    assert!(
        tree.iter()
            .any(|f| f["path"] == "Cargo.lock" && f["sha"] == blob_sha("version = 4\n"))
    );
    assert!(
        tree.iter()
            .any(|f| f["path"] == "mise.toml" && f["sha"] == blob_sha(&p.contents["mise.toml"]))
    );
    assert_eq!(
        s.writes
            .iter()
            .filter(|(path, _)| path.ends_with("/git/commits"))
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn owned_readable_branch_and_pr_can_follow_a_changed_track_identity() {
    let (config, repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    let (mut next, _) = make_plan("22.1.0");
    next.identity = "c".repeat(64);
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        &snapshot,
    )
    .await;
    assert_eq!(next.action.status, ActionStatus::Updated);
    assert_eq!(actions[0].status, ActionStatus::Updated);
    assert!(
        server.0.lock().unwrap().prs[&1]["body"]
            .as_str()
            .unwrap()
            .contains(&next.identity)
    );
}

#[tokio::test(start_paused = true)]
async fn merged_pr_branch_can_be_reused_for_the_next_update_on_its_major_line() {
    let (config, mut repo) = pr_config();
    let (mut p, mut snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    {
        let mut s = server.0.lock().unwrap();
        let sha = p.action.commit.as_ref().unwrap();
        s.merged_heads.insert(sha.clone());
        s.prs.get_mut(&1).unwrap()["state"] = json!("closed");
        s.prs.get_mut(&1).unwrap()["merged_at"] = json!(chrono::Utc::now().to_rfc3339());
        s.base = "base2".into();
        s.refs.insert("refs/heads/main".into(), "base2".into());
        let tree = s.commits[sha]["tree"].clone();
        s.commits.insert("base2".into(), json!({"tree":tree}));
    }
    let blob = snapshot.files.get_mut("mise.toml").unwrap();
    blob.content = blob.content.replace("20.0.0", "22.0.0");
    blob.sha = blob_sha(&blob.content);
    let deps = Scanner::new(&config)
        .unwrap()
        .extract(&Files::from([("mise.toml".into(), blob.content.clone())]))
        .unwrap()
        .dependencies;
    repo.commit = Some("base2".into());
    let r = &mut repo.package_files[0].dependencies[0];
    r.current.value = Some("22.0.0".into());
    r.current.version = Some("22.0.0".into());
    r.updates[0].update_type = "minor".into();
    r.updates[0].new_value = Some("22.1.0".into());
    r.updates[0].new_version = Some("22.1.0".into());
    let mut plans = plan(&repo, &config, &deps, &snapshot, false, chrono::Utc::now()).unwrap();
    assert_eq!(plans[0].action.branch, p.action.branch);
    let actions = pr_cycle(&github, &repo, &config, &mut plans, &snapshot).await;
    assert_eq!(
        plans[0].action.status,
        ActionStatus::Rebased,
        "{:?}",
        plans[0].action
    );
    assert_eq!(actions[0].status, ActionStatus::Created);
    assert_eq!(server.0.lock().unwrap().prs.len(), 2);
}
