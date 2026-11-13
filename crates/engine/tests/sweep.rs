//! The quiet sweep: a repository whose stored answer stands is skipped
//! without a GitHub request of its own, and each forcing condition — a
//! changed answer, a passed due or the six-hour bound — keeps it running
//! in full.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_http::{Exchange, Transport};
use patchula_testkit::github::{FakeGitHub, Hosts};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

/// `demo/app` behind the shared GitHub router: `files` on `main` at head.
struct Sandbox {
    github: Arc<FakeGitHub>,
    hosts: Arc<Hosts>,
}
impl Transport for Sandbox {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        self.hosts.send(request)
    }
}

fn sandbox(files: BTreeMap<String, String>) -> Arc<Sandbox> {
    let github = Arc::new(FakeGitHub::new("demo/app"));
    github.seed_files("main", "head", &files, vec![]);
    Arc::new(Sandbox {
        hosts: Arc::new(Hosts::new(github.clone())),
        github,
    })
}

async fn engine(fake: Arc<Sandbox>) -> Engine {
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    Engine::with_transport("offline-token", cache, 4, fake)
        .unwrap()
        .with_versions_host(false)
}

/// `{repo}`'s tag catalog lists `name` at `sha` — tags and matching-refs
/// reads serve it and `commits/{name}` resolves it — and the release
/// catalog carries the release node published at `published`.
fn tag(github: &FakeGitHub, repo: &str, name: &str, sha: &str, published: &str) {
    let mut state = github.state();
    let repo = state.repositories.get_mut(repo).unwrap();
    repo.refs.insert(format!("refs/tags/{name}"), sha.into());
    repo.commits.insert(sha.into(), json!({"sha":sha}));
    repo.releases.insert(
        0,
        json!({
            "tagName":name,"publishedAt":published,
            "isDraft":false,"isPrerelease":false}),
    );
}

fn at(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&chrono::Utc)
}
fn options() -> Options {
    Options {
        repositories: vec!["demo/app".into()],
        ..Default::default()
    }
}
fn quiet_set(names: &[&str]) -> std::collections::BTreeSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

/// A repository with one github-actions dependency; `waitHours` holds
/// freshly published releases back.
fn files(config: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("patchula.pkl".into(), config.into()),
        (
            ".github/workflows/ci.yml".into(),
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@v20.0.0\n".into(),
        ),
    ])
}
const WAITING: &str = "amends \"PatchulaConfig.pkl\"\nprs { dashboard = false }\ndefaults { waitHours = 2\n releaseNotes = false }\nmanagers = new Listing { \"github-actions\" }\n";
const NOWAIT: &str = "amends \"PatchulaConfig.pkl\"\nprs { dashboard = false }\ndefaults { waitHours = 0\n releaseNotes = false }\nmanagers = new Listing { \"github-actions\" }\n";
const EMPTY: &str =
    "amends \"PatchulaConfig.pkl\"\nprs { dashboard = false }\nmanagers = new Listing {}\n";

/// Hash this test's recorded requests and compare them with the snapshot;
/// `PATCHULA_UPDATE_TRANSCRIPTS=1` rewrites it instead.
fn check_transcript() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

/// Nothing extracted and nothing pending: the sweep's answer is the stored
/// one, reached without a single request of the repository's own.
#[tokio::test]
async fn an_unchanged_repository_is_quiet_without_a_request() {
    let fake = sandbox(BTreeMap::from([("patchula.pkl".into(), EMPTY.to_owned())]));
    let engine = engine(fake.clone()).await;
    engine
        .run_at(options(), false, false, at("2026-10-15T00:00:00Z"))
        .await
        .unwrap();
    fake.github.state().requests.clear();
    let quiet = engine
        .sweep_quiet(&["demo/app".into()], at("2026-10-15T01:00:00Z"))
        .await
        .unwrap();
    assert_eq!(quiet, quiet_set(&["demo/app"]));
    let requests = fake.github.requests();
    assert!(requests.is_empty(), "{requests:?}");
    check_transcript();
}

/// The bound on skipping: a repository with nothing due still runs in full
/// once its last complete run is six hours old — and stays quiet just
/// before it.
#[tokio::test]
async fn the_six_hour_bound_forces_a_full_run() {
    let fake = sandbox(BTreeMap::from([("patchula.pkl".into(), EMPTY.to_owned())]));
    let engine = engine(fake.clone()).await;
    engine
        .run_at(options(), false, false, at("2026-10-15T00:00:00Z"))
        .await
        .unwrap();
    let quiet = engine
        .sweep_quiet(&["demo/app".into()], at("2026-10-15T05:59:59Z"))
        .await
        .unwrap();
    assert_eq!(quiet, quiet_set(&["demo/app"]));
    let quiet = engine
        .sweep_quiet(&["demo/app".into()], at("2026-10-15T06:00:00Z"))
        .await
        .unwrap();
    assert!(quiet.is_empty());
    check_transcript();
}

/// `waitHours` makes the run's `next_due` the held-back release's
/// eligibility: quiet an instant before it, running at the instant — a
/// release becoming eligible exactly at `next_due` is a full run.
#[tokio::test]
async fn a_held_back_release_runs_exactly_at_its_due() {
    let fake = sandbox(files(WAITING));
    tag(
        &fake.github,
        "actions/checkout",
        "v20.0.0",
        &"d".repeat(40),
        "2026-01-01T00:00:00Z",
    );
    tag(
        &fake.github,
        "actions/checkout",
        "v22.0.0",
        &"e".repeat(40),
        "2026-10-14T23:00:00Z",
    );
    let engine = engine(fake.clone()).await;
    let report = engine
        .run_at(options(), false, false, at("2026-10-15T00:00:00Z"))
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let held: Vec<_> = repo
        .package_files
        .iter()
        .flat_map(|file| &file.dependencies)
        .flat_map(|dep| dep.metadata.iter().flat_map(|metadata| &metadata.held_back))
        .collect();
    assert_eq!(held.len(), 1, "{repo:#?}");
    assert_eq!(held[0].eligible_at, "2026-10-15T01:00:00Z");
    let quiet = engine
        .sweep_quiet(&["demo/app".into()], at("2026-10-15T00:59:59Z"))
        .await
        .unwrap();
    assert_eq!(quiet, quiet_set(&["demo/app"]));
    let quiet = engine
        .sweep_quiet(&["demo/app".into()], at("2026-10-15T01:00:00Z"))
        .await
        .unwrap();
    assert!(quiet.is_empty());
    check_transcript();
}

/// A new release in the catalog changes the answer: the hash differs and
/// the repository runs in full even though nothing about it moved. The
/// config waits on nothing, so the sweep refetches the catalog rather than
/// reusing the stored view — within the reuse window a change is honestly
/// invisible and the repository correctly stays quiet.
#[tokio::test]
async fn a_changed_answer_forces_a_full_run() {
    let fake = sandbox(files(NOWAIT));
    tag(
        &fake.github,
        "actions/checkout",
        "v20.0.0",
        &"d".repeat(40),
        "2026-01-01T00:00:00Z",
    );
    tag(
        &fake.github,
        "actions/checkout",
        "v22.0.0",
        &"e".repeat(40),
        "2026-10-14T23:00:00Z",
    );
    let engine = engine(fake.clone()).await;
    engine
        .run_at(options(), false, false, at("2026-10-15T00:00:00Z"))
        .await
        .unwrap();
    let quiet = engine
        .sweep_quiet(&["demo/app".into()], at("2026-10-15T00:30:00Z"))
        .await
        .unwrap();
    assert_eq!(quiet, quiet_set(&["demo/app"]));
    // Published long ago, it is eligible at once: the update lands in the row.
    tag(
        &fake.github,
        "actions/checkout",
        "v23.0.0",
        &"f".repeat(40),
        "2026-01-02T00:00:00Z",
    );
    let quiet = engine
        .sweep_quiet(&["demo/app".into()], at("2026-10-15T00:30:00Z"))
        .await
        .unwrap();
    assert!(quiet.is_empty());
    check_transcript();
}

/// A repository the sweep has no stored answer for runs in full.
#[tokio::test]
async fn an_unseen_repository_runs() {
    let fake = sandbox(BTreeMap::from([("patchula.pkl".into(), EMPTY.to_owned())]));
    let engine = engine(fake.clone()).await;
    let quiet = engine
        .sweep_quiet(&["demo/app".into()], at("2026-10-15T00:00:00Z"))
        .await
        .unwrap();
    assert!(quiet.is_empty());
    check_transcript();
}
