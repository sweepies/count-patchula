//! Copier templates: newer tags are reported as notices and never become an
//! update, a branch or an edit, from offline responses written to the GitLab
//! and Gitea API documentation.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_testkit::{
    Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Redactor, Replay,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn exchange(url: &str, response: Value) -> Interaction {
    Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: url.into(),
            headers: [("accept".to_owned(), vec!["application/json".to_owned()])].into(),
            body: Body::Text("".into()),
        },
        response: RecordedResponse {
            status: 200,
            headers: Default::default(),
            body: Body::Json(response),
        },
    }
}

#[tokio::test]
async fn copier_templates_become_notices_and_never_branches() {
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree");
    std::fs::create_dir_all(tree.join("web")).unwrap();
    std::fs::create_dir_all(tree.join("other")).unwrap();
    std::fs::write(
        tree.join("patchula.pkl"),
        "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"copier\" }\n",
    )
    .unwrap();
    std::fs::write(
        tree.join(".copier-answers.yml"),
        "_commit: v1.4.0\n_src_path: gl:acme/template\nproject: demo\n",
    )
    .unwrap();
    std::fs::write(
        tree.join("web/.copier-answers.web.yml"),
        "_commit: 0.3.0\n_src_path: https://codeberg.org/acme/web.git\n",
    )
    .unwrap();
    std::fs::write(
        tree.join("other/.copier-answers.yml"),
        "_commit: v1.0.0\n_src_path: https://git.example.com/acme/template.git\n",
    )
    .unwrap();
    let sha = |c: char| c.to_string().repeat(40);
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the GitLab and Gitea API documentation".into(),
        interactions: vec![
            exchange(
                "https://gitlab.com/api/v4/projects/acme%2Ftemplate/repository/tags?per_page=100&page=1",
                json!([
                    {"name":"v2.0.0rc1","created_at":"2026-01-03T00:00:00Z","commit":{"id":sha('d')}},
                    {"name":"v1.6.0","created_at":"2026-01-02T00:00:00Z","commit":{"id":sha('c')}},
                    {"name":"v1.5.0","created_at":"2026-01-01T00:00:00Z","commit":{"id":sha('b')}},
                    {"name":"v1.4.0","created_at":"2025-12-01T00:00:00Z","commit":{"id":sha('a')}}
                ]),
            ),
            exchange(
                "https://codeberg.org/api/v1/repos/acme/web/tags?limit=50&page=1",
                json!([
                    {"name":"0.3.1","id":sha('f'),"commit":{"sha":sha('f')}},
                    {"name":"0.3.0","id":sha('e'),"commit":{"sha":sha('e')}}
                ]),
            ),
        ],
    };
    let path = dir.path().join("spec.json");
    patchula_testkit::save(&path, &cassette, &Redactor::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, patchula_testkit::Options::default()).unwrap());
    let cache = Cache::open(
        dir.path().join("cache.sqlite"),
        patchula_cache::Options::default(),
    )
    .await
    .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone())
        .unwrap()
        .with_versions_host(false);
    let report = engine
        .run_at(
            Options {
                local: Some(tree.clone()),
                ..Options::default()
            },
            true,
            false,
            "2026-02-01T00:00:00Z".parse().unwrap(),
        )
        .await
        .unwrap();
    replay.assert_finished().unwrap();
    let repo = &report.repositories[0];
    assert!(repo.warnings.is_empty(), "{:?}", repo.warnings);
    let notices: Vec<_> = repo
        .notices
        .iter()
        .map(|n| (n.path.as_str(), n.text(), n.compare_url.as_deref()))
        .collect();
    assert_eq!(
        notices,
        [
            (
                ".copier-answers.yml",
                "template gl:acme/template: v1.4.0 → v1.6.0 available".into(),
                Some("https://gitlab.com/acme/template/-/compare/v1.4.0...v1.6.0"),
            ),
            (
                "web/.copier-answers.web.yml",
                "template https://codeberg.org/acme/web.git: 0.3.0 → 0.3.1 available".into(),
                Some("https://codeberg.org/acme/web/compare/0.3.0...0.3.1"),
            ),
        ]
    );
    for file in &repo.package_files {
        for dep in &file.dependencies {
            assert_eq!(dep.manager, "copier");
            assert!(dep.updates.is_empty(), "{dep:?}");
            assert!(dep.metadata.as_ref().unwrap().held_back.is_empty());
        }
    }
    let skipped: Vec<_> = repo.skipped.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(skipped, ["other/.copier-answers.yml"]);
    let actions = report.actions.as_ref().unwrap();
    assert!(actions.branches.is_empty(), "{:?}", actions.branches);
    assert!(actions.pull_requests.is_empty());
}
