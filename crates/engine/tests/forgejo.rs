//! Forgejo Actions workflows: bare refs resolve on Forgejo's default action
//! host and URL refs on their own host, both through forge tags, from offline
//! responses written to the Gitea API documentation.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_report::Repository as RepositoryReport;
use patchula_testkit::{
    Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Redactor, Replay,
};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};

fn exchange(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Body,
    response: Value,
    etag: Option<&str>,
) -> Interaction {
    Interaction {
        request: RecordedRequest {
            method: method.into(),
            url: url.into(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).into(), vec![(*v).into()]))
                .collect(),
            body,
        },
        response: RecordedResponse {
            status: 200,
            headers: etag
                .map(|e| [("etag".to_owned(), vec![e.to_owned()])].into())
                .unwrap_or_default(),
            body: Body::Json(response),
        },
    }
}
const GITHUB: &[(&str, &str)] = &[
    ("accept", "application/vnd.github+json"),
    ("x-github-api-version", "2022-11-28"),
];
fn github(path: &str, value: Value) -> Interaction {
    exchange(
        "GET",
        &format!("https://api.github.com/{path}"),
        GITHUB,
        Body::Text("".into()),
        value,
        Some(path),
    )
}
/// The cold reads of demo/app with a config and one Forgejo workflow.
fn repository(config: &str, workflow: &str) -> Vec<Interaction> {
    let mut interactions = vec![exchange(
        "POST",
        "https://api.github.com/graphql",
        &[GITHUB[0], GITHUB[1], ("content-type", "application/json")],
        Body::Json(
            json!({"query":patchula_engine::REPOSITORY_HEAD_QUERY,"variables":{"owner":"demo","name":"app"}}),
        ),
        json!({"data":{"repository":{"isPrivate":false,"defaultBranchRef":{"name":"main","target":{"oid":"head","tree":{"oid":"tree"}}}}}}),
        None,
    )];
    interactions.push(github(
        "repos/demo/app/git/trees/tree?recursive=1",
        json!({"truncated":false,"tree":[
            {"path":"patchula.pkl","type":"blob","sha":"config"},
            {"path":".forgejo/workflows/ci.yml","type":"blob","sha":"workflow"}
        ]}),
    ));
    for (sha, text) in [("config", config), ("workflow", workflow)] {
        interactions.push(github(
            &format!("repos/demo/app/git/blobs/{sha}"),
            json!({"encoding":"base64","content":STANDARD.encode(text)}),
        ));
    }
    interactions
}
fn tags(
    host: &str,
    repo: &str,
    tags: &[(&str, String)],
    etag: &str,
    previous: Option<&str>,
) -> Interaction {
    let mut headers = vec![("accept", "application/json")];
    if let Some(previous) = previous {
        headers.push(("if-none-match", previous));
    }
    exchange(
        "GET",
        &format!("https://{host}/api/v1/repos/{repo}/tags?limit=50&page=1"),
        &headers,
        Body::Text("".into()),
        json!(
            tags.iter()
                .map(|(name, sha)| json!({"name":name,"id":sha,"commit":{"sha":sha}}))
                .collect::<Vec<_>>()
        ),
        Some(etag),
    )
}
async fn engine(dir: &Path, interactions: Vec<Interaction>) -> (Engine, Arc<Replay>) {
    let path = dir.join("spec.json");
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the GitHub and Gitea API documentation".into(),
        interactions,
    };
    patchula_testkit::save(&path, &cassette, &Redactor::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, patchula_testkit::Options::default()).unwrap());
    let cache = Cache::open(dir.join("cache.sqlite"), patchula_cache::Options::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone())
        .unwrap()
        .with_versions_host(false);
    (engine, replay)
}
async fn lookup(engine: &Engine) -> RepositoryReport {
    let mut report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Options::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = report.repositories.remove(0);
    assert!(repo.warnings.is_empty(), "{:?}", repo.warnings);
    repo
}
fn updates(repo: &RepositoryReport, name: &str) -> Vec<(Option<String>, Option<String>)> {
    let deps = &repo.package_files[0].dependencies;
    deps.iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("{name} in {deps:?}"))
        .updates
        .iter()
        .map(|u| (u.new_value.clone(), u.new_digest.clone()))
        .collect()
}

#[tokio::test]
async fn forgejo_workflow_refs_pin_and_update_through_forge_tags() {
    let dir = tempfile::tempdir().unwrap();
    let config = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"github-actions\" }\n";
    let old = "a".repeat(40);
    let workflow = format!(
        "on: push\njobs:\n  test:\n    runs-on: docker\n    steps:\n      - uses: actions/checkout@v7.0.0\n      - uses: https://code.forgejo.org/actions/setup-forgejo@{old} # v3.1.0\n      - uses: actions/cache@main\n"
    );
    let mut interactions = repository(config, &workflow);
    let checkout = "3".repeat(40);
    let forgejo = "0".repeat(40);
    interactions.push(tags(
        "data.forgejo.org",
        "actions/checkout",
        &[
            ("v7", checkout.clone()),
            ("v7.0.1", checkout.clone()),
            ("v7.0.0", "b".repeat(40)),
        ],
        "checkout",
        None,
    ));
    interactions.push(tags(
        "code.forgejo.org",
        "actions/setup-forgejo",
        &[("v3.2.0", forgejo.clone()), ("v3.1.0", old.clone())],
        "setup-forgejo",
        None,
    ));
    let (engine, replay) = engine(dir.path(), interactions).await;
    let repo = lookup(&engine).await;
    assert_eq!(
        updates(&repo, "actions/checkout"),
        [
            (Some("v7.0.0".into()), Some("b".repeat(40))),
            (Some("v7.0.1".into()), Some(checkout))
        ]
    );
    assert_eq!(
        updates(&repo, "code.forgejo.org/actions/setup-forgejo"),
        [(Some("v3.2.0".into()), Some(forgejo))]
    );
    assert!(updates(&repo, "actions/cache").is_empty());
    let sources: Vec<_> = repo.package_files[0]
        .dependencies
        .iter()
        .map(|d| {
            (
                d.package.as_str(),
                d.metadata.as_ref().unwrap().source.as_str(),
            )
        })
        .collect();
    assert_eq!(
        sources,
        [
            ("data.forgejo.org/actions/cache", "forge-tags"),
            ("data.forgejo.org/actions/checkout", "forge-tags"),
            ("code.forgejo.org/actions/setup-forgejo", "forge-tags"),
        ]
    );
    replay.assert_finished().unwrap();
}

#[tokio::test]
async fn forge_pins_follow_a_moved_tag_with_only_digest_updates_and_a_reused_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let config = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"github-actions\" }\nrules {\n  new Rule {\n    select { updates = new Listing { \"major\"; \"minor\"; \"patch\"; \"pin\" } }\n    apply { active = false }\n  }\n}\n";
    let old = "a".repeat(40);
    let workflow = format!(
        "on: push\njobs:\n  test:\n    runs-on: docker\n    steps:\n      - uses: https://code.forgejo.org/actions/setup-forgejo@{old} # v3.1.0\n"
    );
    let mut interactions = repository(config, &workflow);
    let moved = "b".repeat(40);
    let again = "c".repeat(40);
    interactions.push(tags(
        "code.forgejo.org",
        "actions/setup-forgejo",
        &[("v3.2.0", "0".repeat(40)), ("v3.1.0", moved.clone())],
        "first",
        None,
    ));
    // Warm: the tag list is within its release-age wait, but its commits are
    // the pins, so it is revalidated and the tag has moved again.
    interactions.push(github("repos/demo/app", json!({"default_branch":"main"})));
    interactions.push(github(
        "repos/demo/app/branches/main",
        json!({"commit":{"sha":"head","commit":{"tree":{"sha":"tree"}}}}),
    ));
    interactions.push(tags(
        "code.forgejo.org",
        "actions/setup-forgejo",
        &[("v3.2.0", "0".repeat(40)), ("v3.1.0", again.clone())],
        "second",
        Some("first"),
    ));
    let (engine, replay) = engine(dir.path(), interactions).await;
    let name = "code.forgejo.org/actions/setup-forgejo";
    let cold = lookup(&engine).await;
    assert_eq!(updates(&cold, name), [(Some("v3.1.0".into()), Some(moved))]);
    let warm = lookup(&engine).await;
    assert_eq!(updates(&warm, name), [(Some("v3.1.0".into()), Some(again))]);
    replay.assert_finished().unwrap();
}
