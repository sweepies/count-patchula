//! The shared stateful fake GitHub — `patchula_testkit::github::FakeGitHub` —
//! scripts these tests; no sockets, tokens or network client.
use patchula_cache::Cache;
use patchula_config::{Config, Rebase};
use patchula_github::Client;
use patchula_manager::{Files, Scanner};
use patchula_report::{
    BranchStatus, Current, Dependency as ReportDependency, DependencyMetadata, PackageFile,
    Repository, Status, Update,
};
use patchula_testkit::github::FakeGitHub;
use patchula_update::testing::Draft;
use patchula_update::{Blob, Plan, Snapshot, blob_sha, branch_name, plan};
use patchula_words::{Manager, Source, Update as UpdateKind};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Report metadata for an extracted dependency: its source as the engine
/// records it, and nothing else these tests read.
fn metadata(dep: &patchula_manager::Dependency) -> DependencyMetadata {
    DependencyMetadata {
        tag_digests: Default::default(),
        source: dep.source,
        kinds: vec![],
        locations: vec![],
        held_back: vec![],
        stale: Default::default(),
        lookup: None,
        resolution: None,
        advisories: vec![],
        image: None,
        credentialed: false,
    }
}

fn setup(
    target: &str,
    lock: bool,
) -> (
    Config,
    Repository,
    Vec<patchula_manager::Dependency>,
    Snapshot,
) {
    let mut config = patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"".into(),
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
        cargo_cooldowns: Default::default(),
        paths: BTreeSet::from(["mise.toml".into()]),
        files: BTreeMap::from([(
            "mise.toml".into(),
            Blob {
                sha: blob_sha(text),
                mode: "100644".into(),
                content: text.into(),
            },
        )]),
        public_only: Default::default(),
    };
    let report = Repository {
        repository: "demo/app".into(),
        status: Status::Complete,
        reason: None,
        warnings: vec![],
        skipped: vec![],
        notices: vec![],
        lookup_failures: vec![],
        tooling: Vec::new(),
        default_branch: Some("main".into()),
        commit: Some("base".into()),
        package_files: vec![PackageFile {
            path: "mise.toml".into(),
            dependencies: vec![ReportDependency {
                manager: d.manager,
                name: d.name.clone(),
                occurrence: None,
                package: d.package.clone(),
                registry: d.registry.as_ref().map(|r| r.recorded().to_owned()),
                current: Current {
                    value: Some(d.current.clone()),
                    version: d.resolved.clone(),
                    digest: None,
                },
                metadata: Some(metadata(d)),
                updates: vec![Update {
                    cooldown: None,
                    new_value: Some(target.into()),
                    new_version: Some(target.into()),
                    new_digest: None,
                    update_type: UpdateKind::Major,
                    group: None,
                    policy: None,
                    release_age: None,
                    fixes: Vec::new(),
                    image: None,
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
    let plan = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap()
    .pop()
    .unwrap();
    (plan, snapshot)
}
fn fake(snapshot: &Snapshot) -> Arc<FakeGitHub> {
    let tree: Vec<_> = snapshot
        .files
        .iter()
        .filter(|(_, b)| !b.sha.is_empty())
        .map(|(p, b)| json!({"path":p,"sha":b.sha,"mode":b.mode,"type":"blob"}))
        .collect();
    let server = Arc::new(FakeGitHub::new("demo/app"));
    server.seed("main", "base", tree);
    server
}
async fn client(fake: Arc<FakeGitHub>) -> Client {
    Client::with_transport(
        "offline",
        Cache::open(":memory:", Default::default()).await.unwrap(),
        Default::default(),
        fake,
    )
    .unwrap()
}
/// The run's inventory, read the way the engine reads it during the scan;
/// cleanup consults it for the first `owned` step of every head it holds.
async fn read_inventory(github: &Client, cache: &Cache) -> patchula_update::Inventory {
    let (config, repo) = pr_config();
    patchula_update::Inventory::read(github, cache, &repo, &config)
        .await
        .unwrap()
}

/// Hash this test's recorded GitHub requests and compare them with the
/// snapshot; `PATCHULA_UPDATE_TRANSCRIPTS=1` rewrites it instead.
fn check_transcript() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

#[tokio::test(start_paused = true)]
async fn create_update_and_repeat_use_one_verified_commit_on_base_and_no_prs() {
    let (mut first, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut first),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        first.action().status,
        BranchStatus::Created,
        "{:?}",
        first.action()
    );
    assert_eq!(first.action().verified, Some(true));
    let (mut second, _) = make_plan("22.1.0");
    assert_eq!(first.action().branch, second.action().branch);
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut second),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        second.action().status,
        BranchStatus::Updated,
        "{:?}",
        second.action()
    );
    assert_ne!(first.action().commit, second.action().commit);
    let count = server.state().writes.len();
    let (mut repeat, _) = make_plan("22.1.0");
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut repeat),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        repeat.action().status,
        BranchStatus::Unchanged,
        "{:?}",
        repeat.action()
    );
    assert_eq!(server.state().writes.len(), count);
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn identical_forced_rechecks_never_create_git_objects_or_update_refs() {
    let (mut first, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut first),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    let (repeat, _) = make_plan("22.0.0");
    let mut repeat = Draft::from(repeat)
        .retry(true)
        // Policy/renderer fingerprints do not require a new Git commit when
        // the complete intended tree and base are unchanged.
        .fingerprint("changed-policy-fingerprint")
        .build();
    server.state().writes.clear();
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut repeat),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        repeat.action().status,
        BranchStatus::Unchanged,
        "{:?}",
        repeat.action()
    );
    assert_eq!(repeat.action().commit, first.action().commit);
    assert!(
        server.state().writes.is_empty(),
        "no tree, commit or ref mutation on forced recheck"
    );
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn rechecks_repair_leftover_files_and_modes_even_when_planned_blobs_match() {
    let mut servers = Vec::new();
    for extra_file in [false, true] {
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        servers.push(server.clone());
        let github = client(server.clone()).await;
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut first),
            &snapshot,
            "main",
            "patchula[bot]",
            Rebase::Behind,
            false,
        )
        .await;
        {
            let mut state = server.state();
            let entries = state
                .repo_mut()
                .trees
                .get_mut(first.tree().unwrap())
                .unwrap()
                .as_array_mut()
                .unwrap();
            if extra_file {
                entries.push(json!({"path":"obsolete.lock","mode":"100644","type":"blob","sha":blob_sha("obsolete")}));
            } else {
                entries
                    .iter_mut()
                    .find(|entry| entry["path"] == "mise.toml")
                    .unwrap()["mode"] = json!("100755");
            }
            state.writes.clear();
        }
        let (repeat, _) = make_plan("22.0.0");
        let mut repeat = Draft::from(repeat).retry(true).build();
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut repeat),
            &snapshot,
            "main",
            "patchula[bot]",
            Rebase::Behind,
            false,
        )
        .await;
        assert_eq!(repeat.action().status, BranchStatus::Updated);
        let state = server.state();
        let entries = state.repo().trees[repeat.tree().unwrap()]
            .as_array()
            .unwrap();
        assert!(!entries.iter().any(|entry| entry["path"] == "obsolete.lock"));
        assert!(
            entries
                .iter()
                .any(|entry| entry["path"] == "mise.toml" && entry["mode"] == "100644")
        );
        assert_eq!(state.writes.len(), 3);
    }
    check_transcript();
}
#[tokio::test(start_paused = true)]
async fn dry_run_plans_diffs_and_never_sends_mutations() {
    let (mut plan, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    patchula_update::apply_all(
        &client(server.clone()).await,
        std::slice::from_mut(&mut plan),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        true,
    )
    .await;
    assert_eq!(
        plan.action().status,
        BranchStatus::Planned,
        "{:?}",
        plan.action()
    );
    assert!(
        plan.action().files[0]
            .diff
            .contains("--- a/mise.toml\n+++ b/mise.toml\n@@")
    );
    assert!(
        plan.action().files[0]
            .diff
            .contains("+node = '22.0.0' # keep")
    );
    assert!(server.state().writes.is_empty());
    assert!(server.state().requests.is_empty());
    check_transcript();
}
#[tokio::test(start_paused = true)]
async fn behind_rebases_and_conflicts_keeps_an_identical_manifest_on_old_base() {
    let mut servers = Vec::new();
    for mode in [Rebase::Behind, Rebase::Conflicts] {
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        servers.push(server.clone());
        let github = client(server.clone()).await;
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut first),
            &snapshot,
            "main",
            "patchula[bot]",
            mode,
            false,
        )
        .await;
        {
            let mut s = server.state();
            s.repo_mut().move_base("base2");
            s.repo_mut()
                .refs
                .insert("refs/heads/main".into(), "base2".into());
            s.repo_mut().commits.insert(
                "base2".into(),
                json!({"sha":"base2","tree":{"sha":"base-tree"}}),
            );
        }
        let (next, _) = make_plan("22.0.0");
        let mut next = Draft::from(next).base("base2").retry(true).build();
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut next),
            &snapshot,
            "main",
            "patchula[bot]",
            mode,
            false,
        )
        .await;
        assert_eq!(
            next.action().status,
            if mode == Rebase::Behind {
                BranchStatus::Rebased
            } else {
                BranchStatus::Unchanged
            },
            "{:?}",
            next.action()
        );
    }
    check_transcript();
}
#[tokio::test(start_paused = true)]
async fn human_commit_is_protected_even_with_copied_patchula_markers() {
    let (mut first, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut first),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    server
        .state()
        .repo_mut()
        .commits
        .get_mut(first.action().commit.as_ref().unwrap())
        .unwrap()["actor"] = json!("human");
    let count = server.state().writes.len();
    let (mut next, _) = make_plan("22.1.0");
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut next),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        next.action().status,
        BranchStatus::Protected,
        "{:?}",
        next.action()
    );
    assert_eq!(server.state().writes.len(), count);
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn a_human_below_the_bot_head_and_a_foreign_committer_are_protected() {
    let mut servers = Vec::new();
    for earlier in [false, true] {
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        servers.push(server.clone());
        let github = client(server.clone()).await;
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut first),
            &snapshot,
            "main",
            "patchula[bot]",
            Rebase::Behind,
            false,
        )
        .await;
        {
            let mut state = server.state();
            let sha = first.action().commit.as_ref().unwrap();
            if earlier {
                let mut human = state.repo().commits[sha].clone();
                human["actor"] = json!("human");
                state.repo_mut().commits.insert("human".into(), human);
                state.repo_mut().history.push("human".into());
            } else {
                state.repo_mut().commits.get_mut(sha).unwrap()["committer_login"] = json!("human");
            }
        }
        let count = server.state().writes.len();
        let (mut next, _) = make_plan("22.1.0");
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut next),
            &snapshot,
            "main",
            "patchula[bot]",
            Rebase::Behind,
            false,
        )
        .await;
        assert_eq!(
            next.action().status,
            BranchStatus::Protected,
            "{:?}",
            next.action()
        );
        assert_eq!(server.state().writes.len(), count);
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn conflicts_mode_recreates_when_the_default_changes_a_touched_file() {
    let (mut first, mut snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut first),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Conflicts,
        false,
    )
    .await;
    let blob = snapshot.files.get_mut("mise.toml").unwrap();
    blob.content.push_str("# new base comment\n");
    blob.sha = blob_sha(&blob.content);
    {
        let mut state = server.state();
        state.repo_mut().move_base("base2");
        state
            .repo_mut()
            .refs
            .insert("refs/heads/main".into(), "base2".into());
        let mut tree = state.repo().trees["base-tree"].clone();
        tree[0]["sha"] = json!(blob.sha);
        state.repo_mut().trees.insert("base-tree2".into(), tree);
        state.repo_mut().commits.insert(
            "base2".into(),
            json!({"sha":"base2","tree":{"sha":"base-tree2"}}),
        );
    }
    let (config, mut repo, deps, _) = setup("22.0.0", false);
    repo.commit = Some("base2".into());
    let mut next = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap()
    .pop()
    .unwrap();
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut next),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Conflicts,
        false,
    )
    .await;
    assert_eq!(
        next.action().status,
        BranchStatus::Rebased,
        "{:?}",
        next.action()
    );
    assert!(next.contents()["mise.toml"].contains("# new base comment"));
    check_transcript();
}
#[tokio::test(start_paused = true)]
async fn stale_blob_content_tree_moved_base_unsigned_and_lease_race_fail_closed() {
    let mut servers = Vec::new();
    for case in 0..7 {
        let (mut plan, mut snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        servers.push(server.clone());
        {
            let mut s = server.state();
            match case {
                0 => snapshot
                    .files
                    .get_mut("mise.toml")
                    .unwrap()
                    .content
                    .push_str("# changed"),
                1 => s.repo_mut().trees.get_mut("base-tree").unwrap()[0]["sha"] = json!("changed"),
                2 => {
                    s.repo_mut()
                        .refs
                        .insert("refs/heads/main".into(), "new-base".into());
                }
                3 => s.settings.unsigned = true,
                4 => server.update_refs_fail(),
                5 => s.settings.truncated = true,
                _ => server.repo_read_malformed(),
            }
        }
        patchula_update::apply_all(
            &client(server.clone()).await,
            std::slice::from_mut(&mut plan),
            &snapshot,
            "main",
            "patchula[bot]",
            Rebase::Behind,
            false,
        )
        .await;
        assert_eq!(
            plan.action().status,
            if case == 2 {
                BranchStatus::Stale
            } else {
                BranchStatus::Error
            },
            "case={case}: {:?}",
            plan.action()
        );
        assert!(
            !server
                .state()
                .repo()
                .refs
                .contains_key(&format!("refs/heads/{}", plan.action().branch))
        );
        if matches!(case, 0..=2 | 5 | 6) {
            assert!(server.state().writes.is_empty());
        }
    }
    check_transcript();
}
#[tokio::test]
async fn moved_default_base_skips_every_branch_in_a_shared_write_batch() {
    let (plan, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    server
        .state()
        .repo_mut()
        .refs
        .insert("refs/heads/main".into(), "new-base".into());
    let (second, _) = make_plan("22.0.0");
    let second = Draft::from(second).branch("patchula/second").build();
    let mut plans = [plan, second];
    patchula_update::apply_all(
        &client(server.clone()).await,
        &mut plans,
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert!(
        plans
            .iter()
            .all(|plan| plan.action().status == BranchStatus::Stale)
    );
    assert!(server.state().writes.is_empty());
    check_transcript();
}

#[test]
fn names_are_readable_and_validate_git_refs() {
    assert_eq!(
        branch_name("patchula/", "scope-a-2.x").unwrap(),
        "patchula/scope-a-2.x"
    );
    assert!(branch_name("../bad/", "a").is_err());
    assert!(branch_name("custom/deps/", "a").unwrap() == "custom/deps/a");
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
        .map(|(p, b)| (p.clone(), b.content.clone().into()))
        .collect();
    let mut deps = Scanner::new(&config)
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    let first = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(first.action().status, BranchStatus::Planned);
    assert_eq!(first.action().files.len(), 2);
    assert!(first.action().branch.starts_with("deps/major-node-runtime"));
    assert!(first.action().message.starts_with("build(deps):"));
    repo.package_files.reverse();
    deps.reverse();
    let second = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap()
    .pop()
    .unwrap();
    assert_eq!(first.action(), second.action());
    assert_eq!(first.fingerprint(), second.fingerprint());
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
    r.manager = Manager::Npm;
    r.name.clone_from(&d.name);
    r.package.clone_from(&d.package);
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
    // Binary Bun locks stay unsupported; pnpm and Yarn locks are supported.
    snapshot.paths.insert("bun.lockb".into());
    for allow in [false, true] {
        let p = plan(
            &repo,
            &config,
            &deps,
            &snapshot,
            allow,
            None,
            chrono::Utc::now(),
        )
        .unwrap()
        .pop()
        .unwrap();
        assert!(p.action().lockfile_pending);
        if !allow {
            assert!(
                p.action()
                    .reason
                    .as_deref()
                    .is_some_and(|r| r.starts_with("lockfile pending: unsupported lockfile format")),
                "{:?}",
                p.action().reason
            );
        }
        assert_eq!(
            p.action().status,
            if allow {
                BranchStatus::Planned
            } else {
                BranchStatus::Skipped
            }
        );
        assert!(!p.action().files.is_empty());
    }
}

/// A `0.x` crate's in-range release and its next minor share a branch line, so
/// both reach one branch. Only the higher is written, and it is the only lock
/// job: the lower would address an entry the higher has already replaced.
#[test]
fn a_zero_x_crates_in_range_release_yields_to_its_next_minor() {
    let (config, mut repo, _, mut snapshot) = setup("22.0.0", true);
    let manifest = "[package]\nname = 'app'\nversion = '0.1.0'\nedition = '2021'\n\n[dependencies]\nhcl-edit = \"0.9.7\"\n";
    let lock = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"hcl-edit\"]\n\n[[package]]\nname = \"hcl-edit\"\nversion = \"0.9.7\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    let mut config = config;
    config.managers = vec![Manager::Cargo];
    let files = BTreeMap::<String, String>::from([
        ("Cargo.toml".into(), manifest.into()),
        ("Cargo.lock".into(), lock.into()),
    ]);
    let deps = Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let d = &deps[0];
    assert_eq!(d.resolved.as_deref(), Some("0.9.7"));
    snapshot.paths = files.keys().cloned().collect();
    snapshot.files = files
        .iter()
        .map(|(path, content)| {
            (
                path.clone(),
                Blob {
                    sha: blob_sha(content),
                    mode: "100644".into(),
                    content: content.to_owned(),
                },
            )
        })
        .collect();
    repo.package_files[0].path = "Cargo.toml".into();
    let r = &mut repo.package_files[0].dependencies[0];
    r.manager = Manager::Cargo;
    r.name.clone_from(&d.name);
    r.package.clone_from(&d.package);
    r.current.value = Some(d.current.clone());
    r.current.version = Some("0.9.7".into());
    let template = r.updates[0].clone();
    // The lookup reports the in-range patch (lock only) and the next minor.
    r.updates = vec![
        Update {
            new_value: None,
            new_version: Some("0.9.9".into()),
            update_type: UpdateKind::Patch,
            ..template.clone()
        },
        Update {
            new_value: Some("0.10.0".into()),
            new_version: Some("0.10.0".into()),
            update_type: UpdateKind::Major,
            ..template
        },
    ];
    let plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(
        plans.len(),
        1,
        "{:?}",
        plans.iter().map(|p| &p.action().branch).collect::<Vec<_>>()
    );
    let p = &plans[0];
    assert_eq!(p.changes().len(), 1);
    assert_eq!(p.changes()[0].version.as_deref(), Some("0.10.0"));
    assert_eq!(p.lock_jobs().len(), 1, "{:?}", p.lock_jobs());
    assert_eq!(p.lock_jobs()[0].target(), Some("0.10.0"));
    assert!(
        p.action().message.ends_with("update hcl-edit to 0.10.0"),
        "{}",
        p.action().message
    );
}

#[test]
fn pnpm_and_yarn_locks_are_supported_and_never_pending() {
    let (config, mut repo, _, mut snapshot) = setup("22.0.0", true);
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
    r.manager = Manager::Npm;
    r.name.clone_from(&d.name);
    r.package.clone_from(&d.package);
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
    for lock in ["pnpm-lock.yaml", "yarn.lock", "aube-lock.yaml"] {
        snapshot.paths.remove("bun.lock");
        snapshot.paths.insert(lock.into());
        for allow in [false, true] {
            let p = plan(
                &repo,
                &config,
                &deps,
                &snapshot,
                allow,
                None,
                chrono::Utc::now(),
            )
            .unwrap()
            .pop()
            .unwrap();
            assert!(!p.action().lockfile_pending, "{lock}");
            assert_eq!(p.action().status, BranchStatus::Planned, "{lock}");
            assert_eq!(p.lock_jobs().len(), 1, "{lock}");
            assert_eq!(
                p.lock_jobs()[0].ecosystem,
                patchula_lockfiles::Ecosystem::Node,
                "{lock}"
            );
        }
        snapshot.paths.remove(lock);
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
        u.update_type = kind.parse().unwrap();
        u.new_value = Some(version.into());
        u.new_version = Some(version.into());
        updates.push(u);
    }
    let plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    let names: BTreeSet<_> = plans.iter().map(|p| p.action().branch.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "patchula/node-20.x",
            "patchula/node-24.x",
            "patchula/pin-dependencies"
        ])
    );
    let nonmajor = plans
        .iter()
        .find(|p| p.action().branch.ends_with("20.x"))
        .unwrap();
    assert!(nonmajor.contents()["mise.toml"].contains("20.9.0"));
    assert_eq!(nonmajor.action().status, BranchStatus::Planned);
}

#[tokio::test(start_paused = true)]
async fn migration_deletes_owned_hash_branch_with_a_lease_and_preserves_human_work() {
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    let old = "patchula/node-major-0123456789abcdef";
    {
        let mut s = server.state();
        let sha = s
            .repo_mut()
            .refs
            .remove(&format!("refs/heads/{}", p.action().branch))
            .unwrap();
        let commit = s.repo_mut().commits.get_mut(&sha).unwrap();
        commit["message"] = json!(commit["message"].as_str().unwrap().replace(
            "Patchula-Base: base",
            "Patchula-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        let mut human = commit.clone();
        human["actor"] = json!("human");
        s.repo_mut().commits.insert("human".into(), human);
        s.repo_mut().refs.insert(format!("refs/heads/{old}"), sha);
        s.repo_mut()
            .refs
            .insert("refs/heads/patchula/human".into(), "human".into());
    }
    let cache = github.cache();
    let inventory = read_inventory(&github, &cache).await;
    let actions = patchula_update::cleanup(
        &github,
        &cache,
        "demo/app",
        "base",
        "main",
        &[p],
        &inventory,
        false,
    )
    .await
    .unwrap();
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].branch, old);
    // The inventory's copies of the two tips answered the tip reads; both
    // commits recorded a base, so only the compare probes remain.
    let probes = server
        .requests()
        .iter()
        .filter(|(method, path)| {
            method == "GET" && path.contains("/commits/") && !path.contains("/git/")
        })
        .count();
    assert_eq!(probes, 0);
    assert_eq!(actions[0].status, BranchStatus::Deleted);
    let s = server.state();
    assert!(!s.repo().refs.contains_key(&format!("refs/heads/{old}")));
    assert!(s.repo().refs.contains_key("refs/heads/patchula/human"));
    assert_eq!(s.repo().refs["refs/heads/main"], "base");
    check_transcript();
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
    r.manager = Manager::Dockerfile;
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
    let plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    let names: BTreeSet<_> = plans.iter().map(|p| p.action().branch.as_str()).collect();
    assert_eq!(
        names,
        BTreeSet::from(["patchula/docker-node-22.x", "patchula/mise-node-22.x"])
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
    let plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(plans.len(), 2);
    assert_eq!(
        plans
            .iter()
            .find(|p| p.action().branch.contains("mise-node"))
            .unwrap()
            .action()
            .files
            .len(),
        2
    );
}

#[tokio::test(start_paused = true)]
async fn shared_base_write_trace_and_request_budget() {
    let (p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    server.state().settings.latency = std::time::Duration::from_millis(150);
    let github = client(server.clone()).await;
    let mut plans: Vec<_> = (0..12)
        .map(|n| {
            let (p, _) = make_plan("22.0.0");
            Draft::from(p)
                .branch(format!("patchula/tool-{n}-22.x"))
                .build()
        })
        .collect();
    let started = tokio::time::Instant::now();
    patchula_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        true,
    )
    .await;
    assert_eq!(github.stats().await.requests, 0);
    eprintln!(
        "dry-run: {} requests, simulated elapsed {:?}",
        github.stats().await.requests,
        started.elapsed()
    );
    let started = tokio::time::Instant::now();
    patchula_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert!(
        plans
            .iter()
            .all(|p| p.action().status == BranchStatus::Created)
    );
    let state = server.state();
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
            .all(|(_, b)| b["tree"][0]["content"] == p.contents()["mise.toml"])
    );
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn forced_batch_recheck_has_no_mutations_and_no_viewer_read() {
    let (_, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    server.state().settings.latency = std::time::Duration::from_millis(150);
    let github = client(server.clone()).await;
    let plans = || {
        (0..17)
            .map(|n| {
                let (p, _) = make_plan("22.0.0");
                Draft::from(p)
                    .branch(format!("patchula/tool-{n}-22.x"))
                    .retry(true)
                    .build()
            })
            .collect::<Vec<_>>()
    };
    patchula_update::apply_all(
        &github,
        &mut plans(),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    {
        let mut state = server.state();
        state.requests.clear();
        state.writes.clear();
    }
    let mut repeat = plans();
    let started = tokio::time::Instant::now();
    patchula_update::apply_all(
        &github,
        &mut repeat,
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert!(
        repeat
            .iter()
            .all(|p| p.action().status == BranchStatus::Unchanged)
    );
    let state = server.state();
    assert!(state.writes.is_empty());
    // The actor comes with the run's inventory, so no viewer query is sent.
    assert_eq!(
        state
            .requests
            .iter()
            .filter(|(_, path)| path == "/graphql")
            .count(),
        0
    );
    // The base commit and recursive tree are immutable reads now: the first
    // `apply_all` cached them, so this prepare sends only the two fresh
    // precondition reads beside the per-branch probes.
    assert_eq!(state.requests.len(), 2 + 17 * 4);
    eprintln!(
        "forced 17-branch recheck: {} reads, 0 mutations, {:?} simulated at 150ms/request",
        state.requests.len(),
        started.elapsed()
    );
    check_transcript();
}

fn pr_config() -> (Config, Repository) {
    let (mut config, repo, ..) = setup("22.0.0", false);
    config.prs.concurrent = None;
    config.prs.per_hour = None;
    (config, repo)
}
/// Publish with the note sources a run builds, composing what
/// `publish_with_sources` takes directly.
async fn publish(state: &mut patchula_update::Lifecycle, client: &Client, plans: &mut [Plan]) {
    let sources = patchula_notes::Client::new(patchula_datasource::Client::new(
        client.clone(),
        client.public_http(patchula_datasource::http_options().rules),
        client.cache(),
    ));
    state.publish_with_sources(client, &sources, plans).await;
}
async fn pr_cycle(
    client: &Client,
    repo: &Repository,
    config: &Config,
    plans: &mut [Plan],
    snapshot: &Snapshot,
) -> Vec<patchula_report::PullRequestAction> {
    let mut state = patchula_update::Lifecycle::prepare_with_controls(
        client,
        repo,
        plans,
        config,
        chrono::Utc::now(),
        patchula_words::Recheck::NextRun,
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    patchula_update::apply_all(
        client,
        plans,
        snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    publish(&mut state, client, plans).await;
    state.actions
}

#[tokio::test(start_paused = true)]
async fn verified_receipts_reduce_warm_write_to_one_inventory_and_detect_changes() {
    let mut servers = Vec::new();
    for case in [
        "same",
        "line-endings",
        "notes",
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
        "actor",
        "user-with-bot-name",
        "signature",
    ] {
        let (mut config, mut repo) = pr_config();
        config.prs.rebase = Rebase::Behind;
        if case == "conflicts" {
            config.prs.rebase = Rebase::Conflicts;
        }
        let (p, snapshot) = make_plan("22.0.0");
        let mut draft = Draft::from(p);
        if case == "notes" {
            draft.changes_mut()[0].source = Source::GithubTags;
            draft.changes_mut()[0].package = "actions/checkout".into();
            draft.changes_mut()[0].release_notes = true;
        }
        draft.changes_mut()[0].labels = vec!["dependencies".into()];
        if case == "generated" {
            draft = draft
                .fingerprint("d".repeat(64))
                .lock_job(patchula_lockfiles::Job {
                    ecosystem: patchula_lockfiles::Ecosystem::Cargo,
                    directory: String::new(),
                    scope: patchula_lockfiles::Scope::Directory,
                    exact_version: false,
                    publish_age_allow: false,
                    wait_hours: 0,
                })
                .lockfiles_ready(true);
        }
        let mut p = draft.build();
        let server = fake(&snapshot);
        servers.push(server.clone());
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
            let (next, _) = make_plan("22.0.0");
            p = Draft::from(next).changes(p.changes().to_vec()).build();
        }
        let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
            &github,
            &repo,
            std::slice::from_mut(&mut p),
            &config,
            chrono::Utc::now(),
            Default::default(),
            &Default::default(),
            "patchula[bot]",
        )
        .await
        .unwrap();
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut p),
            &snapshot,
            "main",
            "patchula[bot]",
            config.prs.rebase,
            false,
        )
        .await;
        publish(&mut lifecycle, &github, std::slice::from_mut(&mut p)).await;
        patchula_update::save_receipts(
            &cache,
            &repo,
            &config,
            std::slice::from_ref(&p),
            &lifecycle,
        )
        .await
        .unwrap();
        let (next, _) = make_plan(if case == "plan" { "22.1.0" } else { "22.0.0" });
        let mut draft = Draft::from(next);
        if case == "notes" {
            *draft.changes_mut() = p.changes().to_vec();
        }
        draft.changes_mut()[0]
            .labels
            .clone_from(&p.changes()[0].labels);
        *draft.lock_jobs_mut() = p.lock_jobs().to_vec();
        let mut next = draft.build();
        {
            let mut s = server.state();
            match case {
                "pages" => {
                    s.settings.ref_page_size = Some(1);
                    // A second in-prefix branch, not the Count's, so the
                    // prefix-narrowed walk still takes two refs pages.
                    s.repo_mut().commits.insert(
                        "human".into(),
                        json!({"sha":"human","tree":{"sha":"t"},"message":"human","actor":"human"}),
                    );
                    s.repo_mut()
                        .refs
                        .insert("refs/heads/patchula/human".into(), "human".into());
                }
                "human" => {
                    let mut commit = s.repo().commits[p.action().commit.as_ref().unwrap()].clone();
                    commit["message"] = json!("Human work");
                    s.repo_mut().commits.insert("human".into(), commit);
                    s.repo_mut()
                        .refs
                        .insert(format!("refs/heads/{}", p.action().branch), "human".into());
                }
                "actor" => {
                    s.repo_mut().prs.get_mut(&1).unwrap()["user"]["login"] = json!("other[bot]")
                }
                "user-with-bot-name" => {
                    s.repo_mut().prs.get_mut(&1).unwrap()["graphql_author"] =
                        json!({"__typename":"User","login":"patchula"})
                }
                "signature" => s.settings.unsigned = true,
                "body" => {
                    s.repo_mut().prs.get_mut(&1).unwrap()["body"] = json!("Human edited body")
                }
                "line-endings" => {
                    let text = s.repo().prs[&1]["body"]
                        .as_str()
                        .unwrap()
                        .replace('\n', "\r\n");
                    s.repo_mut().prs.get_mut(&1).unwrap()["body"] = json!(text);
                }
                "title" => s.repo_mut().prs.get_mut(&1).unwrap()["title"] = json!("Human title"),
                "labels" => s.repo_mut().prs.get_mut(&1).unwrap()["labels"] = json!([]),
                "merge" => {
                    s.repo_mut().prs.get_mut(&1).unwrap()["auto_merge"] =
                        json!({"enabledAt":"2026-01-01T00:00:00Z"})
                }
                "closed" => s.repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed"),
                "base" | "conflicts" => {
                    s.repo_mut().move_base("base2");
                    repo.commit = Some("base2".into());
                    next = Draft::from(next).base("base2").build();
                }
                "obsolete" | "foreign-obsolete" => {
                    let mut commit = s.repo().commits[p.action().commit.as_ref().unwrap()].clone();
                    if case == "foreign-obsolete" {
                        commit["actor"] = json!("other-app[bot]");
                    }
                    s.repo_mut().commits.insert("obsolete".into(), commit);
                    s.repo_mut()
                        .refs
                        .insert("refs/heads/patchula/obsolete".into(), "obsolete".into());
                }
                _ => {}
            }
            s.requests.clear();
            s.settings.latency = std::time::Duration::from_millis(200);
        }
        let started = tokio::time::Instant::now();
        let inventory = patchula_update::Inventory::read(&github, &cache, &repo, &config)
            .await
            .unwrap();
        let all = patchula_update::restore_inventory(
            &cache,
            &repo,
            &config,
            std::slice::from_mut(&mut next),
            Default::default(),
            None,
            &inventory,
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
            all == patchula_update::RestoreOutcome::Complete,
            matches!(
                case,
                "same"
                    | "line-endings"
                    | "notes"
                    | "adopted"
                    | "generated"
                    | "conflicts"
                    | "pages"
                    | "foreign-obsolete"
            ),
            "{case}"
        );
        assert_eq!(
            next.action().status == BranchStatus::Unchanged,
            matches!(
                case,
                "same"
                    | "line-endings"
                    | "notes"
                    | "adopted"
                    | "generated"
                    | "conflicts"
                    | "pages"
                    | "obsolete"
                    | "foreign-obsolete"
            ),
            "{case}"
        );
        let s = server.state();
        assert_eq!(
            s.requests.len(),
            // The two pages, then the closing re-read of the first page's
            // constants.
            if case == "pages" { 3 } else { 1 },
            "{case}"
        );
        assert!(
            s.requests
                .iter()
                .all(|(m, p)| m == "POST" && p == "/graphql")
        );
    }
    check_transcript();
}
/// A body with a retryable release-notes failure gets no write receipt:
/// the next run, with nothing else changed, is not restored from one and
/// fetches the notes again. Its lasting failure then settles the body.
#[tokio::test(start_paused = true)]
async fn a_retryable_notes_failure_is_fetched_again_on_the_next_run() {
    use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
    let (config, repo) = pr_config();
    let with_notes = |plan: Plan| {
        let mut draft = Draft::from(plan);
        draft.changes_mut()[0].source = Source::Npm;
        draft.changes_mut()[0].package = "left-pad".into();
        draft.changes_mut()[0].release_notes = true;
        draft.build()
    };
    let (p, snapshot) = make_plan("22.0.0");
    let mut p = with_notes(p);
    let server = fake(&snapshot);
    let hosts = Arc::new(patchula_testkit::github::Hosts::new(server.clone()));
    let reads = Arc::new(AtomicUsize::new(0));
    let status = Arc::new(AtomicU16::new(503));
    {
        let (reads, status) = (reads.clone(), status.clone());
        hosts.script("registry.npmjs.org", move |_, _| {
            reads.fetch_add(1, Ordering::SeqCst);
            patchula_http::Response {
                status: reqwest::StatusCode::from_u16(status.load(Ordering::SeqCst)).unwrap(),
                headers: Default::default(),
                body: b"{}".to_vec(),
                stale: None,
            }
        });
    }
    let github = Client::with_transport(
        "offline",
        Cache::open(":memory:", Default::default()).await.unwrap(),
        Default::default(),
        hosts.clone(),
    )
    .unwrap();
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let body = || {
        server.state().repo().prs[&1]["body"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // Run 1: the registry answers 503.
    let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut p),
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        "patchula[bot]",
        config.prs.rebase,
        false,
    )
    .await;
    publish(&mut lifecycle, &github, std::slice::from_mut(&mut p)).await;
    assert!(reads.load(Ordering::SeqCst) > 0);
    assert!(body().contains("could not read these release notes this run"));
    patchula_update::save_receipts(&cache, &repo, &config, std::slice::from_ref(&p), &lifecycle)
        .await
        .unwrap();
    // Run 2: nothing else changed; the registry no longer knows the
    // package, a lasting answer.
    status.store(404, Ordering::SeqCst);
    reads.store(0, Ordering::SeqCst);
    let (next, _) = make_plan("22.0.0");
    let mut next = with_notes(next);
    let inventory = patchula_update::Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    let outcome = patchula_update::restore_inventory(
        &cache,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        Default::default(),
        None,
        &inventory,
    )
    .await
    .unwrap();
    assert_ne!(outcome, patchula_update::RestoreOutcome::Complete);
    assert_eq!(next.action().status, BranchStatus::Planned);
    let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut next),
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut next),
        &snapshot,
        "main",
        "patchula[bot]",
        config.prs.rebase,
        false,
    )
    .await;
    publish(&mut lifecycle, &github, std::slice::from_mut(&mut next)).await;
    assert!(
        reads.load(Ordering::SeqCst) > 0,
        "run 2 fetches the notes again"
    );
    let settled = body();
    assert!(
        settled.contains("source repository could not be resolved"),
        "{settled}"
    );
    assert!(!settled.contains("could not read these release notes"));
    check_transcript();
}
#[tokio::test(start_paused = true)]
async fn published_pr_heads_wait_for_our_old_head_but_reject_foreign_commits() {
    let mut servers = Vec::new();
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
        servers.push(server.clone());
        let github = client(server.clone()).await;
        pr_cycle(
            &github,
            &repo,
            &config,
            std::slice::from_mut(&mut first),
            &snapshot,
        )
        .await;
        let (next, _) = make_plan("22.1.0");
        repo.commit = Some("base2".into());
        let mut next = Draft::from(next).base("base2").build();
        {
            let mut s = server.state();
            let mut base = s.repo().commits["base"].clone();
            base["sha"] = json!("base2");
            s.repo_mut().commits.insert("base2".into(), base);
            s.repo_mut().move_base("base2");
            s.repo_mut()
                .refs
                .insert("refs/heads/main".into(), "base2".into());
        }
        let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
            &github,
            &repo,
            std::slice::from_mut(&mut next),
            &config,
            chrono::Utc::now(),
            Default::default(),
            &Default::default(),
            "patchula[bot]",
        )
        .await
        .unwrap();
        {
            let mut s = server.state();
            s.settings.pr_lag_reads = if case == "timeout" { usize::MAX } else { 2 };
            if case == "foreign" {
                server.override_pull_head("human");
            }
            if case == "closed" {
                server.close_pulls_on_read();
            }
        }
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut next),
            &snapshot,
            "main",
            "patchula[bot]",
            Rebase::Behind,
            false,
        )
        .await;
        assert_eq!(next.action().status, BranchStatus::Rebased);
        server.state().requests.clear();
        if case == "closed-while-waiting" {
            server.close_pulls_after_reads(1);
        }
        let started = tokio::time::Instant::now();
        publish(&mut lifecycle, &github, std::slice::from_mut(&mut next)).await;
        let result = &lifecycle.actions[0];
        let s = server.state();
        let reads = s
            .requests
            .iter()
            .filter(|(m, p)| m == "GET" && p.ends_with("/pulls/1"))
            .count();
        match case {
            "lag" => {
                assert_ne!(
                    result.status,
                    patchula_report::PullRequestStatus::Error,
                    "{result:?}"
                );
                assert_eq!(reads, 3);
                assert!(started.elapsed() >= std::time::Duration::from_millis(300));
            }
            "timeout" => {
                assert_eq!(result.status, patchula_report::PullRequestStatus::Stale);
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
                assert_eq!(result.status, patchula_report::PullRequestStatus::Stale);
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
                assert_eq!(result.status, patchula_report::PullRequestStatus::Skipped);
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
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn receipts_save_verified_branches_despite_unrelated_pr_errors_and_skips() {
    let (config, repo) = pr_config();
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
    server.state().repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
    let (closed, _) = make_plan("22.0.0");
    let (live, _) = make_plan("24.0.0");
    let (mut failed, _) = make_plan("26.0.0");
    failed.error("unrelated failure");
    let (mut skipped, _) = make_plan("28.0.0");
    skipped.hold(patchula_update::Hold::Blocked(
        "unrelated policy skip".into(),
    ));
    let mut plans = vec![closed, live, failed, skipped];
    let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        &mut plans,
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert_eq!(plans[0].action().status, BranchStatus::Skipped);
    patchula_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        "patchula[bot]",
        config.prs.rebase,
        false,
    )
    .await;
    publish(&mut lifecycle, &github, &mut plans).await;
    assert_eq!(plans[1].action().verified, Some(true));
    lifecycle.actions.push(patchula_report::PullRequestAction {
        repository: repo.repository.clone(),
        branch: "unrelated".into(),
        number: None,
        url: None,
        status: patchula_report::PullRequestStatus::Error,
        reason: Some("unrelated publication failure".into()),
    });
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    patchula_update::save_receipts(&cache, &repo, &config, &plans, &lifecycle)
        .await
        .unwrap();
    let receipts = patchula_update::stored_receipts(&cache, &repo.repository, &config.prs.branch)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        receipts["writes"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        [&plans[1].action().branch]
    );
    assert_eq!(
        receipts["closed"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        [&plans[0].action().branch]
    );
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn closed_update_receipts_are_freshly_verified_and_invalidated() {
    let mut servers = Vec::new();
    for case in [
        "same", "reopened", "merged", "body", "actor", "target", "deleted",
    ] {
        let (config, repo) = pr_config();
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        servers.push(server.clone());
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
        server.state().repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
        let (mut next, _) = make_plan("22.0.0");
        let lifecycle = patchula_update::Lifecycle::prepare_with_controls(
            &github,
            &repo,
            std::slice::from_mut(&mut next),
            &config,
            chrono::Utc::now(),
            Default::default(),
            &Default::default(),
            "patchula[bot]",
        )
        .await
        .unwrap();
        assert_eq!(next.action().status, BranchStatus::Skipped);
        patchula_update::save_receipts(&cache, &repo, &config, &[next], &lifecycle)
            .await
            .unwrap();
        {
            let mut s = server.state();
            match case {
                "reopened" => s.repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("open"),
                "merged" => {
                    s.repo_mut().prs.get_mut(&1).unwrap()["merged_at"] =
                        json!("2026-10-15T00:00:00Z")
                }
                "body" => s.repo_mut().prs.get_mut(&1).unwrap()["body"] = json!("changed"),
                "actor" => {
                    s.repo_mut().prs.get_mut(&1).unwrap()["user"]["login"] = json!("other-app[bot]")
                }
                "deleted" => {
                    s.repo_mut().prs.remove(&1);
                }
                _ => {}
            }
            s.requests.clear();
        }
        let (mut warm, _) = make_plan(if case == "target" { "22.1.0" } else { "22.0.0" });
        let inventory = patchula_update::Inventory::read(&github, &cache, &repo, &config)
            .await
            .unwrap();
        let all = patchula_update::restore_inventory(
            &cache,
            &repo,
            &config,
            std::slice::from_mut(&mut warm),
            Default::default(),
            None,
            &inventory,
        )
        .await
        .unwrap();
        assert_eq!(
            all == patchula_update::RestoreOutcome::Complete,
            case == "same",
            "{case}"
        );
        assert_eq!(
            warm.action().status == BranchStatus::Skipped,
            case == "same",
            "{case}"
        );
        assert_eq!(
            server.state().requests,
            [("POST".into(), "/graphql".into())]
        );
    }
    check_transcript();
}

/// The split inventory gathers `nodes(ids:$closed)` from its first page
/// alone, so a pull request the receipts still name closed can be reopened
/// while the walk pages: the closing re-read sees it open and the plan
/// takes the full-write path, as the last page's answer made it before.
#[tokio::test(start_paused = true)]
async fn a_pull_reopened_mid_walk_takes_the_full_write_path() {
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
    server.state().repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
    let (mut next, _) = make_plan("22.0.0");
    let lifecycle = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut next),
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert_eq!(next.action().status, BranchStatus::Skipped);
    patchula_update::save_receipts(&cache, &repo, &config, &[next], &lifecycle)
        .await
        .unwrap();
    // One ref a page: the walk's first page still reports the pull request
    // closed and the second page's request point reopens it. A second
    // in-prefix branch, not the Count's, keeps the prefix-narrowed walk at
    // two pages.
    {
        let mut s = server.state();
        s.repo_mut().commits.insert(
            "human".into(),
            json!({"sha":"human","tree":{"sha":"t"},"message":"human","actor":"human"}),
        );
        s.repo_mut()
            .refs
            .insert("refs/heads/patchula/human".into(), "human".into());
        s.settings.ref_page_size = Some(1);
    }
    server
        .on(patchula_testkit::github::Matcher::Graphql(
            "associatedPullRequests",
        ))
        .after(1)
        .mutate(|state, _| {
            state.repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("open");
        });
    server.state().requests.clear();
    let (mut warm, _) = make_plan("22.0.0");
    let inventory = patchula_update::Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    let outcome = patchula_update::restore_inventory(
        &cache,
        &repo,
        &config,
        std::slice::from_mut(&mut warm),
        Default::default(),
        None,
        &inventory,
    )
    .await
    .unwrap();
    assert_eq!(outcome, patchula_update::RestoreOutcome::Partial);
    assert_eq!(warm.action().status, BranchStatus::Planned);
    // Two paged requests, then the closing re-read; a one-page walk stays
    // one request.
    assert_eq!(
        server.state().requests,
        [
            ("POST".into(), "/graphql".into()),
            ("POST".into(), "/graphql".into()),
            ("POST".into(), "/graphql".into())
        ]
    );
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn pr_create_repeat_update_labels_notes_and_checked_retry() {
    let (config, repo) = pr_config();
    let (p, snapshot) = make_plan("22.0.0");
    let mut draft = Draft::from(p);
    draft.changes_mut()[0].labels = vec!["dependencies".into()];
    draft.changes_mut()[0].body_notes = vec!["Review our deployment first.".into()];
    let mut p = draft.build();
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
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Created
    );
    {
        let state = server.state();
        let pr = &state.repo().prs[&1];
        assert_eq!(pr["title"], p.action().message);
        let body = pr["body"].as_str().unwrap();
        assert!(body.contains("| Package | Change | Type | File |"));
        assert!(body.contains("mise.toml") && body.contains("22.0.0"));
        assert!(body.contains("Review our deployment first."));
        assert!(body.contains("Tick to re-check this PR on the next run."));
        assert!(body.contains("<!-- patchula:") && body.contains("<!-- patchula-recheck -->"));
        assert_eq!(pr["labels"][0]["name"], "dependencies");
    }
    let count = server.state().writes.len();
    let (repeat, _) = make_plan("22.0.0");
    let mut repeat = Draft::from(repeat).changes(p.changes().to_vec()).build();
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Unchanged
    );
    assert_eq!(server.state().writes.len(), count);
    {
        let mut s = server.state();
        let body = s.repo().prs[&1]["body"].as_str().unwrap().replace(
            "- [ ] Tick to re-check this PR on the next run.",
            "- [x] Any operator wording",
        );
        s.repo_mut().prs.get_mut(&1).unwrap()["body"] = json!(body);
        s.repo_mut().prs.get_mut(&1).unwrap()["labels"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"human-label"}));
    }
    let (retry, _) = make_plan("22.0.0");
    let mut retry = Draft::from(retry).changes(p.changes().to_vec()).build();
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut retry),
        &snapshot,
    )
    .await;
    assert!(retry.retry());
    assert_eq!(retry.action().status, BranchStatus::Unchanged);
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Updated
    );
    let (next, _) = make_plan("22.1.0");
    let mut draft = Draft::from(next);
    draft.changes_mut()[0].labels = vec!["new-policy".into()];
    let mut next = draft.build();
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        &snapshot,
    )
    .await;
    let state = server.state();
    assert_eq!(state.repo().prs.len(), 1);
    assert!(
        state.repo().prs[&1]["body"]
            .as_str()
            .unwrap()
            .contains("22.1.0")
    );
    assert!(
        state.repo().prs[&1]["labels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == "human-label")
    );
    check_transcript();
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
        let mut s = server.state();
        s.repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
        let mut decoy = s.repo().prs[&1].clone();
        decoy["number"] = json!(2);
        decoy["user"]["login"] = json!("human");
        s.repo_mut().prs.insert(2, decoy);
        s.settings.pr_page_size = Some(1);
    }
    let count = server.state().writes.len();
    let (repeat, _) = make_plan("22.0.0");
    let mut row = repeat.changes()[0].clone();
    row.file = "other.toml".into();
    let mut repeat = Draft::from(repeat)
        .message("build: different wording")
        .change(row)
        .build();
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(repeat.action().status, BranchStatus::Skipped);
    assert!(
        repeat
            .action()
            .reason
            .clone()
            .unwrap()
            .contains("manually closed")
    );
    assert_eq!(server.state().writes.len(), count);
    let (mut next, _) = make_plan("22.1.0");
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        &snapshot,
    )
    .await;
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Created
    );
    assert_eq!(server.state().repo().prs.len(), 3);
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn security_updates_skip_the_concurrent_and_hourly_limits() {
    let (mut config, repo) = pr_config();
    config.prs.concurrent = Some(1);
    config.prs.per_hour = Some(1);
    let (p, snapshot) = make_plan("22.0.0");
    let (security, _) = make_plan("24.0.0");
    let mut draft = Draft::from(security);
    draft.changes_mut()[0].advisories = vec![
        serde_json::from_value(
            json!({"id":"GHSA-test","url":"https://osv.dev/vulnerability/GHSA-test"}),
        )
        .unwrap(),
    ];
    let security = draft.build();
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let mut plans = vec![p, security];
    let actions = pr_cycle(&github, &repo, &config, &mut plans, &snapshot).await;
    assert!(
        actions
            .iter()
            .all(|a| a.status == patchula_report::PullRequestStatus::Created),
        "{actions:?}"
    );
    check_transcript();
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
    assert_eq!(plans[1].action().status, BranchStatus::Skipped);
    assert!(
        plans[1]
            .action()
            .reason
            .as_ref()
            .unwrap()
            .contains("concurrent")
    );
    server.state().repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
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
    assert_eq!(other.action().status, BranchStatus::Skipped);
    assert!(other.action().reason.as_ref().unwrap().contains("perHour"));
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
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Created
    );
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn obsolete_pr_closes_then_branch_deletes_and_human_history_is_protected() {
    let mut servers = Vec::new();
    for case in ["bot", "human", "closed-by-owner"] {
        let human = case == "human";
        let (config, repo) = pr_config();
        let (mut p, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        servers.push(server.clone());
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
            let mut s = server.state();
            let commit = s
                .repo_mut()
                .commits
                .get_mut(p.action().commit.as_ref().unwrap())
                .unwrap();
            commit["message"] = json!(commit["message"].as_str().unwrap().replace(
                "Patchula-Base: base",
                "Patchula-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            ));
            if human {
                commit["actor"] = json!("human");
            }
            if case == "closed-by-owner" {
                server.close_pulls_on_read();
            }
        }
        let state = patchula_update::Lifecycle::prepare_with_controls(
            &github,
            &repo,
            &mut [],
            &config,
            chrono::Utc::now(),
            Default::default(),
            &Default::default(),
            "patchula[bot]",
        )
        .await
        .unwrap();
        assert_eq!(
            state.actions[0].status,
            if human {
                patchula_report::PullRequestStatus::Protected
            } else if case == "closed-by-owner" {
                patchula_report::PullRequestStatus::Skipped
            } else {
                patchula_report::PullRequestStatus::Closed
            }
        );
        let cache = github.cache();
        let inventory = read_inventory(&github, &cache).await;
        let cleanup = patchula_update::cleanup(
            &github,
            &cache,
            "demo/app",
            "base",
            "main",
            &[],
            &inventory,
            false,
        )
        .await
        .unwrap();
        assert_eq!(cleanup.len(), usize::from(!human));
        let s = server.state();
        assert_eq!(
            s.repo().prs[&1]["state"],
            if human { "open" } else { "closed" }
        );
        if !human && case != "closed-by-owner" {
            let close = s
                .writes
                .iter()
                .position(|(_, b)| b["state"] == "closed")
                .unwrap();
            let delete = s
                .writes
                .iter()
                .rposition(|(_, b)| b["query"] == patchula_update::REF_UPDATE)
                .unwrap();
            assert!(close < delete);
        }
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn generated_tables_policy_text_and_markers_do_not_gain_joiners() {
    let (config, repo) = pr_config();
    let (p, snapshot) = make_plan("22.0.0");
    let mut draft = Draft::from(p);
    draft.changes_mut()[0].package = "@scope/package".into();
    draft.changes_mut()[0].current = "20.0.0@sha256:abcdef".into();
    draft.changes_mut()[0].digest = Some("sha256:fedcba".into());
    draft.changes_mut()[0].body_notes =
        vec!["Policy @owner name@version <!-- patchula:fake -->".into()];
    let mut p = draft.build();
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
    let s = server.state();
    let body = s.repo().prs[&1]["body"].as_str().unwrap();
    assert!(body.contains("| @scope/package | 20.0.0@sha256:abcdef → 22.0.0@sha256:fedcba |"));
    assert!(body.contains("Policy @owner name@version &lt;!-- patchula:fake --&gt;"));
    assert!(!body.contains('\u{200d}'));
    assert_eq!(body.matches("<!-- patchula:").count(), 1);
    assert_eq!(body.matches("<!-- patchula-content:").count(), 1);
    check_transcript();
}

const FOREIGN: &str = "<!-- tool:summary:start -->\nA generated summary, with a [link](https://example.org/x) and @someone.\n<!-- tool:summary:end -->";

/// How many times the Count patched the body of PR 1.
fn body_patches(server: &FakeGitHub) -> usize {
    server
        .requests()
        .iter()
        .filter(|(method, path)| method == "PATCH" && path == "/repos/demo/app/pulls/1")
        .count()
}

#[tokio::test(start_paused = true)]
async fn text_others_wrote_in_the_description_is_left_alone_and_never_doubled() {
    let (config, repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let changes = p.changes().to_vec();
    let publish = |retry: bool| {
        let (next, _) = make_plan("22.0.0");
        let mut next = Draft::from(next)
            .changes(changes.clone())
            .retry(retry)
            .build();
        let (github, repo, config, snapshot) = (&github, &repo, &config, &snapshot);
        async move {
            pr_cycle(
                github,
                repo,
                config,
                std::slice::from_mut(&mut next),
                snapshot,
            )
            .await;
        }
    };
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    let own = server.state().repo().prs[&1]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(own.starts_with("<!-- patchula-begin -->\n"), "{own}");
    // Appended, prepended, and both.
    for (before, after) in [
        (String::new(), format!("\n\n{FOREIGN}")),
        (format!("{FOREIGN}\n\n"), String::new()),
        (
            "Hello from a person.\n\n".to_owned(),
            format!("\n\n{FOREIGN}"),
        ),
    ] {
        let edited = format!("{before}{own}{after}");
        server.state().repo_mut().prs.get_mut(&1).unwrap()["body"] = json!(edited);
        // No change in content: nothing is patched, three publishes over.
        server.state().requests.clear();
        for _ in 0..3 {
            publish(false).await;
            let mut s = server.state();
            assert_eq!(s.repo_mut().prs[&1]["body"].as_str().unwrap(), edited);
        }
        assert_eq!(body_patches(&server), 0);
        // A rewrite (a re-check re-renders the body) keeps the same text, once.
        for _ in 0..2 {
            publish(true).await;
            let mut s = server.state();
            let now = s.repo_mut().prs[&1]["body"].as_str().unwrap();
            assert_eq!(
                now.matches("tool:summary:start").count(),
                usize::from(edited.contains(FOREIGN)),
                "{now}"
            );
            assert_eq!(
                now.matches("Hello from a person.").count(),
                usize::from(edited.contains("Hello"))
            );
            assert!(now.starts_with(&before) && now.ends_with(&after), "{now}");
            assert_eq!(now.matches("<!-- patchula-begin -->").count(), 1);
        }
        // Whatever is edited back to what the Count wrote is stable again.
        server.state().repo_mut().prs.get_mut(&1).unwrap()["body"] = json!(own);
        publish(true).await;
    }
    // A tool rewrites its own block in place between runs: the latest text stays.
    let rewritten =
        format!("{own}\n\n<!-- tool:summary:start -->\nrewritten\n<!-- tool:summary:end -->");
    server.state().repo_mut().prs.get_mut(&1).unwrap()["body"] = json!(rewritten);
    publish(true).await;
    let s = server.state();
    assert!(
        s.repo().prs[&1]["body"]
            .as_str()
            .unwrap()
            .ends_with("rewritten\n<!-- tool:summary:end -->")
    );
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn rechecks_refresh_notes_but_only_patch_changed_body_hashes() {
    let mut servers = Vec::new();
    for origin in ["repository", "pr"] {
        let (config, repo) = pr_config();
        let (p, snapshot) = make_plan("22.0.0");
        let mut draft = Draft::from(p);
        draft.changes_mut()[0].source = Source::GithubTags;
        draft.changes_mut()[0].package = "actions/checkout".into();
        draft.changes_mut()[0].release_notes = true;
        let mut p = draft.build();
        let server = fake(&snapshot);
        servers.push(server.clone());
        server
            .state()
            .repositories
            .get_mut("actions/checkout")
            .unwrap()
            .release_note_body = Some("Original upstream notes".into());
        let github = client(server.clone()).await;
        pr_cycle(
            &github,
            &repo,
            &config,
            std::slice::from_mut(&mut p),
            &snapshot,
        )
        .await;
        // Exercise both unchanged and changed re-renders in distinct batches.
        for changed in [false, true] {
            let (next, _) = make_plan("22.0.0");
            let mut next = Draft::from(next)
                .changes(p.changes().to_vec())
                .retry(origin == "repository")
                .build();
            {
                let mut s = server.state();
                if changed {
                    s.repositories
                        .get_mut("actions/checkout")
                        .unwrap()
                        .release_note_body = Some("Changed upstream notes".into());
                }
                let mut body = s.repo().prs[&1]["body"]
                    .as_str()
                    .unwrap()
                    .replace("\r\n", "\n");
                if origin == "pr" {
                    body = body.replace("- [ ] Tick to re-check", "- [x] Tick to re-check");
                }
                s.repo_mut().prs.get_mut(&1).unwrap()["body"] = json!(body.replace('\n', "\r\n"));
                s.requests.clear();
                s.writes.clear();
                s.release_queries.clear();
            }
            let actions = pr_cycle(
                &github,
                &repo,
                &config,
                std::slice::from_mut(&mut next),
                &snapshot,
            )
            .await;
            let s = server.state();
            assert_eq!(
                s.release_queries.len(),
                if changed { 2 } else { 1 },
                "{origin}: recheck validates timestamps and fetches only changed bodies"
            );
            let patches = s
                .requests
                .iter()
                .filter(|(method, path)| method == "PATCH" && path == "/repos/demo/app/pulls/1")
                .count();
            assert_eq!(
                patches,
                usize::from(changed || origin == "pr"),
                "{origin}: skip identical body, but acknowledge a checked PR"
            );
            assert_eq!(
                actions[0].status,
                if changed || origin == "pr" {
                    patchula_report::PullRequestStatus::Updated
                } else {
                    patchula_report::PullRequestStatus::Unchanged
                }
            );
            let body = s.repo().prs[&1]["body"].as_str().unwrap();
            assert!(body.contains(if changed {
                "Changed upstream notes"
            } else {
                "Original upstream notes"
            }));
            assert!(!body.contains("- [x]"));
        }
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn release_notes_are_bounded_deduplicated_cached_and_policy_controlled() {
    let (config, repo) = pr_config();
    let (p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let mut draft = Draft::from(p);
    draft.changes_mut()[0].source = Source::GithubTags;
    draft.changes_mut()[0].package = "actions/checkout".into();
    draft.changes_mut()[0].release_notes = true;
    let mut row = draft.changes_mut()[0].clone();
    row.file = "other.yml".into();
    draft.changes_mut().push(row);
    let mut p = draft.build();
    let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut p),
        &config,
        chrono::Utc::now(),
        patchula_words::Recheck::Now,
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    publish(&mut lifecycle, &github, std::slice::from_mut(&mut p)).await;
    {
        let s = server.state();
        let body = s.repo().prs[&1]["body"].as_str().unwrap();
        assert!(body.len() <= patchula_update::BODY_LIMIT);
        assert!(
            body.contains("Release details 🦀") && body.contains("Tick to re-check this PR now.")
        );
        assert_eq!(s.release_queries.len(), 1);
    }
    let (repeat, _) = make_plan("22.0.0");
    let mut repeat = Draft::from(repeat).changes(p.changes().to_vec()).build();
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(server.state().release_queries.len(), 2);
    assert!(
        !server.state().release_queries[1]["query"]
            .as_str()
            .unwrap()
            .contains("body:description")
    );
    let reads = server
        .state()
        .requests
        .iter()
        .filter(|(_, p)| p.starts_with("/data/github/"))
        .count();
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(
        reads,
        server
            .state()
            .requests
            .iter()
            .filter(|(_, p)| p.contains("/releases/tags/") || p.starts_with("/data/github/"))
            .count(),
        "unchanged body inputs reuse notes even without receipt restoration"
    );
    repeat.unchanged(
        repeat.action().commit.clone().unwrap(),
        repeat.tree().unwrap().to_owned(),
    );
    let mut draft = Draft::from(repeat);
    draft.changes_mut().iter_mut().for_each(|c| {
        c.release_notes = false;
        c.body_notes = vec!["huge 🦀 <!-- patchula:fake -->".repeat(5000)];
    });
    let mut repeat = draft.build();
    let count = server.state().release_queries.len();
    publish(&mut lifecycle, &github, std::slice::from_mut(&mut repeat)).await;
    let s = server.state();
    let body = s.repo().prs[&1]["body"].as_str().unwrap();
    assert!(body.len() <= patchula_update::BODY_LIMIT);
    assert_eq!(body.matches("<!-- patchula:").count(), 1);
    assert_eq!(s.release_queries.len(), count);
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn merge_policy_prefers_native_auto_merge_and_lockfile_pending_disables_it() {
    let (config, repo) = pr_config();
    let (p, snapshot) = make_plan("22.0.0");
    let mut draft = Draft::from(p);
    draft.changes_mut()[0].merge = true;
    let mut p = draft.build();
    let server = fake(&snapshot);
    {
        let mut s = server.state();
        let repo = s.repo_mut();
        repo.merge.auto_merge = true;
        repo.merge.squash = true;
    }
    let github = client(server.clone()).await;
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Created
    );
    {
        let s = server.state();
        assert!(s.writes.iter().any(|(_, b)| {
            b["query"]
                .as_str()
                .is_some_and(|q| q.contains("enablePullRequestAutoMerge"))
                && b["variables"]["input"]["mergeMethod"] == "SQUASH"
                && b["variables"]["input"]["expectedHeadOid"]
                    == p.action().commit.as_deref().unwrap()
        }));
        assert!(!s.requests.iter().any(|(_, p)| p.ends_with("/merge")));
    }
    let (next, _) = make_plan("22.1.0");
    let mut draft = Draft::from(next);
    draft.changes_mut()[0].merge = true;
    let mut next = draft.lockfile_pending(true).build();
    pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        &snapshot,
    )
    .await;
    let s = server.state();
    assert!(s.repo().prs[&1]["auto_merge"].is_null());
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn uncertain_pr_creation_is_reconciled_next_run_and_never_blindly_retried() {
    let (config, repo) = pr_config();
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    server.pull_create_fails();
    let github = client(server.clone()).await;
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut p),
        &snapshot,
    )
    .await;
    assert_eq!(actions[0].status, patchula_report::PullRequestStatus::Error);
    assert_eq!(server.state().repo().prs.len(), 1);
    let (mut repeat, _) = make_plan("22.0.0");
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut repeat),
        &snapshot,
    )
    .await;
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Unchanged
    );
    assert_eq!(server.state().repo().prs.len(), 1);
    check_transcript();
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
    let state = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        &mut [],
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert!(state.actions.is_empty());
    assert_eq!(server.state().repo().prs[&1]["state"], "open");
    check_transcript();
}

#[test]
fn supplied_sandbox_targets_converge_to_sixteen_plans_with_supported_pins() {
    // Self-authored manifests and target facts from the owner's patchula report.
    let (config, mut repo, ..) = setup("22.0.0", false);
    let mut config = config;
    config.managers = vec![
        Manager::GithubActions,
        Manager::Dockerfile,
        Manager::Compose,
        Manager::Mise,
        Manager::Npm,
    ];
    let files = BTreeMap::<String, String>::from([
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
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let mut snapshot = Snapshot {
        cargo_cooldowns: Default::default(),
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
        public_only: Default::default(),
    };
    snapshot.paths.insert("bun.lock".into());
    repo.package_files.clear();
    for dep in &deps {
        let targets: &[(&str, &str)] = match (dep.package.as_str(), dep.source.as_str()) {
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
            ("bun", _) => &[("patch", "1.1.45"), ("minor", "1.4.3")],
            ("jq", _) => &[("minor", "1.8.2")],
            ("prettier", _) => &[("pin", "3.9.10")],
            other => panic!("unexpected synthetic sandbox dependency {other:?}"),
        };
        let updates = targets
            .iter()
            .map(|(kind, target)| Update {
                cooldown: None,
                new_value: Some((*target).into()),
                new_version: Some((*target).into()),
                new_digest: if dep.source == patchula_words::Source::Docker {
                    Some(format!("sha256:{}", "b".repeat(64)))
                } else if dep.manager == Manager::GithubActions
                    && dep.source == patchula_words::Source::GithubTags
                {
                    Some("b".repeat(40))
                } else {
                    None
                },
                update_type: (*kind).parse().unwrap(),
                group: None,
                policy: None,
                release_age: None,
                fixes: Vec::new(),
                image: None,
            })
            .collect();
        let r = ReportDependency {
            manager: dep.manager,
            name: dep.name.clone(),
            package: dep.package.clone(),
            registry: dep.registry.as_ref().map(|r| r.recorded().to_owned()),
            occurrence: None,
            current: Current {
                value: Some(dep.current.clone()),
                version: dep.resolved.clone(),
                digest: dep.digest.clone(),
            },
            metadata: Some(metadata(dep)),
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
    let plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    let names: BTreeSet<_> = plans
        .iter()
        .filter(|p| p.action().status == BranchStatus::Planned)
        .map(|p| p.action().branch.as_str())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            "patchula/actions-checkout-4.x",
            "patchula/actions-checkout-7.x",
            "patchula/docker-node-20.x",
            "patchula/docker-node-26.x",
            "patchula/jdx-mise-action-5.x",
            "patchula/jq-1.x",
            "patchula/mise-node-22.x",
            "patchula/mise-node-26.x",
            "patchula/nginx-1.x",
            "patchula/bun-1.x",
            "patchula/pin-dependencies",
            "patchula/postgres-16.x",
            "patchula/postgres-18.x",
            "patchula/redis-7.x",
            "patchula/redis-8.x",
            "patchula/ubuntu-26.x",
        ])
    );
    let pins = plans
        .iter()
        .find(|p| p.action().branch == "patchula/pin-dependencies")
        .unwrap();
    assert_eq!(pins.changes().len(), 6);
    assert_eq!(pins.action().message, "chore(deps): pin dependencies");
    assert!(pins.contents().contains_key("package.json"));
    assert!(!pins.lockfiles_ready());
    assert!(!pins.action().lockfile_pending);
    assert_eq!(
        plans
            .iter()
            .filter(|p| p.action().status == BranchStatus::Skipped)
            .count(),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn pin_title_respects_prefix_and_pin_rows_never_request_release_notes() {
    let (mut config, mut repo, deps, snapshot) = setup("22.0.0", false);
    config.defaults.commit_prefix = "build(deps):".into();
    repo.package_files[0].dependencies[0].updates[0].update_type = UpdateKind::Pin;
    let mut plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(plans[0].action().message, "build(deps): pin dependencies");
    let mut draft = Draft::from(plans.remove(0));
    draft.changes_mut()[0].source = Source::GithubTags;
    draft.changes_mut()[0].package = "actions/checkout".into();
    draft.changes_mut()[0].release_notes = true;
    let mut other = draft.changes_mut()[0].clone();
    other.source = Source::Docker;
    draft.changes_mut().push(other);
    let mut plans = vec![draft.build()];
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    pr_cycle(&github, &repo, &config, &mut plans, &snapshot).await;
    let s = server.state();
    assert_eq!(s.repo().prs[&1]["title"], "build(deps): pin dependencies");
    let text = s.repo().prs[&1]["body"].as_str().unwrap();
    assert!(text.contains("| Package | Change | Type | File |"));
    assert!(text.contains("| pin |"));
    assert!(!text.to_lowercase().contains("release notes"));
    assert!(!s.requests.iter().any(|(_, p)| p.contains("/releases/")));
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn batch_ref_race_keeps_every_prepared_branch_unpublished() {
    let (p, snapshot) = make_plan("22.0.0");
    let (other, _) = make_plan("24.0.0");
    let mut plans = vec![p, other];
    let server = fake(&snapshot);
    server.update_refs_fail();
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert!(
        plans
            .iter()
            .all(|p| p.action().status == BranchStatus::Error && p.action().commit.is_none())
    );
    let state = server.state();
    assert_eq!(state.repo().refs.len(), 1);
    let mutations: Vec<_> = state
        .writes
        .iter()
        .filter(|(_, b)| b["query"] == patchula_update::REF_UPDATE)
        .collect();
    assert_eq!(mutations.len(), 1);
    assert_eq!(
        mutations[0].1["variables"]["input"]["refUpdates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    for p in &plans {
        let reason = p.action().reason.as_deref().unwrap();
        assert!(
            reason.contains("atomic batch ref lease failed")
                && reason.ends_with("GitHub GraphQL returned errors: reference changed"),
            "{reason}"
        );
    }
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn single_branch_ref_failure_names_githubs_reason() {
    let (mut p, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    server.update_refs_fail();
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Error);
    assert!(
        p.action()
            .reason
            .as_deref()
            .unwrap()
            .ends_with("GitHub GraphQL returned errors: reference changed"),
        "{:?}",
        p.action().reason
    );
    check_transcript();
}

/// Every `updateRefs` input recorded by the fake, as lists of ref names.
fn ref_update_names(server: &FakeGitHub) -> Vec<Vec<String>> {
    server
        .writes()
        .iter()
        .filter(|(_, b)| b["query"] == patchula_update::REF_UPDATE)
        .map(|(_, b)| {
            b["variables"]["input"]["refUpdates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["name"].as_str().unwrap().to_owned())
                .collect()
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn no_ref_update_names_the_default_branch() {
    // Rulesets that require pull requests reject even a no-op update of it.
    let (mut single, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut single),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(single.action().status, BranchStatus::Created);
    let names = ref_update_names(&server);
    assert_eq!(
        names,
        vec![vec![format!("refs/heads/{}", single.action().branch)]]
    );

    let (p, snapshot) = make_plan("22.0.0");
    let (other, _) = make_plan("24.0.0");
    let mut plans = vec![p, other];
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert!(
        plans
            .iter()
            .all(|p| p.action().status == BranchStatus::Created)
    );
    let names = ref_update_names(&server);
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].len(), 2);
    assert!(names[0].iter().all(|n| n != "refs/heads/main"));

    // Deleting an obsolete owned branch.
    let before = ref_update_names(&server).len();
    {
        let mut s = server.state();
        let sha = s.repo().refs[&format!("refs/heads/{}", plans[0].action().branch)].clone();
        let commit = s.repo_mut().commits.get_mut(&sha).unwrap();
        commit["message"] = json!(commit["message"].as_str().unwrap().replace(
            "Patchula-Base: base",
            "Patchula-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }
    let cache = github.cache();
    let inventory = read_inventory(&github, &cache).await;
    let actions = patchula_update::cleanup(
        &github,
        &cache,
        "demo/app",
        "base",
        "main",
        &[],
        &inventory,
        false,
    )
    .await
    .unwrap();
    assert!(actions.iter().any(|a| a.status == BranchStatus::Deleted));
    let names = ref_update_names(&server);
    assert!(names.len() > before);
    assert!(names.iter().flatten().all(|n| n != "refs/heads/main"));
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn cleanup_deletes_nothing_once_the_default_branch_moves() {
    let (p, snapshot) = make_plan("22.0.0");
    let mut plans = vec![p];
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(plans[0].action().status, BranchStatus::Created);
    {
        let mut s = server.state();
        let sha = s.repo().refs[&format!("refs/heads/{}", plans[0].action().branch)].clone();
        let commit = s.repo_mut().commits.get_mut(&sha).unwrap();
        commit["message"] = json!(commit["message"].as_str().unwrap().replace(
            "Patchula-Base: base",
            "Patchula-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }
    let cache = github.cache();
    let inventory = read_inventory(&github, &cache).await;
    server.state().repo_mut().refs.insert(
        "refs/heads/main".into(),
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
    );
    let before = ref_update_names(&server).len();
    let actions = patchula_update::cleanup(
        &github,
        &cache,
        "demo/app",
        "base",
        "main",
        &[],
        &inventory,
        false,
    )
    .await
    .unwrap();
    assert!(actions.is_empty());
    assert_eq!(ref_update_names(&server).len(), before);
    // A recorded base GitHub cannot compare marks a branch that is not the
    // Count's; cleanup leaves it and carries on rather than failing the run.
    let retag = |from: &str, to: &str| {
        let mut s = server.state();
        let sha = s.repo().refs[&format!("refs/heads/{}", plans[0].action().branch)].clone();
        let commit = s.repo_mut().commits.get_mut(&sha).unwrap();
        commit["message"] = json!(commit["message"].as_str().unwrap().replace(from, to));
    };
    retag(
        "Patchula-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "Patchula-Base: unknown",
    );
    server
        .state()
        .repo_mut()
        .refs
        .insert("refs/heads/main".into(), "base".into());
    let inventory = read_inventory(&github, &cache).await;
    let actions = patchula_update::cleanup(
        &github,
        &cache,
        "demo/app",
        "base",
        "main",
        &[],
        &inventory,
        false,
    )
    .await
    .unwrap();
    assert!(actions.is_empty());
    assert_eq!(ref_update_names(&server).len(), before);
    retag(
        "Patchula-Base: unknown",
        "Patchula-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    // The same branch is deleted once the plans match the default branch again.
    server
        .state()
        .repo_mut()
        .refs
        .insert("refs/heads/main".into(), "base".into());
    let inventory = read_inventory(&github, &cache).await;
    let actions = patchula_update::cleanup(
        &github,
        &cache,
        "demo/app",
        "base",
        "main",
        &[],
        &inventory,
        false,
    )
    .await
    .unwrap();
    assert!(actions.iter().any(|a| a.status == BranchStatus::Deleted));
    check_transcript();
}

/// `cleanup`'s candidate loop as it ran before the inventory answered the
/// tip reads: every head was fetched, then compared, one at a time in list
/// order, stopping at the first failed probe.
async fn cleanup_as_before(
    github: &Client,
    repo: &str,
    base: &str,
    default_branch: &str,
    plans: &[Plan],
    actor: &str,
) -> Result<Vec<patchula_report::BranchAction>, patchula_update::Error> {
    let refs = github
        .rest_pages::<patchula_github::model::BranchHead>(
            &format!("repos/{repo}/branches?per_page=100"),
            |_| false,
        )
        .await?;
    let desired: BTreeSet<_> = plans.iter().map(|p| p.action().branch.as_str()).collect();
    let candidates: Vec<_> = refs
        .iter()
        .filter(|r| r.name != default_branch && !desired.contains(r.name.as_str()))
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let metadata = github
        .get_fresh::<patchula_github::model::Repository>(&format!("repos/{repo}"))
        .await?;
    if metadata.default_branch != default_branch {
        return Err(patchula_update::Error::Raced(
            "default branch changed before cleanup; rerun",
        ));
    }
    let current = github
        .get_fresh::<patchula_github::model::GitRef>(&format!(
            "repos/{repo}/git/ref/heads/{default_branch}"
        ))
        .await?;
    if current.object.sha != base {
        return Ok(Vec::new());
    }
    let mut actions = Vec::new();
    for r in candidates {
        let name = &r.name;
        let head = &r.commit.sha;
        if !patchula_update::owned(github, repo, head, actor).await? {
            continue;
        }
        patchula_update::delete_owned(github, &metadata.node_id, default_branch, name, head)
            .await?;
        actions.push(patchula_report::BranchAction {
            repository: repo.into(),
            branch: name.clone(),
            base_commit: base.into(),
            status: BranchStatus::Deleted,
            reason: Some("obsolete patchula update".into()),
            message: String::new(),
            previous_commit: Some(head.clone()),
            commit: None,
            verified: Some(true),
            lockfile_pending: false,
            files: Vec::new(),
        });
    }
    Ok(actions)
}

/// Seed the branch list the cleanup differential drives: two owned
/// branches, a human branch, a human commit wearing the Count's copied
/// trailers, an unverifiable commit on a Count-named branch, and
/// `patchula/moved` at `moved` — `"m-old"` for an inventory to hold,
/// `"m-moved"` for the head it moved to.
fn seed_cleanup_branches(server: &FakeGitHub, moved: &str) {
    let trailers = "Patchula: v1\nPatchula-Branch: id\nPatchula-Base: base\nPatchula-Plan: fp";
    let owned = |sha: &str| {
        json!({"sha":sha,"tree":{"sha":"seed-tree"},
               "message":format!("update\n\n{trailers}")})
    };
    let mut s = server.state();
    let repo = s.repo_mut();
    repo.trees.insert("seed-tree".into(), json!([]));
    for (name, sha, commit) in [
        ("patchula/owned-a", "owned-a", owned("owned-a")),
        ("patchula/owned-b", "owned-b", owned("owned-b")),
        (
            "human/work",
            "h1",
            json!({"sha":"h1","tree":{"sha":"seed-tree"},"message":"human work",
                   "actor":"human","committer_login":"human"}),
        ),
        (
            "human/copied-marker",
            "h2",
            json!({"sha":"h2","tree":{"sha":"seed-tree"},
                   "message":format!("update\n\n{trailers}"),
                   "actor":"human","committer_login":"human"}),
        ),
        (
            "patchula/foreign",
            "f1",
            json!({"sha":"f1","tree":{"sha":"seed-tree"},
                   "message":format!("update\n\n{trailers}"),
                   "verification":{"verified":false}}),
        ),
        ("patchula/moved", moved, owned("m-old")),
        (
            "-",
            "m-moved",
            json!({"sha":"m-moved","tree":{"sha":"seed-tree"},"message":"human work",
                   "actor":"human","committer_login":"human"}),
        ),
    ] {
        if name != "-" {
            repo.refs.insert(format!("refs/heads/{name}"), sha.into());
        }
        repo.commits.insert(sha.into(), commit);
    }
}
/// `GET /repos/{repo}/commits/{sha}` requests in the fake's log — the tip
/// reads the probes used to pay per candidate.
fn tip_reads(server: &FakeGitHub) -> usize {
    server
        .requests()
        .iter()
        .filter(|(method, path)| {
            method == "GET" && path.contains("/commits/") && !path.contains("/git/")
        })
        .count()
}
#[tokio::test(start_paused = true)]
async fn cleanup_deletes_the_same_branches_probing_only_heads_the_inventory_missed() {
    let (_, _, _, snapshot) = setup("22.0.0", false);
    // Before: a tip read for every one of the six candidates, in order.
    let before_server = fake(&snapshot);
    seed_cleanup_branches(&before_server, "m-moved");
    let before_github = client(before_server.clone()).await;
    let before = cleanup_as_before(
        &before_github,
        "demo/app",
        "base",
        "main",
        &[],
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert_eq!(tip_reads(&before_server), 6);
    let server = fake(&snapshot);
    seed_cleanup_branches(&server, "m-old");
    let github = client(server.clone()).await;
    let cache = github.cache();
    let inventory = read_inventory(&github, &cache).await;
    // The head moved between the inventory and the branch listing: the
    // inventory holds the old tip, so the new one is probed as before.
    server
        .state()
        .repo_mut()
        .refs
        .insert("refs/heads/patchula/moved".into(), "m-moved".into());
    let actions = patchula_update::cleanup(
        &github,
        &cache,
        "demo/app",
        "base",
        "main",
        &[],
        &inventory,
        false,
    )
    .await
    .unwrap();
    // The deleted set is the sequential probes': the two owned branches.
    assert_eq!(actions, before);
    // Only the moved head still needed its tip read.
    assert_eq!(tip_reads(&server), 1);
    // Its negative answer is kept: a second run skips it without probing.
    let actions = patchula_update::cleanup(
        &github,
        &cache,
        "demo/app",
        "base",
        "main",
        &[],
        &inventory,
        false,
    )
    .await
    .unwrap();
    assert!(actions.is_empty());
    assert_eq!(tip_reads(&server), 1);
    check_transcript();
}
#[tokio::test(start_paused = true)]
async fn a_probe_error_stops_cleanup_after_the_deletions_before_it() {
    let (_, _, _, snapshot) = setup("22.0.0", false);
    let seed = |moved: &str| {
        let server = fake(&snapshot);
        {
            let mut s = server.state();
            let repo = s.repo_mut();
            repo.trees.insert("seed-tree".into(), json!([]));
            for (name, sha) in [
                ("patchula/a-owned", "a-owned"),
                ("patchula/m-moved", moved),
                ("patchula/z-owned", "z-owned"),
            ] {
                repo.refs.insert(format!("refs/heads/{name}"), sha.into());
            }
            let owned = |sha: &str| {
                json!({"sha":sha,"tree":{"sha":"seed-tree"},
                       "message":"update\n\nPatchula: v1\nPatchula-Branch: id\nPatchula-Base: base\nPatchula-Plan: fp"})
            };
            repo.commits.insert("a-owned".into(), owned("a-owned"));
            repo.commits.insert("z-owned".into(), owned("z-owned"));
            repo.commits.insert("m-old".into(), owned("m-old"));
            repo.commits.insert(
                "m-new".into(),
                json!({"sha":"m-new","tree":{"sha":"seed-tree"},"message":"human work",
                       "actor":"human","committer_login":"human"}),
            );
        }
        server
            .on(patchula_testkit::github::Matcher::Route {
                method: reqwest::Method::GET,
                path: "/repos/demo/app/commits/m-new".into(),
            })
            .respond(500, json!({"message":"probe boom"}));
        server
    };
    // Before: the first candidate deletes, the moved head's probe fails,
    // and the candidate after it is never probed or deleted.
    let before_server = seed("m-new");
    let before_github = client(before_server.clone()).await;
    let before = cleanup_as_before(
        &before_github,
        "demo/app",
        "base",
        "main",
        &[],
        "patchula[bot]",
    )
    .await
    .unwrap_err()
    .to_string();
    let refs = || before_server.state().repo().refs.clone();
    assert!(refs().contains_key("refs/heads/patchula/z-owned"));
    assert!(!refs().contains_key("refs/heads/patchula/a-owned"));
    // Now: the same error after the same deletion, with the inventory
    // holding the head the branch moved from.
    let server = seed("m-old");
    let github = client(server.clone()).await;
    let cache = github.cache();
    let inventory = read_inventory(&github, &cache).await;
    server
        .state()
        .repo_mut()
        .refs
        .insert("refs/heads/patchula/m-moved".into(), "m-new".into());
    let failure = patchula_update::cleanup(
        &github,
        &cache,
        "demo/app",
        "base",
        "main",
        &[],
        &inventory,
        false,
    )
    .await
    .unwrap_err()
    .to_string();
    assert_eq!(failure, before);
    let refs = || server.state().repo().refs.clone();
    assert!(refs().contains_key("refs/heads/patchula/z-owned"));
    assert!(!refs().contains_key("refs/heads/patchula/a-owned"));
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn generated_lockfile_additions_and_deletions_share_the_manifest_commit() {
    let (p, mut snapshot) = make_plan("22.0.0");
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
    let mut draft = Draft::from(p);
    for (path, sha, content) in [
        ("go.sum", blob_sha(old), ""),
        ("Cargo.lock", String::new(), "version = 4\n"),
    ] {
        draft = draft.edit(
            path,
            content,
            patchula_report::FileAction {
                path: path.into(),
                blob_sha: sha,
                diff: String::new(),
            },
        );
    }
    let mut p = draft.delete("go.sum").build();
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Created, "{:?}", p.action());
    let s = server.state();
    let commit = &s.repo().commits[p.action().commit.as_ref().unwrap()];
    let tree = s.repo().trees[commit["tree"]["sha"].as_str().unwrap()]
        .as_array()
        .unwrap();
    assert!(!tree.iter().any(|f| f["path"] == "go.sum"));
    assert!(
        tree.iter()
            .any(|f| f["path"] == "Cargo.lock" && f["sha"] == blob_sha("version = 4\n"))
    );
    assert!(
        tree.iter()
            .any(|f| f["path"] == "mise.toml" && f["sha"] == blob_sha(&p.contents()["mise.toml"]))
    );
    assert_eq!(
        s.writes
            .iter()
            .filter(|(path, _)| path.ends_with("/git/commits"))
            .count(),
        1
    );
    check_transcript();
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
    let (next, _) = make_plan("22.1.0");
    let mut next = Draft::from(next).identity("c".repeat(64)).build();
    let actions = pr_cycle(
        &github,
        &repo,
        &config,
        std::slice::from_mut(&mut next),
        &snapshot,
    )
    .await;
    assert_eq!(next.action().status, BranchStatus::Updated);
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Updated
    );
    assert!(
        server.state().repo().prs[&1]["body"]
            .as_str()
            .unwrap()
            .contains(next.identity())
    );
    check_transcript();
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
        let mut s = server.state();
        let sha = p.action().commit.as_ref().unwrap();
        s.repo_mut().merged_heads.insert(sha.clone());
        s.repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
        s.repo_mut().prs.get_mut(&1).unwrap()["merged_at"] = json!(chrono::Utc::now().to_rfc3339());
        s.repo_mut().move_base("base2");
        s.repo_mut()
            .refs
            .insert("refs/heads/main".into(), "base2".into());
        let tree = s.repo().commits[sha]["tree"].clone();
        s.repo_mut()
            .commits
            .insert("base2".into(), json!({"tree":tree}));
    }
    let blob = snapshot.files.get_mut("mise.toml").unwrap();
    blob.content = blob.content.replace("20.0.0", "22.0.0");
    blob.sha = blob_sha(&blob.content);
    let deps = Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&BTreeMap::from([(
            "mise.toml".into(),
            blob.content.clone(),
        )])))
        .unwrap()
        .dependencies;
    repo.commit = Some("base2".into());
    let r = &mut repo.package_files[0].dependencies[0];
    r.current.value = Some("22.0.0".into());
    r.current.version = Some("22.0.0".into());
    r.updates[0].update_type = UpdateKind::Minor;
    r.updates[0].new_value = Some("22.1.0".into());
    r.updates[0].new_version = Some("22.1.0".into());
    let mut plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(plans[0].action().branch, p.action().branch);
    let actions = pr_cycle(&github, &repo, &config, &mut plans, &snapshot).await;
    assert_eq!(
        plans[0].action().status,
        BranchStatus::Rebased,
        "{:?}",
        plans[0].action()
    );
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Created
    );
    assert_eq!(server.state().repo().prs.len(), 2);
    check_transcript();
}

#[tokio::test(start_paused = true)]
async fn titles_name_versions_and_branches_skip_registry_urls() {
    let (config, mut repo, mut deps, snapshot) = setup("22.0.0", false);
    let package = "https://charts.example.test/repo|node".to_string();
    deps[0].package.clone_from(&package);
    let dep = &mut repo.package_files[0].dependencies[0];
    dep.package = package;
    dep.updates[0].new_value = Some("==22.0.0".into());
    let plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(
        plans[0].action().message,
        "chore(deps): update node to 22.0.0"
    );
    assert_eq!(plans[0].action().branch, "patchula/node-22.x");
}

/// Pins share one branch, but a reference data pin gets its own, so it can
/// merge without waiting on unrelated pins.
#[test]
fn reference_data_pins_get_their_own_branch() {
    let (mut config, mut repo, ..) = setup("22.0.0", false);
    config.managers = vec![Manager::Npm];
    let files = BTreeMap::<String, String>::from([(
        "package.json".to_string(),
        "{\"dependencies\":{\"prettier\":\"^3.3.0\",\"psl\":\"^1.9.0\"}}\n".to_string(),
    )]);
    let deps = Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::all_read(&files))
        .unwrap()
        .dependencies;
    let mut snapshot = Snapshot {
        cargo_cooldowns: Default::default(),
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
        public_only: Default::default(),
    };
    snapshot.paths.insert("bun.lock".into());
    repo.package_files.clear();
    for dep in &deps {
        // psl also has a minor upgrade, which keeps its own branch.
        let targets: &[(&str, &str)] = if dep.package == "psl" {
            &[("pin", "1.9.0"), ("minor", "1.15.0")]
        } else {
            &[("pin", "3.9.10")]
        };
        let report = ReportDependency {
            manager: dep.manager,
            name: dep.name.clone(),
            package: dep.package.clone(),
            registry: dep.registry.as_ref().map(|r| r.recorded().to_owned()),
            occurrence: None,
            current: Current {
                value: Some(dep.current.clone()),
                version: dep.resolved.clone(),
                digest: dep.digest.clone(),
            },
            metadata: Some(metadata(dep)),
            updates: targets
                .iter()
                .map(|(kind, target)| Update {
                    cooldown: None,
                    new_value: Some((*target).into()),
                    new_version: Some((*target).into()),
                    new_digest: None,
                    update_type: (*kind).parse().unwrap(),
                    group: None,
                    policy: None,
                    release_age: None,
                    fixes: Vec::new(),
                    image: None,
                })
                .collect(),
        };
        match repo.package_files.iter_mut().find(|f| f.path == dep.file) {
            Some(file) => file.dependencies.push(report),
            None => repo.package_files.push(PackageFile {
                path: dep.file.clone(),
                dependencies: vec![report],
            }),
        }
    }
    let plans = plan(
        &repo,
        &config,
        &deps,
        &snapshot,
        false,
        None,
        chrono::Utc::now(),
    )
    .unwrap();
    let branches: BTreeSet<_> = plans.iter().map(|p| p.action().branch.as_str()).collect();
    assert!(
        branches.contains("patchula/pin-dependencies"),
        "{branches:?}"
    );
    let pins = plans
        .iter()
        .find(|p| p.action().branch == "patchula/pin-dependencies")
        .unwrap();
    assert_eq!(
        pins.changes().len(),
        1,
        "only prettier shares the pins branch"
    );
    assert!(branches.contains("patchula/pin-psl"), "{branches:?}");
    assert!(
        branches.iter().any(|b| b.starts_with("patchula/psl-")),
        "{branches:?}"
    );
    assert!(
        plans
            .iter()
            .filter(|p| p.action().branch.contains("psl"))
            .all(|p| p.action().status == BranchStatus::Planned),
        "both psl branches are planned, not skipped as overlapping"
    );
}

/// Closing several obsolete PRs fetches the repository's metadata and the
/// default branch's head once each, not once per pull.
#[tokio::test(start_paused = true)]
async fn obsolete_prs_fetch_repository_metadata_once() {
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
        let mut s = server.state();
        let head = p.action().commit.clone().unwrap();
        // A second patchula PR, whose branch is not desired either.
        let mut pr2 = s.repo().prs[&1].clone();
        pr2["number"] = json!(2);
        pr2["html_url"] = json!("https://github.com/demo/app/pull/2");
        pr2["head"]["ref"] = json!("patchula/obsolete-two");
        pr2["head"]["sha"] = json!("obsolete-two");
        s.repo_mut().prs.insert(2, pr2);
        s.repo_mut().refs.insert(
            "refs/heads/patchula/obsolete-two".into(),
            "obsolete-two".into(),
        );
        let commit = s.repo().commits[&head].clone();
        s.repo_mut().commits.insert("obsolete-two".into(), commit);
        // Move the base marker in every commit so both PRs are obsolete.
        for commit in s.repo_mut().commits.values_mut() {
            if let Some(message) = commit["message"].as_str() {
                commit["message"] = json!(message.replace(
                    "Patchula-Base: base",
                    "Patchula-Base: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                ));
            }
        }
    }
    server.state().requests.clear();
    let state = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        &mut [],
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert_eq!(state.actions.len(), 2, "{:?}", state.actions);
    let s = server.state();
    let metas = s
        .requests
        .iter()
        .filter(|(method, path)| method == "GET" && path == "/repos/demo/app")
        .count();
    assert_eq!(metas, 1, "{:?}", s.requests);
    let heads = s
        .requests
        .iter()
        .filter(|(method, path)| method == "GET" && path == "/repos/demo/app/branches/main")
        .count();
    assert_eq!(
        heads, 2,
        "the head is re-read before each closure: {:?}",
        s.requests
    );
    check_transcript();
}

/// Labels or associated pull requests past their first page leave the
/// inventory's PR view incomplete, so verified receipts cannot settle the
/// repository: it takes the full write path instead.
#[tokio::test(start_paused = true)]
async fn paginated_inventory_connections_close_the_receipts_fast_path() {
    let mut servers = Vec::new();
    for paginated in ["labels", "pull-requests"] {
        let (mut config, repo) = pr_config();
        config.prs.rebase = Rebase::Behind;
        let (mut p, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        servers.push(server.clone());
        let github = client(server.clone()).await;
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
            &github,
            &repo,
            std::slice::from_mut(&mut p),
            &config,
            chrono::Utc::now(),
            Default::default(),
            &Default::default(),
            "patchula[bot]",
        )
        .await
        .unwrap();
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut p),
            &snapshot,
            "main",
            "patchula[bot]",
            config.prs.rebase,
            false,
        )
        .await;
        publish(&mut lifecycle, &github, std::slice::from_mut(&mut p)).await;
        patchula_update::save_receipts(
            &cache,
            &repo,
            &config,
            std::slice::from_ref(&p),
            &lifecycle,
        )
        .await
        .unwrap();
        {
            let mut s = server.state();
            let mut other = s.repo().prs.get(&1).unwrap().clone();
            other["number"] = json!(2);
            other["node_id"] = json!("PR_other");
            other["html_url"] = json!("https://github.com/demo/app/pull/2");
            other["head"]["ref"] = json!("patchula/other-two");
            other["head"]["sha"] = json!("other");
            other["title"] = json!("unrelated work");
            s.repo_mut().prs.insert(2, other);
            s.repo_mut().commits.insert(
                "other".into(),
                json!({
                    "sha":"other",
                    "tree":{"sha":"base-tree"},
                    "message":"human work",
                    "actor":"human"
                }),
            );
            s.repo_mut()
                .refs
                .insert("refs/heads/patchula/other-two".into(), "other".into());
            match paginated {
                "labels" => s.settings.label_next_page = Some(2),
                _ => s.settings.pr_next_page = Some("patchula/other-two".into()),
            }
        }
        let (mut next, _) = make_plan("22.0.0");
        let inventory = patchula_update::Inventory::read(&github, &cache, &repo, &config)
            .await
            .unwrap();
        let outcome = patchula_update::restore_inventory(
            &cache,
            &repo,
            &config,
            std::slice::from_mut(&mut next),
            Default::default(),
            None,
            &inventory,
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            patchula_update::RestoreOutcome::Partial,
            "{paginated}"
        );
    }
    check_transcript();
}

/// A branch whose tip records a base GitHub cannot compare is left alone as
/// someone else's, rather than failing the run.
#[tokio::test(start_paused = true)]
async fn an_uncomparable_recorded_base_leaves_the_branch_protected() {
    let (mut first, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut first),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(first.action().status, BranchStatus::Created);
    {
        let mut s = server.state();
        let sha = s.repo().refs[&format!("refs/heads/{}", first.action().branch)].clone();
        let commit = s.repo_mut().commits.get_mut(&sha).unwrap();
        commit["message"] = json!(
            commit["message"]
                .as_str()
                .unwrap()
                .replace("Patchula-Base: base", "Patchula-Base: unknown")
        );
    }
    let writes = server.state().writes.len();
    let (mut second, _) = make_plan("22.1.0");
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut second),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(
        second.action().status,
        BranchStatus::Protected,
        "{:?}",
        second.action()
    );
    assert!(
        second
            .action()
            .reason
            .as_deref()
            .unwrap()
            .contains("not a commit here")
    );
    assert_eq!(server.state().writes.len(), writes);
    check_transcript();
}

/// The report reads the plan's action: each named transition's status and
/// reason are the bytes `BranchAction` serializes.
#[test]
fn transitions_keep_the_serialized_status_and_reason() {
    let pair = |plan: &Plan| {
        let action = serde_json::to_value(plan.action()).unwrap();
        (action["status"].clone(), action["reason"].clone())
    };
    let (planned, _) = make_plan("22.0.0");
    assert_eq!(pair(&planned), (json!("planned"), Value::Null));
    let (mut held, _) = make_plan("22.0.0");
    held.hold(patchula_update::Hold::Blocked("digest unavailable".into()));
    assert_eq!(pair(&held), (json!("skipped"), json!("digest unavailable")));
    let (mut failed, _) = make_plan("22.0.0");
    failed.error("git data refused the write");
    assert_eq!(
        pair(&failed),
        (json!("error"), json!("git data refused the write"))
    );
    let (mut same, _) = make_plan("22.0.0");
    same.unchanged("head".into(), "tree".into());
    assert_eq!(pair(&same), (json!("unchanged"), Value::Null));
}

#[test]
fn only_newly_ticked_controls_count_as_edits() {
    use patchula_update::pull_requests::{checked, controls_edited};
    let recheck = "<!-- patchula-recheck -->";
    let add = "<!-- patchula-batch-add -->";
    let line = |tick: &str, marker: &str| format!("- [{tick}] item {marker}");
    assert!(controls_edited(&line(" ", recheck), &line("x", recheck)));
    assert!(!controls_edited(&line("x", recheck), &line("x", recheck)));
    assert!(controls_edited("", &line("x", add)));
    assert!(!controls_edited(&line(" ", add), &line(" ", add)));
    let remove = "<!-- patchula-batch-remove:abc -->";
    assert!(controls_edited("", &line("x", remove)));
    // `checked` wants exactly one recheck marker and a ticked line ending in it.
    let pr = |text: &str| {
        serde_json::from_value::<patchula_github::model::PullRequest>(json!({
            "number":1,"node_id":"N1","html_url":"u","state":"open","title":"t",
            "body":text,"labels":[],"user":{"login":"bot"},"head":{"ref":"b","sha":"s"},"base":{"ref":"main"}
        }))
        .unwrap()
    };
    assert!(checked(&pr(&line("x", recheck))));
    assert!(!checked(&pr(&line(" ", recheck))));
    assert!(!checked(&pr(&format!(
        "{}{}",
        line("x", recheck),
        line("x", recheck)
    ))));
}

/// A plan whose lockfiles were never generated fails before a single GitHub
/// request — publication requires them ready.
#[tokio::test(start_paused = true)]
async fn ungenerated_lockfiles_fail_before_any_github_request() {
    let (p, snapshot) = make_plan("22.0.0");
    let mut p = Draft::from(p)
        .lock_jobs(vec![patchula_lockfiles::Job {
            ecosystem: patchula_lockfiles::Ecosystem::Node,
            directory: String::new(),
            scope: patchula_lockfiles::Scope::Directory,
            exact_version: false,
            publish_age_allow: false,
            wait_hours: 0,
        }])
        .lockfiles_ready(false)
        .build();
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    assert_eq!(p.action().status, BranchStatus::Error, "{:?}", p.action());
    assert_eq!(
        p.action().reason.as_deref(),
        Some("lockfiles must be generated successfully before publication")
    );
    assert!(server.state().requests.is_empty());
    check_transcript();
}

/// A branch tip without the Count's base marker, or with one that cannot
/// name a commit, is left untouched.
#[tokio::test(start_paused = true)]
async fn unmarked_and_badly_marked_branch_tips_are_protected() {
    let mut servers = Vec::new();
    for (message, expected) in [
        (json!("human work"), "unmarked branch history"),
        (
            json!(format!(
                "subject\n\nPatchula: v1\nPatchula-Base: {}",
                "a".repeat(65)
            )),
            "invalid recorded branch base",
        ),
        (
            json!("subject\n\nPatchula: v1\nPatchula-Base: not-a-sha!"),
            "invalid recorded branch base",
        ),
    ] {
        let (mut first, snapshot) = make_plan("22.0.0");
        let server = fake(&snapshot);
        servers.push(server.clone());
        let github = client(server.clone()).await;
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut first),
            &snapshot,
            "main",
            "patchula[bot]",
            Rebase::Behind,
            false,
        )
        .await;
        assert_eq!(first.action().status, BranchStatus::Created);
        server
            .state()
            .repo_mut()
            .commits
            .get_mut(first.action().commit.as_ref().unwrap())
            .unwrap()["message"] = message;
        let count = server.state().writes.len();
        let (mut next, _) = make_plan("22.1.0");
        patchula_update::apply_all(
            &github,
            std::slice::from_mut(&mut next),
            &snapshot,
            "main",
            "patchula[bot]",
            Rebase::Behind,
            false,
        )
        .await;
        assert_eq!(
            next.action().status,
            BranchStatus::Protected,
            "{expected}: {:?}",
            next.action()
        );
        assert!(
            next.action()
                .reason
                .as_deref()
                .is_some_and(|r| r.contains(expected)),
            "{expected}: {:?}",
            next.action().reason
        );
        assert_eq!(server.state().writes.len(), count);
    }
    check_transcript();
}

/// The shared tail turns lifecycle failures into warnings and goes on:
/// a batched closure and a checkbox acknowledgement that fail are reported,
/// and a good recheck is still unticked.
#[tokio::test(start_paused = true)]
async fn settle_turns_lifecycle_failures_into_report_warnings() {
    let (mut config, repo) = pr_config();
    config.prs.dashboard = false;
    let (p, snapshot) = make_plan("22.0.0");
    let key = "f".repeat(64);
    let owned = |number: u64, branch: &str, body: &str| {
        json!({"number":number,"node_id":format!("N{number}"),"html_url":"u",
            "state":"open","title":"t","body":body,
            "user":{"login":"patchula[bot]"},
            "head":{"ref":branch,"sha":"base","repo":{"full_name":"demo/app"}},
            "base":{"ref":"main","repo":{"full_name":"demo/app"}},
            "labels":[],"auto_merge":null,"merged_at":null})
    };
    let marker = |update: &str| {
        format!(
            "<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{update}\"}} -->",
            "a".repeat(64)
        )
    };
    let server = fake(&snapshot);
    {
        let mut s = server.state();
        // A batched PR the batch now carries, which the run cannot read.
        s.repo_mut().prs.insert(
            7,
            owned(
                7,
                "patchula/left-pad-1.x",
                &format!("- [x] Add to the batch PR. <!-- patchula-batch-add -->\n- [x] re-check <!-- patchula-recheck -->\n{}", marker(&key)),
            ),
        );
        // A PR whose re-check box is ticked and can be acknowledged.
        s.repo_mut().prs.insert(
            8,
            owned(
                8,
                "patchula/other-1.x",
                &format!(
                    "- [x] re-check <!-- patchula-recheck -->\n{}",
                    marker(&"b".repeat(64))
                ),
            ),
        );
        server.pull_read_fails(7);
    }
    let github = client(server.clone()).await;
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let targets = p.changes().to_vec();
    let mut p = Draft::from(p)
        .batch(vec![patchula_update::batch::Item {
            origin: key,
            number: Some(7),
            title: "batched".into(),
            targets,
        }])
        .build();
    let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut p),
        &config,
        chrono::Utc::now(),
        patchula_words::Recheck::NextRun,
        &patchula_update::dashboard::Controls {
            batched: BTreeSet::from(["f".repeat(64)]),
            ..Default::default()
        },
        "patchula[bot]",
    )
    .await
    .unwrap();
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        "patchula[bot]",
        Rebase::Behind,
        false,
    )
    .await;
    publish(&mut lifecycle, &github, std::slice::from_mut(&mut p)).await;
    let inventory = patchula_update::Inventory::default();
    let settlement = patchula_update::settle(
        &github,
        &cache,
        &repo,
        &config,
        &inventory,
        std::slice::from_mut(&mut p),
        Some(&mut lifecycle),
        patchula_words::Recheck::NextRun,
        None,
        chrono::Utc::now(),
    )
    .await;
    assert!(
        settlement
            .warnings
            .iter()
            .any(|w| w.starts_with("batched PR closure: ")),
        "{:?}",
        settlement.warnings
    );
    assert!(
        settlement
            .warnings
            .iter()
            .any(|w| w.starts_with("PR checkbox acknowledgement: ")),
        "{:?}",
        settlement.warnings
    );
    // The reachable re-check was still unticked.
    let body = server.state().repo().prs[&8]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(body.contains("- [ ]"), "{body}");
    assert!(!body.contains("- [x]"), "{body}");
    check_transcript();
}

/// A maintenance plan's PR lists the files it refreshed.
#[tokio::test(start_paused = true)]
async fn a_maintenance_pr_lists_the_files_it_refreshed() {
    let (config, repo) = pr_config();
    let (p, snapshot) = make_plan("22.0.0");
    let mut p = Draft::from(p).maintenance().build();
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
    assert_eq!(
        actions[0].status,
        patchula_report::PullRequestStatus::Created
    );
    let body = server.state().repo().prs[&1]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(body.contains("Scheduled"), "{body}");
    assert!(body.contains("mise.toml"), "{body}");
    check_transcript();
}

/// After eight release-note sections the PR body says the source
/// repositories have the rest.
#[tokio::test(start_paused = true)]
async fn release_notes_stop_after_eight_with_a_pointer() {
    let (config, repo) = pr_config();
    let (p, snapshot) = make_plan("22.0.0");
    let base = p.changes()[0].clone();
    let mut draft = Draft::from(p);
    *draft.changes_mut() = (0..9)
        .map(|i| patchula_update::Change {
            version: Some(format!("22.0.{i}")),
            value: Some(format!("22.0.{i}")),
            ..base.clone()
        })
        .collect();
    for change in draft.changes_mut() {
        change.release_notes = true;
    }
    let mut p = draft.build();
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
    let body = server.state().repo().prs[&1]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        body.matches("### node").count(),
        8,
        "nine releases must render exactly eight sections: {body}"
    );
    assert!(
        body.contains("Out of room for more release notes"),
        "{body}"
    );
    check_transcript();
}

/// An image rebuild that fixes advisories shows the fixed rows without the
/// "nothing to fix" note.
#[tokio::test(start_paused = true)]
async fn a_rebuild_that_fixes_advisories_skips_the_empty_note() {
    let (config, repo) = pr_config();
    let (p, snapshot) = make_plan("22.0.0");
    let mut draft = Draft::from(p);
    draft.changes_mut()[0].image = Some(patchula_report::ImageCheck {
        state: patchula_report::ImageState::Checked,
        fixed: vec![patchula_report::ImageAdvisory {
            id: "GHSA-1".into(),
            package: "openssl".into(),
            old: "1.0".into(),
            new: Some("1.1".into()),
            platforms: vec!["linux".into()],
        }],
        ..Default::default()
    });
    draft.changes_mut()[0].advisories = vec![patchula_report::Advisory {
        id: "GHSA-1".into(),
        aliases: vec![],
        severity: None,
        summary: None,
        url: "https://a.example".into(),
        fixed_in: None,
    }];
    let mut p = draft.build();
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
    let body = server.state().repo().prs[&1]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        !body.contains("OSV.dev knows of no vulnerability"),
        "{body}"
    );
    assert!(body.contains("GHSA-1"), "{body}");
    check_transcript();
}

/// When the controls and ownership marker would overflow the body budget,
/// the controls give way and the ownership marker stays.
#[tokio::test(start_paused = true)]
async fn an_overfull_tail_drops_the_controls_but_keeps_ownership() {
    let (config, repo) = pr_config();
    let (p, snapshot) = make_plan("22.0.0");
    // A TARGETS marker just under its own cap, and release-age permits that
    // together push the tail past the body budget.
    let base = p.changes()[0].clone();
    let mut draft = Draft::from(p);
    draft.changes_mut().clear();
    while serde_json::to_string(draft.changes_mut().as_slice())
        .unwrap()
        .len()
        < 11_200
    {
        let n = draft.changes_mut().len();
        draft.changes_mut().push(patchula_update::Change {
            package: format!("package-number-{n}"),
            ..base.clone()
        });
    }
    let mut p = draft.build();
    let permits: BTreeSet<String> = (0..340).map(|i| format!("{i:064x}")).collect();
    let branch = p.action().branch.clone();
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut p),
        &config,
        chrono::Utc::now(),
        patchula_words::Recheck::NextRun,
        &patchula_update::dashboard::Controls {
            branch_permits: BTreeMap::from([(branch, permits)]),
            ..Default::default()
        },
        "patchula[bot]",
    )
    .await
    .unwrap();
    patchula_update::apply_all(
        &github,
        std::slice::from_mut(&mut p),
        &snapshot,
        "main",
        "patchula[bot]",
        config.prs.rebase,
        false,
    )
    .await;
    publish(&mut lifecycle, &github, std::slice::from_mut(&mut p)).await;
    let body = server.state().repo().prs[&1]["body"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!body.contains("Add to the batch PR"), "{body}");
    assert!(body.contains("<!-- patchula:"), "{body}");
    check_transcript();
}

/// A merge the Count never recorded is not presented as a merge outcome on
/// the dashboard.
#[test]
fn dashboard_history_hides_merges_the_count_did_not_record() {
    let merged = |number: u64, noted: bool| {
        let mut tracked = patchula_update::pull_requests::Tracked::from(
            serde_json::from_value::<patchula_github::model::PullRequest>(json!({
                "number":number,"node_id":format!("N{number}"),"html_url":"u",
                "state":"closed","title":"t","merged_at":"2026-01-01T00:00:00Z",
                "labels":[],"user":{"login":"bot"},"head":{"ref":"b","sha":"s"},"base":{"ref":"main"}
            }))
            .unwrap(),
        );
        if noted {
            tracked.set_merge_note(Some(patchula_update::pull_requests::MergeNote::Native));
        }
        tracked
    };
    let prs = vec![merged(1, false), merged(2, true)];
    let history = patchula_update::automerge::dashboard_history(
        &patchula_update::Inventory::default(),
        prs,
        &[],
    );
    assert_eq!(history.open.len(), 1);
    assert_eq!(history.open[0].pr.number, 2);
    assert_eq!(history.merged.len(), 1);
    assert_eq!(history.merged[0].number, Some(2));
}

/// An owned, signed ref outside the branch prefix whose tip carries the
/// Count's mark still counts as his obsolete work, and the receipts fast
/// path closes.
#[tokio::test(start_paused = true)]
async fn a_marked_ref_outside_the_prefix_counts_as_obsolete_work() {
    let (config, repo) = pr_config();
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    // No plans: the ref scan still marks his marked, signed, own-authored
    // branch outside the prefix obsolete, which defeats the fast path.
    let inventory = patchula_update::Inventory {
        actor: "patchula[bot]".into(),
        refs: patchula_update::Page::of(vec![
            serde_json::from_value(json!({
                "name": "renovate/other",
                "target": {
                    "__typename": "Commit",
                    "oid": "abc",
                    "tree": {"oid": "tree"},
                    "author": {"user": {"login": "patchula[bot]"}},
                    "committer": {"user": {"login": "web-flow"}},
                    "signature": {"__typename": "GpgSignature", "isValid": true},
                    "message": "subject\n\nPatchula: v1"
                },
                "associatedPullRequests": {"nodes": [], "pageInfo": {"hasNextPage": false}}
            }))
            .unwrap(),
        ]),
        ..Default::default()
    };
    let outcome = patchula_update::restore_inventory(
        &cache,
        &repo,
        &config,
        &mut [],
        Default::default(),
        None,
        &inventory,
    )
    .await
    .unwrap();
    assert_eq!(outcome, patchula_update::RestoreOutcome::Partial);
}

/// A closed pull request on the plan's branch, as `ClosedPulls` returns
/// it: the stored PR `create_pull` writes, then flipped closed.
fn seeded_closed(branch: &str, body: &str, created: &str) -> Value {
    json!({"number":2,"node_id":"pr-seeded",
        "html_url":"https://github.com/demo/app/pull/2",
        "state":"closed","title":"seeded","body":body,
        "head":{"ref":branch,"sha":"seeded-sha","repo":{"full_name":"demo/app"}},
        "base":{"ref":"main","repo":{"full_name":"demo/app"}},
        "user":{"login":"patchula[bot]"},"labels":[],"auto_merge":null,
        "created_at":created,"merged_at":null,
        "draft":false,"mergeable":true,"mergeable_state":"clean"})
}
/// His ownership marker's bytes with `update` as the key.
fn marker_body(update: &str) -> String {
    format!(
        "<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{update}\"}} -->",
        "a".repeat(64)
    )
}

/// More than a hundred pull requests of history no longer widen the
/// reads: the open listing pages the open pulls alone, the planned
/// branch's `ClosedPulls` read finds the suppressor and the plan stays
/// held by it.
#[tokio::test(start_paused = true)]
async fn a_hundred_pulls_of_history_narrow_to_open_and_planned_reads() {
    let (config, repo) = pr_config();
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
    // The owner closed his PR; 150 pulls of history surround it, fifteen
    // of them open on branches no plan wants.
    server.state().repo_mut().prs.get_mut(&1).unwrap()["state"] = json!("closed");
    server.seed_pulls(150);
    let (mut plan, _) = make_plan("22.0.0");
    {
        let mut state = server.state();
        state.requests.clear();
        state.traffic.clear();
    }
    patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut plan),
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert!(
        plan.held(&patchula_update::Hold::ClosedByOwner),
        "{:?}",
        plan.action()
    );
    let state = server.state();
    // One open page (fifteen open of 151 pulls), no whole-history listing
    // — `prs.perHour` is unset — and one `ClosedPulls` request.
    let listing = |needle: &str| -> usize {
        state
            .traffic
            .iter()
            .filter(|(route, _)| route.starts_with("GET /repos/demo/app/pulls?"))
            .filter(|(route, _)| route.contains(needle))
            .map(|(_, traffic)| traffic.requests)
            .sum()
    };
    assert_eq!(listing("state=open"), 1, "{:?}", state.traffic);
    assert_eq!(listing("state=all"), 0, "{:?}", state.traffic);
    assert_eq!(
        state.requests.iter().filter(|(m, _)| m == "POST").count(),
        1
    );
    check_transcript();
}

/// A closed pull request whose head repository was deleted decodes with a
/// null `headRepository`, is not his and does not suppress the plan.
#[tokio::test(start_paused = true)]
async fn a_closed_pull_on_a_deleted_head_repository_does_not_suppress() {
    let (config, repo) = pr_config();
    let (mut plan, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let mut pr = seeded_closed(
        &plan.action().branch,
        &marker_body(&plan.update_key()),
        &chrono::Utc::now().to_rfc3339(),
    );
    pr["head"]["repo"] = Value::Null;
    server.state().repo_mut().prs.insert(2, pr);
    patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut plan),
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert_eq!(
        plan.action().status,
        BranchStatus::Planned,
        "{:?}",
        plan.action()
    );
    check_transcript();
}

/// A branch with more closed pull requests than a page holds pages on its
/// own: the suppressor sits on the second page and still holds the plan.
#[tokio::test(start_paused = true)]
async fn a_branchs_closed_pulls_page_on_their_own() {
    let (config, repo) = pr_config();
    let (mut plan, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let now = chrono::Utc::now();
    {
        let mut state = server.state();
        let repo = state.repo_mut();
        // 120 newer closed pulls of others', then his suppressor oldest —
        // it lands on the branch's second `ClosedPulls` page.
        for i in 0..120_u64 {
            let mut pr = seeded_closed(
                &plan.action().branch,
                "",
                &(now + chrono::Duration::minutes(i as i64)).to_rfc3339(),
            );
            pr["number"] = json!(100 + i);
            pr["user"]["login"] = json!("octocat");
            repo.prs.insert(100 + i, pr);
        }
        let mut suppressor = seeded_closed(
            &plan.action().branch,
            &marker_body(&plan.update_key()),
            &(now - chrono::Duration::hours(1)).to_rfc3339(),
        );
        suppressor["number"] = json!(2);
        repo.prs.insert(2, suppressor);
    }
    server.state().requests.clear();
    patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut plan),
        &config,
        now,
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert!(
        plan.held(&patchula_update::Hold::ClosedByOwner),
        "{:?}",
        plan.action()
    );
    assert_eq!(
        server
            .state()
            .requests
            .iter()
            .filter(|(m, _)| m == "POST")
            .count(),
        2
    );
    check_transcript();
}

/// A null node in a branch's `ClosedPulls` page leaves the branch unread:
/// the plan is protected, not admitted.
#[tokio::test(start_paused = true)]
async fn a_null_closed_pull_node_protects_the_plan() {
    let (config, repo) = pr_config();
    let (mut plan, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    server
        .on(patchula_testkit::github::Matcher::Route {
            method: reqwest::Method::POST,
            path: "/graphql".into(),
        })
        .times(1)
        .respond(
            200,
            json!({"data":{"repository":{"b0":{
                "nodes":[null],
                "pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
        );
    patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut plan),
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert_eq!(plan.action().status, BranchStatus::Protected);
    assert_eq!(
        plan.action().reason.as_deref(),
        Some("closed pull requests for this branch could not be read; left untouched")
    );
    check_transcript();
}

/// `prs.perHour`'s recent read counts owned creations whether they still
/// stand or not: a pull request merged within the hour holds the plan.
#[tokio::test(start_paused = true)]
async fn a_pull_merged_within_the_hour_counts_against_per_hour() {
    let (mut config, repo) = pr_config();
    config.prs.per_hour = Some(1);
    let (mut plan, snapshot) = make_plan("22.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    let now = chrono::Utc::now();
    let mut pr = seeded_closed(
        "patchula/other-1.x",
        &marker_body(&"b".repeat(64)),
        &(now - chrono::Duration::minutes(10)).to_rfc3339(),
    );
    pr["merged_at"] = json!((now - chrono::Duration::minutes(5)).to_rfc3339());
    server.state().repo_mut().prs.insert(2, pr);
    patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        std::slice::from_mut(&mut plan),
        &config,
        now,
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert!(
        plan.held(&patchula_update::Hold::PerHour),
        "{:?}",
        plan.action()
    );
    assert!(
        server
            .state()
            .traffic
            .keys()
            .any(|route| route.contains("state=all")),
        "{:?}",
        server.state().traffic
    );
    check_transcript();
}

/// An unreadable `ClosedPulls` page protects its plan while the run
/// carries on: the other branch's read answered, its plan published.
#[tokio::test(start_paused = true)]
async fn an_unreadable_closed_branch_protects_its_plan_only() {
    let (config, repo) = pr_config();
    let (first, snapshot) = make_plan("22.0.0");
    let (second, _) = make_plan("24.0.0");
    let server = fake(&snapshot);
    let github = client(server.clone()).await;
    // `closed_reads` names the branches in plan order: `b0` is the first
    // plan's, its page a null node; `b1` answers empty.
    server
        .on(patchula_testkit::github::Matcher::Route {
            method: reqwest::Method::POST,
            path: "/graphql".into(),
        })
        .times(1)
        .respond(
            200,
            json!({"data":{"repository":{
                "b0":{"nodes":[null],
                    "pageInfo":{"hasNextPage":false,"endCursor":null}},
                "b1":{"nodes":[],
                    "pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
        );
    let mut plans = vec![first, second];
    let mut lifecycle = patchula_update::Lifecycle::prepare_with_controls(
        &github,
        &repo,
        &mut plans,
        &config,
        chrono::Utc::now(),
        Default::default(),
        &Default::default(),
        "patchula[bot]",
    )
    .await
    .unwrap();
    assert_eq!(plans[0].action().status, BranchStatus::Protected);
    patchula_update::apply_all(
        &github,
        &mut plans,
        &snapshot,
        "main",
        "patchula[bot]",
        config.prs.rebase,
        false,
    )
    .await;
    publish(&mut lifecycle, &github, &mut plans).await;
    assert_eq!(plans[1].action().status, BranchStatus::Created);
    check_transcript();
}
