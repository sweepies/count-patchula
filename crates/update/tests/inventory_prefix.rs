//! Differential coverage for the prefix walk: with a write receipt stored
//! the inventory asks `refs(query:)` for the branch prefix and keeps only
//! the names starting with it; without one it walks every branch as before.
//! Both reads must reach the same receipts-path decisions — `RestoreOutcome`,
//! dashboard controls and tooling evidence — except for the one intended
//! difference a named test pins: an owned branch outside the prefix is
//! invisible until a full write.
use patchula_cache::Cache;
use patchula_config::Config;
use patchula_github::Client;
use patchula_report::{BranchAction, BranchStatus, Repository};
use patchula_testkit::github::FakeGitHub;
use patchula_update::{
    Inventory, RestoreOutcome, Snapshot, dashboard, restore_inventory, seed_receipts, tooling,
};
use patchula_words::Recheck;
use serde_json::{Value, json};
use std::sync::Arc;

fn repository() -> Repository {
    serde_json::from_value(
        json!({"repository":"demo/app","status":"complete","reason":null,
        "packageFiles":[],"defaultBranch":"main","commit":"base"}),
    )
    .unwrap()
}

fn fake() -> Arc<FakeGitHub> {
    let server = Arc::new(FakeGitHub::new("demo/app"));
    server.seed("main", "base", vec![]);
    server
}

fn actor_author() -> Value {
    json!({"__typename":"Bot","login":"patchula"})
}

fn foreign_author() -> Value {
    json!({"__typename":"User","login":"octocat"})
}

/// `branch` pointing at `sha`'s commit: the Count's own work — his actor,
/// web-flow committer, a valid signature and his trailer block, all of
/// which the fake answers — or a human's (`owned: false`).
fn seed_branch(server: &FakeGitHub, branch: &str, sha: &str, owned: bool) {
    let message = if owned {
        format!(
            "chore(deps): update a dependency\n\nPatchula: v1\nPatchula-Branch: {branch}\nPatchula-Base: base\nPatchula-Plan: p"
        )
    } else {
        "human work".into()
    };
    let actor = if owned { "patchula[bot]" } else { "octocat" };
    let mut state = server.state();
    let repo = state.repo_mut();
    repo.refs.insert(format!("refs/heads/{branch}"), sha.into());
    repo.commits.insert(
        sha.into(),
        json!({"sha":sha,"tree":{"sha":"t"},"message":message,"actor":actor}),
    );
}

fn seed_issue(server: &FakeGitHub, number: u64, body: &str, author: Value) {
    server.state().repo_mut().issues.insert(
        number,
        json!({"number":number,"id":format!("i{number}"),"title":format!("issue {number}"),
        "body":body,"state":"OPEN","author":author}),
    );
}

fn client(cache: &Cache, server: Arc<FakeGitHub>) -> Client {
    Client::with_transport("offline", cache.clone(), Default::default(), server).unwrap()
}

fn config() -> Config {
    patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap()
}

/// The decisions the receipts path makes from an inventory: whether the
/// plans settle, the dashboard's controls, the tooling evidence and the
/// open pull requests the lifecycle sees — everything the prefix walk
/// must leave identical.
async fn decisions(
    cache: &Cache,
    repo: &Repository,
    config: &Config,
    inventory: &Inventory,
) -> (RestoreOutcome, String, String, Vec<u64>) {
    let mut plans: Vec<patchula_update::Plan> = Vec::new();
    let outcome = restore_inventory(
        cache,
        repo,
        config,
        &mut plans,
        Recheck::default(),
        None,
        inventory,
    )
    .await
    .unwrap();
    let controls = dashboard::Controls::read(inventory, "demo/app", true)
        .map(|controls| format!("{controls:?}"))
        .unwrap_or_else(|error| error.to_string());
    let evidence = tooling::Evidence::read(&Snapshot::default(), "main", inventory);
    let prs: Vec<u64> = inventory
        .pull_requests("demo/app")
        .iter()
        .map(|tracked| tracked.pr.number)
        .collect();
    (outcome, controls, format!("{evidence:?}"), prs)
}

fn names(inventory: &Inventory) -> Vec<String> {
    inventory
        .refs
        .items
        .iter()
        .flatten()
        .map(|r| r.name.clone())
        .collect()
}

fn check_transcript() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

/// A repository in steady state — the Count's branches among hundreds of
/// foreign ones — decides identically under either walk: the prefix read
/// gathers exactly the Count's branches, infix and case hits the
/// substring filter answers are kept out by the client, and every
/// receipts-path decision is the full walk's.
#[tokio::test]
async fn many_foreign_branches_decide_alike() {
    let server = fake();
    // In-prefix branches past one page, so the prefix read exercises the
    // `refs` continuation carrying `query` too.
    for number in 1..=150 {
        seed_branch(
            &server,
            &format!("patchula/{number}"),
            &format!("p{number}"),
            true,
        );
    }
    // `vendor/patchula/thing` is an infix hit and `Patchula/case` a case
    // hit on GitHub's `query`: both arrive in the answer and only the
    // client-side prefix check drops them.
    seed_branch(&server, "vendor/patchula/thing", "infix", false);
    seed_branch(&server, "Patchula/case", "case", false);
    for number in 1..=200 {
        seed_branch(
            &server,
            &format!("topic/{number}"),
            &format!("t{number}"),
            false,
        );
    }
    seed_issue(
        &server,
        1,
        "dashboard <!-- patchula:dashboard -->",
        actor_author(),
    );
    for number in 2..=10 {
        seed_issue(&server, number, "filler", foreign_author());
    }
    // An in-prefix branch whose pull requests page past the first is
    // unsettled in both walks.
    server.state().settings.pr_next_page = Some("patchula/1".into());
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let github = client(&cache, server.clone());
    let config = config();
    let repo = repository();
    // The full walk: no receipt stored, so every branch is read.
    let full = Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    assert_eq!(full.refs.items.len(), 353);
    seed_receipts(&cache, "demo/app", &config).await.unwrap();
    // The prefix walk: the receipt narrows `refs` to the branch prefix.
    let prefix = Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    let wanted: std::collections::BTreeSet<String> =
        (1..=150).map(|n| format!("patchula/{n}")).collect();
    assert_eq!(
        names(&prefix)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        wanted
    );
    assert_eq!(prefix.unsettled, full.unsettled);
    assert!(!prefix.unsettled.is_empty());
    assert_eq!(
        decisions(&cache, &repo, &config, &full).await,
        decisions(&cache, &repo, &config, &prefix).await
    );
    check_transcript();
}

/// The one intended difference: a branch a person made by copying the
/// Count's — an owned commit under a name outside the prefix — is
/// obsolete evidence only the full walk sees. Today the run it tips
/// partial deletes the copy; under the prefix walk the next full write
/// does, and a receipts-settled run never sees it.
#[tokio::test]
async fn a_copy_of_a_counts_branch_is_obsolete_only_to_the_full_walk() {
    let server = fake();
    seed_branch(&server, "human/copied-update", "copy", true);
    for number in 1..=50 {
        seed_branch(
            &server,
            &format!("topic/{number}"),
            &format!("t{number}"),
            false,
        );
    }
    seed_issue(
        &server,
        1,
        "dashboard <!-- patchula:dashboard -->",
        actor_author(),
    );
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let github = client(&cache, server.clone());
    let config = config();
    let repo = repository();
    let full = Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    seed_receipts(&cache, "demo/app", &config).await.unwrap();
    let prefix = Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    assert_eq!(names(&prefix), Vec::<String>::new());
    let mut plans: Vec<patchula_update::Plan> = Vec::new();
    assert_eq!(
        restore_inventory(
            &cache,
            &repo,
            &config,
            &mut plans,
            Recheck::default(),
            None,
            &full
        )
        .await
        .unwrap(),
        RestoreOutcome::Partial
    );
    assert_eq!(
        restore_inventory(
            &cache,
            &repo,
            &config,
            &mut plans,
            Recheck::default(),
            None,
            &prefix
        )
        .await
        .unwrap(),
        RestoreOutcome::Complete
    );
    check_transcript();
}

/// A `patchula/` branch the plans want is restored identically: the
/// receipt's fast path reads it in either walk.
#[tokio::test]
async fn a_plan_settles_alike_under_either_walk() {
    let server = fake();
    seed_branch(&server, "patchula/kept", "kept", true);
    seed_branch(&server, "patchula/spare", "spare", true);
    for number in 1..=120 {
        seed_branch(
            &server,
            &format!("topic/{number}"),
            &format!("t{number}"),
            false,
        );
    }
    seed_issue(
        &server,
        1,
        "dashboard <!-- patchula:dashboard -->",
        actor_author(),
    );
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let github = client(&cache, server.clone());
    let config = config();
    let repo = repository();
    let full = Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    seed_receipts(&cache, "demo/app", &config).await.unwrap();
    let prefix = Inventory::read(&github, &cache, &repo, &config)
        .await
        .unwrap();
    assert_eq!(names(&prefix), vec!["patchula/kept", "patchula/spare"]);
    let planned = |branch: &str| {
        patchula_update::testing::Draft::new(BranchAction {
            repository: "demo/app".into(),
            branch: branch.into(),
            base_commit: "base".into(),
            status: BranchStatus::Planned,
            reason: None,
            message: "chore(deps): update a dependency".into(),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        })
        .build()
    };
    // `patchula/kept` planned (no matching receipt — it falls back, but the
    // same way in both walks) beside `patchula/spare`, which no plan wants:
    // the spare is obsolete in both walks.
    let mut plans = vec![planned("patchula/kept")];
    assert_eq!(
        restore_inventory(
            &cache,
            &repo,
            &config,
            &mut plans,
            Recheck::default(),
            None,
            &full
        )
        .await
        .unwrap(),
        restore_inventory(
            &cache,
            &repo,
            &config,
            &mut plans,
            Recheck::default(),
            None,
            &prefix
        )
        .await
        .unwrap()
    );
    assert_eq!(
        decisions(&cache, &repo, &config, &full).await,
        decisions(&cache, &repo, &config, &prefix).await
    );
    check_transcript();
}
