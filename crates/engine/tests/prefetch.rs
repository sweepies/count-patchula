//! Lookup hints: a cold run records the mutable GitHub tags and image tags
//! its digest resolution wanted, so the very next run prefetches them beside
//! extraction and its digest stage reads nothing, while a recheck click still
//! reads them again.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options, Progress, Stage};
use patchula_http::{Exchange, Response, Transport};
use patchula_testkit::github::{FakeGitHub, Hosts, State};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const OLD: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const NEW: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";

/// The shared GitHub plus its router, which scripts `registry.example`.
struct Sandbox {
    github: Arc<FakeGitHub>,
    hosts: Arc<Hosts>,
}

impl Sandbox {
    fn log(&self) -> Vec<(String, String, Value)> {
        self.hosts.log()
    }
    /// The registry's manifest reads, as `METHOD path`.
    fn manifest_reads(&self) -> Vec<String> {
        self.log()
            .into_iter()
            .filter(|(_, path, _)| path.contains("/manifests/"))
            .map(|(method, path, _)| format!("{method} {path}"))
            .collect()
    }
    /// The batched mutable-tag reads.
    fn tag_reads(&self) -> usize {
        self.log()
            .iter()
            .filter(|(_, path, body)| {
                path == "/graphql"
                    && body["query"]
                        .as_str()
                        .is_some_and(|q| q.contains("FreshTagDigests"))
            })
            .count()
    }
}

impl Transport for Sandbox {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        self.hosts.send(request)
    }
}

/// Every stage a batch announced, with its total.
#[derive(Default)]
struct Stages(Mutex<Vec<(Stage, usize)>>);

impl Stages {
    /// The total of each digest stage announced since the last call.
    fn take_digests(&self) -> Vec<usize> {
        std::mem::take(&mut *self.0.lock().unwrap())
            .into_iter()
            .filter(|(stage, _)| *stage == Stage::Digests)
            .map(|(_, total)| total)
            .collect()
    }
}

impl Progress for Stages {
    fn stage(&self, stage: Stage, total: usize) {
        self.0.lock().unwrap().push((stage, total));
    }
    fn advance(&self, _: usize) {}
}

/// `demo/app` with a workflow on a mutable action tag and a Dockerfile on a
/// digest-pinned image; the registry lists `1.0` and `1.1`. The actions wait
/// an hour, so the next run may reuse their tag catalog.
fn sandbox() -> Arc<Sandbox> {
    let github = Arc::new(FakeGitHub::new("demo/app"));
    let files = BTreeMap::from([
        (
            "patchula.pkl".to_owned(),
            "amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 1 }\nrules { new Rule { select = new Selector { managers = new Listing { \"dockerfile\" } }; apply = new Overrides { waitHours = 0 } } }\nmanagers = new Listing {\n  \"github-actions\"\n  \"dockerfile\"\n}\n"
                .to_owned(),
        ),
        (
            ".github/workflows/ci.yml".to_owned(),
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@v4.2.1\n"
                .to_owned(),
        ),
        (
            "Dockerfile".to_owned(),
            format!("FROM registry.example/spec/app:1.0@{OLD}\n"),
        ),
    ]);
    github.seed_files("main", "head", &files, vec![]);
    {
        let mut state = github.state();
        let repo = state.repositories.get_mut("actions/checkout").unwrap();
        for (name, sha) in [("v4.2.1", "d".repeat(40)), ("v4.3.0", "e".repeat(40))] {
            repo.refs.insert(format!("refs/tags/{name}"), sha.clone());
            repo.commits.insert(sha.clone(), json!({ "sha": sha }));
            repo.releases.push(json!({
                "tagName":name,"publishedAt":"2026-10-01T00:00:00Z",
                "isDraft":false,"isPrerelease":false}));
        }
        state.repo_mut().checks =
            vec![json!({"__typename":"CheckRun","status":"COMPLETED","conclusion":"SUCCESS"})];
    }
    let hosts = Arc::new(Hosts::new(github.clone()));
    hosts.script("registry.example", |request, _| {
        let path = request.url().path();
        let mut headers = reqwest::header::HeaderMap::new();
        let (status, body) = if path.ends_with("/tags/list") {
            (
                200,
                serde_json::to_vec(&json!({"tags":["1.0","1.1"]})).unwrap(),
            )
        } else if let Some(tag) = path.strip_prefix("/v2/spec/app/manifests/") {
            let digest = if tag == "1.0" { OLD } else { NEW };
            headers.insert("docker-content-digest", digest.parse().unwrap());
            (200, Vec::new())
        } else {
            (404, Vec::new())
        };
        Response {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            headers,
            body,
            stale: None,
        }
    });
    // Release notes for the PR bodies find nothing on the mirror.
    hosts.script("mise-versions.jdx.dev", |_, _| Response {
        status: reqwest::StatusCode::NOT_FOUND,
        headers: Default::default(),
        body: Vec::new(),
        stale: None,
    });
    Arc::new(Sandbox { github, hosts })
}

async fn engine(fake: Arc<Sandbox>, stages: Arc<Stages>) -> (Engine, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline-installation", cache, 4, fake)
        .unwrap()
        .with_versions_host(false)
        .with_progress(stages);
    (engine, directory)
}

async fn run(engine: &Engine, now: chrono::DateTime<chrono::Utc>) -> patchula_report::Report {
    let report = engine
        .run_at(
            Options {
                repositories: vec!["demo/app".into()],
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
        patchula_report::Status::Complete,
        "{report:?}"
    );
    report
}

/// The digests the report pins: each dependency's proposed digests.
fn pinned(report: &patchula_report::Report) -> Vec<String> {
    let mut digests: Vec<_> = report.repositories[0]
        .package_files
        .iter()
        .flat_map(|file| &file.dependencies)
        .flat_map(|dep| &dep.updates)
        .filter_map(|update| update.new_digest.clone())
        .collect();
    digests.sort();
    digests
}

/// Hash this test's recorded requests and compare them with the snapshot;
/// `PATCHULA_UPDATE_TRANSCRIPTS=1` rewrites it instead.
fn check_transcript() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

/// Tick the dashboard issue's `action` checkbox as a person's edit would.
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

/// The cold run resolves the action tags from the catalog it fetched and
/// the image tags in its digest stage; both are hinted, so the first warm
/// run, which reuses the tag catalog, reads them during the prefetch and its
/// digest stage has nothing left to read, and still pins the same digests.
#[tokio::test(start_paused = true)]
async fn a_cold_run_hints_tags_and_images_so_the_first_warm_run_prefetches_them() {
    let fake = sandbox();
    let stages = Arc::new(Stages::default());
    let (engine, _directory) = engine(fake.clone(), stages.clone()).await;
    let now = chrono::Utc::now();
    let cold = run(&engine, now).await;
    let cold_digests = stages.take_digests();
    assert!(
        cold_digests.iter().any(|total| *total > 0),
        "the cold run reads image digests in its digest stage: {cold_digests:?}"
    );
    assert!(
        pinned(&cold).contains(&"e".repeat(40)) && pinned(&cold).contains(&NEW.to_owned()),
        "{cold:?}"
    );
    fake.hosts.clear();
    let warm = run(&engine, now + chrono::Duration::minutes(1)).await;
    assert_eq!(
        stages.take_digests(),
        [0],
        "the warm run's digest stage reads nothing: {:?}",
        fake.log()
    );
    // The mutable tags went out in the one batched prefetch read.
    assert_eq!(fake.tag_reads(), 1, "{:?}", fake.log());
    assert_eq!(pinned(&warm), pinned(&cold));
    check_transcript();
}

/// A recheck click revalidates every dependency: the hinted image and tag
/// digests the prefetch answered are read again in the digest stage.
#[tokio::test(start_paused = true)]
async fn a_recheck_click_reads_prefetched_digests_again() {
    let fake = sandbox();
    let stages = Arc::new(Stages::default());
    let (engine, _directory) = engine(fake.clone(), stages.clone()).await;
    let now = chrono::Utc::now();
    let cold = run(&engine, now).await;
    run(&engine, now + chrono::Duration::minutes(1)).await;
    stages.take_digests();
    tick_issue(&mut fake.github.state(), "recheck");
    fake.hosts.clear();
    let rechecked = run(&engine, now + chrono::Duration::minutes(2)).await;
    let totals = stages.take_digests();
    assert!(
        totals.iter().any(|total| *total > 0),
        "a recheck reads digests again: {totals:?}"
    );
    let manifests = fake.manifest_reads();
    for tag in ["1.0", "1.1"] {
        let read = format!("/v2/spec/app/manifests/{tag}");
        assert!(
            manifests.iter().any(|m| m.ends_with(&read)),
            "the recheck reads {tag} from the registry: {manifests:?}"
        );
    }
    assert!(
        fake.log()
            .iter()
            .any(|(_, path, _)| path.contains("/actions/checkout/git/matching-refs/tags/")),
        "the recheck reads the action's tag refs again: {:?}",
        fake.log()
    );
    assert_eq!(pinned(&rechecked), pinned(&cold));
    check_transcript();
}
