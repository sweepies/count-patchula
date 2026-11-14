//! The same in-repo Pkl and token-only engine entry point used by the sandbox.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_http::{Exchange, Transport};
use patchula_report::{BranchStatus, Report, Status};
use patchula_testkit::github::{self, FakeGitHub, Hosts, State};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard},
};

/// The write receipts patchula_update stores, read through the same
/// namespace and scope.
#[derive(serde::Serialize, serde::Deserialize)]
struct Receipts(Value);
#[derive(serde::Serialize)]
struct ReceiptsScope {
    repository: String,
    branch_prefix: String,
}
impl patchula_cache::State for Receipts {
    const NAMESPACE: &'static str = "verified-write-receipts";
    const VERSION: u32 = 1;
    type Scope = ReceiptsScope;
}

/// The sandbox repository and the scripted registry and advisory hosts
/// every write test shares: GitHub comes from the shared
/// [`FakeGitHub`]; `registry.npmjs.org` and `api.osv.dev` are scripted
/// routes on a [`Hosts`] router.
struct Sandbox {
    github: Arc<FakeGitHub>,
    hosts: Arc<Hosts>,
    /// The npm registry document.
    catalog: Arc<Mutex<Value>>,
    /// Registry reads fail while set.
    fail_lookup: Arc<std::sync::atomic::AtomicBool>,
}
impl Sandbox {
    /// The locked GitHub state: arrange what the server holds and read
    /// what it recorded.
    fn state(&self) -> MutexGuard<'_, State> {
        self.github.state()
    }
    /// `(method, path, body)` for every request — GitHub and scripted
    /// hosts together — in arrival order.
    fn log(&self) -> Vec<(String, String, Value)> {
        self.hosts.log()
    }
    /// Forget every recorded request.
    fn clear_log(&self) {
        self.hosts.clear();
    }
}
impl Transport for Sandbox {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        self.hosts.send(request)
    }
}

/// The files every sandboxed repository starts with.
fn base_files() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n".into(),
        ),
        (
            "package.json".into(),
            "{\n  \"dependencies\": {\"fixture\": \"1.0.0\"}\n}\n".into(),
        ),
    ])
}

/// A `demo/sandbox` repository holding `files` — `lock` names an extra
/// lock-file entry whose blob serves `{}`; `extra` entries join the tree
/// verbatim — behind a router scripting the npm registry and OSV.
fn sandbox(files: BTreeMap<String, String>, lock: Option<&str>, extra: Vec<Value>) -> Arc<Sandbox> {
    let github = Arc::new(FakeGitHub::new("demo/sandbox"));
    let hosts = Arc::new(Hosts::new(github.clone()));
    let catalog = Arc::new(Mutex::new(
        json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}}}),
    ));
    let fail_lookup = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let osv = Arc::new(Mutex::new(json!({"results":[{}]})));
    {
        let catalog = catalog.clone();
        let fail = fail_lookup.clone();
        hosts.script("registry.npmjs.org", move |_, _| {
            if fail.load(std::sync::atomic::Ordering::Relaxed) {
                github::json(503, json!({}))
            } else {
                github::json(200, catalog.lock().unwrap().clone())
            }
        });
    }
    {
        let osv = osv.clone();
        hosts.script("api.osv.dev", move |_, _| {
            github::json(200, osv.lock().unwrap().clone())
        });
    }
    let mut extra = extra;
    if let Some(lock) = lock {
        extra.push(json!({"path":lock,"sha":"unselected-lock","type":"blob","mode":"100644"}));
    }
    github.seed_files("main", "base", &files, extra);
    {
        let mut state = github.state();
        let repo = state.repo_mut();
        if lock.is_some() {
            repo.blobs.insert("unselected-lock".into(), "{}".into());
        }
        repo.checks =
            vec![json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"})];
    }
    Arc::new(Sandbox {
        github,
        hosts,
        catalog,
        fail_lookup,
    })
}

/// The one signed patchula commit a branch head carries — the same commit
/// the write path proves it owns before deleting.
fn ours(sha: &str) -> Value {
    json!({"sha":sha,"tree":{"sha":"base-tree"},
           "message":format!("update\n\nPatchula: v1\nPatchula-Branch: b\nPatchula-Base: {}", "b".repeat(40)),
           "verification":{"verified":true}})
}

/// A `heads/name` ref at `sha` — one owned patchula commit — for the
/// `/branches` listing cleanup inspects.
fn branch(github: &FakeGitHub, name: &str, sha: &str) {
    let mut state = github.state();
    let repo = state.repo_mut();
    repo.refs.insert(format!("refs/heads/{name}"), sha.into());
    repo.commits.insert(sha.into(), ours(sha));
}

struct Rotating(std::sync::atomic::AtomicUsize);
struct Authenticated(Arc<Sandbox>);
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
impl patchula_github::Credentials for Rotating {
    fn scope(&self) -> &str {
        "offline-app:1/installation:10"
    }
    fn token(&self) -> patchula_github::Token<'_> {
        let count = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Box::pin(async move { Ok(format!("offline-token-{count}")) })
    }
}

/// Hash this test's recorded requests and compare them with the snapshot;
/// `PATCHULA_UPDATE_TRANSCRIPTS=1` rewrites it instead.
fn check_transcript() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

/// Pin this test's run depths: each `elapsed` is a repetition's virtual
/// time on the paused clock divided by the fake's per-request `latency` —
/// the round trips on the run's critical path. The pin is exact: every
/// repetition must end the pinned number of waves after it began, and an
/// elapsed off the latency grid fails too — a wobble means a real wait
/// reached the run without the clock seeing it. The transcript multiset
/// still pins which requests are made; depth only pins when they are
/// sent.
fn check_depth(latency: std::time::Duration, samples: Vec<std::time::Duration>) {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/depth.tsv");
    patchula_testkit::transcripts::depth(&snapshot, latency, samples);
}
#[tokio::test]
async fn installation_and_caller_token_batches_match_and_token_rotation_keeps_receipts_warm() {
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let cli_fake = sandbox(base_files(), None, vec![]);
    let server_fake = sandbox(base_files(), None, vec![]);
    let cli_cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let server_cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let credentials = Arc::new(Rotating(Default::default()));
    let cli = Engine::with_transport("offline-caller-token", cli_cache, 4, cli_fake)
        .unwrap()
        .with_versions_host(false)
        .with_recheck(patchula_words::Recheck::Now);
    let server = Engine::with_credentials(
        credentials.clone(),
        server_cache,
        4,
        Arc::new(Authenticated(server_fake.clone())),
        Default::default(),
        Default::default(),
    )
    .unwrap()
    .with_recheck(patchula_words::Recheck::Now);
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    let mut cli_report = cli.run_at(options(), false, false, now).await.unwrap();
    let mut server_report = server.run_at(options(), false, false, now).await.unwrap();
    cli_report.stats = None;
    server_report.stats = None;
    assert_eq!(cli_report, server_report);
    server_fake.state().writes.clear();
    let repeated = server.run_at(options(), false, false, now).await.unwrap();
    assert_eq!(
        repeated.actions.unwrap().branches[0].status,
        BranchStatus::Unchanged
    );
    assert!(server_fake.state().writes.is_empty());
    assert!(credentials.0.load(std::sync::atomic::Ordering::Relaxed) > 2);
    check_transcript();
}

/// A new commit that leaves the files the scan reads as they were reuses the
/// earlier extraction and config, and reads no blob again.
#[tokio::test]
async fn a_commit_that_changes_no_read_file_reuses_the_extraction() {
    let fake = sandbox(base_files(), None, vec![]);
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
            true,
            false,
        )
    };
    let first = run().await.unwrap();
    assert_eq!(first.stats.as_ref().unwrap().extraction_hits, 0);
    {
        let mut state = fake.state();
        let repo = state.repo_mut();
        repo.move_base("moved");
        repo.refs.insert("refs/heads/main".into(), "moved".into());
        repo.commits.insert("moved".into(), ours("moved"));
    }
    fake.clear_log();
    let second = run().await.unwrap();
    assert_eq!(second.repositories[0].commit.as_deref(), Some("moved"));
    assert_eq!(second.stats.as_ref().unwrap().extraction_hits, 1);
    let reads = fake.log();
    assert!(
        !reads
            .iter()
            .any(|(_, path, body)| path.contains("/git/blobs/")
                || body["query"]
                    .as_str()
                    .is_some_and(|q| q.contains("... on Blob"))),
        "{reads:?}"
    );
    let found = |report: &patchula_report::Report| {
        report.repositories[0]
            .package_files
            .iter()
            .flat_map(|file| &file.dependencies)
            .map(|dep| (dep.name.clone(), dep.current.clone(), dep.updates.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(found(&second), found(&first));
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn steady_runs_only_lookup_and_one_inventory_with_open_or_closed_updates() {
    let mut keep = Vec::new();
    for closed in [false, true] {
        let fake = sandbox(base_files(), None, vec![]);
        keep.push(fake.clone());
        fake.state().settings.latency = std::time::Duration::from_millis(200);
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
            BranchStatus::Created
        );
        if closed {
            fake.state().repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
            let verified = run().await.unwrap();
            assert_eq!(
                verified.actions.as_ref().unwrap().branches[0].status,
                BranchStatus::Skipped
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
        fake.clear_log();
        fake.state().writes.clear();
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
            fake.state().writes.is_empty(),
            "steady dashboard must not mutate"
        );
        assert_eq!(
            report.actions.as_ref().unwrap().branches[0].status,
            if closed {
                BranchStatus::Skipped
            } else {
                BranchStatus::Unchanged
            }
        );
        let requests = fake.log();
        assert_eq!(requests.len(), 4, "{requests:?}");
        assert_eq!(
            requests
                .iter()
                .filter(|(_, path, body)| path == "/graphql"
                    && body["query"] == patchula_update::INVENTORY_QUERY)
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
    check_transcript();
}

/// GitHub's branch rename keeps an open pull request and repoints its
/// head: moved outside `prs.branch`, the branch is invisible to the
/// steady-state refs walk. The receipts still name it, so the run must
/// take the full write path — the REST pull listing and cleanup's own
/// branch list see every branch — and the renamed branch's pull request
/// is closed as obsolete and the branch deleted, exactly as an
/// unfiltered run would.
#[tokio::test(start_paused = true)]
async fn a_branch_renamed_outside_the_prefix_still_takes_the_full_write_path() {
    let fake = sandbox(base_files(), None, vec![]);
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
    let first = run().await.unwrap();
    assert_eq!(
        first.actions.as_ref().unwrap().branches[0].status,
        BranchStatus::Created
    );
    {
        let mut state = fake.state();
        let repo = state.repo_mut();
        // The rename: the ref moves outside the prefix and the open pull
        // request's head follows it — the filtered refs walk cannot see
        // either.
        let (name, sha) = repo
            .refs
            .iter()
            .find(|(name, _)| name.starts_with("refs/heads/patchula/"))
            .map(|(name, sha)| (name.clone(), sha.clone()))
            .unwrap();
        repo.refs.remove(&name);
        repo.refs.insert("refs/heads/human/renamed".into(), sha);
        repo.prs.get_mut(&1).unwrap()["head"]["ref"] = json!("human/renamed");
        // Drop the update itself, so no plan names the receipted branch.
        let content = "{\"dependencies\":{\"fixture\":\"2.0.0\"}}\n";
        let sha = patchula_github::blob_sha(content.as_bytes());
        repo.blobs.insert(sha.clone(), content.into());
        for entry in repo
            .trees
            .get_mut("base-tree")
            .unwrap()
            .as_array_mut()
            .unwrap()
        {
            if entry["path"] == "package.json" {
                entry["sha"] = json!(sha);
            }
        }
    }
    fake.clear_log();
    let report = run().await.unwrap();
    assert_eq!(report.repositories[0].status, Status::Complete);
    {
        let state = fake.state();
        let repo = state.repo();
        assert_eq!(repo.prs[&1]["state"], "closed");
        assert!(!repo.refs.contains_key("refs/heads/human/renamed"));
    }
    // The filtered walk ran — `refs(query:)` held the prefix — and the
    // receipted branch's absence took the full write path: the REST pull
    // listing and cleanup's own branch list, not the settled run's few
    // requests.
    let requests = fake.log();
    assert!(
        requests
            .iter()
            .any(|(_, path, body)| path == "/graphql"
                && body["variables"]["refQuery"] == "patchula/"),
        "{requests:?}"
    );
    for listed in ["pulls", "branches"] {
        assert!(
            requests.iter().any(|(method, path, _)| method == "GET"
                && path == &format!("/repos/demo/sandbox/{listed}")),
            "{listed}: {requests:?}"
        );
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn dashboard_renames_are_preserved_and_disabling_closes_only_the_owned_issue() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache.clone(), 4, fake.clone())
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
    fake.state().repo_mut().issues.get_mut(&100).unwrap()["title"] = json!("Owner dashboard title");
    fake.state().writes.clear();
    run(None).await.unwrap();
    assert!(fake.state().writes.is_empty());
    assert_eq!(
        fake.state().repo().issues[&100]["title"],
        "Owner dashboard title"
    );
    // Simulate the previous layout while retaining the cached content hash.
    {
        let mut state = fake.state();
        let issue = state.repo_mut().issues.get_mut(&100).unwrap();
        let body = issue["body"].as_str().unwrap();
        let (actions, details) = body.split_once("\n<details>").unwrap();
        let (intro, prs) = actions.split_once("\n## Open PRs").unwrap();
        issue["body"] = json!(format!("{intro}\n<details>{details}\n## Open PRs{prs}"));
    }
    run(None).await.unwrap();
    {
        let writes = fake.state().writes.clone();
        assert_eq!(
            writes.len(),
            1,
            "layout migration must only patch the dashboard"
        );
        assert!(writes[0].0.contains("/issues/"));
    }
    fake.state().writes.clear();
    run(None).await.unwrap();
    assert!(
        fake.state().writes.is_empty(),
        "unchanged content hash must converge after one patch"
    );
    let mut config =
        patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap();
    config.prs.dashboard = false;
    config.defaults.wait_hours = 0;
    assert!(
        patchula_update::dashboard::issue_number(&cache, "demo/sandbox")
            .await
            .is_some()
    );
    run(Some(config)).await.unwrap();
    assert_eq!(fake.state().repo().issues[&100]["state"], "CLOSED");
    assert_eq!(
        patchula_update::dashboard::issue_number(&cache, "demo/sandbox").await,
        None,
        "a closed dashboard is forgotten"
    );
    fake.state().writes.clear();
    let mut config =
        patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap();
    config.prs.dashboard = false;
    config.defaults.wait_hours = 0;
    run(Some(config)).await.unwrap();
    assert!(fake.state().writes.is_empty());
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn owner_closure_during_publication_keeps_repository_successful() {
    let fake = sandbox(base_files(), None, vec![]);
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
    fake.state().repo_mut().prs.get_mut(&1).unwrap()["title"] = json!("operator title");
    fake.github.close_pulls_on_read();
    let report = run().await.unwrap();
    assert_eq!(report.repositories[0].status, Status::Complete);
    let pr = &report.actions.as_ref().unwrap().pull_requests[0];
    assert_eq!(pr.status, patchula_report::PullRequestStatus::Skipped);
    assert!(pr.reason.as_deref().unwrap().contains("closed-by-owner"));
    check_transcript();
}
#[tokio::test(start_paused = true)]
async fn token_only_run_uses_repository_config_cold_and_cached_and_reports_actions() {
    let mut keep = Vec::new();
    for (lock, allow, dry_run) in [
        (false, false, true),
        (false, false, false),
        (true, false, false),
        (true, true, true),
        (true, true, false),
    ] {
        let fake = sandbox(base_files(), lock.then_some("bun.lockb"), vec![]);
        keep.push(fake.clone());
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
            assert_eq!(
                actions.schema_version,
                patchula_report::ACTIONS_SCHEMA_VERSION
            );
            assert_eq!(actions.dry_run, dry_run);
            assert_eq!(actions.branches.len(), 1, "{actions:?}");
            let action = &actions.branches[0];
            assert_eq!(action.lockfile_pending, lock);
            assert_eq!(
                action.status,
                if lock && !allow {
                    BranchStatus::Skipped
                } else if dry_run {
                    BranchStatus::Planned
                } else {
                    if repeat == 0 {
                        BranchStatus::Created
                    } else {
                        BranchStatus::Unchanged
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
                assert!(fake.state().writes.is_empty());
            } else if lock && !allow {
                assert!(
                    fake.state()
                        .writes
                        .iter()
                        .all(|(p, _)| p.contains("/issues"))
                );
            }
            if repeat == 1 {
                assert_eq!(report.stats.as_ref().unwrap().extraction_hits, 1);
            }
        }
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn an_advertised_server_is_hidden_in_the_pr_and_the_dashboard() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let mut engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Options::default()
    };
    // Turning it on later still reaches the PR the first run wrote.
    for server in [None, Some("https://patchula.acme.dev")] {
        fake.state().writes.clear();
        engine.set_advertised_server(server.map(str::to_owned));
        let report = engine.run(options(), false, false).await.unwrap();
        assert_eq!(
            report.repositories[0].status,
            Status::Complete,
            "{report:?}"
        );
    }
    let writes = fake.state().writes.clone();
    for kind in ["/pulls", "/issues"] {
        let body = writes
            .iter()
            .filter(|(path, _)| path.contains(kind))
            .find_map(|(_, body)| body["body"].as_str())
            .unwrap_or_else(|| panic!("no {kind} write: {writes:?}"));
        assert_eq!(
            patchula_update::advertise::find(body),
            Some("https://patchula.acme.dev"),
            "{kind}"
        );
    }
    check_transcript();
}

struct FailTools;

/// GitHub offers native auto-merge only on a PR that cannot be merged yet, so
/// a PR whose merge state is CLEAN, UNSTABLE (non-required checks failing or
/// pending) or HAS_HOOKS is refused with "is in <state> status". That is not an
/// error, and no licence to merge: the Count's own merge takes over on a later
/// run, behind the same state and check gate. Other states stay errors.
#[tokio::test(start_paused = true)]
async fn a_pr_github_finds_mergeable_now_is_merged_by_the_count_only_when_clean_and_green() {
    // (state in the refusal, state of the PR on the next run, checks, merges)
    let mut keep = Vec::new();
    for (refusal, state, checks, merges) in [
        ("clean", "clean", "none", false),
        ("clean", "clean", "pass", true),
        ("unstable", "unstable", "fail", false),
        ("unstable", "clean", "pass", true),
        ("has_hooks", "has_hooks", "pass", false),
        ("dirty", "dirty", "pass", false),
    ] {
        let fake = sandbox(base_files(), None, vec![]);
        keep.push(fake.clone());
        fake.state().repo_mut().merge.auto_merge = true;
        fake.github
            .auto_merge_refused(&format!("Pull request Pull request is in {refusal} status"));
        match checks {
            "none" => fake.state().repo_mut().checks.clear(),
            "fail" => fake.state().repo_mut().checks[0]["conclusion"] = json!("FAILURE"),
            _ => {}
        }
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let mut config = checkbox_config();
        config.defaults.merge = true;
        let now = "2026-10-15T12:00:00Z".parse().unwrap();
        let first = engine
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
        let actions = first.actions.unwrap();
        // The refusal of a state that is not mergeable now is still an error.
        assert_eq!(
            actions
                .pull_requests
                .iter()
                .any(|a| a.status == patchula_report::PullRequestStatus::Error),
            refusal == "dirty",
            "{refusal}: {actions:?}"
        );
        if refusal == "dirty" {
            continue;
        }
        assert_eq!(
            actions.branches[0].status,
            BranchStatus::Created,
            "{refusal}"
        );
        let enables = fake
            .state()
            .writes
            .iter()
            .filter(|(_, b)| {
                b["query"]
                    .as_str()
                    .is_some_and(|q| q.contains("enablePullRequestAutoMerge"))
            })
            .count();
        assert_eq!(enables, 1, "{refusal}: GitHub is asked once");
        assert!(fake.state().repo_mut().prs[&1]["auto_merge"].is_null());
        assert!(
            !fake
                .log()
                .iter()
                .any(|(_, path, body)| path.ends_with("/merge")
                    || body["query"] == patchula_update::automerge::CHECKS_QUERY),
            "{refusal}: a new PR waits for a later run"
        );
        fake.state().repo_mut().prs.get_mut(&1).unwrap()["mergeable_state"] = json!(state);
        fake.clear_log();
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
        let actions = &report.actions.as_ref().unwrap().pull_requests;
        assert!(
            actions
                .iter()
                .all(|a| a.status != patchula_report::PullRequestStatus::Error),
            "{refusal}: {actions:?}"
        );
        assert_eq!(
            actions
                .iter()
                .any(|a| a.status == patchula_report::PullRequestStatus::Merged),
            merges,
            "{refusal}/{state}/{checks}: {actions:?}"
        );
        let merged = fake
            .log()
            .iter()
            .any(|(_, path, _)| path.ends_with("/merge"));
        assert_eq!(merged, merges, "{refusal}/{state}/{checks}");
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn fallback_auto_merge_requires_a_prior_receipt_and_every_current_head_check() {
    let mut keep = Vec::new();
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
        let fake = sandbox(base_files(), None, vec![]);
        keep.push(fake.clone());
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
            BranchStatus::Created
        );
        assert!(
            !fake
                .log()
                .iter()
                .any(|(_, path, body)| path.ends_with("/merge")
                    || body["query"] == patchula_update::automerge::CHECKS_QUERY),
            "{case}: new PR must wait for another run"
        );
        match case {
            "neutral-skipped-status" => fake.state().repo_mut().checks = vec![
                json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"NEUTRAL"}),
                json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"SKIPPED"}),
                json!({"__typename":"StatusContext","state":"SUCCESS"}),
            ],
            "status-only" => fake.state().repo_mut().checks = vec![json!({"__typename":"StatusContext","state":"SUCCESS"})],
            "pending" => fake.state().repo_mut().checks[0]["status"] = json!("IN_PROGRESS"),
            "failure" => fake.state().repo_mut().checks[0]["conclusion"] = json!("FAILURE"),
            "cancelled" => fake.state().repo_mut().checks[0]["conclusion"] = json!("CANCELLED"),
            "no-checks" => fake.state().repo_mut().checks.clear(),
            "pending-status" | "failed-status" => fake.state().repo_mut().checks.push(json!({"__typename":"StatusContext","state":if case == "pending-status" {"PENDING"} else {"FAILURE"}})),
            "late-page-failure" => {
                let green = fake.state().repo_mut().checks[0].clone();
                fake.state().repo_mut().checks = vec![green; 101];
                fake.state().repo_mut().checks[100]["conclusion"] = json!("FAILURE");
            }
            "draft" => fake.state().repo_mut().prs.get_mut(&1).unwrap()["draft"] = json!(true),
            "conflict" => fake.state().repo_mut().prs.get_mut(&1).unwrap()["mergeable"] = json!(false),
            "protected" | "behind" | "unknown" => fake.state().repo_mut().prs.get_mut(&1).unwrap()["mergeable_state"] = json!(if case == "protected" {"blocked"} else {case}),
            "head-race" => fake.github.checks_move_head("human-commit"),
            "human-head" => fake.state().repo_mut().prs.get_mut(&1).unwrap()["head"]["sha"] = json!("human-commit"),
            "human-owner" => fake.state().repo_mut().prs.get_mut(&1).unwrap()["user"]["login"] = json!("human"),
            "api-refusal" => fake.github.pull_merge_refused(),
            "delete-race" => fake.github.ref_delete_fails("head changed"),
            _ => {}
        }
        // The head the client knows before this run (delete-race zeroes the
        // recorded PR head when the branch ref is deleted).
        let head = fake.state().repo().prs[&1]["head"]["sha"].clone();
        fake.clear_log();
        fake.state().writes.clear();
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
            actions
                .iter()
                .any(|a| a.status == patchula_report::PullRequestStatus::Merged),
            merges,
            "{case}: {report:?}"
        );
        let requests = fake.log();
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
            assert_eq!(*body, json!({"sha":head,"merge_method":"squash"}));
        }
        let deletes: Vec<_> = requests
            .iter()
            .filter(|(_, _, b)| {
                b["query"] == patchula_update::REF_UPDATE
                    && b["variables"]["input"]["refUpdates"][0]["afterOid"]
                        == "0000000000000000000000000000000000000000"
            })
            .collect();
        assert_eq!(deletes.len(), usize::from(merges), "{case}");
        if let Some((_, _, body)) = deletes.first() {
            assert_eq!(
                body["variables"]["input"]["refUpdates"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                body["variables"]["input"]["refUpdates"][0]["beforeOid"],
                head
            );
        }
        let dashboard = fake.state().repo().issues[&100]["body"]
            .as_str()
            .unwrap()
            .to_owned();
        if merges {
            assert!(
                dashboard.contains("## Merged") && dashboard.contains("merged by patchula"),
                "{case}: {dashboard}"
            );
            let receipts = cache
                .state::<Receipts>(&ReceiptsScope {
                    repository: "demo/sandbox".into(),
                    branch_prefix: "patchula/".into(),
                })
                .await
                .unwrap()
                .unwrap()
                .0;
            assert!(
                receipts["writes"].as_object().unwrap().is_empty(),
                "merged heads must not remain eligible for another merge"
            );
            let github = patchula_github::Client::with_transport(
                "offline-installation",
                cache.clone(),
                Default::default(),
                fake.clone(),
            )
            .unwrap();
            let inventory =
                patchula_update::Inventory::read(&github, &cache, &report.repositories[0], &config)
                    .await
                    .unwrap();
            let prs = patchula_update::automerge::dashboard_history(
                &inventory,
                inventory.pull_requests("demo/sandbox"),
                &[],
            );
            let next_dashboard = patchula_update::dashboard::render(
                &report.repositories[0],
                &[],
                &prs.open,
                &prs.merged,
                Default::default(),
                None,
            );
            assert!(
                next_dashboard.contains("## Merged")
                    && next_dashboard.contains("merged by patchula"),
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
                    .filter(|(_, _, b)| b["query"] == patchula_update::automerge::CHECKS_QUERY)
                    .count(),
                2
            );
        }
        if matches!(case, "human-head" | "human-owner") {
            assert!(
                !requests
                    .iter()
                    .any(|(_, _, b)| b["query"] == patchula_update::automerge::CHECKS_QUERY)
            );
        }
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn auto_merge_uses_inventory_methods_and_prefers_native_without_status_reads() {
    let mut keep = Vec::new();
    for method in ["native", "squash", "merge", "rebase", "none"] {
        let fake = sandbox(base_files(), None, vec![]);
        keep.push(fake.clone());
        {
            let mut state = fake.state();
            let merge = &mut state.repo_mut().merge;
            merge.auto_merge = method == "native";
            merge.squash = matches!(method, "native" | "squash");
            merge.merge_commit = matches!(method, "native" | "squash" | "merge");
            merge.rebase = method != "none";
            merge.delete_branch = true;
        }
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let mut config = checkbox_config();
        config.defaults.merge = true;
        let now = chrono::Utc::now();
        checkbox_run(&engine, &config, now).await;
        fake.clear_log();
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
        let requests = fake.log();
        assert_eq!(
            requests
                .iter()
                .filter(|(_, _, b)| b["query"] == patchula_update::INVENTORY_QUERY)
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
                .any(|(_, _, b)| b["query"] == patchula_update::REF_UPDATE),
            "GitHub deletes branches in this case"
        );
        if method == "native" {
            assert!(!fake.state().repo_mut().prs[&1]["auto_merge"].is_null());
            assert!(!requests.iter().any(|(_, path, b)| path.ends_with("/merge")
                || b["query"] == patchula_update::automerge::CHECKS_QUERY));
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
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn manual_create_age_and_schedule_permits_do_not_authorize_fallback_merging() {
    let mut keep = Vec::new();
    for hold in ["age", "schedule"] {
        let fake = sandbox(base_files(), None, vec![]);
        keep.push(fake.clone());
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
            config.defaults.windows = vec![patchula_config::Window {
                cron: vec!["0 0 1 1 *".into()],
                zone: "UTC".into(),
            }];
        }
        checkbox_run(&engine, &config, now).await;
        tick_dashboard(&fake, "create");
        checkbox_run(&engine, &config, now).await;
        assert_eq!(fake.state().repo().prs.len(), 1);
        fake.clear_log();
        checkbox_run(&engine, &config, now).await;
        assert!(
            !fake
                .log()
                .iter()
                .any(|(_, path, b)| path.ends_with("/merge")
                    || b["query"] == patchula_update::automerge::CHECKS_QUERY),
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
                    .any(|a| a.status == patchula_report::PullRequestStatus::Merged)
            );
        }
    }
    check_transcript();
}

fn checkbox_config() -> patchula_config::Config {
    let mut config =
        patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap();
    config.prs.dashboard = true;
    config.defaults.wait_hours = 0;
    config
}

#[tokio::test(start_paused = true)]
async fn age_cached_catalogs_refresh_for_checked_pr_and_repository_controls_only() {
    let mut files = base_files();
    files.insert(
        "package.json".into(),
        json!({"dependencies":{"fixture":"1.0.0","other":"1.0.0"}}).to_string(),
    );
    let fake = sandbox(files, None, vec![]);
    *fake.catalog.lock().unwrap() = json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}},"time":{"1.0.0":"2020-01-01T00:00:00Z","2.0.0":"2021-01-01T00:00:00Z"}});
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let mut config = checkbox_config();
    config.defaults.wait_hours = 24;
    let now: chrono::DateTime<chrono::Utc> = "2026-10-15T12:00:00Z".parse().unwrap();
    checkbox_run(&engine, &config, now).await;
    fake.clear_log();
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
            .log()
            .iter()
            .any(|(_, path, _)| matches!(path.as_str(), "/fixture" | "/other"))
    );
    {
        let mut state = fake.state();
        let pr = state
            .repo_mut()
            .prs
            .values_mut()
            .find(|pr| pr["head"]["ref"].as_str().unwrap().contains("fixture-"))
            .unwrap();
        pr["body"] = json!(
            pr["body"]
                .as_str()
                .unwrap()
                .replace("- [ ] Tick to re-check", "- [x] Tick to re-check")
        );
    }
    *fake.catalog.lock().unwrap() = json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}},"time":{"1.0.0":"2020-01-01T00:00:00Z","2.0.0":"2022-01-01T00:00:00Z"}});
    fake.clear_log();
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
        let reads = fake.log();
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
    fake.clear_log();
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
            fake.log()
                .iter()
                .filter(|(_, path, _)| path == package)
                .count(),
            1
        );
    }
    assert_unticked(&fake);
    check_transcript();
}
async fn checkbox_run(
    engine: &Engine,
    config: &patchula_config::Config,
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
/// Tick the dashboard issue's `action` checkbox as a person's edit
/// would — the wording deliberately changes, since only the hidden
/// marker may select behavior. On the locked `State`, so a `mutate`
/// rule can land the edit mid-run as easily as a test lands it between
/// runs.
fn tick_issue(state: &mut State, action: &str) {
    let issue = state.repo_mut().issues.get_mut(&100).unwrap();
    let body = issue["body"].as_str().unwrap();
    let line = body
        .lines()
        .find(|l| l.contains(&format!("\"action\":\"{action}\"")))
        .unwrap();
    let marker = line.split_once("<!-- patchula-action:").unwrap().1;
    issue["body"] = json!(body.replace(
        line,
        &format!("- [X] Operator-edited wording <!-- patchula-action:{marker}")
    ));
}
fn tick_dashboard(fake: &Sandbox, action: &str) {
    tick_issue(&mut fake.state(), action);
}
fn assert_unticked(fake: &Sandbox) {
    for issue in fake
        .state()
        .repo()
        .issues
        .values()
        .filter(|i| i["state"] == "OPEN" && i["author"]["login"] == "patchula")
    {
        let body = issue["body"].as_str().unwrap();
        assert!(!body.contains("- [x]") && !body.contains("- [X]"), "{body}");
    }
}

#[tokio::test(start_paused = true)]
async fn dashboard_age_click_bypasses_only_the_selected_target_and_survives_unticking() {
    let fake = sandbox(base_files(), None, vec![]);
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
        fake.state().repo().issues[&100]["body"]
            .as_str()
            .unwrap()
            .contains(" UTC)")
    );
    assert!(
        !fake.state().repo().issues[&100]["body"]
            .as_str()
            .unwrap()
            .contains("catalog")
    );
    fake.clear_log();
    fake.state().writes.clear();
    checkbox_run(&engine, &config, now).await;
    assert!(fake.state().writes.is_empty());
    assert!(
        !fake.log().iter().any(|(_, p, _)| p.ends_with("/pulls")),
        "empty steady repo must use inventory only"
    );
    tick_dashboard(&fake, "create");
    let created = checkbox_run(&engine, &config, now).await;
    assert_eq!(
        created.actions.as_ref().unwrap().branches[0].status,
        BranchStatus::Created
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
    assert_eq!(fake.state().repo().prs.len(), 1);
    fake.state().writes.clear();
    let steady = checkbox_run(&engine, &config, now).await;
    assert_eq!(
        steady.actions.unwrap().branches[0].status,
        BranchStatus::Unchanged
    );
    assert!(fake.state().writes.is_empty());
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn dashboard_create_click_bypasses_concurrent_hourly_and_schedule_holds() {
    let mut keep = Vec::new();
    for reason in ["concurrent", "hourly", "schedule"] {
        let mut files = base_files();
        if reason != "schedule" {
            files.insert(
                "package.json".into(),
                "{\"dependencies\":{\"fixture\":\"1.0.0\",\"other\":\"1.0.0\"}}".into(),
            );
        }
        let fake = sandbox(files, None, vec![]);
        keep.push(fake.clone());
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
            config.defaults.windows = vec![patchula_config::Window {
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
                .any(|b| b.status == BranchStatus::Skipped),
            "{reason}: {first:?}"
        );
        tick_dashboard(&fake, "create");
        let next = checkbox_run(&engine, &config, now).await;
        assert!(
            next.actions
                .unwrap()
                .branches
                .iter()
                .all(|b| matches!(b.status, BranchStatus::Created | BranchStatus::Unchanged)),
            "{reason}"
        );
        assert_unticked(&fake);
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn targeted_clicks_keep_unrelated_open_and_closed_receipts_fast() {
    let mut keep = Vec::new();
    for origin in ["recreate", "create", "pr", "retry"] {
        let mut request_counts = Vec::new();
        for dependencies in [3, 6] {
            let mut files = base_files();
            let deps: BTreeMap<_, _> = (0..dependencies)
                .map(|i| (format!("fixture{i}"), "1.0.0"))
                .collect();
            files.insert(
                "package.json".into(),
                json!({"dependencies":deps}).to_string(),
            );
            let fake = sandbox(files, None, vec![]);
            keep.push(fake.clone());
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
                for pr in fake.state().repo_mut().prs.values_mut().take(2) {
                    pr["state"] = json!("closed");
                }
                checkbox_run(&engine, &config, now).await;
            }
            if origin == "pr" {
                let mut state = fake.state();
                let pr = state.repo_mut().prs.get_mut(&1).unwrap();
                pr["body"] = json!(
                    pr["body"]
                        .as_str()
                        .unwrap()
                        .replace("- [ ] Tick to re-check", "- [x] Tick to re-check")
                );
            } else if origin == "retry" {
                // An unknown/stale target is consumed without invalidating any write receipt.
                let mut state = fake.state();
                let issue = state.repo_mut().issues.get_mut(&100).unwrap();
                let body = issue["body"].as_str().unwrap();
                issue["body"] = json!(format!(
                    "{body}\n- [x] Retry <!-- patchula-action:{{\"action\":\"retry\",\"target\":\"{}\"}} -->",
                    "a".repeat(64)
                ));
            } else {
                tick_dashboard(&fake, origin);
            }
            let untouched: Vec<_> = fake
                .state()
                .repo()
                .prs
                .values()
                .filter(|p| origin != "pr" || !p["body"].as_str().unwrap().contains("- [x]"))
                .map(|p| {
                    (
                        p["number"].as_u64().unwrap(),
                        p["head"]["ref"].as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            fake.clear_log();
            let report = checkbox_run(&engine, &config, now).await;
            assert_unticked(&fake);
            let requests = fake.log();
            assert_eq!(
                requests
                    .iter()
                    .filter(|(_, _, body)| body["query"] == patchula_update::INVENTORY_QUERY)
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
                            && matches!(b.status, BranchStatus::Unchanged | BranchStatus::Skipped)),
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
            fake.state().writes.clear();
            checkbox_run(&engine, &config, now).await;
            assert!(
                fake.state().writes.is_empty(),
                "{origin}: next run must converge"
            );
        }
        assert_eq!(
            request_counts[0], request_counts[1],
            "{origin}: unrelated branches added requests"
        );
        // A write's prepare sends only the two fresh precondition reads:
        // the base commit and tree are immutable reads answered by the scan's
        // own earlier fetch of this run's commit. A dashboard write adds
        // one more: the issue's re-check before its PATCH.
        assert_eq!(
            request_counts[0],
            match origin {
                "retry" => 5,
                "pr" => 13,
                // The held-back plan's branch has no open pull: one batched
                // `ClosedPulls` read checks it before admission.
                "create" => 16,
                _ => 15,
            }
        );
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn dashboard_recreate_click_opens_a_new_pr_and_does_not_repeat_the_old_owner_hold() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let config = checkbox_config();
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    fake.state().repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
    checkbox_run(&engine, &config, now).await;
    tick_dashboard(&fake, "recreate");
    checkbox_run(&engine, &config, now).await;
    assert_eq!(fake.state().repo().prs.len(), 2);
    assert_eq!(fake.state().repo_mut().prs[&1]["state"], "closed");
    assert_unticked(&fake);
    let next = checkbox_run(&engine, &config, now).await;
    assert_eq!(
        next.actions.unwrap().branches[0].status,
        BranchStatus::Unchanged
    );
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn repository_and_pr_recheck_clicks_force_branch_work_and_clear_the_checkboxes() {
    let mut keep = Vec::new();
    for origin in ["dashboard", "pr"] {
        let fake = sandbox(base_files(), None, vec![]);
        keep.push(fake.clone());
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
            let mut state = fake.state();
            let pr = state.repo_mut().prs.get_mut(&1).unwrap();
            pr["body"] = json!(
                pr["body"]
                    .as_str()
                    .unwrap()
                    .replace("- [ ] Tick to re-check", "- [x] Tick to re-check")
            );
        }
        fake.state().writes.clear();
        fake.clear_log();
        checkbox_run(&engine, &config, now).await;
        // The click is consumed (its checkbox is patched back) and the branch
        // is re-evaluated: an up-to-date branch needs no new commit.
        assert!(
            !fake.state().writes.is_empty(),
            "{origin}: the click must produce the untick write"
        );
        assert!(
            fake.log()
                .iter()
                .any(|(m, p, _)| m == "GET" && p.contains("/git/commits/")),
            "{origin}: the click must re-evaluate the branch"
        );
        assert!(
            !fake.state().repo_mut().prs[&1]["body"]
                .as_str()
                .unwrap()
                .contains("- [x]")
        );
        assert_unticked(&fake);
        fake.state().writes.clear();
        checkbox_run(&engine, &config, now).await;
        assert!(fake.state().writes.is_empty());
        // A stale branch still forces real work on a recheck: a head that
        // is marked but carries the old manifest must be rewritten, not
        // merely re-read.
        let branch = fake.state().repo().prs[&1]["head"]["ref"]
            .as_str()
            .unwrap()
            .to_owned();
        {
            let mut state = fake.state();
            let mut commit = state.repo().commits["commit-1"].clone();
            commit["sha"] = json!("stale");
            commit["tree"]["sha"] = json!("base-tree");
            state.repo_mut().commits.insert("stale".into(), commit);
            state
                .repo_mut()
                .refs
                .insert(format!("refs/heads/{branch}"), "stale".into());
        }
        if origin == "dashboard" {
            tick_dashboard(&fake, "recheck");
        } else {
            let mut state = fake.state();
            let pr = state.repo_mut().prs.get_mut(&1).unwrap();
            pr["body"] = json!(
                pr["body"]
                    .as_str()
                    .unwrap()
                    .replace("- [ ] Tick to re-check", "- [x] Tick to re-check")
            );
        }
        fake.state().writes.clear();
        checkbox_run(&engine, &config, now).await;
        assert!(
            fake.state()
                .writes
                .iter()
                .any(|(p, _)| p.ends_with("/git/commits")),
            "{origin}: a stale branch must be rewritten on re-check"
        );
        assert_unticked(&fake);
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn failed_lookup_click_retries_the_source_and_unticks_even_when_it_still_fails() {
    let fake = sandbox(base_files(), None, vec![]);
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
    fake.clear_log();
    let report = checkbox_run(&engine, &config, now).await;
    let failures = &report.repositories[0].lookup_failures;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].name, "fixture");
    assert_eq!(
        fake.log()
            .iter()
            .filter(|(_, p, _)| p == "/fixture")
            .count(),
        1
    );
    assert_unticked(&fake);
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn human_authored_dashboard_checkboxes_never_request_branch_work() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let config = checkbox_config();
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    tick_dashboard(&fake, "recheck");
    fake.state().repo_mut().issues.get_mut(&100).unwrap()["author"] =
        json!({"__typename":"User","login":"human"});
    fake.state().writes.clear();
    checkbox_run(&engine, &config, now).await;
    assert!(
        !fake
            .state()
            .writes
            .iter()
            .any(|(p, _)| p.ends_with("/git/commits"))
    );
    assert!(
        fake.state().repo().issues[&100]["body"]
            .as_str()
            .unwrap()
            .contains("- [X]")
    );
    assert_eq!(fake.state().repo().issues.len(), 2);
    check_transcript();
}
impl patchula_process::Executor for FailTools {
    fn run(
        &self,
        _: patchula_process::CommandSpec,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<u8>, patchula_process::Failure>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            Err(patchula_process::Failure::Io(
                "fixture tool unavailable".into(),
            ))
        })
    }
}
#[tokio::test(start_paused = true)]
async fn lockfile_failure_is_a_branch_error_and_publishes_no_manifest_only_branch() {
    let fake = sandbox(base_files(), Some("bun.lock"), vec![]);
    let temp = tempfile::tempdir().unwrap();
    let runner = patchula_tools::Runner::new(temp.path().join("worktrees"), "offline-installation")
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
    assert_eq!(branch.status, BranchStatus::Error);
    assert!(
        branch
            .reason
            .as_ref()
            .unwrap()
            .contains("fixture tool unavailable")
    );
    assert!(
        fake.state()
            .writes
            .clone()
            .iter()
            .all(|(p, _)| p.contains("/issues"))
    );
    check_transcript();
}

/// A lockfile executor for an Actions job: the repository's git calls
/// materialize `files`, and the shell-wrapped `gh-actions-lock` reports
/// success through the stream and status files the driver reads, then
/// leaves `lock` as the regenerated `.github/workflows/actions.lock`.
struct ActionsTools {
    files: BTreeMap<String, String>,
    lock: String,
}
impl patchula_process::Executor for ActionsTools {
    fn run(
        &self,
        spec: patchula_process::CommandSpec,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<u8>, patchula_process::Failure>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            if spec.args.iter().any(|s| s == "uninstall") {
                return Ok(Vec::new());
            }
            if spec.args.iter().any(|s| s == "git") {
                if let Some(i) = spec.args.iter().position(|s| s == "init") {
                    let path = std::path::Path::new(&spec.args[i + 2]);
                    std::fs::create_dir_all(path).unwrap();
                    std::fs::write(path.join("HEAD"), "ref: refs/heads/main\n").unwrap();
                }
                if spec.args.iter().any(|s| s == "ls-tree") {
                    return Ok(self
                        .files
                        .iter()
                        .map(|(path, content)| {
                            format!(
                                "100644 blob {}\t{path}\0",
                                patchula_github::blob_sha(content.as_bytes())
                            )
                        })
                        .collect::<String>()
                        .into_bytes());
                }
                if let Some(i) = spec.args.iter().position(|s| s == "checkout") {
                    let root = std::path::Path::new(
                        spec.args
                            .iter()
                            .find_map(|s| s.strip_prefix("--work-tree="))
                            .unwrap(),
                    );
                    for path in &spec.args[i + 3..] {
                        let target = root.join(path);
                        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                        std::fs::write(target, &self.files[path]).unwrap();
                    }
                }
                return Ok(Vec::new());
            }
            if spec.args.iter().any(|s| s == "/bin/sh") {
                let env = |key: &str| {
                    spec.env
                        .get(std::ffi::OsStr::new(key))
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_default()
                };
                std::fs::write(env("ACTIONS_LOCK_STDOUT"), "{}").unwrap();
                std::fs::write(env("ACTIONS_LOCK_STDERR"), "").unwrap();
                std::fs::write(env("ACTIONS_LOCK_STATUS"), "0").unwrap();
                let lock = std::path::Path::new(&env("GH_ACTIONS_LOCK_REPO_ROOT"))
                    .join(".github/workflows/actions.lock");
                std::fs::write(lock, &self.lock).unwrap();
            }
            Ok(Vec::new())
        })
    }
}

/// A `uses:` digest update in a listed workflow commits the workflow and
/// the regenerated `actions.lock` together.
#[tokio::test(start_paused = true)]
async fn an_actions_update_commits_the_workflow_and_its_lock() {
    let old = "d".repeat(40);
    let new = "e".repeat(40);
    let workflow = |pin: &str| {
        format!(
            "name: ci\non: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@{pin} # v4.3.0\n"
        )
    };
    let lock = |pin: &str| {
        format!(
            "version: 'v0.0.3'\nworkflows:\n    '.github/workflows/ci.yml':\n        - 'actions/checkout@{pin}'\ndependencies:\n    'actions/checkout@{pin}':\n        ref: 'v4.3.0'\n        commit: 'sha1-{pin}'\n"
        )
    };
    let files = BTreeMap::from([
        (
            "patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"github-actions\" }\n".into(),
        ),
        (".github/workflows/ci.yml".into(), workflow(&old)),
        (".github/workflows/actions.lock".into(), lock(&old)),
    ]);
    let fake = sandbox(files.clone(), None, vec![]);
    fake.github
        .seed_files("main", &"b".repeat(40), &files, vec![]);
    {
        let github = fake.github.clone();
        fake.hosts
            .script("mise-versions.jdx.dev", move |request, _| {
                let path = request.url().path().to_owned();
                let (repo, kind) = path
                    .strip_prefix("/data/github/")
                    .and_then(|doc| doc.rsplit_once('/'))
                    .expect("a versions-host read");
                let state = github.state();
                let upstream = &state.repositories[repo];
                github::json(
                    200,
                    match kind {
                        "releases.json" => upstream.mirror_releases.clone(),
                        "tags.json" => upstream.mirror_tags.clone(),
                        _ => panic!("unexpected read {path}"),
                    },
                )
            });
    }
    {
        let mut state = fake.state();
        let repo = state.repositories.get_mut("actions/checkout").unwrap();
        for (tag, sha) in [("v4.3.0", &old), ("v4.3.1", &new)] {
            repo.refs.insert(format!("refs/tags/{tag}"), sha.clone());
            repo.commits.insert(sha.clone(), json!({"sha":sha}));
            repo.releases.push(json!({
                "tagName":tag,"publishedAt":"2026-10-01T00:00:00Z",
                "isDraft":false,"isPrerelease":false}));
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let runner = patchula_tools::Runner::new(temp.path().join("worktrees"), "offline-installation")
        .with_executor(Arc::new(ActionsTools {
            files: files.clone(),
            lock: lock(&new),
        }));
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
    assert_eq!(
        report.repositories[0].status,
        Status::Complete,
        "{report:?}"
    );
    let branch = &report.actions.unwrap().branches[0];
    assert_eq!(branch.status, BranchStatus::Created, "{branch:?}");
    let written: BTreeSet<&str> = branch.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        written,
        BTreeSet::from([".github/workflows/ci.yml", ".github/workflows/actions.lock"]),
        "{written:?}"
    );
    {
        let state = fake.state();
        let blobs: Vec<&str> = state.repo().blobs.values().map(String::as_str).collect();
        assert!(
            blobs.iter().any(|blob| blob.contains(&new)),
            "no written workflow pins the update"
        );
        assert!(
            blobs
                .iter()
                .any(|blob| blob.contains(&format!("sha1-{new}"))),
            "no written lockfile records the update"
        );
    }
    check_transcript();
}

fn open_prs(fake: &Sandbox) -> Vec<Value> {
    fake.state()
        .repo()
        .prs
        .values()
        .filter(|p| p["state"] == "open")
        .cloned()
        .collect()
}
fn tick_pr(fake: &Sandbox, number: u64, label: &str) {
    let mut state = fake.state();
    let pr = state
        .repo_mut()
        .prs
        .values_mut()
        .find(|p| p["number"] == number)
        .unwrap();
    let body = pr["body"].as_str().unwrap();
    let line = body.lines().find(|l| l.contains(label)).unwrap();
    pr["body"] = json!(body.replace(line, &line.replacen("- [ ]", "- [x]", 1)));
}

#[tokio::test(start_paused = true)]
async fn batching_moves_updates_into_one_pr_and_never_suppresses_them() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let config = checkbox_config();
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    let update = open_prs(&fake);
    assert_eq!(update.len(), 1);
    let (number, branch) = (
        update[0]["number"].as_u64().unwrap(),
        update[0]["head"]["ref"].as_str().unwrap().to_owned(),
    );
    assert!(
        update[0]["body"]
            .as_str()
            .unwrap()
            .contains("patchula-batch-add")
    );

    // Ticking the box moves the update into the batch PR and closes the PR.
    tick_pr(&fake, number, "patchula-batch-add");
    checkbox_run(&engine, &config, now).await;
    let open = open_prs(&fake);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0]["head"]["ref"], "patchula/batch");
    let batch = open[0]["number"].as_u64().unwrap();
    let body = open[0]["body"].as_str().unwrap();
    assert!(body.contains(&format!("(from #{number})")), "{body}");
    let closed = fake
        .state()
        .repo()
        .prs
        .values()
        .find(|p| p["number"] == number)
        .unwrap()
        .clone();
    assert_eq!(closed["state"], "closed");
    assert!(
        closed["body"]
            .as_str()
            .unwrap()
            .contains("patchula-closed:batched")
    );
    assert!(fake.state().writes.iter().any(|(p, b)| {
        p.ends_with(&format!("/issues/{number}/comments"))
            && b["body"].as_str().unwrap().contains(&format!("#{batch}"))
    }));

    let dashboard = fake.state().repo().issues[&100]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(dashboard.contains("Batched"), "{dashboard}");

    // A steady run keeps the batch and does not reopen the update.
    checkbox_run(&engine, &config, now).await;
    let open = open_prs(&fake);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["number"], batch);

    // Removing the item returns the update to its own PR; the empty batch closes.
    tick_pr(&fake, batch, "patchula-batch-remove");
    checkbox_run(&engine, &config, now).await;
    let open = open_prs(&fake);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0]["head"]["ref"], branch.as_str());
    let returned = open[0]["number"].as_u64().unwrap();

    // Closing a batch without merging returns its updates too.
    tick_pr(&fake, returned, "patchula-batch-add");
    checkbox_run(&engine, &config, now).await;
    let open = open_prs(&fake);
    assert_eq!(open[0]["head"]["ref"], "patchula/batch");
    let second = open[0]["number"].as_u64().unwrap();
    fake.state()
        .repo_mut()
        .prs
        .values_mut()
        .find(|p| p["number"] == second)
        .unwrap()["state"] = json!("closed");
    checkbox_run(&engine, &config, now).await;
    let open = open_prs(&fake);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0]["head"]["ref"], branch.as_str());
    check_transcript();
}

/// The batch PR replaces the PRs it carries, which count toward
/// `prs.concurrent` until it exists; at the limit it is still created.
#[tokio::test(start_paused = true)]
async fn a_batch_is_created_at_the_concurrent_limit() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let mut config = checkbox_config();
    config.prs.concurrent = Some(1);
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    let update = open_prs(&fake);
    assert_eq!(update.len(), 1);
    let number = update[0]["number"].as_u64().unwrap();
    tick_pr(&fake, number, "patchula-batch-add");
    checkbox_run(&engine, &config, now).await;
    let open = open_prs(&fake);
    assert_eq!(open.len(), 1, "{open:?}");
    assert_eq!(open[0]["head"]["ref"], "patchula/batch");
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn tooling_warnings_reach_the_pr_and_dashboard_and_clear_with_the_condition() {
    let fake = sandbox(base_files(), None, vec![]);
    let protection = |rules: Value| {
        let mut state = fake.state();
        let repo = &mut state.repo_mut();
        repo.merge.auto_merge = true;
        repo.default_branch_ref = Some(json!({
            "name":"main",
            "target":{"__typename":"Commit","oid":"base","history":{"nodes":[{"statusCheckRollup":{"state":"SUCCESS"}}]}},
            "rules":rules,
            "refUpdateRule":null
        }));
    };
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let mut config = checkbox_config();
    config.defaults.merge = true;
    let now = chrono::Utc::now();
    let latest = |kind: &str| {
        fake.state()
            .writes
            .clone()
            .iter()
            .rev()
            .filter(|(path, _)| path.contains(kind))
            .find_map(|(_, body)| body["body"].as_str().map(str::to_owned))
    };
    let callout = "> **Nothing makes this PR pass its checks before GitHub merges it.** The Count uses GitHub's auto-merge here, and `main` requires no status checks";
    protection(
        json!({"nodes":[{"type":"DELETION","parameters":null}],"pageInfo":{"hasNextPage":false}}),
    );
    let report = checkbox_run(&engine, &config, now).await;
    assert_eq!(
        report.repositories[0].tooling,
        [patchula_report::ToolingWarning {
            rule: patchula_words::ToolingRule::MergeWithoutRequiredChecks,
            detail: None,
            branches: vec![
                fake.state().repo_mut().prs[&1]["head"]["ref"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            ],
            silenced: None,
        }]
    );
    assert!(latest("/pulls").unwrap().contains(callout));
    let dashboard = latest("/issues").unwrap();
    assert!(dashboard.contains("## Tooling warnings\n"), "{dashboard}");
    assert!(dashboard.contains("Open now: #"), "{dashboard}");
    protection(
        json!({"nodes":[{"type":"REQUIRED_STATUS_CHECKS","parameters":{"__typename":"RequiredStatusChecksParameters","requiredStatusChecks":[{"context":"ci"}]}}],"pageInfo":{"hasNextPage":false}}),
    );
    let report = checkbox_run(&engine, &config, now).await;
    assert!(report.repositories[0].tooling.is_empty());
    let pr = fake.state().repo_mut().prs[&1]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!pr.contains("[!CAUTION]"), "{pr}");
    assert!(!latest("/issues").unwrap().contains("Tooling warnings"));
    protection(Value::Null);
    let report = checkbox_run(&engine, &config, now).await;
    assert!(
        report.repositories[0].tooling.is_empty(),
        "unreadable protection never warns"
    );
    check_transcript();
}

/// A repository that commits its own registry credentials is refused during
/// extraction: no lookup and no write happens for it.
#[tokio::test(start_paused = true)]
async fn a_repository_that_commits_credentials_is_refused() {
    let mut files = base_files();
    files.insert(
        ".npmrc".into(),
        "//npm.acme.test/:_authToken=t0ken\n".into(),
    );
    let fake = sandbox(files, None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
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
    let reason = report.repositories[0].reason.as_deref().unwrap();
    assert!(
        reason.contains(".npmrc holds a credential in repository config"),
        "{reason}"
    );
    // No package lookup and no API write: extraction refused the repository.
    assert!(
        !fake.log().iter().any(|(_, path, _)| path == "/fixture"),
        "{:?}",
        fake.log()
    );
    assert!(fake.state().writes.is_empty());
    check_transcript();
}

/// A credentialed URL in a file no lockfile tool or registry lookup reads —
/// a CI workflow — never reaches the credential scan, so the repository
/// still runs.
#[tokio::test(start_paused = true)]
async fn a_credential_in_a_ci_workflow_is_not_refused() {
    let mut files = base_files();
    files.insert(
        "patchula.pkl".into(),
        "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\"; \"github-actions\" }\n"
            .into(),
    );
    // Built at runtime so no credential-shaped string sits in the tree.
    let url = format!(
        "https://{}:{}@packages.acme.test/x",
        "ci",
        "s3".to_owned() + "cret"
    );
    files.insert(
        ".github/workflows/ci.yml".into(),
        format!("on: push\njobs:\n  test:\n    steps:\n      - run: curl {url}\n"),
    );
    let fake = sandbox(files, None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
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
    assert_eq!(
        report.repositories[0].status,
        Status::Complete,
        "{report:?}"
    );
    check_transcript();
}

/// The write path reads the viewer login once, inside the run's inventory;
/// lifecycle preparation, cleanup and branch publication reuse it.
#[tokio::test(start_paused = true)]
async fn the_write_path_reads_the_viewer_login_once() {
    let fake = sandbox(base_files(), None, vec![]);
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
    run().await.unwrap();
    *fake.catalog.lock().unwrap() =
        json!({"dist-tags":{"latest":"2.1.0"},"versions":{"1.0.0":{},"2.0.0":{},"2.1.0":{}}});
    fake.clear_log();
    run().await.unwrap();
    let requests = fake.log();
    assert_eq!(
        requests
            .iter()
            .filter(|(_, _, body)| body["query"] == "query{viewer{login}}")
            .count(),
        0,
        "{requests:?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(_, path, body)| path == "/graphql"
                && body["query"] == patchula_update::INVENTORY_QUERY)
            .count(),
        1,
        "{requests:?}"
    );
    check_transcript();
}

/// The write inventory and the dashboard controls are read inside the scan;
/// either failing fails the repository before any write is attempted.
#[tokio::test(start_paused = true)]
async fn inventory_and_controls_failures_fail_the_repository_before_writes() {
    let mut keep = Vec::new();
    for failure in ["inventory", "controls"] {
        let fake = sandbox(base_files(), None, vec![]);
        keep.push(fake.clone());
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
            .unwrap()
            .with_versions_host(false);
        let config = checkbox_config();
        let now = chrono::Utc::now();
        let options = || Options {
            repositories: vec!["demo/sandbox".into()],
            config: Some(config.clone()),
            ..Default::default()
        };
        if failure == "inventory" {
            fake.github.inventory_fails();
        } else {
            // Two bot-owned dashboards cannot be told apart; the run refuses.
            checkbox_run(&engine, &config, now).await;
            let mut dupe = fake.state().repo().issues[&100].clone();
            dupe["number"] = json!(999);
            fake.state().repo_mut().issues.insert(999, dupe);
            fake.state().writes.clear();
        }
        let report = engine.run_at(options(), false, false, now).await.unwrap();
        let repo = &report.repositories[0];
        assert_eq!(repo.status, Status::Error, "{failure}: {repo:?}");
        let reason = repo.reason.as_deref().unwrap();
        if failure == "controls" {
            assert!(
                reason.contains("multiple bot-owned dependency dashboards"),
                "{reason}"
            );
        }
        assert!(fake.state().writes.is_empty(), "{failure}");
    }
    check_transcript();
}

/// A branch plan that cannot be computed fails the repository rather than
/// being dropped: the invalid `prs.branch` names the step's failure.
#[tokio::test(start_paused = true)]
async fn an_invalid_branch_prefix_fails_the_repository() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let mut config = checkbox_config();
    config.prs.branch = "bad..prefix/".into();
    let report = engine
        .run_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                config: Some(config),
                ..Default::default()
            },
            false,
            false,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Error, "{repo:?}");
    assert!(
        repo.reason
            .as_deref()
            .unwrap()
            .contains("prs.branch produces an invalid Git branch name"),
        "{repo:?}"
    );
    assert!(fake.state().writes.is_empty());
    check_transcript();
}

/// A write receipt that cannot be decoded is a cache miss, not an error:
/// the run reconciles and writes fresh rather than failing closed on a
/// value it cannot read.
#[tokio::test(start_paused = true)]
async fn an_undecodable_write_receipt_is_a_miss_and_rewritten() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    cache
        .set_state(
            &ReceiptsScope {
                repository: "demo/sandbox".into(),
                branch_prefix: "patchula/".into(),
            },
            &Receipts(json!("corrupted")),
        )
        .await
        .unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let report = engine
        .run_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                config: Some(checkbox_config()),
                ..Default::default()
            },
            false,
            true,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Complete, "{repo:?}");
    assert!(
        fake.state()
            .writes
            .clone()
            .iter()
            .any(|(p, _)| p.ends_with("/git/commits")),
        "a missed receipt must reconcile by writing"
    );
    check_transcript();
}

/// A pull-request listing that fails after the receipt check ends the
/// repository's write with the API error as the reason.
#[tokio::test(start_paused = true)]
async fn a_pulls_listing_failure_fails_the_repository() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    fake.github.pulls_list_fails();
    let report = engine
        .run_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                config: Some(checkbox_config()),
                ..Default::default()
            },
            false,
            true,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Error, "{repo:?}");
    assert!(
        !fake
            .log()
            .iter()
            .any(|(_, path, _)| path.ends_with("/merge")),
        "a failed lifecycle read must not merge"
    );
    check_transcript();
}

/// An obsolete owned branch whose deletion GitHub refuses fails the
/// repository rather than being retried silently.
#[tokio::test(start_paused = true)]
async fn a_failed_branch_deletion_fails_the_repository() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    // An owned branch the plans no longer want is a cleanup candidate.
    branch(&fake.github, "patchula/stale", "stale-head");
    fake.github.ref_delete_fails("update refused");
    let report = engine
        .run_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                config: Some(checkbox_config()),
                ..Default::default()
            },
            false,
            true,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Error, "{repo:?}");
    assert!(
        fake.state().writes.iter().any(|(_, body)| {
            body["variables"]["input"]["refUpdates"]
                .as_array()
                .is_some_and(|refs| {
                    refs.iter()
                        .any(|r| r["afterOid"] == "0000000000000000000000000000000000000000")
                })
        }),
        "a deletion refUpdate must have been attempted: {:?}",
        fake.state().writes.clone()
    );
    assert!(
        repo.reason.as_deref().unwrap().contains("update refused"),
        "{repo:?}"
    );
    check_transcript();
}

/// A dashboard write failure fails the repository with the step's reason,
/// leaving the branch work reported.
#[tokio::test(start_paused = true)]
async fn a_dashboard_write_failure_fails_the_repository() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    fake.github.issue_writes_fail();
    let report = engine
        .run_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                config: Some(checkbox_config()),
                ..Default::default()
            },
            false,
            true,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Error, "{repo:?}");
    assert!(
        repo.reason
            .as_deref()
            .unwrap()
            .contains("dependency dashboard:"),
        "{repo:?}"
    );
    check_transcript();
}

/// Lockfile maintenance plans its own branch in its window; without a disk
/// cache or an injected runner the generation failure is a branch error
/// that stops publication.
#[tokio::test(start_paused = true)]
async fn lockfile_maintenance_plans_and_a_missing_runner_fails_the_branch() {
    let fake = sandbox(base_files(), Some("package-lock.json"), vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let mut config = checkbox_config();
    config.lockfiles.refresh = true;
    // Monday inside the default maintenance window keeps the plan unheld.
    let now = "2026-10-12T01:00:00Z".parse().unwrap();
    let report = engine
        .run_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                config: Some(config),
                ..Default::default()
            },
            false,
            true,
            now,
        )
        .await
        .unwrap();
    let branches = &report.actions.as_ref().unwrap().branches;
    let maintenance = branches
        .iter()
        .find(|b| b.branch.contains("lock-file-maintenance"))
        .unwrap_or_else(|| panic!("no maintenance plan: {branches:?}"));
    assert_eq!(maintenance.status, BranchStatus::Error, "{maintenance:?}");
    assert!(
        maintenance
            .reason
            .as_deref()
            .unwrap()
            .contains("lockfile generation requires a disk cache or an injected runner"),
        "{maintenance:?}"
    );
    check_transcript();
}

/// A blob the batch cannot produce is a repository error, not an empty file:
/// the read fails closed.
#[tokio::test(start_paused = true)]
async fn an_unfetchable_blob_fails_the_repository() {
    let fake = sandbox(
        base_files(),
        None,
        vec![
            json!({"path":"nested/package.json","sha":"0".repeat(40),"mode":"100644","type":"blob"}),
        ],
    );
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Error, "{repo:?}");
    assert!(repo.reason.is_some(), "{repo:?}");
    check_transcript();
}

/// A recheck click revalidates every dependency: the advisory query is sent
/// again through the bypassing source rather than served from the cache.
#[tokio::test(start_paused = true)]
async fn a_recheck_click_revalidates_advisories() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false)
        .with_advisories();
    let config = checkbox_config();
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    let osv_requests = || {
        fake.log()
            .iter()
            .filter(|(_, path, _)| path.contains("querybatch"))
            .count()
    };
    assert_eq!(osv_requests(), 1);
    tick_dashboard(&fake, "recheck");
    fake.clear_log();
    checkbox_run(&engine, &config, now).await;
    assert_eq!(osv_requests(), 1, "the retry must bypass the cached answer");
    check_transcript();
}

/// A PR recheck box ticked against saved controls retries the branch it
/// names: on a warm cache an unticked run writes nothing, so the commit a
/// ticked run writes can only be the tick driving the revalidation.
#[tokio::test(start_paused = true)]
async fn a_ticked_pr_retries_the_branch_it_names() {
    let fake = sandbox(base_files(), None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let config = checkbox_config();
    let now = chrono::Utc::now();
    checkbox_run(&engine, &config, now).await;
    fake.state().writes.clear();
    checkbox_run(&engine, &config, now + chrono::Duration::minutes(1)).await;
    assert!(
        !fake
            .state()
            .writes
            .iter()
            .any(|(p, _)| p.ends_with("/git/commits")),
        "an unticked run on a warm cache writes nothing: {:?}",
        fake.state().writes.clone()
    );
    let body = fake.state().repo_mut().prs[&1]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        body.contains("- [ ] Tick to re-check"),
        "the re-check marker drifted: {body}"
    );
    fake.state().repo_mut().prs.get_mut(&1).unwrap()["body"] =
        json!(body.replace("- [ ] Tick to re-check", "- [x] Tick to re-check"));
    fake.state().writes.clear();
    fake.clear_log();
    checkbox_run(&engine, &config, now + chrono::Duration::minutes(2)).await;
    assert!(
        !fake.state().writes.is_empty(),
        "the tick must be consumed and written back"
    );
    assert!(
        fake.log()
            .iter()
            .any(|(m, p, _)| m == "GET" && p.contains("/git/commits/")),
        "the tick must re-evaluate the branch"
    );
    // A stale branch still forces real work: a head that is marked but
    // carries the old manifest makes the next tick write a new commit,
    // not just re-read it.
    let branch = fake.state().repo().prs[&1]["head"]["ref"]
        .as_str()
        .unwrap()
        .to_owned();
    {
        let mut state = fake.state();
        let mut commit = state.repo().commits["commit-1"].clone();
        commit["sha"] = json!("stale");
        commit["tree"]["sha"] = json!("base-tree");
        state.repo_mut().commits.insert("stale".into(), commit);
        state
            .repo_mut()
            .refs
            .insert(format!("refs/heads/{branch}"), "stale".into());
    }
    let body = fake.state().repo_mut().prs[&1]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    fake.state().repo_mut().prs.get_mut(&1).unwrap()["body"] =
        json!(body.replace("- [ ] Tick to re-check", "- [x] Tick to re-check"));
    fake.state().writes.clear();
    checkbox_run(&engine, &config, now + chrono::Duration::minutes(3)).await;
    assert!(
        fake.state()
            .writes
            .iter()
            .any(|(p, _)| p.ends_with("/git/commits")),
        "a stale branch must be rewritten"
    );
    check_transcript();
}

/// A policy that cannot evaluate a dependency — an LTS channel a catalog
/// does not carry — is a warning naming the dependency, not a scan failure.
#[tokio::test(start_paused = true)]
async fn a_policy_that_cannot_evaluate_warns_the_dependency() {
    let mut files = base_files();
    files.insert(
        "patchula.pkl".into(),
        "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\" }\nrules { new Rule { select = new Selector { packages = new Listing { \"fixture\" } }; apply = new Overrides { channel = \"lts\" } } }\n".into(),
    );
    files.insert(
        "package.json".into(),
        "{\n  \"dependencies\": {\"fixture\": \"^1.0.0\"}\n}\n".into(),
    );
    let fake = sandbox(files, None, vec![]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/sandbox".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Complete, "{repo:?}");
    assert!(
        repo.warnings.iter().any(|w| w.contains("fixture:")),
        "{:?}",
        repo.warnings
    );
    check_transcript();
}

/// The schedule turn a completed run records, read through the same
/// namespace and scope the engine writes — a stale run must leave it
/// untouched so the rerun keeps the window.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
struct LastRun(i64);
impl patchula_cache::State for LastRun {
    const NAMESPACE: &'static str = "last-run";
    const VERSION: u32 = 1;
    type Scope = patchula_cache::RepositoryScope;
}

/// An owned open PR on `name` whose head is the signed `ours` commit — the
/// obsolete-closure path's inspection target.
fn owned_pr_seed(fake: &Sandbox, number: u64, name: &str, sha: &str) {
    let mut state = fake.state();
    let repo = state.repo_mut();
    repo.refs.insert(format!("refs/heads/{name}"), sha.into());
    repo.commits.insert(sha.into(), ours(sha));
    repo.prs.insert(
        number,
        json!({"number":number,"node_id":format!("pr-{number}"),
            "html_url":format!("https://github.com/demo/sandbox/pull/{number}"),
            "state":"open","title":"stale update","body":format!("<!-- patchula:{} -->",
                json!({"version":1,"identity":"0".repeat(64),"update":"1".repeat(64)})),
            "head":{"ref":name,"sha":sha,"repo":{"full_name":"demo/sandbox"}},
            "base":{"ref":"main","repo":{"full_name":"demo/sandbox"}},
            "user":{"login":"patchula[bot]"},"labels":[],"auto_merge":null,
            "created_at":"2026-01-01T00:00:00Z","merged_at":null,
            "draft":false,"mergeable":true,"mergeable_state":"clean"}),
    );
}

/// A caller-token engine over `fake`, whose cache the caller keeps so a
/// stale run can be proved not to have recorded its schedule turn.
async fn raced_engine(fake: &Arc<Sandbox>) -> (patchula_engine::Engine, Cache) {
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let engine = Engine::with_transport("offline-installation", cache.clone(), 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    (engine, cache)
}

/// One run must report the repository `stale` for `reason`, carry no
/// `error` action and record no schedule turn; once `repair` settles the
/// state the race disturbed, the next run must complete.
async fn assert_stale_then_completes(
    name: &str,
    reason: &str,
    engine: &patchula_engine::Engine,
    cache: &Cache,
    repair: impl FnOnce(),
) {
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    let scope = patchula_cache::RepositoryScope {
        repository: "demo/sandbox".into(),
    };
    let turn = cache.state::<LastRun>(&scope).await.unwrap();
    let report = engine.run(options(), false, false).await.unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Stale, "{name}: {repo:?}");
    assert!(
        repo.reason.as_deref().unwrap_or_default().contains(reason),
        "{name}: {:?}",
        repo.reason
    );
    let actions = report.actions.as_ref().unwrap();
    assert!(
        actions
            .branches
            .iter()
            .all(|a| a.status != BranchStatus::Error)
            && actions
                .pull_requests
                .iter()
                .all(|a| a.status != patchula_report::PullRequestStatus::Error),
        "{name}: {actions:?}"
    );
    assert_eq!(
        cache.state::<LastRun>(&scope).await.unwrap(),
        turn,
        "{name}: a stale run must not record the schedule turn"
    );
    repair();
    let report = engine.run(options(), false, false).await.unwrap();
    assert_eq!(
        report.repositories[0].status,
        Status::Complete,
        "{name}: {report:?}"
    );
}

/// Every site the update layer reports `Error::Raced` makes the repository
/// `stale`, not `error`: the branch's publish lease (`apply::prepare`), the
/// write inventory, the obsolete-PR refresh and its closure lease, the
/// cleanup metadata read, the post-publish head read with its bounded
/// replication retries, and the dashboard issue's re-check before its
/// write (`a_dashboard_edit_mid_run_is_never_written_over_and_the_rerun_honours_it`).
#[tokio::test(start_paused = true)]
async fn races_under_the_run_are_stale_never_errors_and_the_rerun_completes() {
    use github::Matcher;
    // The publish lease: the default branch moved between the inventory
    // and the branch write, so `prepare` refuses the whole plan set.
    let fake = sandbox(base_files(), None, vec![]);
    let (engine, cache) = raced_engine(&fake).await;
    fake.github
        .on(Matcher::Route {
            method: reqwest::Method::GET,
            path: "/repos/demo/sandbox/pulls".into(),
        })
        .times(1)
        .mutate(|state, _| state.repo_mut().move_base("moved"));
    assert_stale_then_completes(
        "publish lease",
        "default branch moved since extraction",
        &engine,
        &cache,
        || {},
    )
    .await;

    // The write inventory: the default branch moved between the
    // extraction and the inventory read. The head check re-asserts its
    // own answer so the rules pin what each side saw no matter which
    // request lands first — the inventory now reads beside it.
    let fake = sandbox(base_files(), None, vec![]);
    let (engine, cache) = raced_engine(&fake).await;
    fake.github
        .on(Matcher::Graphql("isPrivate"))
        .times(1)
        .mutate(|state, _| state.repo_mut().move_base("base"));
    fake.github
        .on(Matcher::Graphql("associatedPullRequests"))
        .mutate(|state, _| state.repo_mut().move_base("moved"));
    assert_stale_then_completes(
        "write inventory",
        "default branch changed since extraction",
        &engine,
        &cache,
        || {},
    )
    .await;

    // The obsolete-PR refresh: its head changed between the pulls listing
    // and the fresh read closure inspects.
    let fake = sandbox(base_files(), None, vec![]);
    owned_pr_seed(&fake, 1, "patchula/old", "old-sha");
    let (engine, cache) = raced_engine(&fake).await;
    fake.github
        .on(Matcher::PullRead {
            repo: "demo/sandbox".into(),
            number: Some(1),
        })
        .times(1)
        .mutate(|state, hit| {
            state
                .repositories
                .get_mut(hit.repo.as_deref().unwrap())
                .unwrap()
                .prs
                .get_mut(&1)
                .unwrap()["head"]["sha"] = json!("changed");
        });
    let repair = |fake: &Arc<Sandbox>| {
        fake.github.state().repo_mut().prs.get_mut(&1).unwrap()["head"]["sha"] = json!("old-sha");
    };
    assert_stale_then_completes(
        "obsolete PR refresh",
        "obsolete PR changed during inspection",
        &engine,
        &cache,
        || repair(&fake),
    )
    .await;

    // The closure lease: the repository's default branch changed between
    // the pulls read and the closure's metadata check.
    let fake = sandbox(base_files(), None, vec![]);
    owned_pr_seed(&fake, 1, "patchula/old", "old-sha");
    let (engine, cache) = raced_engine(&fake).await;
    fake.github
        .on(Matcher::PullRead {
            repo: "demo/sandbox".into(),
            number: Some(1),
        })
        .times(1)
        .mutate(|state, _| {
            state.repo_mut().default_branch = "other".into();
        });
    let repair = |fake: &Arc<Sandbox>| {
        fake.github.state().repo_mut().default_branch = "main".into();
    };
    assert_stale_then_completes(
        "closure lease",
        "default branch changed before PR closure",
        &engine,
        &cache,
        || repair(&fake),
    )
    .await;

    // The cleanup lease: the default branch changed between the branches
    // listing and the metadata check before obsolete-branch deletion.
    let fake = sandbox(base_files(), None, vec![]);
    branch(&fake.github, "patchula/old", "old-sha");
    let (engine, cache) = raced_engine(&fake).await;
    fake.github
        .on(Matcher::Route {
            method: reqwest::Method::GET,
            path: "/repos/demo/sandbox/branches".into(),
        })
        .times(1)
        .mutate(|state, _| {
            state.repo_mut().default_branch = "other".into();
        });
    let repair = |fake: &Arc<Sandbox>| {
        fake.github.state().repo_mut().default_branch = "main".into();
    };
    assert_stale_then_completes(
        "cleanup lease",
        "default branch changed before cleanup",
        &engine,
        &cache,
        || repair(&fake),
    )
    .await;

    // The post-publish head read: a foreign push moved the PR's head to a
    // commit the run cannot name, so it stops rather than describe someone
    // else's commit.
    let fake = sandbox(base_files(), None, vec![]);
    let (engine, cache) = raced_engine(&fake).await;
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    let report = engine.run(options(), false, false).await.unwrap();
    assert_eq!(report.repositories[0].status, Status::Complete);
    *fake.catalog.lock().unwrap() =
        json!({"dist-tags":{"latest":"2.1.0"},"versions":{"1.0.0":{},"2.0.0":{},"2.1.0":{}}});
    fake.github
        .on(Matcher::PullRead {
            repo: "demo/sandbox".into(),
            number: None,
        })
        .times(1)
        .mutate(|state, hit| {
            state
                .repositories
                .get_mut(hit.repo.as_deref().unwrap())
                .unwrap()
                .prs
                .get_mut(&hit.pull.unwrap())
                .unwrap()["head"]["sha"] = json!("human-commit");
        });
    let repair = |fake: &Arc<Sandbox>| {
        let mut state = fake.github.state();
        let repo = state.repo_mut();
        let head = repo.refs["refs/heads/patchula/fixture-2.x"].clone();
        repo.prs.get_mut(&1).unwrap()["head"]["sha"] = json!(head);
    };
    assert_stale_then_completes(
        "published head read",
        "unexpected commit",
        &engine,
        &cache,
        || repair(&fake),
    )
    .await;

    // The bounded retries: replication never shows the published head, so
    // the run stops after its retries rather than trusting the stale read.
    let fake = sandbox(base_files(), None, vec![]);
    let (engine, cache) = raced_engine(&fake).await;
    let report = engine.run(options(), false, false).await.unwrap();
    assert_eq!(report.repositories[0].status, Status::Complete);
    *fake.catalog.lock().unwrap() =
        json!({"dist-tags":{"latest":"2.1.0"},"versions":{"1.0.0":{},"2.0.0":{},"2.1.0":{}}});
    fake.github.state().settings.pr_lag_reads = usize::MAX;
    let repair = |fake: &Arc<Sandbox>| {
        fake.github.state().settings.pr_lag_reads = 0;
    };
    assert_stale_then_completes(
        "bounded retries",
        "bounded retries",
        &engine,
        &cache,
        || repair(&fake),
    )
    .await;
    check_transcript();
}

/// The inventory reads beside the head check, so a head that moves on the
/// early read's first page leaves it answering for a head the scan never
/// saw: the run reads the inventory again and races, exactly as it did
/// when the read followed the scan. The head check re-asserts its own
/// answer so the rules pin what each side saw no matter which request
/// lands first.
#[tokio::test(start_paused = true)]
async fn a_head_move_on_the_early_inventory_read_rereads_it() {
    use github::Matcher;
    let fake = sandbox(base_files(), None, vec![]);
    let (engine, _cache) = raced_engine(&fake).await;
    fake.github
        .on(Matcher::Graphql("isPrivate"))
        .times(1)
        .mutate(|state, _| state.repo_mut().move_base("base"));
    fake.github
        .on(Matcher::Graphql("associatedPullRequests"))
        .mutate(|state, _| state.repo_mut().move_base("moved"));
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    let report = engine.run(options(), false, false).await.unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Stale, "{repo:?}");
    assert!(
        repo.reason
            .as_deref()
            .unwrap_or_default()
            .contains("default branch changed since extraction"),
        "{repo:?}"
    );
    let requests = fake.log();
    assert_eq!(
        requests
            .iter()
            .filter(|(_, path, body)| path == "/graphql"
                && body["query"] == patchula_update::INVENTORY_QUERY)
            .count(),
        2,
        "the early inventory read and its re-read: {requests:?}"
    );
    // The moved head stays authoritative, so the rerun's early read and
    // scan agree again and one inventory read serves the run.
    let report = engine.run(options(), false, false).await.unwrap();
    assert_eq!(
        report.repositories[0].status,
        Status::Complete,
        "{report:?}"
    );
    check_transcript();
}

/// A dashboard edit that lands between the inventory read and the
/// reconcile is never written over: the issue re-check before the PATCH
/// sees the moved body, the run goes stale for the rerun, and that run
/// honours the edit it kept.
#[tokio::test(start_paused = true)]
async fn a_dashboard_edit_mid_run_is_never_written_over_and_the_rerun_honours_it() {
    use github::Matcher;
    let fake = sandbox(base_files(), None, vec![]);
    let (engine, _cache) = raced_engine(&fake).await;
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    let report = engine.run(options(), false, false).await.unwrap();
    assert_eq!(report.repositories[0].status, Status::Complete);
    // A new proposal re-renders the dashboard, so the reconcile wants to
    // write; the person's tick lands after the inventory's read, at the
    // issue re-check the PATCH makes first.
    *fake.catalog.lock().unwrap() =
        json!({"dist-tags":{"latest":"2.1.0"},"versions":{"1.0.0":{},"2.0.0":{},"2.1.0":{}}});
    fake.github
        .on(Matcher::Route {
            method: reqwest::Method::GET,
            path: "/repos/demo/sandbox/issues/100".into(),
        })
        .times(1)
        .mutate(|state, _| tick_issue(state, "recheck"));
    fake.clear_log();
    let report = engine.run(options(), false, false).await.unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Stale, "{repo:?}");
    assert!(
        repo.reason
            .as_deref()
            .unwrap_or_default()
            .contains("dashboard changed under the run"),
        "{repo:?}"
    );
    let log = fake.log();
    assert!(
        log.iter()
            .any(|(m, p, _)| m.as_str() == "GET" && p == "/repos/demo/sandbox/issues/100"),
        "the reconcile never re-read the issue: {log:?}"
    );
    assert!(
        !log.iter()
            .any(|(m, p, _)| m.as_str() == "PATCH" && p == "/repos/demo/sandbox/issues/100"),
        "the run patched over the person's edit: {log:?}"
    );
    let body = fake.state().repo().issues[&100]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(body.contains("- [X]"), "the tick was lost: {body}");
    // The rerun reads the ticked body: the recheck refreshes catalogs,
    // and the dashboard's own write goes through with the box unticked.
    fake.clear_log();
    let report = engine.run(options(), false, false).await.unwrap();
    assert_eq!(
        report.repositories[0].status,
        Status::Complete,
        "{report:?}"
    );
    assert!(
        fake.log().iter().any(|(_, path, _)| path == "/fixture"),
        "the ticked recheck never happened: {:?}",
        fake.log()
    );
    assert_unticked(&fake);
    check_transcript();
}

/// A repository that raced keeps its schedule turn: the window `since`
/// stays empty, so the next run still sees the cron time the raced run
/// covered. With the turn recorded the second run's window — only the
/// cron time in its last minute counts — would hold the update a day.
#[tokio::test(start_paused = true)]
async fn a_stale_run_keeps_the_schedule_window_for_its_rerun() {
    use github::Matcher;
    let mut files = base_files();
    files.insert(
        "patchula.pkl".into(),
        "amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0; windows = new Listing { new Window { cron = new Listing { \"0 12 * * *\" }; zone = \"UTC\" } } }\nmanagers = new Listing { \"npm\" }\n".into(),
    );
    let fake = sandbox(files, None, vec![]);
    let (engine, _cache) = raced_engine(&fake).await;
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    let at = |text: &str| chrono::DateTime::parse_from_rfc3339(text).unwrap().to_utc();
    fake.github
        .on(Matcher::Route {
            method: reqwest::Method::GET,
            path: "/repos/demo/sandbox/pulls".into(),
        })
        .times(1)
        .mutate(|state, _| state.repo_mut().move_base("moved"));
    let report = engine
        .run_at(options(), false, false, at("2026-10-15T12:00:30Z"))
        .await
        .unwrap();
    assert_eq!(report.repositories[0].status, Status::Stale);
    // Seconds later the noon window is still inside the previous-minute
    // look-back only because the raced run recorded no turn; with the turn
    // recorded its `since` would sit after the cron time and the update
    // would wait a day.
    let report = engine
        .run_at(options(), false, false, at("2026-10-15T12:00:45Z"))
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Complete, "{repo:?}");
    let actions = report.actions.as_ref().unwrap();
    assert_eq!(actions.branches.len(), 1, "{actions:?}");
    assert_eq!(actions.branches[0].status, BranchStatus::Created);
    check_transcript();
}

/// A repository holding both a raced action and a failed action reports
/// `error`: failures outrank the stale the race would otherwise leave.
#[tokio::test(start_paused = true)]
async fn an_error_outranks_a_raced_pull_request() {
    use github::Matcher;
    let mut files = base_files();
    files.insert(
        "package.json".into(),
        "{\n  \"dependencies\": {\"fixture\": \"1.0.0\", \"other\": \"1.0.0\"}\n}\n".into(),
    );
    let fake = sandbox(files, None, vec![]);
    let (engine, _cache) = raced_engine(&fake).await;
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    // Both updates publish, one PR each: `patchula/fixture-2.x` is #1 and
    // `patchula/other-2.x` is #2.
    let report = engine.run(options(), false, false).await.unwrap();
    assert_eq!(report.repositories[0].status, Status::Complete);
    *fake.catalog.lock().unwrap() =
        json!({"dist-tags":{"latest":"2.1.0"},"versions":{"1.0.0":{},"2.0.0":{},"2.1.0":{}}});
    // The first pull read races; the second PR's update write fails.
    fake.github
        .on(Matcher::PullRead {
            repo: "demo/sandbox".into(),
            number: Some(1),
        })
        .times(1)
        .mutate(|state, hit| {
            state
                .repositories
                .get_mut(hit.repo.as_deref().unwrap())
                .unwrap()
                .prs
                .get_mut(&hit.pull.unwrap())
                .unwrap()["head"]["sha"] = json!("human-commit");
        });
    fake.github
        .on(Matcher::Route {
            method: reqwest::Method::PATCH,
            path: "/repos/demo/sandbox/pulls/2".into(),
        })
        .times(1)
        .then_fail(500);
    let report = engine.run(options(), false, false).await.unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, Status::Error, "{repo:?}");
    assert!(
        repo.reason
            .as_deref()
            .unwrap_or_default()
            .contains("pull request actions failed"),
        "{:?}",
        repo.reason
    );
    let actions = report.actions.as_ref().unwrap();
    let statuses: Vec<_> = actions
        .pull_requests
        .iter()
        .map(|a| (a.branch.as_str(), a.status))
        .collect();
    assert!(
        statuses
            .iter()
            .any(|(_, s)| *s == patchula_report::PullRequestStatus::Stale)
            && statuses
                .iter()
                .any(|(_, s)| *s == patchula_report::PullRequestStatus::Error),
        "{statuses:?}"
    );
    check_transcript();
}

/// The latency every depth pin charges: 100 ms a request, a divisor of
/// the governor's one-second write pacing, so virtual elapsed divides
/// into whole depths.
const DEPTH_LATENCY: std::time::Duration = std::time::Duration::from_millis(100);

/// How many times a depth pin repeats its scenario: exactness makes one
/// measurement the run's depth, but a once-in-a-while slip still shows
/// in eleven.
const DEPTH_SAMPLES: usize = 11;

async fn depth_engine(fake: &Arc<Sandbox>) -> Engine {
    fake.hosts.set_latency(DEPTH_LATENCY);
    Engine::with_transport(
        "offline-installation",
        Cache::open(":memory:", Default::default()).await.unwrap(),
        4,
        fake.clone(),
    )
    .unwrap()
    .with_versions_host(false)
    .with_advisories()
}

/// The depth pins' files: `base_files` with the github-actions manager on
/// and a CI workflow pinning `actions/checkout` to a tag's commit, so a
/// run holds a GitHub-tags dependency the way a pinned repository does.
/// The rule's nonzero wait lets the tag catalog be reused on warm runs,
/// which is what makes the tag mutable and sends its ref to
/// `fresh_tag_digests` again instead of trusting the catalog.
fn depth_files() -> BTreeMap<String, String> {
    let mut files = base_files();
    files.insert(
        "patchula.pkl".into(),
        "amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\"; \"github-actions\" }\nrules { new Rule { select = new Selector { packages = new Listing { \"actions/checkout\" } }; apply = new Overrides { waitHours = 2 } } }\n".into(),
    );
    files.insert(
        ".github/workflows/ci.yml".into(),
        format!(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@{} # v4.3.0\n",
            "e".repeat(40)
        ),
    );
    files
}

/// `sandbox(depth_files())` with `actions/checkout`'s tags, commits and
/// releases seeded in the upstream repository every fake carries. The pin
/// already matches the tag's target, so the action proposes nothing and a
/// write's plan count stays the fixture's npm updates alone.
fn depth_sandbox(files: BTreeMap<String, String>) -> Arc<Sandbox> {
    let fake = sandbox(files, None, vec![]);
    let mut state = fake.state();
    let repo = state.repositories.get_mut("actions/checkout").unwrap();
    for (tag, sha) in [("v4.3.0", 'e'), ("v4.2.1", 'd')] {
        let sha = sha.to_string().repeat(40);
        repo.refs.insert(format!("refs/tags/{tag}"), sha.clone());
        repo.commits.insert(sha.clone(), json!({"sha":sha}));
        repo.releases.push(json!({
            "tagName":tag,"publishedAt":"2026-10-01T00:00:00Z",
            "isDraft":false,"isPrerelease":false}));
    }
    drop(state);
    fake
}

/// A warm lookup's depth pin: the second and later runs' head check, OSV
/// batch and mutable-tag read are the waves the latency counts.
#[tokio::test(start_paused = true)]
async fn depth_pin_warm_lookup() {
    let fake = depth_sandbox(depth_files());
    let engine = depth_engine(&fake).await;
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    // The first lookup's catalog is too fresh to leave the mutable-tag
    // prefetch hint; the second is the first rep the tag read co-fires
    // with the lookup wave — the steady state the pin measures.
    engine.lookup(options()).await.unwrap();
    engine.lookup(options()).await.unwrap();
    let mut samples = Vec::new();
    for _ in 0..DEPTH_SAMPLES {
        fake.clear_log();
        let started = tokio::time::Instant::now();
        let warm = engine.lookup(options()).await.unwrap();
        samples.push(started.elapsed());
        assert_eq!(warm.repositories[0].status, Status::Complete);
    }
    check_depth(DEPTH_LATENCY, samples);
    check_transcript();
}

/// A no-op real run's depth pin: the run after a write must only read.
#[tokio::test(start_paused = true)]
async fn depth_pin_noop_run() {
    let fake = depth_sandbox(depth_files());
    let engine = depth_engine(&fake).await;
    let options = || Options {
        repositories: vec!["demo/sandbox".into()],
        ..Default::default()
    };
    let first = engine.run(options(), false, false).await.unwrap();
    assert_eq!(
        first.actions.as_ref().unwrap().branches[0].status,
        BranchStatus::Created
    );
    fake.state().writes.clear();
    // The write's catalog was too fresh to leave the mutable-tag prefetch
    // hint: one more run reaches the steady state the pin measures.
    engine.run(options(), false, false).await.unwrap();
    let mut samples = Vec::new();
    for _ in 0..DEPTH_SAMPLES {
        fake.clear_log();
        let started = tokio::time::Instant::now();
        let report = engine.run(options(), false, false).await.unwrap();
        samples.push(started.elapsed());
        assert_eq!(report.repositories[0].status, Status::Complete);
        assert!(
            fake.state().writes.is_empty(),
            "the no-op run wrote: {:?}",
            fake.state().writes
        );
    }
    check_depth(DEPTH_LATENCY, samples);
    check_transcript();
}

/// A one-plan repository's full write: the run's latency counts its
/// publish reads, its writes and the write pacing between them.
#[tokio::test(start_paused = true)]
async fn depth_pin_full_write_of_one_plan() {
    // Every repetition's fake stays alive until the transcript check
    // marks it: a FakeGitHub dropped unchecked panics.
    let mut fakes = Vec::new();
    let mut samples = Vec::new();
    for _ in 0..DEPTH_SAMPLES {
        let fake = depth_sandbox(depth_files());
        fakes.push(fake.clone());
        let engine = depth_engine(&fake).await;
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
        samples.push(started.elapsed());
        let actions = report.actions.as_ref().unwrap();
        assert_eq!(actions.branches.len(), 1, "{actions:?}");
        assert_eq!(actions.branches[0].status, BranchStatus::Created);
    }
    check_depth(DEPTH_LATENCY, samples);
    check_transcript();
}

/// The same pin for three plans in one repository.
#[tokio::test(start_paused = true)]
async fn depth_pin_full_write_of_three_plans() {
    let mut fakes = Vec::new();
    let mut samples = Vec::new();
    for _ in 0..DEPTH_SAMPLES {
        let mut files = depth_files();
        let deps: BTreeMap<_, _> = (0..3).map(|i| (format!("fixture{i}"), "1.0.0")).collect();
        files.insert(
            "package.json".into(),
            json!({"dependencies":deps}).to_string(),
        );
        let fake = depth_sandbox(files);
        fakes.push(fake.clone());
        let engine = depth_engine(&fake).await;
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
        samples.push(started.elapsed());
        let actions = report.actions.as_ref().unwrap();
        assert_eq!(actions.branches.len(), 3, "{actions:?}");
        assert!(
            actions
                .branches
                .iter()
                .all(|b| b.status == BranchStatus::Created),
            "{actions:?}"
        );
    }
    check_depth(DEPTH_LATENCY, samples);
    check_transcript();
}

/// What `seed_files` does for the primary repository, for a repository
/// `add_repository` made: `files` become blobs and tree entries behind
/// `main` at `base`.
fn seed_repository(github: &FakeGitHub, name: &str, files: &BTreeMap<String, String>) {
    let entries: Vec<Value> = files
        .iter()
        .map(|(path, content)| {
            json!({"path":path,"sha":patchula_github::blob_sha(content.as_bytes()),"mode":"100644","type":"blob"})
        })
        .collect();
    let base = "a".repeat(40);
    let mut state = github.state();
    let repo = state.repositories.get_mut(name).unwrap();
    repo.default_branch = "main".into();
    repo.base = base.clone();
    repo.refs.insert("refs/heads/main".into(), base.clone());
    repo.trees.insert("base-tree".into(), json!(entries));
    repo.commits.insert(
        base,
        json!({"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","tree":{"sha":"base-tree"}}),
    );
    for content in files.values() {
        repo.blobs.insert(
            patchula_github::blob_sha(content.as_bytes()),
            content.clone(),
        );
    }
    repo.checks =
        vec![json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"})];
}

/// The version `defaults.toml` pins for `tool`, read straight from it so a
/// pin bump does not stale the seeded usage records.
fn pinned(tool: &str) -> String {
    let defaults = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tools/src/lockfiles/defaults.toml"
    ))
    .unwrap();
    defaults
        .lines()
        .find_map(|line| {
            line.strip_prefix(&format!("{tool} = "))
                .map(|version| version.trim().trim_matches('"').to_owned())
        })
        .unwrap_or_else(|| panic!("defaults.toml pins no {tool}"))
}

/// A scripted lockfile-tool executor covering every repository in the run.
/// `git` calls address each repository's clone by its `clones/<sha-256>`
/// path, so `ls-tree` and `checkout` materialize that repository's `files`.
/// The tool call sleeps `tool_time` before writing the lockfile its working
/// directory names; `(repository, start, end)` goes into `windows` so the
/// test can see two repositories' tool runs overlap.
struct Tools {
    /// Each repository's checkout file set, keyed by the clone directory
    /// name `checkout()` derives: `patchula_manager::hash(repository)`.
    files: BTreeMap<String, BTreeMap<String, String>>,
    /// The lockfile directory name to its repository: `pkg-a` is alpha's.
    dirs: BTreeMap<String, String>,
    /// The lockfile leaf each directory's tool writes.
    locks: BTreeMap<String, String>,
    tool_time: std::time::Duration,
    windows: Mutex<Vec<(String, tokio::time::Instant, tokio::time::Instant)>>,
}
impl Tools {
    /// The file set of the clone `--git-dir` addresses.
    fn clone_files(&self, spec: &patchula_process::CommandSpec) -> &BTreeMap<String, String> {
        let dir = spec
            .args
            .iter()
            .find_map(|arg| arg.strip_prefix("--git-dir="))
            .expect("a repository's git call");
        let name = std::path::Path::new(dir)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a named clone");
        &self.files[name]
    }
}
impl patchula_process::Executor for Tools {
    fn run(
        &self,
        spec: patchula_process::CommandSpec,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Vec<u8>, patchula_process::Failure>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            if spec.args.iter().any(|s| s == "uninstall") {
                return Ok(Vec::new());
            }
            if spec.args.iter().any(|s| s == "git") {
                if let Some(i) = spec.args.iter().position(|s| s == "init") {
                    let path = std::path::Path::new(&spec.args[i + 2]);
                    std::fs::create_dir_all(path).unwrap();
                    std::fs::write(path.join("HEAD"), "ref: refs/heads/main\n").unwrap();
                }
                if spec.args.iter().any(|s| s == "ls-tree") {
                    return Ok(self
                        .clone_files(&spec)
                        .iter()
                        .map(|(path, content)| {
                            format!(
                                "100644 blob {}\t{path}\0",
                                patchula_github::blob_sha(content.as_bytes())
                            )
                        })
                        .collect::<String>()
                        .into_bytes());
                }
                if let Some(i) = spec.args.iter().position(|s| s == "checkout") {
                    let root = std::path::Path::new(
                        spec.args
                            .iter()
                            .find_map(|s| s.strip_prefix("--work-tree="))
                            .unwrap(),
                    );
                    for path in &spec.args[i + 3..] {
                        let target = root.join(path);
                        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                        std::fs::write(target, &self.clone_files(&spec)[path]).unwrap();
                    }
                }
                return Ok(Vec::new());
            }
            // A lockfile tool run: take its window, then write the lockfile
            // the run would produce inside its working directory.
            let directory = spec
                .args
                .iter()
                .zip(spec.args.iter().skip(1))
                .find_map(|(flag, value)| {
                    matches!(flag.as_str(), "--cwd" | "--prefix" | "-C")
                        .then(|| std::path::PathBuf::from(value))
                })
                .expect("a lockfile tool names its working directory");
            let leaf = directory
                .file_name()
                .and_then(|name| name.to_str())
                .expect("a named tool directory");
            let repo = self.dirs[leaf].clone();
            let start = tokio::time::Instant::now();
            tokio::time::sleep(self.tool_time).await;
            let content = match self.locks[leaf].as_str() {
                "bun.lock" => {
                    "{\"lockfileVersion\":3,\"packages\":{\"fixture\":[\"fixture@2.0.0\"]}}\n"
                }
                "package-lock.json" => {
                    "{\"lockfileVersion\":3,\"packages\":{\"node_modules/fixture\":{\"version\":\"2.0.0\"}}}\n"
                }
                leaf => panic!("unexpected lockfile {leaf}"),
            };
            std::fs::write(directory.join(&self.locks[leaf]), content).unwrap();
            self.windows
                .lock()
                .unwrap()
                .push((repo, start, tokio::time::Instant::now()));
            Ok(Vec::new())
        })
    }
}

/// The three-repository sandbox plus everything a run needs to reach: the
/// scripted tools and the runner's worktree root (which must outlive the run).
struct ThreeRepos {
    engine: Engine,
    sandbox: Arc<Sandbox>,
    tools: Arc<Tools>,
    runner: patchula_tools::Runner,
    temp: tempfile::TempDir,
}
impl ThreeRepos {
    /// `demo/alpha` regenerates `pkg-a/bun.lock` and `demo/bravo`
    /// `pkg-b/package-lock.json` alongside the same dependency update
    /// `demo/charlie` publishes without lockfile work. bun's serial cache
    /// lock and npm's shared one let the two runs really overlap.
    async fn build(concurrency: usize) -> Self {
        let github = Arc::new(FakeGitHub::new("demo/alpha"));
        github.add_repository("demo/bravo", true);
        github.add_repository("demo/charlie", true);
        let hosts = Arc::new(Hosts::new(github.clone()));
        let catalog = Arc::new(Mutex::new(
            json!({"dist-tags":{"latest":"2.0.0"},"versions":{"1.0.0":{},"2.0.0":{}}}),
        ));
        let osv = Arc::new(Mutex::new(json!({"results":[{}]})));
        {
            let catalog = catalog.clone();
            hosts.script("registry.npmjs.org", move |_, _| {
                github::json(200, catalog.lock().unwrap().clone())
            });
        }
        hosts.script("api.osv.dev", move |_, _| {
            github::json(200, osv.lock().unwrap().clone())
        });
        let files = |lock: Option<(&str, &str)>| {
            let mut files = base_files();
            if let Some((directory, leaf)) = lock {
                let manifest = files.remove("package.json").unwrap();
                files.insert(format!("{directory}/package.json"), manifest);
                files.insert(format!("{directory}/{leaf}"), "{}\n".into());
            }
            files
        };
        // npm's cache is documented concurrent-safe, so a native npm run
        // overlaps bun's serial one in the same partition; aube's is not.
        let native_npm = |files: &mut BTreeMap<String, String>| {
            files
                .get_mut("patchula.pkl")
                .unwrap()
                .push_str("lockfiles { npmEngine = \"npm\" }\n");
        };
        let mut bravo_files = files(Some(("pkg-b", "package-lock.json")));
        native_npm(&mut bravo_files);
        let manifests = [
            ("demo/alpha", Some(("pkg-a", "bun.lock"))),
            ("demo/bravo", Some(("pkg-b", "package-lock.json"))),
            ("demo/charlie", None),
        ];
        for (name, lock) in manifests {
            let seeded = if name == "demo/bravo" {
                bravo_files.clone()
            } else {
                files(lock)
            };
            seed_repository(github.as_ref(), name, &seeded);
        }
        github.state().settings.latency = std::time::Duration::from_millis(100);
        let sandbox = Arc::new(Sandbox {
            github,
            hosts,
            catalog,
            fail_lookup: Default::default(),
        });
        let tools = Arc::new(Tools {
            files: manifests
                .iter()
                .filter(|(_, lock)| lock.is_some())
                .map(|(name, lock)| {
                    let seeded = if *name == "demo/bravo" {
                        bravo_files.clone()
                    } else {
                        files(*lock)
                    };
                    (patchula_manager::hash(name.as_bytes()), seeded)
                })
                .collect(),
            dirs: manifests
                .iter()
                .filter_map(|(name, lock)| {
                    lock.map(|(directory, _)| (directory.to_owned(), name.to_string()))
                })
                .collect(),
            locks: manifests
                .iter()
                .filter_map(|(_, lock)| {
                    lock.map(|(directory, leaf)| (directory.to_owned(), leaf.to_owned()))
                })
                .collect(),
            tool_time: std::time::Duration::from_millis(800),
            windows: Mutex::new(Vec::new()),
        });
        let temp = tempfile::tempdir().unwrap();
        let runner =
            patchula_tools::Runner::new(temp.path().join("worktrees"), "offline-installation")
                .with_executor(tools.clone());
        // A proven install takes the store's shared lock only: record both
        // tools as used so neither run's first use waits for the other.
        let usage: BTreeMap<String, i64> = [
            (format!("bun@{}", pinned("bun")), 1),
            (format!("node@{}", pinned("node")), 1),
        ]
        .into_iter()
        .collect();
        let store = temp.path().join("tools");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            store.join("patchula-usage.json"),
            serde_json::to_vec(&usage).unwrap(),
        )
        .unwrap();
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let engine =
            Engine::with_transport("offline-installation", cache, concurrency, sandbox.clone())
                .unwrap();
        Self {
            engine,
            sandbox,
            tools,
            runner,
            temp,
        }
    }
}

/// Three repositories write side by side: two regenerate lockfiles while
/// every repository's writes flow. The side-by-side run must produce the
/// report the sequential one does, byte for byte.
#[tokio::test(start_paused = true)]
async fn repositories_write_side_by_side_with_an_identical_report() {
    let names = || Options {
        repositories: vec![
            "demo/alpha".into(),
            "demo/bravo".into(),
            "demo/charlie".into(),
        ],
        ..Default::default()
    };
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let sequential = ThreeRepos::build(1).await;
    let sequential_engine = sequential
        .engine
        .with_versions_host(false)
        .with_lockfile_runner(sequential.runner.clone());
    // Wall time, not the paused clock's virtual time.
    let started = std::time::Instant::now();
    let mut sequential_report = sequential_engine
        .run_at(names(), false, false, now)
        .await
        .unwrap();
    let sequential_elapsed = started.elapsed();

    let parallel = ThreeRepos::build(4).await;
    assert!(parallel.temp.path().is_dir());
    let parallel_engine = parallel
        .engine
        .with_versions_host(false)
        .with_lockfile_runner(parallel.runner.clone());
    let started = std::time::Instant::now();
    let mut parallel_report = parallel_engine
        .run_at(names(), false, false, now)
        .await
        .unwrap();
    let parallel_elapsed = started.elapsed();

    // Both repositories' tool runs overlapped under the runner's shared
    // semaphore; the sequential run's cannot.
    let overlaps =
        |a: &(String, tokio::time::Instant, tokio::time::Instant),
         b: &(String, tokio::time::Instant, tokio::time::Instant)| a.1 < b.2 && b.1 < a.2;
    let sequential_windows = sequential.tools.windows.lock().unwrap().clone();
    assert_eq!(sequential_windows.len(), 2, "{sequential_windows:?}");
    assert!(
        !overlaps(&sequential_windows[0], &sequential_windows[1]),
        "sequential tool runs overlapped: {sequential_windows:?}"
    );
    let windows = parallel.tools.windows.lock().unwrap().clone();
    assert_eq!(windows.len(), 2, "{windows:?}");
    assert!(
        overlaps(&windows[0], &windows[1]),
        "lockfile tool runs did not overlap: {windows:?}"
    );

    // Writes interleave across repositories behind the shared governor: some
    // repository's first mutation lands before another's last.
    let mutation = |method: &str, body: &Value| {
        method != "GET"
            || body["query"]
                .as_str()
                .is_some_and(|query| query.starts_with("mutation"))
    };
    let repository = |path: &str, body: &Value| {
        ["alpha", "bravo", "charlie"].into_iter().find(|name| {
            path.contains(&format!("demo/{name}"))
                || body["variables"]["name"].as_str() == Some(*name)
        })
    };
    let mut spans: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (index, (method, path, body)) in parallel.sandbox.log().iter().enumerate() {
        if !mutation(method, body) {
            continue;
        }
        if let Some(name) = repository(path, body) {
            spans
                .entry(name)
                .and_modify(|span| span.1 = index)
                .or_insert((index, index));
        }
    }
    assert!(
        ["alpha", "bravo", "charlie"]
            .iter()
            .all(|name| spans.contains_key(name)),
        "a repository wrote nothing: {spans:?}"
    );
    let overlapped = ["alpha", "bravo", "charlie"]
        .iter()
        .enumerate()
        .any(|(i, a)| {
            ["alpha", "bravo", "charlie"]
                .iter()
                .skip(i + 1)
                .any(|b| spans[a].0 < spans[b].1 && spans[b].0 < spans[a].1)
        });
    assert!(overlapped, "writes did not interleave: {spans:?}");

    eprintln!(
        "three-repository write: sequential {sequential_elapsed:?}, \
         side by side {parallel_elapsed:?}"
    );
    assert!(
        parallel_elapsed < sequential_elapsed,
        "{parallel_elapsed:?} !< {sequential_elapsed:?}"
    );
    sequential_report.stats = None;
    parallel_report.stats = None;
    assert_eq!(
        serde_json::to_string(&sequential_report).unwrap(),
        serde_json::to_string(&parallel_report).unwrap(),
        "the side-by-side write's report differs from the sequential one"
    );
    check_transcript();
}
