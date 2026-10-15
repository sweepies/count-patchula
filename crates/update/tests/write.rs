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
    latency: std::time::Duration,
    refs: BTreeMap<String, String>,
    trees: BTreeMap<String, Value>,
    commits: BTreeMap<String, Value>,
    writes: Vec<(String, Value)>,
    unsigned: bool,
    race: bool,
    truncated: bool,
    history: Vec<String>,
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
            if body["query"] == "query{viewer{login}}" {
                response(200, json!({"data":{"viewer":{"login":"rustovate[bot]"}}}))
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
            if path.ends_with("/git/blobs") {
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
                    tree.as_array_mut().unwrap().push(entry.clone());
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
        } else if path == "/repos/demo/app/branches" {
            response(200, json!(state.refs.iter().map(|(n, sha)| json!({"name":n.strip_prefix("refs/heads/").unwrap(),"commit":{"sha":sha}})).collect::<Vec<_>>()))
        } else if let Some(sha) = path.strip_prefix("/repos/demo/app/commits/") {
            response(200, json!({"commit":state.commits[sha]}))
        } else if path == "/repos/demo/app" {
            response(200, json!({"node_id":"repo-id","default_branch":"main"}))
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
            let commits: Vec<_> = history.iter().map(|sha| {
                let commit = &state.commits[*sha];
                json!({"sha":sha,"author":{"login":commit.get("actor").and_then(Value::as_str).unwrap_or("rustovate[bot]")},"committer":{"login":commit.get("committer_login").and_then(Value::as_str).unwrap_or("web-flow")},"commit":commit})
            }).collect();
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
fn lockfile_plans_skip_by_default_and_opt_in_marks_manifest_only_updates() {
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
    assert_eq!(state.requests.len(), 4 + 12 * 4);
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
