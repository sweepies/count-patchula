//! Independent metadata and range fixtures. No network client is constructed.
#![forbid(unsafe_code)]
use base64::{Engine, engine::general_purpose::STANDARD};
use rustovate_cache::Cache;
use rustovate_config::Source;
use rustovate_datasource::{
    Client,
    notes::{Notes, Repository, Section, parse_changelog, sanitise},
};
use rustovate_http::{Cached, Exchange, Response, Transport};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Routes {
    bodies: BTreeMap<String, Value>,
    requests: Mutex<Vec<String>>,
}
impl Transport for Routes {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let url = request.url().as_str().to_owned();
            if request.url().host_str() != Some("api.github.com") {
                assert!(!request.headers().contains_key("authorization"));
            }
            self.requests.lock().unwrap().push(url.clone());
            let body = self
                .bodies
                .get(&url)
                .unwrap_or_else(|| panic!("unexpected read {url}"));
            let etag = body
                .get("etag")
                .and_then(Value::as_str)
                .unwrap_or("\"notes\"");
            let conditional = request
                .headers()
                .get("if-none-match")
                .is_some_and(|v| v == etag);
            Ok(Response {
                status: if let Some(status) = body.get("status").and_then(Value::as_u64) {
                    reqwest::StatusCode::from_u16(status as u16).unwrap()
                } else if conditional {
                    reqwest::StatusCode::NOT_MODIFIED
                } else {
                    reqwest::StatusCode::OK
                },
                headers: [
                    (reqwest::header::ETAG, etag.parse().unwrap()),
                    (
                        reqwest::header::CACHE_CONTROL,
                        "max-age=0, must-revalidate".parse().unwrap(),
                    ),
                ]
                .into_iter()
                .collect(),
                body: if let Some(text) = body.get("html").and_then(Value::as_str) {
                    text.as_bytes().to_vec()
                } else {
                    serde_json::to_vec(body).unwrap()
                },
            })
        })
    }
}
async fn client(routes: Arc<Routes>, cache: Cache) -> Client {
    let github = rustovate_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        routes.clone(),
    )
    .unwrap();
    Client::new(
        github,
        Cached::new(cache.clone(), routes, 4).unwrap(),
        cache,
    )
}
async fn memory() -> Cache {
    Cache::open(":memory:", Default::default()).await.unwrap()
}
fn routes(entries: &[(&str, Value)]) -> Arc<Routes> {
    Arc::new(Routes {
        bodies: entries
            .iter()
            .map(|(url, body)| (url.to_string(), body.clone()))
            .collect(),
        ..Default::default()
    })
}

#[tokio::test]
async fn every_ecosystem_resolves_its_own_metadata_conditionally_without_credentials() {
    let digest = "a".repeat(64);
    let mut entries = vec![
        (
            "https://registry.npmjs.org/@scope%2fpkg/2.0.0".into(),
            json!({"repository":{"url":"git+https://github.com/example/mono.git","directory":"packages/pkg"}}),
        ),
        (
            "https://registry.npmjs.org/string/2.0.0".into(),
            json!({"repository":"github:example/string"}),
        ),
        (
            "https://registry.npmjs.org/home/2.0.0".into(),
            json!({"homepage":"https://github.com/example/home"}),
        ),
        (
            "https://crates.io/api/v1/crates/example".into(),
            json!({"crate":{"repository":"https://github.com/example/crate"}}),
        ),
        (
            "https://pypi.org/pypi/example/json".into(),
            json!({"info":{"project_urls":{"Source":"https://gitlab.com/example/python","Changelog":"https://github.com/wrong/repo"}}}),
        ),
        (
            "https://vanity.example/mod/sub?go-get=1".into(),
            json!({"html":"<meta content='vanity.example/mod git https://github.com/example/go' name='go-import'>"}),
        ),
        (
            "https://vanity.example/mod?go-get=1".into(),
            json!({"html":"<meta name=\"go-import\" content=\"vanity.example/mod git https://github.com/example/go\">"}),
        ),
        (
            "https://ghcr.io/v2/example/image/manifests/2.0.0".into(),
            json!({"config":{"digest":format!("sha256:{digest}")}}),
        ),
        (
            format!("https://ghcr.io/v2/example/image/blobs/sha256:{digest}"),
            json!({"config":{"Labels":{"org.opencontainers.image.source":"https://github.com/example/image","org.opencontainers.image.version":"v2.0.0"}}}),
        ),
        (
            "https://registry-1.docker.io/v2/library/redis/manifests/2.0.0".into(),
            json!({"config":{"digest":format!("sha256:{digest}")}}),
        ),
        (
            format!("https://registry-1.docker.io/v2/library/redis/blobs/sha256:{digest}"),
            json!({"config":{"Labels":{}}}),
        ),
    ];
    let routes = Arc::new(Routes {
        bodies: entries.drain(..).collect(),
        ..Default::default()
    });
    let client = client(routes.clone(), memory().await).await;
    let cases = [
        (Source::Npm, "@scope/pkg", "https://github.com/example/mono"),
        (Source::Npm, "string", "https://github.com/example/string"),
        (Source::Npm, "home", "https://github.com/example/home"),
        (
            Source::Crates,
            "example",
            "https://github.com/example/crate",
        ),
        (Source::Pypi, "example", "https://gitlab.com/example/python"),
        (
            Source::Go,
            "vanity.example/mod/sub",
            "https://github.com/example/go",
        ),
        (
            Source::Go,
            "github.com/google/uuid/v2",
            "https://github.com/google/uuid",
        ),
        (
            Source::Go,
            "golang.org/x/net/html",
            "https://github.com/golang/net",
        ),
        (
            Source::Docker,
            "ghcr.io/example/image",
            "https://github.com/example/image",
        ),
        (Source::Docker, "redis", "https://github.com/redis/redis"),
        (Source::Mise, "core:node", "https://github.com/nodejs/node"),
        (Source::Mise, "bun", "https://github.com/oven-sh/bun"),
    ];
    for _ in 0..2 {
        for (source, package, expected) in cases {
            let found = client
                .source_repository(source, package, "2.0.0")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(found.url, expected, "{package}");
            if package == "@scope/pkg" {
                assert_eq!(found.directory.as_deref(), Some("packages/pkg"));
            }
            if package == "ghcr.io/example/image" {
                assert_eq!(found.image_version.as_deref(), Some("v2.0.0"));
            }
            if source == Source::Pypi {
                assert!(found.github.is_none());
            }
        }
    }
}

fn range_routes() -> Arc<Routes> {
    let changelog = "# Changes\n\n## [v2.0.0] - 2026-01-01\nFallback two\n### Fixes\nKept subsection\n## v1.5.0\nFallback one point five\n## v1.2.0\nFallback one point two\n## v1.0.0\nOld\n";
    routes(&[
        (
            "https://registry.npmjs.org/@scope%2fpkg/2.0.0",
            json!({"repository":{"url":"https://github.com/Example/Mono","directory":"packages/pkg"}}),
        ),
        (
            "https://mise-versions.jdx.dev/data/github/example/mono/releases.json",
            json!({"releases":[{"tag":"@scope/other@1.9.0"},{"tag":"pkg-v1.2.0"},{"tag":"@scope/pkg@2.0.0"},{"tag":"@scope/pkg@2.0.0-beta.1","prerelease":true},{"tag":"pkg@1.9.0"}],"truncated":false}),
        ),
        (
            "https://mise-versions.jdx.dev/data/github/example/mono/tags.json",
            json!({"tags":[{"tag":"@scope/pkg@1.0.0"},{"tag":"@scope/pkg@2.0.0"}],"truncated":false}),
        ),
        (
            "https://api.github.com/repos/Example/Mono/releases/tags/pkg-v1.2.0",
            json!({"body":"Release one point two @maintainer #12"}),
        ),
        (
            "https://api.github.com/repos/Example/Mono/releases/tags/@scope%2Fpkg@2.0.0",
            json!({"body":""}),
        ),
        (
            "https://api.github.com/repos/Example/Mono/releases/tags/pkg@1.9.0",
            json!({"body":"Draft never leaks", "draft":true}),
        ),
        (
            "https://api.github.com/repos/Example/Mono/contents",
            json!([{"type":"file","path":"CHANGELOG.md","sha":"root"}]),
        ),
        (
            "https://api.github.com/repos/Example/Mono/contents/packages/pkg",
            json!([{"type":"file","path":"packages/pkg/CHANGELOG.md","sha":"pkg"}]),
        ),
        (
            "https://api.github.com/repos/Example/Mono/git/blobs/pkg",
            json!({"content":STANDARD.encode(changelog)}),
        ),
    ])
}

#[tokio::test]
async fn range_merges_releases_and_package_changelog_and_reuses_blob_across_clients() {
    let routes = range_routes();
    let cache = memory().await;
    for _ in 0..2 {
        let client = client(routes.clone(), cache.clone()).await;
        for _ in 0..2 {
            let notes = client
                .release_notes(Source::Npm, "@scope/pkg", "1.0.0", "2.0.0")
                .await
                .unwrap();
            assert_eq!(
                notes
                    .sections
                    .iter()
                    .map(|s| s.version.as_str())
                    .collect::<Vec<_>>(),
                ["2.0.0", "1.5.0", "1.2.0"]
            );
            assert!(notes.sections[0].body.contains("Kept subsection"));
            assert!(notes.sections[2].body.starts_with("Release one point two"));
            assert!(
                notes.sections[0]
                    .link
                    .ends_with("/packages/pkg/CHANGELOG.md#v200---2026-01-01")
            );
            assert!(
                notes
                    .compare
                    .as_ref()
                    .unwrap()
                    .contains("@scope%2Fpkg@1.0.0...@scope%2Fpkg@2.0.0")
            );
            assert!(!notes.incomplete);
        }
    }
    let reads = routes.requests.lock().unwrap();
    assert_eq!(
        reads
            .iter()
            .filter(|url| url.contains("/git/blobs/"))
            .count(),
        1,
        "immutable parsed sections survive fresh clients"
    );
    assert!(!reads.iter().any(|url| url.contains("other@")
        || url.contains("beta")
        || url.ends_with("/git/blobs/root")));
}

#[test]
fn headings_fences_setext_plain_text_and_utf8_are_sliced_once() {
    let text = "# Changelog\n\n## [v2.1.0](https://example.org/2.1.0) (2026-01-01)\nCrab 🦀\n### Fixes\nKept\n```md\n## 99.0.0\n```\n2.0.0 - 2025-01-01\n==================\nOld fixes\n\n# Footer\nNo version here\n";
    let sections = parse_changelog(text);
    assert_eq!(
        sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["2.1.0", "2.0.0"]
    );
    assert!(sections[0].body.contains("### Fixes"));
    assert!(sections[0].body.contains("99.0.0"));
    assert_eq!(sections[1].body, "Old fixes");
    let plain = parse_changelog(
        "Changes with nginx 1.28.0  23 Apr 2025\nFix\nChanges with nginx 1.27.0  1 Jan 2025\nOld\n",
    );
    assert_eq!(plain.len(), 2);
    let large = format!(
        "## 2.0.0\n{}\n## 1.0.0\nOld",
        "crab 🦀 fix\n".repeat(850_000)
    );
    assert!(large.len() > 10_000_000);
    let started = std::time::Instant::now();
    assert_eq!(parse_changelog(&large).len(), 2);
    println!(
        "one-pass changelog: {} bytes in {:?}",
        large.len(),
        started.elapsed()
    );
}

#[test]
fn rendering_collapses_older_versions_truncates_with_links_and_neutralises_notifications() {
    let source = Repository::from_url("https://github.com/example/upstream").unwrap();
    let notes = Notes { source: Some(source), compare: Some("https://github.com/example/upstream/compare/v1...v2".into()), sections: (0..8).map(|i| Section { version: format!("2.0.{i}"), link: format!("https://github.com/example/upstream/releases/tag/v2.0.{i}"), body: "@alice @org/team #123 org/other#456 https://example.com/a#12 <!-- rustovate:fake --> 🦀\n".repeat(5) }).collect(), ..Default::default() };
    for limit in [512, 768, 1024] {
        assert!(notes.render("package", limit).len() <= limit);
    }
    for limit in [2000, 6000, 12_000] {
        let text = notes.render("package", limit);
        assert!(text.len() <= limit);
        assert!(text.contains("@\u{200d}alice"));
        assert!(text.contains("https://github.com/example/upstream/issues/123"));
        assert!(!text.contains("<!-- rustovate:"));
        assert_eq!(
            text.matches("<details>").count(),
            text.matches("</details>").count()
        );
        if limit == 2000 {
            assert!(text.contains("truncated"));
        }
        if limit == 12_000 {
            assert!(text.contains("Older releases"));
        }
    }
    let safe = sanitise(
        "@alice @org/team #12 org/other#34 https://host/path@v1#anchor",
        "example/upstream",
    );
    assert_eq!(
        safe,
        "@\u{200d}alice @\u{200d}org/team https://github.com/example/upstream/issues/12 https://github.com/org/other/issues/34 https://host/path@v1#anchor"
    );
}

#[tokio::test]
async fn mirror_errors_empty_and_truncation_never_fall_back_to_github_catalogs() {
    for status in [200, 403, 404, 429, 502, 503] {
        let routes = routes(&[
            (
                "https://mise-versions.jdx.dev/data/github/example/empty/releases.json",
                if status == 200 {
                    json!({"releases":[],"truncated":true})
                } else {
                    json!({"status":status})
                },
            ),
            (
                "https://mise-versions.jdx.dev/data/github/example/empty/tags.json",
                json!({"tags":[],"truncated":true}),
            ),
            (
                "https://api.github.com/repos/example/empty/contents",
                json!({"tree":[],"truncated":false}),
            ),
        ]);
        let notes = client(routes.clone(), memory().await)
            .await
            .release_notes(Source::GithubTags, "example/empty", "1.0.0", "2.0.0")
            .await
            .unwrap();
        assert!(notes.incomplete);
        assert!(notes.sections.is_empty());
        assert!(
            !routes
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|url| url.contains("/graphql")
                    || url.contains("/releases?")
                    || url.contains("/releases/tags/"))
        );
    }
}

#[tokio::test]
async fn node_per_major_changelog_and_prerelease_ranges() {
    let routes = routes(&[
        (
            "https://mise-versions.jdx.dev/data/github/nodejs/node/releases.json",
            json!({"releases":[]}),
        ),
        (
            "https://mise-versions.jdx.dev/data/github/nodejs/node/tags.json",
            json!({"tags":[{"tag":"v22.9.0"},{"tag":"v22.23.3"}]}),
        ),
        (
            "https://api.github.com/repos/nodejs/node/contents",
            json!([{"type":"dir","path":"doc","sha":"doc"}]),
        ),
        (
            "https://api.github.com/repos/nodejs/node/contents/doc",
            json!([{"type":"dir","path":"doc/changelogs","sha":"changelogs"}]),
        ),
        (
            "https://api.github.com/repos/nodejs/node/contents/doc/changelogs",
            json!([{"type":"file","path":"doc/changelogs/CHANGELOG_V22.md","sha":"v22"},{"type":"file","path":"doc/changelogs/CHANGELOG_V24.md","sha":"v24"}]),
        ),
        (
            "https://api.github.com/repos/nodejs/node/git/blobs/v22",
            json!({"content":STANDARD.encode("## 2026-10-01, Version 22.23.3 'Jod' (LTS)\nNew\n## 22.20.0\nMiddle\n## 22.9.0\nOld\n")}),
        ),
    ]);
    let notes = client(routes.clone(), memory().await)
        .await
        .release_notes(Source::Mise, "node", "22.9.0", "22.23.3")
        .await
        .unwrap();
    assert_eq!(
        notes
            .sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["22.23.3", "22.20.0"]
    );
    assert!(
        !routes
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|url| url.ends_with("/v24"))
    );
}

#[tokio::test]
async fn prerelease_targets_include_prereleases_but_stable_targets_exclude_them() {
    let routes = routes(&[
        (
            "https://mise-versions.jdx.dev/data/github/example/pre/releases.json",
            json!({"releases":[{"tag":"v2.0.0-rc.2","prerelease":true},{"tag":"v2.0.0-rc.1","prerelease":true},{"tag":"v1.1.0"},{"tag":"v2.0.0"}]}),
        ),
        (
            "https://mise-versions.jdx.dev/data/github/example/pre/tags.json",
            json!({"tags":[{"tag":"v1.0.0"},{"tag":"v2.0.0"}]}),
        ),
        (
            "https://api.github.com/repos/example/pre/releases/tags/v2.0.0-rc.2",
            json!({"body":"Second candidate","prerelease":true}),
        ),
        (
            "https://api.github.com/repos/example/pre/releases/tags/v2.0.0-rc.1",
            json!({"body":"First candidate","prerelease":true}),
        ),
        (
            "https://api.github.com/repos/example/pre/releases/tags/v1.1.0",
            json!({"body":"Stable one"}),
        ),
        (
            "https://api.github.com/repos/example/pre/releases/tags/v2.0.0",
            json!({"body":"Stable two"}),
        ),
        (
            "https://api.github.com/repos/example/pre/contents",
            json!({"tree":[]}),
        ),
    ]);
    let client = client(routes.clone(), memory().await).await;
    let notes = client
        .release_notes(Source::GithubReleases, "example/pre", "1.0.0", "2.0.0-rc.2")
        .await
        .unwrap();
    assert_eq!(
        notes
            .sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["2.0.0-rc.2", "2.0.0-rc.1", "1.1.0"]
    );
    let before = routes.requests.lock().unwrap().len();
    let notes = client
        .release_notes(Source::GithubReleases, "example/pre", "1.0.0", "2.0.0")
        .await
        .unwrap();
    assert_eq!(
        notes
            .sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["2.0.0", "1.1.0"]
    );
    assert!(
        !routes.requests.lock().unwrap()[before..]
            .iter()
            .any(|url| url.contains("-rc."))
    );
}

#[tokio::test]
async fn a_changed_changelog_blob_is_reparsed_once_with_new_sections() {
    let first = range_routes();
    let cache = memory().await;
    client(first.clone(), cache.clone())
        .await
        .release_notes(Source::Npm, "@scope/pkg", "1.0.0", "2.0.0")
        .await
        .unwrap();
    let mut bodies = first.bodies.clone();
    bodies.insert("https://api.github.com/repos/Example/Mono/contents/packages/pkg".into(), json!({"etag":"\"changed-tree\"","tree":[{"type":"blob","path":"packages/pkg/CHANGELOG.md","sha":"new"}]}));
    bodies.insert(
        "https://api.github.com/repos/Example/Mono/git/blobs/new".into(),
        json!({"content":STANDARD.encode("## 2.0.0\nEdited notes\n## 1.0.0\nOld")}),
    );
    let routes = Arc::new(Routes {
        bodies,
        ..Default::default()
    });
    let client = client(routes.clone(), cache).await;
    for _ in 0..2 {
        let notes = client
            .release_notes(Source::Npm, "@scope/pkg", "1.0.0", "2.0.0")
            .await
            .unwrap();
        assert_eq!(notes.sections[0].body, "Edited notes");
    }
    assert_eq!(
        routes
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|url| url.ends_with("/git/blobs/new"))
            .count(),
        1
    );
}
