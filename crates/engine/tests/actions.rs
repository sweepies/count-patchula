//! GitHub Actions and forge action lookups: refs resolve to commits, a bare
//! SHA pin warns its replacement tag is unknown, a forge ref missing from its
//! catalog never reaches GitHub, and a failed release-catalog batch falls
//! back to per-package reads.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use patchula_cache::Cache;
use patchula_engine::{Engine, Options, REPOSITORY_HEAD_QUERY};
use patchula_http::{Exchange, Response, Transport};
use patchula_update::blob_sha;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// `owner/repo` -> ref -> status and commit sha.
type Commits = BTreeMap<(&'static str, &'static str), (u16, String)>;

const CONFIG: &str =
    "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"github-actions\" }\n";

/// `demo/app`: a config and one workflow; actions' tags, commits and release
/// queries answered per route. Requests are recorded for assertions.
struct Fake {
    files: BTreeMap<&'static str, String>,
    /// `owner/repo` -> tag names; a name absent resolves nothing.
    tags: Mutex<BTreeMap<&'static str, Vec<Value>>>,
    /// `owner/repo` -> ref -> commit sha or status.
    commits: Mutex<Commits>,
    /// Forge tag pages, keyed by request path.
    forge: Mutex<BTreeMap<&'static str, Value>>,
    /// Fails the multi-package releases query so the per-package fallback runs.
    fail_release_batch: std::sync::atomic::AtomicBool,
    /// Package count of each release-catalog query, in order.
    release_queries: Mutex<Vec<usize>>,
    requests: Mutex<Vec<(String, String)>>,
}

impl Fake {
    fn new(files: BTreeMap<&'static str, String>) -> Self {
        Self {
            files,
            tags: Default::default(),
            commits: Default::default(),
            forge: Default::default(),
            fail_release_batch: Default::default(),
            release_queries: Default::default(),
            requests: Default::default(),
        }
    }
    fn tree(&self) -> Value {
        json!({"truncated":false,"tree":self.files.iter().map(|(p, c)| {
            json!({"path":p,"sha":blob_sha(c),"mode":"100644","type":"blob"})
        }).collect::<Vec<_>>()})
    }
    fn answer(&self, status: u16, value: Value) -> Exchange<'static> {
        Box::pin(async move {
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
                stale: None,
            })
        })
    }
}

impl Transport for Fake {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let path = request.url().path().to_owned();
        let body: Value = request
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        self.requests
            .lock()
            .unwrap()
            .push((request.method().to_string(), path.clone()));
        patchula_testkit::transcripts::record(&request, &body);
        if let Some(spec) = path.strip_prefix("/api/v1/repos/") {
            let key = spec.strip_suffix("/tags").unwrap_or(spec);
            return self.answer(
                200,
                self.forge
                    .lock()
                    .unwrap()
                    .get(key)
                    .cloned()
                    .unwrap_or(json!([])),
            );
        }
        if path == "/graphql" {
            let query = body["query"].as_str().unwrap_or_default();
            if query == REPOSITORY_HEAD_QUERY {
                return self.answer(
                    200,
                    json!({"data":{"repository":{"isPrivate":false,"defaultBranchRef":{"name":"main","target":{"oid":"head","tree":{"oid":"tree"}}}}}}),
                );
            }
            if query.contains("... on Blob") {
                let mut repository = serde_json::Map::new();
                for (key, sha) in body["variables"].as_object().unwrap() {
                    let Some(index) = key.strip_prefix('o').filter(|i| i.parse::<usize>().is_ok())
                    else {
                        continue;
                    };
                    let text = self
                        .files
                        .values()
                        .find(|c| blob_sha(c) == sha.as_str().unwrap());
                    repository.insert(
                        format!("b{index}"),
                        text.map_or(
                            Value::Null,
                            |text| json!({"text":text,"isBinary":false,"isTruncated":false}),
                        ),
                    );
                }
                return self.answer(200, json!({"data":{"repository":repository}}));
            }
            if query.contains("FreshTagDigests") {
                let mut data = serde_json::Map::new();
                for (key, value) in body["variables"].as_object().unwrap() {
                    if let Some(index) =
                        key.strip_prefix('e').filter(|i| i.parse::<usize>().is_ok())
                    {
                        let reference = value
                            .as_str()
                            .unwrap()
                            .strip_prefix("refs/tags/")
                            .unwrap()
                            .to_owned();
                        let oid = self
                            .commits
                            .lock()
                            .unwrap()
                            .iter()
                            .find(|((_, r), _)| **r == reference)
                            .map(|(_, (_, sha))| sha.clone())
                            .unwrap_or_default();
                        data.insert(
                            "r0".to_owned(),
                            json!({format!("t{index}"):{"__typename":"Commit","oid":oid}}),
                        );
                    }
                }
                return self.answer(200, json!({"data":data}));
            }
            if query.contains("releases(first:100") {
                let variables = body["variables"].as_object().unwrap();
                self.release_queries
                    .lock()
                    .unwrap()
                    .push(variables.len() / 2);
                if variables.len() > 2
                    && self
                        .fail_release_batch
                        .load(std::sync::atomic::Ordering::Relaxed)
                {
                    return self.answer(200, json!({"errors":[{"message":"batch refused"}]}));
                }
                let package = format!(
                    "{}/{}",
                    variables["o0"].as_str().unwrap(),
                    variables["n0"].as_str().unwrap()
                );
                let nodes: Vec<Value> = self
                    .tags
                    .lock()
                    .unwrap()
                    .get(package.as_str())
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|tag| {
                        json!({"tagName":tag["name"],"publishedAt":"2026-10-01T00:00:00Z","isDraft":false,"isPrerelease":false})
                    })
                    .collect();
                return self.answer(
                    200,
                    json!({"data":{"r0":{"releases":{"nodes":nodes,"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}),
                );
            }
            if query.contains("object(oid:") {
                let mut repository = serde_json::Map::new();
                for (key, value) in body["variables"].as_object().unwrap() {
                    if key.starts_with('s') && value.is_string() {
                        repository.insert(
                            format!("t{}", key.strip_prefix('s').unwrap()),
                            json!({"oid":value,"__typename":"Commit"}),
                        );
                    }
                }
                return self.answer(200, json!({"data":{"repository":repository}}));
            }
            panic!("unexpected GraphQL query {query}");
        }
        if path == "/repos/demo/app/git/trees/tree" {
            return self.answer(200, self.tree());
        }
        if let Some(sha) = path.strip_prefix("/repos/demo/app/git/blobs/") {
            let content = self
                .files
                .values()
                .find(|c| blob_sha(c) == sha)
                .expect("blob the repository has");
            return self.answer(
                200,
                json!({"encoding":"base64","content":STANDARD.encode(content)}),
            );
        }
        if path == "/repos/demo/app" {
            return self.answer(200, json!({"default_branch":"main"}));
        }
        if path == "/repos/demo/app/branches/main" {
            return self.answer(
                200,
                json!({"commit":{"sha":"head","commit":{"tree":{"sha":"tree"}}}}),
            );
        }
        if let Some(package) = path
            .strip_prefix("/repos/")
            .and_then(|p| p.strip_suffix("/git/matching-refs/tags/"))
        {
            if request
                .headers()
                .contains_key(reqwest::header::IF_NONE_MATCH)
            {
                return Box::pin(async move {
                    Ok(Response {
                        status: reqwest::StatusCode::NOT_MODIFIED,
                        headers: Default::default(),
                        body: Vec::new(),
                        stale: None,
                    })
                });
            }
            let refs: Vec<Value> = self
                .tags
                .lock()
                .unwrap()
                .get(package)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|tag| {
                    json!({"ref":format!("refs/tags/{}",tag["name"].as_str().unwrap()),
                        "object":{"sha":tag["sha"].as_str().unwrap_or(""),"type":"commit"}})
                })
                .collect();
            return Box::pin(async move {
                Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers: reqwest::header::HeaderMap::from_iter([(
                        reqwest::header::ETAG,
                        "tags".parse().unwrap(),
                    )]),
                    body: serde_json::to_vec(&refs).unwrap(),
                    stale: None,
                })
            });
        }
        if let Some(package) = path
            .strip_prefix("/repos/")
            .and_then(|p| p.strip_suffix("/tags"))
        {
            if request
                .headers()
                .contains_key(reqwest::header::IF_NONE_MATCH)
            {
                return Box::pin(async move {
                    Ok(Response {
                        status: reqwest::StatusCode::NOT_MODIFIED,
                        headers: Default::default(),
                        body: Vec::new(),
                        stale: None,
                    })
                });
            }
            let tags: Vec<Value> = self
                .tags
                .lock()
                .unwrap()
                .get(package)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|tag| {
                    json!({"name":tag["name"],"commit":{"sha":tag["sha"].as_str().unwrap_or("")}})
                })
                .collect();
            return Box::pin(async move {
                Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers: reqwest::header::HeaderMap::from_iter([(
                        reqwest::header::ETAG,
                        "tags".parse().unwrap(),
                    )]),
                    body: serde_json::to_vec(&tags).unwrap(),
                    stale: None,
                })
            });
        }
        if let Some((package, reference)) = path
            .strip_prefix("/repos/")
            .and_then(|p| p.rsplit_once("/commits/"))
        {
            let found = self
                .commits
                .lock()
                .unwrap()
                .iter()
                .find(|((pkg, r), _)| *pkg == package && *r == reference)
                .map(|(_, v)| v.clone());
            if let Some((status, sha)) = found {
                return self.answer(status, json!({"sha":sha}));
            }
            return self.answer(404, json!({"message":"not found"}));
        }
        panic!("unexpected request {} {path}", request.method());
    }
}

async fn engine(fake: Arc<Fake>) -> Engine {
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    Engine::with_transport("offline-token", cache, 4, fake)
        .unwrap()
        .with_versions_host(false)
}

fn files(workflow: &str) -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("patchula.pkl", CONFIG.to_owned()),
        (".github/workflows/ci.yml", workflow.to_owned()),
    ])
}

fn lookup(report: &patchula_report::Report) -> &patchula_report::Repository {
    &report.repositories[0]
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
    let fake = Arc::new(Fake::new(files(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@main\n",
    )));
    fake.commits
        .lock()
        .unwrap()
        .insert(("actions/checkout", "main"), (200, "c".repeat(40)));
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
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, path)| path == "/repos/actions/checkout/commits/main"),
        "{:?}",
        fake.requests.lock().unwrap()
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
    let fake = Arc::new(Fake::new(files(&format!(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@{pin}\n"
    ))));
    let report = engine(fake)
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
    let fake = Arc::new(Fake::new(files(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: acme/tool@v4.2.1\n",
    )));
    fake.tags.lock().unwrap().insert("acme/tool", vec![]);
    let report = engine(fake)
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
    let fake = Arc::new(Fake::new(files(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@v4.2.1\n",
    )));
    fake.tags.lock().unwrap().insert(
        "actions/checkout",
        vec![
            json!({"name":"v4.2.1","sha":"d".repeat(40)}),
            json!({"name":"v4.3.0","sha":"e".repeat(40)}),
        ],
    );
    fake.commits
        .lock()
        .unwrap()
        .insert(("actions/checkout", "v4.3.0"), (500, String::new()));
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
    let fake = Arc::new(Fake::new(files(
        "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/checkout@v4.2.1\n",
    )));
    fake.tags.lock().unwrap().insert(
        "actions/checkout",
        vec![
            json!({"name":"v4.2.1","sha":"d".repeat(40)}),
            json!({"name":"v4.3.0","sha":"e".repeat(40)}),
        ],
    );
    fake.commits
        .lock()
        .unwrap()
        .insert(("actions/checkout", "v4.2.1"), (200, "d".repeat(40)));
    fake.commits
        .lock()
        .unwrap()
        .insert(("actions/checkout", "v4.3.0"), (200, "e".repeat(40)));
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
    fake.requests.lock().unwrap().clear();
    let report = engine.lookup_at(options(), now).await.unwrap();
    let repo = lookup(&report);
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    // The current ref's commit is a per-ref read; the proposal's tag digest
    // must come from the batched GraphQL read, not REST.
    let requests = fake.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .any(|(_, path)| path == "/repos/actions/checkout/commits/v4.2.1"),
        "{requests:?}"
    );
    assert!(
        !requests
            .iter()
            .any(|(_, path)| path == "/repos/actions/checkout/commits/v4.3.0"),
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
    let fixture = Fake::new(BTreeMap::from([
        ("patchula.pkl", CONFIG.to_owned()),
        (
            ".forgejo/workflows/ci.yml",
            "on: push\njobs:\n  t:\n    steps:\n      - uses: https://forgejo.acme.dev/owner/tool@v1\n"
                .to_owned(),
        ),
    ]));
    fixture.forge.lock().unwrap().insert(
        "owner/tool/tags",
        json!([{"name":"v2","commit":{"sha":"f".repeat(40)}}]),
    );
    let fake = Arc::new(fixture);
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
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(_, path)| path.contains("/commits/")),
        "{:?}",
        fake.requests.lock().unwrap()
    );
    check_transcript();
}

/// A multi-package release-catalog query that fails falls back to one lookup
/// per package rather than failing the repository.
#[tokio::test]
async fn a_failed_release_catalog_batch_falls_back_per_package() {
    let mut fake = Fake::new(BTreeMap::from([
        ("patchula.pkl", CONFIG.to_owned()),
        (
            "package.json",
            "{\"packageManager\":\"bun@1.0.0\",\"dependencies\":{}}".to_owned(),
        ),
        (
            "main.tf",
            "terraform {\n  required_version = \">= 1.5\"\n}\n".to_owned(),
        ),
    ]));
    fake.files.insert(
        "patchula.pkl",
        "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\"; \"terraform\" }\n"
            .to_owned(),
    );
    fake.tags.lock().unwrap().insert(
        "oven-sh/bun",
        vec![json!({"name":"v1.0.0"}), json!({"name":"v1.1.0"})],
    );
    fake.tags.lock().unwrap().insert(
        "hashicorp/terraform",
        vec![json!({"name":"v1.5.0"}), json!({"name":"v1.6.0"})],
    );
    fake.fail_release_batch
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let fake = Arc::new(fake);
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
        *fake.release_queries.lock().unwrap(),
        [2, 1, 1],
        "the failed batch must fall back to one query per package"
    );
    check_transcript();
}
