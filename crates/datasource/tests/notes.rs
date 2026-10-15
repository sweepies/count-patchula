//! Independent metadata and range fixtures. No network client is constructed.
#![forbid(unsafe_code)]
mod support;
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
    queries: Mutex<Vec<Value>>,
}
impl Transport for Routes {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let url = request.url().as_str().to_owned();
            if request.url().host_str() != Some("api.github.com") {
                assert!(!request.headers().contains_key("authorization"));
            }
            self.requests.lock().unwrap().push(url.clone());
            if request.url().path() == "/graphql" {
                let request: Value =
                    serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
                self.queries.lock().unwrap().push(request.clone());
                let body = support::releases(&request, |repo, tag| {
                    let mut url = reqwest::Url::parse(&format!(
                        "https://api.github.com/repos/{repo}/releases/tags/"
                    ))
                    .unwrap();
                    url.path_segments_mut().unwrap().pop_if_empty().push(tag);
                    self.bodies
                        .iter()
                        .find(|(key, _)| key.eq_ignore_ascii_case(url.as_str()))
                        .map(|(_, body)| body.clone())
                });
                return Ok(Response {
                    status: reqwest::StatusCode::OK,
                    headers: Default::default(),
                    body: serde_json::to_vec(&body).unwrap(),
                });
            }
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
    .with_versions_host(false)
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

#[tokio::test]
async fn publication_batch_deduplicates_overlapping_upstream_reads_and_next_batch_revalidates() {
    let routes = routes(&[
        (
            "https://mise-versions.jdx.dev/data/github/nodejs/node/releases.json",
            json!({"releases":[{"tag":"v22.9.0"},{"tag":"v22.20.0"},{"tag":"v22.23.3"}]}),
        ),
        (
            "https://api.github.com/repos/nodejs/node/releases/tags/v22.20.0",
            json!({"body":"Middle release"}),
        ),
        (
            "https://api.github.com/repos/nodejs/node/releases/tags/v22.23.3",
            json!({"body":"Newest release"}),
        ),
        (
            "https://api.github.com/repos/nodejs/node/contents",
            json!([]),
        ),
    ]);
    let source = client(routes.clone(), memory().await).await;
    let batch = source.notes_batch();
    batch
        .prepare_release_notes(&[
            (
                Source::Mise,
                "node".into(),
                "22.9.0".into(),
                "22.23.3".into(),
            ),
            (
                Source::GithubTags,
                "nodejs/node".into(),
                "22.20.0".into(),
                "22.23.3".into(),
            ),
        ])
        .await;
    let clone = batch.clone();
    let (mise, github) = tokio::join!(
        batch.release_notes(Source::Mise, "node", "22.9.0", "22.23.3"),
        clone.release_notes(Source::GithubTags, "nodejs/node", "22.20.0", "22.23.3")
    );
    assert_eq!(mise.unwrap().sections.len(), 2);
    assert_eq!(github.unwrap().sections.len(), 1);
    let go = batch
        .release_notes(Source::Go, "github.com/nodejs/node", "22.9.0", "22.23.3")
        .await
        .unwrap();
    assert_eq!(go.sections.len(), 2);
    {
        let reads = routes.requests.lock().unwrap();
        assert_eq!(
            reads.len(),
            3,
            "one read per mutable upstream resource, even for sequential PRs: {reads:?}"
        );
        for url in routes
            .bodies
            .keys()
            .filter(|url| !url.contains("/releases/tags/"))
        {
            assert_eq!(reads.iter().filter(|r| *r == url).count(), 1, "{url}");
        }
    }
    source
        .notes_batch()
        .release_notes(Source::Mise, "node", "22.9.0", "22.23.3")
        .await
        .unwrap();
    let reads = routes.requests.lock().unwrap();
    assert_eq!(
        reads.len(),
        6,
        "a later batch must revalidate, even while an old batch is alive"
    );
}

#[tokio::test]
async fn release_body_batches_validate_timestamps_and_fetch_only_edited_bodies_after_reopen() {
    let cache_dir = tempfile::tempdir().unwrap();
    let path = cache_dir.path().join("notes.sqlite");
    let original = range_routes();
    let ranges = vec![
        (
            Source::Npm,
            "@scope/pkg".into(),
            "1.0.0".into(),
            "2.0.0".into(),
        ),
        (
            Source::Npm,
            "@scope/pkg".into(),
            "1.1.0".into(),
            "2.0.0".into(),
        ),
    ];
    for step in 0..7 {
        let mut bodies = original.bodies.clone();
        let url = "https://api.github.com/repos/Example/Mono/releases/tags/pkg-v1.2.0";
        if step >= 2 {
            bodies.insert(
                url.into(),
                json!({"updated_at":"2026-10-16T00:00:00Z","body":"Edited release notes"}),
            );
        }
        if step == 3 {
            bodies.insert(url.into(), json!({"status":404}));
        }
        if step == 5 {
            bodies.insert(url.into(), json!({"status":503}));
        }
        let routes = Arc::new(Routes {
            bodies,
            ..Default::default()
        });
        let cache = Cache::open(&path, Default::default()).await.unwrap();
        let source = client(routes.clone(), cache).await;
        let batch = source.notes_batch();
        batch.prepare_release_notes(&ranges).await;
        for (_, package, current, target) in &ranges {
            let notes = batch
                .release_notes(Source::Npm, package, current, target)
                .await
                .unwrap();
            let section = notes
                .sections
                .iter()
                .find(|s| s.version == "1.2.0")
                .unwrap();
            assert_eq!(
                section.body,
                match step {
                    0 | 1 => "Release one point two @maintainer #12",
                    3 | 5 => "Fallback one point two",
                    _ => "Edited release notes",
                }
            );
        }
        let queries = routes.queries.lock().unwrap();
        assert_eq!(queries.len(), if step == 2 { 2 } else { 1 });
        assert_eq!(
            queries[0]["variables"].as_object().unwrap().len(),
            5,
            "three selected tags plus owner/name"
        );
        assert_eq!(
            queries[0]["query"]
                .as_str()
                .unwrap()
                .matches("body:description")
                .count(),
            if step == 0 { 3 } else { 0 }
        );
        if step == 2 {
            assert_eq!(queries[1]["variables"]["t0"], "pkg-v1.2.0");
            assert!(queries[1]["variables"].get("t1").is_none());
            assert_eq!(
                queries[1]["query"]
                    .as_str()
                    .unwrap()
                    .matches("body:description")
                    .count(),
                1
            );
        }
        assert!(
            !routes
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|url| url.contains("/releases/tags/"))
        );
    }
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
        assert!(text.contains(
            "[example/upstream#123](https://redirect.github.com/example/upstream/issues/123)"
        ));
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
        "@\u{200d}alice @\u{200d}org/team [example/upstream#12](https://redirect.github.com/example/upstream/issues/12) [org/other#34](https://redirect.github.com/org/other/issues/34) https://host/path@v1#anchor"
    );
}

#[test]
fn mention_neutralisation_only_changes_mentions_in_upstream_prose() {
    for text in [
        "@alice",
        "Thanks (@alice),",
        "Thanks @alice.",
        "-@org/team!",
    ] {
        assert_eq!(
            sanitise(text, "example/upstream"),
            text.replace('@', "@\u{200d}")
        );
    }
    // The live jq body (#8) corrupted these code tokens; checkout (#12)
    // corrupted commit pins. Cover each exclusion independently.
    for text in [
        "`@uri` and `@urid` and `@base64d`",
        "``literal ` @alice #1``",
        "`multiline\n@alice #1\ncode`",
        "```sh\n@alice #1 https://github.com/a/b/pull/2\n```\n",
        "~~~~\n@org/team #1\n~~~\n@alice #2\n~~~~\n",
        "    ```\n@alice\n    ```\n",
        "> ~~~\n> @alice #1\n> ~~~\n",
        "- ~~~\n  @alice #1\n  ~~~\n",
        "1. ~~~\n   @alice #1\n   ~~~\n",
        "https://host/path/@alice?email=name@example.org#12",
        "ssh://git@host/org/repo#12 www.example.org/@alice",
        "name@example.org first.last+tag@example.org",
        "package@version actions/checkout@692973e3d937",
        "v4.1.7@692973e3d937 image@sha256:abcdef @sha256:abcdef",
        "@scope/pkg@2.0.0 `@actions/core`",
        "* Bump @actions/core and @actions/tool-cache and Remove uuid",
        "[an @alice label](https://example.org)",
        "\\@alice \\#12",
        "word#12 owner/repo@version",
    ] {
        assert_eq!(sanitise(text, "example/upstream"), text, "{text}");
    }
    assert_eq!(
        sanitise("```\n@code\n```\n@alice", "a/b"),
        "```\n@code\n```\n@\u{200d}alice"
    );
    assert_eq!(
        sanitise("* Bump @actions/core by @org/team", "a/b"),
        "* Bump @actions/core by @\u{200d}org/team"
    );
}

#[test]
fn links_keep_their_labels_and_only_bare_references_gain_readable_labels() {
    let plain = sanitise("[see #123]", "google/uuid");
    assert_eq!(
        plain,
        "[see [google/uuid#123](https://redirect.github.com/google/uuid/issues/123)]"
    );
    for label in [
        "label `[` #123",
        "label `]` #123",
        "label `` ` [ `` #123",
        "label \\` #123",
    ] {
        let link = format!("[{label}](https://github.com/google/uuid/pull/123)");
        assert_eq!(
            sanitise(&link, "google/uuid"),
            link.replace("https://github.com/", "https://redirect.github.com/")
        );
    }
    assert_eq!(
        sanitise(
            "[fix #123]\n\n[fix #123]: https://github.com/google/uuid/pull/123",
            "google/uuid"
        ),
        "[fix #123]\n\n[fix #123]: https://redirect.github.com/google/uuid/pull/123"
    );
    for link in [
        "[anchor](#123)",
        "[ref]: #123\n",
        "[ref]: https://example.org \"@alice #1\"\n",
        "[mail](mailto:user@example.org)",
        "[nested](https://example.org/path(v1)/@alice \"@bob #1\")",
        "[empty]()",
        "[empty]( )",
    ] {
        assert_eq!(sanitise(link, "google/uuid"), link);
    }
    let text = "([#149](https://github.com/google/uuid/issues/149))\n[other/repo#2][ref]\n[ref]: https://github.com/other/repo/pull/2\n[#3](https://example.org/a#3 \"title\") #4 other/repo#5 `#6`";
    assert_eq!(
        sanitise(text, "google/uuid"),
        "([#149](https://redirect.github.com/google/uuid/issues/149))\n[other/repo#2][ref]\n[ref]: https://redirect.github.com/other/repo/pull/2\n[#3](https://example.org/a#3 \"title\") [google/uuid#4](https://redirect.github.com/google/uuid/issues/4) [other/repo#5](https://redirect.github.com/other/repo/issues/5) `#6`"
    );
    let nested = "[**Fix #149** and `code #3`](https://github.com/google/uuid/issues/149)";
    assert_eq!(
        sanitise(nested, "google/uuid"),
        nested.replace("https://github.com/", "https://redirect.github.com/")
    );
}

#[test]
fn upstream_reference_urls_use_githubs_documented_backlink_redirect() {
    for kind in ["issues", "pull", "discussions", "commit", "commits"] {
        let url = format!("https://github.com/other/repo/{kind}/123?x=1#issuecomment-456");
        for text in [
            url.clone(),
            format!("[original #123]({url})"),
            format!("<{url}>"),
        ] {
            let expected = text.replace("https://github.com/", "https://redirect.github.com/");
            assert_eq!(sanitise(&text, "example/upstream"), expected);
            assert_eq!(
                sanitise(&expected, "example/upstream"),
                expected,
                "idempotent"
            );
        }
    }
    for url in [
        "https://github.com/other/repo/compare/v1...v2",
        "https://github.com/other/repo/releases/tag/v2",
        "https://github.com/other/repo/blob/HEAD/CHANGELOG.md",
        "https://github.com/other/repo",
        "https://github.com.evil.example/other/repo/issues/1",
        "https://github.example/other/repo/issues/1",
    ] {
        assert_eq!(sanitise(url, "example/upstream"), url);
    }
}

#[test]
fn generated_note_heading_source_and_comparison_are_never_mention_neutralised() {
    let notes = Notes {
        source: Repository::from_url("https://github.com/example/upstream"),
        compare: Some("https://github.com/example/upstream/compare/pkg@v1...pkg@v2".into()),
        sections: vec![Section {
            version: "2.0.0".into(),
            link: "https://github.com/example/upstream/releases/tag/pkg@v2".into(),
            body: "Thanks @alice".into(),
        }],
        ..Default::default()
    };
    let text = notes.render("@scope/package", 12_000);
    assert!(text.starts_with("### @scope/package\n"));
    assert!(text.contains("compare/pkg@v1...pkg@v2)"));
    assert!(text.contains("releases/tag/pkg@v2)"));
    assert_eq!(text.matches('\u{200d}').count(), 1);
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
async fn publication_batch_deduplicates_failed_note_reads_without_poisoning_the_next_batch() {
    let routes = routes(&[
        (
            "https://mise-versions.jdx.dev/data/github/nodejs/node/releases.json",
            json!({"status":404}),
        ),
        (
            "https://mise-versions.jdx.dev/data/github/nodejs/node/tags.json",
            json!({"status":404}),
        ),
        (
            "https://api.github.com/repos/nodejs/node/contents",
            json!({"status":404}),
        ),
    ]);
    let source = client(routes.clone(), memory().await).await;
    let batch = source.notes_batch();
    for package in ["node", "core:node"] {
        let notes = batch
            .release_notes(Source::Mise, package, "22.9.0", "22.23.3")
            .await
            .unwrap();
        assert!(notes.incomplete);
        assert!(notes.sections.is_empty());
    }
    assert_eq!(routes.requests.lock().unwrap().len(), 3);
    source
        .notes_batch()
        .release_notes(Source::Mise, "node", "22.9.0", "22.23.3")
        .await
        .unwrap();
    assert_eq!(routes.requests.lock().unwrap().len(), 6);
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
