//! GitHub Actions and forge action lookups: refs resolve to commits, a bare
//! SHA pin warns its replacement tag is unknown, a forge ref missing from its
//! catalog never reaches GitHub, and a failed release-catalog batch falls
//! back to per-package reads.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_http::{Exchange, Response, Transport};
use patchula_testkit::github::{FakeGitHub, Hosts, Matcher};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const CONFIG: &str =
    "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"github-actions\" }\n";

/// The shared GitHub plus its router; `forge` serves the scripted forge
/// host's tag pages, keyed by `{owner}/{repo}/tags`.
struct Sandbox {
    github: Arc<FakeGitHub>,
    hosts: Arc<Hosts>,
    forge: Arc<Mutex<BTreeMap<&'static str, Value>>>,
}

impl Sandbox {
    fn log(&self) -> Vec<(String, String, Value)> {
        self.hosts.log()
    }
    fn clear_log(&self) {
        self.hosts.clear();
    }
}

impl Transport for Sandbox {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        self.hosts.send(request)
    }
}

/// `demo/app`: `files` on `main` at head `head`; the forge host answers its
/// tag pages from `forge`.
fn sandbox(files: BTreeMap<String, String>) -> Arc<Sandbox> {
    let github = Arc::new(FakeGitHub::new("demo/app"));
    github.seed_files("main", "head", &files, vec![]);
    let hosts = Arc::new(Hosts::new(github.clone()));
    let forge: Arc<Mutex<BTreeMap<&'static str, Value>>> = Default::default();
    let catalog = forge.clone();
    hosts.script("forgejo.acme.dev", move |request, _| {
        let spec = request
            .url()
            .path()
            .strip_prefix("/api/v1/repos/")
            .unwrap_or_default();
        let key = spec.strip_suffix("/tags").unwrap_or(spec);
        let body = catalog
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .unwrap_or(json!([]));
        Response {
            status: reqwest::StatusCode::OK,
            headers: Default::default(),
            body: serde_json::to_vec(&body).unwrap(),
            stale: None,
        }
    });
    Arc::new(Sandbox {
        github,
        hosts,
        forge,
    })
}

/// `{repo}`'s tag catalog lists `name` at `sha`: tags and matching-refs
/// reads serve it, and the release catalog carries the release node.
fn tag(github: &FakeGitHub, repo: &str, name: &str, sha: &str) {
    let mut state = github.state();
    let repo = state.repositories.get_mut(repo).unwrap();
    repo.refs.insert(format!("refs/tags/{name}"), sha.into());
    repo.releases.push(json!({
        "tagName":name,"publishedAt":"2026-10-01T00:00:00Z",
        "isDraft":false,"isPrerelease":false}));
}

/// `commits/{reference}` and `object(expression:)` on `{repo}` resolve
/// `reference` to commit `sha`.
fn commit(github: &FakeGitHub, repo: &str, reference: &str, sha: &str) {
    let mut state = github.state();
    let repo = state.repositories.get_mut(repo).unwrap();
    repo.refs
        .insert(format!("refs/heads/{reference}"), sha.into());
    repo.commits.insert(sha.into(), json!({"sha":sha}));
}

async fn engine(fake: Arc<Sandbox>) -> Engine {
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    Engine::with_transport("offline-token", cache, 4, fake)
        .unwrap()
        .with_versions_host(false)
}

fn files(workflow: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("patchula.pkl".to_owned(), CONFIG.to_owned()),
        (".github/workflows/ci.yml".to_owned(), workflow.to_owned()),
    ])
}

fn lookup(report: &patchula_report::Report) -> &patchula_report::Repository {
    &report.repositories[0]
}

/// Package counts of each release-catalog query, in order — batched
/// variables come in `{o,n}` pairs.
fn release_batch_sizes(fake: &Sandbox) -> Vec<usize> {
    fake.log()
        .iter()
        .filter(|(_, path, body)| {
            path == "/graphql"
                && body["query"]
                    .as_str()
                    .is_some_and(|q| q.contains("releases(first:100"))
        })
        .map(|(_, _, body)| body["variables"].as_object().unwrap().len() / 2)
        .collect()
}

/// Hash this test's recorded requests and compare them with the snapshot;
/// `PATCHULA_UPDATE_TRANSCRIPTS=1` rewrites it instead.
fn check_transcript() {
    let snapshot = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv");
    patchula_testkit::transcripts::check(&snapshot);
}

/// A mutable ref is not a version; its commit is read up front so a digest
/// pin can quote it.
#[tokio::test]
async fn an_action_on_a_branch_ref_resolves_its_commit() {
    let fake = sandbox(files(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@main\n",
    ));
    commit(&fake.github, "actions/checkout", "main", &"c".repeat(40));
    let report = engine(fake.clone())
        .await
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = lookup(&report);
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    assert!(
        fake.log()
            .iter()
            .any(|(_, path, _)| path == "/repos/actions/checkout/commits/main"),
        "{:?}",
        fake.log()
    );
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::FloatingRef);
    assert_eq!(resolution.version, "main");
    assert!(
        dep.updates
            .iter()
            .any(|u| u.new_digest.as_deref() == Some(&"c".repeat(40))),
        "the resolved commit must land in the report: {dep:?}"
    );
    check_transcript();
}

/// A bare SHA pin has no version comment: the replacement tag is unknown and
/// the report warns rather than guessing.
#[tokio::test]
async fn a_sha_pin_without_a_version_comment_warns() {
    let pin = "a".repeat(40);
    let fake = sandbox(files(&format!(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@{pin}\n"
    )));
    let report = engine(fake.clone())
        .await
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = lookup(&report);
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    assert!(
        repo.warnings
            .iter()
            .any(|w| w.contains("SHA pin has no version comment; replacement tag is unknown")),
        "{:?}",
        repo.warnings
    );
    check_transcript();
}

/// A tag catalog that comes back empty is a warning, not an error: version
/// lookup is unavailable for the action.
#[tokio::test]
async fn an_empty_tag_catalog_warns_the_lookup_is_unavailable() {
    let fake = sandbox(files(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: acme/tool@v4.2.1\n",
    ));
    fake.github.add_repository("acme/tool", false);
    let report = engine(fake.clone())
        .await
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = lookup(&report);
    assert!(
        repo.warnings
            .iter()
            .any(|w| w
                .contains("registry returned an empty tag catalog; version lookup is unavailable")),
        "{repo:#?}"
    );
    check_transcript();
}

/// A ref whose commit cannot be resolved reports its digest failure; a
/// proposal whose pin fails keeps no digest. On a reused catalog the refs go
/// through the mutable-tag read, whose misses fall back to REST.
#[tokio::test]
async fn unresolved_and_failed_digests_are_lookup_failures() {
    let fake = sandbox(files(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@v4.2.1\n",
    ));
    tag(&fake.github, "actions/checkout", "v4.2.1", &"d".repeat(40));
    tag(&fake.github, "actions/checkout", "v4.3.0", &"e".repeat(40));
    fake.github
        .on(Matcher::Route {
            method: reqwest::Method::GET,
            path: "/repos/actions/checkout/commits/v4.3.0".into(),
        })
        .respond(500, json!({"message":"boom"}));
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline-token", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let options = || Options {
        repositories: vec!["demo/app".into()],
        ..Default::default()
    };
    let now = chrono::Utc::now();
    engine.lookup_at(options(), now).await.unwrap();
    let report = engine.lookup_at(options(), now).await.unwrap();
    let repo = lookup(&report);
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let messages: Vec<_> = repo
        .lookup_failures
        .iter()
        .map(|f| f.message.clone())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cannot resolve the digest of v4.2.1")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cannot pin v4.3.0 to a digest")),
        "{messages:?}"
    );
    check_transcript();
}

/// On a warm run the catalog is reused, so mutable tag targets are refreshed
/// through the batched GraphQL read rather than per-ref REST calls.
#[tokio::test]
async fn a_warm_run_resolves_mutable_tags_in_one_graphql_read() {
    let fake = sandbox(files(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@v4.2.1\n",
    ));
    tag(&fake.github, "actions/checkout", "v4.2.1", &"d".repeat(40));
    tag(&fake.github, "actions/checkout", "v4.3.0", &"e".repeat(40));
    commit(&fake.github, "actions/checkout", "v4.2.1", &"d".repeat(40));
    commit(&fake.github, "actions/checkout", "v4.3.0", &"e".repeat(40));
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline-token", cache, 4, fake.clone())
        .unwrap()
        .with_versions_host(false);
    let options = || Options {
        repositories: vec!["demo/app".into()],
        ..Default::default()
    };
    let now = chrono::Utc::now();
    engine.lookup_at(options(), now).await.unwrap();
    fake.clear_log();
    let report = engine.lookup_at(options(), now).await.unwrap();
    let repo = lookup(&report);
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    // The current ref's commit and the proposal's tag digest both come from
    // the batched GraphQL read: the shared fake answers every aliased field,
    // so no per-ref REST read is needed.
    let requests = fake.log();
    assert!(
        !requests
            .iter()
            .any(|(_, path, _)| path.contains("/commits/")),
        "{requests:?}"
    );
    let digests: Vec<_> = requests
        .iter()
        .filter(|(_, path, body)| {
            path == "/graphql"
                && body["query"]
                    .as_str()
                    .is_some_and(|q| q.contains("FreshTagDigests"))
        })
        .collect();
    assert_eq!(digests.len(), 1, "{requests:?}");
    let expressions: Vec<_> = digests[0].2["variables"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        expressions.contains(&"refs/tags/v4.2.1") && expressions.contains(&"refs/tags/v4.3.0"),
        "{requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|(_, path, _)| path == "/repos/actions/checkout/commits/v4.3.0"),
        "{requests:?}"
    );
    let dep = &repo.package_files[0].dependencies[0];
    assert!(
        dep.updates
            .iter()
            .any(|u| u.new_digest.as_deref() == Some(&"e".repeat(40))),
        "{dep:?}"
    );
    check_transcript();
}

/// A forge ref absent from its tag catalog is "not found" without ever being
/// sent to GitHub: forge refs resolve only through the forge's own tags.
#[tokio::test]
async fn a_forge_ref_missing_from_its_catalog_is_not_sent_to_github() {
    // The workflow lives in the forgejo path so the bare URL form is a forge
    // action on its own host.
    let fake = sandbox(BTreeMap::from([
        ("patchula.pkl".to_owned(), CONFIG.to_owned()),
        (
            ".forgejo/workflows/ci.yml".to_owned(),
            "on: push\njobs:\n  t:\n    steps:\n      - uses: https://forgejo.acme.dev/owner/tool@v1\n"
                .to_owned(),
        ),
    ]));
    fake.forge.lock().unwrap().insert(
        "owner/tool/tags",
        json!([{"name":"v2","commit":{"sha":"f".repeat(40)}}]),
    );
    let report = engine(fake.clone())
        .await
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = lookup(&report);
    assert!(
        repo.lookup_failures
            .iter()
            .any(|f| f.message.contains("tag v1 not found")),
        "{:?}",
        repo.lookup_failures
    );
    assert!(
        !fake
            .log()
            .iter()
            .any(|(_, path, _)| path.contains("/commits/")),
        "{:?}",
        fake.log()
    );
    check_transcript();
}

/// A multi-package release-catalog query that fails falls back to one lookup
/// per package rather than failing the repository.
#[tokio::test]
async fn a_failed_release_catalog_batch_falls_back_per_package() {
    let mut files = BTreeMap::from([
        (
            "package.json".to_owned(),
            "{\"packageManager\":\"bun@1.0.0\",\"dependencies\":{}}".to_owned(),
        ),
        (
            "main.tf".to_owned(),
            "terraform {\n  required_version = \">= 1.5\"\n}\n".to_owned(),
        ),
    ]);
    files.insert(
        "patchula.pkl".to_owned(),
        "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\"; \"terraform\" }\n"
            .to_owned(),
    );
    let fake = sandbox(files);
    fake.github.add_repository("oven-sh/bun", false);
    fake.github.add_repository("hashicorp/terraform", false);
    for name in ["v1.0.0", "v1.1.0"] {
        tag(&fake.github, "oven-sh/bun", name, "");
    }
    for name in ["v1.5.0", "v1.6.0"] {
        tag(&fake.github, "hashicorp/terraform", name, "");
    }
    fake.github
        .on(Matcher::Graphql("releases"))
        .times(1)
        .respond(200, json!({"errors":[{"message":"batch refused"}]}));
    let report = engine(fake.clone())
        .await
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = lookup(&report);
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    assert!(
        repo.lookup_failures.is_empty(),
        "{:?}",
        repo.lookup_failures
    );
    for (name, version) in [("bun", "v1.1.0"), ("hashicorp/terraform", "1.6.0")] {
        let dep = repo
            .package_files
            .iter()
            .flat_map(|f| &f.dependencies)
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("no {name} dependency: {repo:?}"));
        let reached = dep
            .updates
            .iter()
            .any(|u| u.new_version.as_deref() == Some(version))
            || dep
                .metadata
                .as_ref()
                .and_then(|m| m.resolution.as_ref())
                .is_some_and(|r| r.version == version);
        assert!(
            reached,
            "{name} must reach {version} through the fallback lookups: {dep:?}"
        );
        assert!(
            dep.metadata
                .as_ref()
                .and_then(|m| m.lookup.as_ref())
                .and_then(|l| l.catalog.as_ref())
                .is_some_and(|c| !c.reused),
            "{name} must have been fetched individually: {dep:?}"
        );
    }
    assert_eq!(
        release_batch_sizes(&fake),
        [2, 1, 1],
        "the failed batch must fall back to one query per package"
    );
    check_transcript();
}

/// A commit pin's version comment is the resolved value: the report says
/// `pinned-commit`, and the tag catalog drives the update lanes.
#[tokio::test]
async fn a_commit_pin_with_a_version_comment_reports_pinned_commit() {
    let pin = "b".repeat(40);
    let fake = sandbox(files(&format!(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@{pin} # v4.2.1\n"
    )));
    tag(&fake.github, "actions/checkout", "v4.2.1", &"d".repeat(40));
    tag(&fake.github, "actions/checkout", "v4.3.0", &"e".repeat(40));
    commit(&fake.github, "actions/checkout", "v4.2.1", &"d".repeat(40));
    commit(&fake.github, "actions/checkout", "v4.3.0", &"e".repeat(40));
    let report = engine(fake.clone())
        .await
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = lookup(&report);
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let dep = &repo.package_files[0].dependencies[0];
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(
        resolution.kind,
        patchula_words::ResolutionKind::PinnedCommit
    );
    assert_eq!(resolution.version, "v4.2.1");
    check_transcript();
}

/// A setup action's `with:` tool input resolves through its release catalog
/// (`release-line`); a toolchain constraint resolves through the newest
/// matching release (`supported-range`).
#[tokio::test]
async fn tool_and_toolchain_resolutions_report_their_kinds() {
    let fake = sandbox(BTreeMap::from([
        (
            "patchula.pkl".to_owned(),
            "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"github-actions\"; \"terraform\" }\n"
                .to_owned(),
        ),
        (
            ".github/workflows/ci.yml".to_owned(),
            "on: push\njobs:\n  t:\n    steps:\n      - uses: golangci/golangci-lint-action@v8\n        with:\n          version: 'v2.1.6'\n"
                .to_owned(),
        ),
        (
            "main.tf".to_owned(),
            "terraform {\n  required_version = \">= 1.5\"\n}\n".to_owned(),
        ),
    ]));
    for repo in [
        "golangci/golangci-lint-action",
        "golangci/golangci-lint",
        "hashicorp/terraform",
    ] {
        fake.github.add_repository(repo, false);
    }
    tag(
        &fake.github,
        "golangci/golangci-lint-action",
        "v8.0.0",
        &"f".repeat(40),
    );
    tag(&fake.github, "golangci/golangci-lint", "v2.1.6", "");
    tag(&fake.github, "golangci/golangci-lint", "v2.2.0", "");
    tag(&fake.github, "hashicorp/terraform", "v1.5.0", "");
    tag(&fake.github, "hashicorp/terraform", "v1.6.0", "");
    commit(
        &fake.github,
        "golangci/golangci-lint-action",
        "v8",
        &"f".repeat(40),
    );
    commit(
        &fake.github,
        "golangci/golangci-lint-action",
        "v8.0.0",
        &"f".repeat(40),
    );
    let report = engine(fake.clone())
        .await
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = lookup(&report);
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    let resolution = |name: &str| {
        repo.package_files
            .iter()
            .flat_map(|f| &f.dependencies)
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("no {name} dependency: {repo:?}"))
            .metadata
            .as_ref()
            .and_then(|m| m.resolution.as_ref())
            .unwrap_or_else(|| panic!("no resolution recorded for {name}"))
            .kind
    };
    assert_eq!(
        resolution("golangci/golangci-lint"),
        patchula_words::ResolutionKind::ReleaseLine
    );
    assert_eq!(
        resolution("hashicorp/terraform"),
        patchula_words::ResolutionKind::SupportedRange
    );
    check_transcript();
}

/// A toolchain pin whose catalog cannot re-resolve it is `declared`, not
/// `supported-range`: the resolution kind says how the current value was
/// resolved, not what kind the dependency is.
#[tokio::test]
async fn a_toolchain_pin_without_a_catalog_resolution_reports_declared() {
    let fake = sandbox(BTreeMap::from([
        (
            "patchula.pkl".to_owned(),
            "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"cargo\" }\n"
                .to_owned(),
        ),
        ("rust-toolchain".to_owned(), "1.75.0\n".to_owned()),
    ]));
    let report = engine(fake.clone())
        .await
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = lookup(&report);
    let dep = repo
        .package_files
        .iter()
        .flat_map(|f| &f.dependencies)
        .find(|d| d.name == "rust")
        .unwrap_or_else(|| panic!("no rust toolchain dependency: {repo:?}"));
    let resolution = dep
        .metadata
        .as_ref()
        .and_then(|m| m.resolution.as_ref())
        .unwrap_or_else(|| panic!("no resolution recorded: {dep:?}"));
    assert_eq!(resolution.kind, patchula_words::ResolutionKind::Declared);
    assert_eq!(resolution.version, "1.75.0");
    check_transcript();
}
