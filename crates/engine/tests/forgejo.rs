//! Forgejo Actions workflows: bare refs resolve on Forgejo's default action
//! host and URL refs on their own host, both through forge tags, from offline
//! responses written to the Gitea API documentation.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_testkit::{Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Replay};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

fn exchange(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Body,
    response: Value,
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
            headers: BTreeMap::new(),
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
        Body::Text(String::new()),
        value,
    )
}
fn tags(host: &str, repo: &str, tags: &[(&str, String)]) -> Interaction {
    exchange(
        "GET",
        &format!("https://{host}/api/v1/repos/{repo}/tags?limit=50&page=1"),
        &[("accept", "application/json")],
        Body::Text(String::new()),
        json!(
            tags.iter()
                .map(|(name, sha)| json!({"name":name,"id":sha,"commit":{"sha":sha}}))
                .collect::<Vec<_>>()
        ),
    )
}

#[tokio::test]
async fn forgejo_workflow_refs_pin_and_update_through_forge_tags() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spec.json");
    let config = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"github-actions\" }\n";
    let old = "a".repeat(40);
    let workflow = format!(
        "on: push\njobs:\n  test:\n    runs-on: docker\n    steps:\n      - uses: actions/checkout@v7.0.0\n      - uses: https://code.forgejo.org/actions/setup-forgejo@{old} # v3.1.0\n      - uses: actions/cache@main\n"
    );
    let mut interactions = vec![exchange(
        "POST",
        "https://api.github.com/graphql",
        &[GITHUB[0], GITHUB[1], ("content-type", "application/json")],
        Body::Json(
            json!({"query":patchula_engine::REPOSITORY_HEAD_QUERY,"variables":{"owner":"demo","name":"app"}}),
        ),
        json!({"data":{"repository":{"isPrivate":false,"defaultBranchRef":{"name":"main","target":{"oid":"head","tree":{"oid":"tree"}}}}}}),
    )];
    interactions.push(github(
        "repos/demo/app/git/trees/tree?recursive=1",
        json!({"truncated":false,"tree":[
            {"path":"patchula.pkl","type":"blob","sha":"config"},
            {"path":".forgejo/workflows/ci.yml","type":"blob","sha":"workflow"}
        ]}),
    ));
    for (sha, text) in [("config", config), ("workflow", workflow.as_str())] {
        interactions.push(github(
            &format!("repos/demo/app/git/blobs/{sha}"),
            json!({"encoding":"base64","content":STANDARD.encode(text)}),
        ));
    }
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
    ));
    interactions.push(tags(
        "code.forgejo.org",
        "actions/setup-forgejo",
        &[("v3.2.0", forgejo.clone()), ("v3.1.0", old.clone())],
    ));
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the GitHub and Gitea API documentation".into(),
        interactions,
    };
    patchula_testkit::save(&path, &cassette, &Default::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone())
        .unwrap()
        .with_versions_host(false);
    let report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Options::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert!(repo.warnings.is_empty(), "{:?}", repo.warnings);
    let deps = &repo.package_files[0].dependencies;
    let updates = |name: &str| -> Vec<(Option<String>, Option<String>)> {
        deps.iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("{name} in {deps:?}"))
            .updates
            .iter()
            .map(|u| (u.new_value.clone(), u.new_digest.clone()))
            .collect()
    };
    assert_eq!(
        updates("actions/checkout"),
        [
            (Some("v7.0.0".into()), Some("b".repeat(40))),
            (Some("v7.0.1".into()), Some(checkout))
        ]
    );
    assert_eq!(
        updates("code.forgejo.org/actions/setup-forgejo"),
        [(Some("v3.2.0".into()), Some(forgejo))]
    );
    assert!(updates("actions/cache").is_empty());
    let sources: Vec<_> = deps
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
