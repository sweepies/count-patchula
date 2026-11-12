//! Differential coverage for the split inventory walk: the same seeded fake
//! is read through the pre-split document (`read_legacy_inventory`) and
//! through the new documents — once without an actor hint (unfiltered,
//! restarting a paged issue connection on the answer's actor), once with
//! the hint that read stored (`filterBy.createdBy`). Every value a caller
//! could tell apart must be equal; the issue connection itself is equal
//! only on the Count's own nodes, which is the filter's point.
use patchula_cache::Cache;
use patchula_config::Config;
use patchula_github::Client;
use patchula_report::Repository;
use patchula_testkit::github::FakeGitHub;
use patchula_update::{
    Inventory, dashboard, markers::DASHBOARD as MARKER, read_legacy_inventory, restore_inventory,
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

/// The fake's actor node: `Bot`/`patchula`, whose REST login is
/// `patchula[bot]` — the value `viewer.login` reports.
fn actor_author() -> Value {
    json!({"__typename":"Bot","login":"patchula"})
}

fn foreign_author() -> Value {
    json!({"__typename":"Bot","login":"renovate"})
}

fn issue(number: u64, body: &str, author: Value) -> Value {
    json!({"number":number,"id":format!("i{number}"),"title":format!("issue {number}"),
        "body":body,"state":"OPEN","author":author})
}

fn seed_issue(server: &FakeGitHub, number: u64, body: &str, author: Value) {
    server
        .state()
        .repo_mut()
        .issues
        .insert(number, issue(number, body, author));
}

/// `issue`'s author resolves to `login` — the same `Bot` → `[bot]`-suffixed
/// read `actor_login` makes; kept local so the assertion states what it
/// compares rather than sharing the implementation under test.
fn authored_by(issue: &Option<patchula_update::Issue>, login: &str) -> bool {
    let Some(issue) = issue else { return false };
    let Some(author) = &issue.author else {
        return false;
    };
    let rest = author.login.strip_suffix("[bot]").unwrap_or(&author.login);
    let login_rest = login.strip_suffix("[bot]").unwrap_or(login);
    (author.typename == "Bot") == login.ends_with("[bot]") && rest == login_rest
}

struct Reads {
    legacy: Inventory,
    unfiltered: Inventory,
    filtered: Inventory,
}

/// The three reads over one seeded fake: the split documents first — the
/// first read has no stored actor hint, so its first page asks unfiltered;
/// a paged issue connection then restarts on the answer's `viewer.login`,
/// which the read stores — then the pre-split document (which ignores the
/// hint, its `issues` field having no `filterBy` argument), then a hinted
/// read filtering issues by `createdBy` from its first page.
async fn reads(server: Arc<FakeGitHub>) -> (Cache, Config, Reads) {
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let client =
        Client::with_transport("offline", cache.clone(), Default::default(), server).unwrap();
    let config = patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap();
    let repo = repository();
    let unfiltered = Inventory::read(&client, &cache, &repo, &config)
        .await
        .unwrap();
    let legacy = read_legacy_inventory(&client, &cache, &repo, &config)
        .await
        .unwrap();
    let filtered = Inventory::read(&client, &cache, &repo, &config)
        .await
        .unwrap();
    (
        cache,
        config,
        Reads {
            legacy,
            unfiltered,
            filtered,
        },
    )
}

/// The reads gather the same inventory the legacy document gathered; the
/// filtered walk's issue connection is compared by the caller.
fn same_values(a: &Inventory, b: &Inventory) {
    assert_eq!(a.actor, b.actor);
    assert_eq!(format!("{:?}", a.refs), format!("{:?}", b.refs));
    assert_eq!(format!("{:?}", a.closed), format!("{:?}", b.closed));
    assert_eq!(
        format!("{:?}", a.merge_settings),
        format!("{:?}", b.merge_settings)
    );
    assert_eq!(
        format!("{:?}", a.default_branch),
        format!("{:?}", b.default_branch)
    );
    assert_eq!(a.unsettled, b.unsettled);
    assert_eq!(format!("{:?}", a.merged_prs), format!("{:?}", b.merged_prs));
}

async fn same_decisions(
    cache: &Cache,
    repo: &Repository,
    config: &Config,
    a: &Inventory,
    b: &Inventory,
) {
    let outcome = |inventory: &Inventory| {
        dashboard::issue(inventory)
            .map(|issue| issue.map(|issue| issue.number))
            .map_err(|e| e.to_string())
    };
    assert_eq!(outcome(a), outcome(b));
    let controls = |inventory: &Inventory| {
        dashboard::Controls::read(inventory, "demo/app", true)
            .map(|c| format!("{c:?}"))
            .map_err(|e| e.to_string())
    };
    assert_eq!(controls(a), controls(b));
    let mut a_plans = vec![];
    let mut b_plans = vec![];
    assert_eq!(
        restore_inventory(
            cache,
            repo,
            config,
            &mut a_plans,
            Recheck::default(),
            None,
            a
        )
        .await
        .unwrap(),
        restore_inventory(
            cache,
            repo,
            config,
            &mut b_plans,
            Recheck::default(),
            None,
            b
        )
        .await
        .unwrap()
    );
}

/// The filtered read's issue connection must hold exactly the unfiltered
/// read's nodes authored by the actor — no more, no fewer.
fn filtered_issues(legacy: &Inventory, filtered: &Inventory, actor: &str) {
    let authored: Vec<&Option<patchula_update::Issue>> = legacy
        .issues
        .items
        .iter()
        .filter(|issue| authored_by(issue, actor))
        .collect();
    assert_eq!(
        format!("{:?}", filtered.issues.items),
        format!("{:?}", authored),
        "filtered read must keep exactly the actor's issues"
    );
    assert!(filtered.issues.complete);
}

fn check_transcript() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

/// Two dashboards on the Count's name — plus a dashboard another bot owns —
/// must refuse the dashboard lookup identically through every document.
#[tokio::test]
async fn two_dashboards_and_a_foreign_one_refuse_alike() {
    let server = fake();
    for number in 1..=2 {
        seed_issue(
            &server,
            number,
            &format!("dashboard {MARKER}"),
            actor_author(),
        );
    }
    seed_issue(&server, 3, &format!("foreign {MARKER}"), foreign_author());
    for number in 4..=20 {
        seed_issue(&server, number, "filler", foreign_author());
    }
    let (cache, config, reads) = reads(server.clone()).await;
    let repo = repository();
    same_values(&reads.legacy, &reads.unfiltered);
    assert_eq!(
        format!("{:?}", reads.legacy.issues),
        format!("{:?}", reads.unfiltered.issues)
    );
    same_values(&reads.legacy, &reads.filtered);
    filtered_issues(&reads.legacy, &reads.filtered, &reads.legacy.actor);
    same_decisions(&cache, &repo, &config, &reads.legacy, &reads.filtered).await;
    check_transcript();
}

/// 101 open issues — a page boundary — interleaving the Count's issues with
/// everyone else's. The unfiltered read's first page leaves the connection
/// unfinished, so it restarts filtered: its issue set is the hinted read's.
#[tokio::test]
async fn a_hundred_and_one_issues_decide_alike() {
    let server = fake();
    seed_issue(&server, 1, &format!("dashboard {MARKER}"), actor_author());
    for number in 2..=101 {
        let author = if number % 3 == 0 {
            actor_author()
        } else {
            foreign_author()
        };
        seed_issue(&server, number, "filler", author);
    }
    let (cache, config, reads) = reads(server.clone()).await;
    let repo = repository();
    same_values(&reads.legacy, &reads.unfiltered);
    same_values(&reads.legacy, &reads.filtered);
    filtered_issues(&reads.legacy, &reads.unfiltered, &reads.legacy.actor);
    filtered_issues(&reads.legacy, &reads.filtered, &reads.legacy.actor);
    assert_eq!(reads.legacy.issues.items.len(), 101);
    same_decisions(&cache, &repo, &config, &reads.legacy, &reads.filtered).await;
    check_transcript();
}

/// The restart path, fail closed: a cold read — no stored actor — whose
/// unfiltered first page leaves the issue connection unfinished drops that
/// page and walks `createdBy` for the actor the answer named. The set it
/// ends with is exactly the actor's open issues — the two dashboards —
/// and the foreign dashboard never joins it.
#[tokio::test]
async fn an_unfiltered_first_page_with_paged_issues_restarts_on_the_actor() {
    let server = fake();
    for number in 1..=2 {
        seed_issue(
            &server,
            number,
            &format!("dashboard {MARKER}"),
            actor_author(),
        );
    }
    seed_issue(&server, 3, &format!("foreign {MARKER}"), foreign_author());
    for number in 4..=104 {
        seed_issue(&server, number, "filler", foreign_author());
    }
    let (cache, config, reads) = reads(server.clone()).await;
    let repo = repository();
    same_values(&reads.legacy, &reads.unfiltered);
    same_values(&reads.legacy, &reads.filtered);
    assert_eq!(reads.legacy.issues.items.len(), 104);
    assert_eq!(reads.unfiltered.issues.items.len(), 2);
    filtered_issues(&reads.legacy, &reads.unfiltered, &reads.legacy.actor);
    filtered_issues(&reads.legacy, &reads.filtered, &reads.legacy.actor);
    same_decisions(&cache, &repo, &config, &reads.legacy, &reads.unfiltered).await;
    same_decisions(&cache, &repo, &config, &reads.legacy, &reads.filtered).await;
    check_transcript();
}

/// 250 branches force three `refs` pages; the continuations must gather
/// exactly what the single document gathered.
#[tokio::test]
async fn two_hundred_and_fifty_branches_decide_alike() {
    let server = fake();
    {
        let mut state = server.state();
        let repo = state.repo_mut();
        for number in 1..=249 {
            let sha = format!("sha{number}");
            repo.refs
                .insert(format!("refs/heads/topic/{number}"), sha.clone());
            repo.commits.insert(
                sha.clone(),
                json!({"sha":sha,"tree":{"sha":"t"},"message":"human","actor":"human"}),
            );
        }
    }
    seed_issue(&server, 1, &format!("dashboard {MARKER}"), actor_author());
    let (cache, config, reads) = reads(server.clone()).await;
    let repo = repository();
    same_values(&reads.legacy, &reads.unfiltered);
    same_values(&reads.legacy, &reads.filtered);
    filtered_issues(&reads.legacy, &reads.filtered, &reads.legacy.actor);
    assert_eq!(reads.legacy.refs.items.len(), 250);
    same_decisions(&cache, &repo, &config, &reads.legacy, &reads.filtered).await;
    check_transcript();
}

/// An unreadable node — an issue node of the Count's missing the fields the
/// projection needs — fails the connection closed through every document.
/// A ref whose pull requests page past the first marks the branch unsettled
/// the same way.
#[tokio::test]
async fn an_unreadable_node_fails_closed_alike() {
    let server = fake();
    seed_issue(&server, 1, &format!("dashboard {MARKER}"), actor_author());
    // The author survives the filter, so the node reaches both readers and
    // fails to decode in both.
    server.state().repo_mut().issues.insert(
        99,
        json!({"id":"broken","state":"OPEN","author":{"__typename":"Bot","login":"patchula"}}),
    );
    server.state().settings.pr_next_page = Some("main".into());
    let (cache, config, reads) = reads(server.clone()).await;
    let repo = repository();
    same_values(&reads.legacy, &reads.unfiltered);
    same_values(&reads.legacy, &reads.filtered);
    assert!(!reads.legacy.issues.complete && !reads.filtered.issues.complete);
    // The unreadable node is the actor's own, so the filtered connection
    // holds it too: same items, same incompleteness.
    assert_eq!(
        format!("{:?}", reads.legacy.issues.items),
        format!("{:?}", reads.filtered.issues.items)
    );
    assert!(!reads.legacy.unsettled.is_empty());
    same_decisions(&cache, &repo, &config, &reads.legacy, &reads.filtered).await;
    check_transcript();
}

/// A stale hint — a login that is not `viewer.login` — costs one first-page
/// read, then the walk retries unfiltered and stores the truth.
#[tokio::test]
async fn a_stale_actor_hint_retries_unfiltered_and_corrects_itself() {
    let server = fake();
    seed_issue(&server, 1, &format!("dashboard {MARKER}"), actor_author());
    seed_issue(&server, 2, "filler", foreign_author());
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let client =
        Client::with_transport("offline", cache.clone(), Default::default(), server.clone())
            .unwrap();
    #[derive(serde::Serialize, serde::Deserialize)]
    struct ActorHint(String);
    #[derive(serde::Serialize)]
    struct ActorScope {
        credential_scope: String,
    }
    impl patchula_cache::State for ActorHint {
        const NAMESPACE: &'static str = "inventory-actor";
        const VERSION: u32 = 1;
        type Scope = ActorScope;
    }
    let scope = ActorScope {
        credential_scope: client.credential_scope().to_string(),
    };
    cache
        .set_state(&scope, &ActorHint("someone-else".into()))
        .await
        .unwrap();
    let config = patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap();
    let repo = repository();
    let inventory = Inventory::read(&client, &cache, &repo, &config)
        .await
        .unwrap();
    // The retry read unfiltered: both issues are in, and the hint now
    // holds the login the server actually answered.
    assert_eq!(inventory.issues.items.len(), 2);
    assert_eq!(
        cache
            .state::<ActorHint>(&scope)
            .await
            .unwrap()
            .map(|hint| hint.0),
        Some("patchula[bot]".into())
    );
    check_transcript();
}
