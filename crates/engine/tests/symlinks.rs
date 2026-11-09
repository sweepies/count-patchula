//! A symlink is never read as a file: on GitHub its tree entry is mode
//! `120000`, and the link target's text must not become the file's content.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options, REPOSITORY_HEAD_QUERY};
use patchula_http::{Exchange, Response, Transport};
use patchula_update::blob_sha;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const CONFIG: &str = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\" }\n";

/// The tree `demo/app` serves: a config and a manifest that is a symlink,
/// whose blob is the target's path, as Git stores it.
struct Fake {
    /// Every blob SHA a read asked for.
    asked: Mutex<Vec<String>>,
}

impl Fake {
    fn tree(&self) -> Value {
        let link = blob_sha("base/package.json");
        json!({"truncated":false,"tree":[
            {"path":"patchula.pkl","sha":blob_sha(CONFIG),"type":"blob","mode":"100644"},
            {"path":"package.json","sha":link,"type":"blob","mode":"120000"},
        ]})
    }
    /// The text of the blob `sha`, when it is one this repository has.
    fn content(&self, sha: &str) -> Option<&'static str> {
        if sha == blob_sha(CONFIG) {
            Some(CONFIG)
        } else {
            None
        }
    }
}

impl Transport for Fake {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let path = request.url().path();
        let body: Value = request
            .body()
            .and_then(|b| b.as_bytes())
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        let (status, value) = if path == "/graphql" {
            let query = body["query"].as_str().unwrap_or_default();
            if query == REPOSITORY_HEAD_QUERY {
                (
                    200,
                    json!({"data":{"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base","tree":{"oid":"base-tree"}}}}}}),
                )
            } else if query.contains("... on Blob") {
                // Blobs read in one batch, as their text.
                let mut repository = serde_json::Map::new();
                for (key, sha) in body["variables"].as_object().unwrap() {
                    let Some(index) = key.strip_prefix('o').filter(|i| i.parse::<usize>().is_ok())
                    else {
                        continue;
                    };
                    let sha = sha.as_str().unwrap();
                    self.asked.lock().unwrap().push(sha.to_owned());
                    repository.insert(
                        format!("b{index}"),
                        self.content(sha).map_or(
                            Value::Null,
                            |text| json!({"text":text,"isBinary":false,"isTruncated":false}),
                        ),
                    );
                }
                (200, json!({"data":{"repository":repository}}))
            } else {
                panic!("unexpected GraphQL query {query}");
            }
        } else if path == "/repos/demo/app/git/trees/base-tree" {
            (200, self.tree())
        } else if let Some(sha) = path.strip_prefix("/repos/demo/app/git/blobs/") {
            let content = self.content(sha).expect("blob the repository has");
            (
                200,
                json!({"encoding":"base64","content":base64_encode(content)}),
            )
        } else {
            panic!("unexpected read {path}");
        };
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

fn base64_encode(content: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(content)
}

/// The link's blob is the target's path; the scan must mark the manifest
/// unreadable and never ask for that blob.
#[tokio::test]
async fn a_symlinked_manifest_is_unreadable_on_github() {
    let fake = Arc::new(Fake {
        asked: Mutex::new(Vec::new()),
    });
    let directory = tempfile::tempdir().unwrap();
    let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline-token", cache, 4, fake.clone()).unwrap();
    let report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Default::default()
            },
            chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert_eq!(repo.status, patchula_report::Status::Complete, "{repo:?}");
    assert!(
        repo.warnings
            .iter()
            .any(|w| w == "package.json: skipped: the Count could not read it"),
        "{:?}",
        repo.warnings
    );
    let link = blob_sha("base/package.json");
    assert!(
        !fake.asked.lock().unwrap().contains(&link),
        "the link's blob was read"
    );
}
