//! Independent metadata and range fixtures. No network client is constructed.
#![forbid(unsafe_code)]
mod support;
use base64::{Engine, engine::general_purpose::STANDARD};
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::{
    Client,
    notes::{
        Links, Notes, Repository, Section, parse_changelog, parse_go_history, parse_release_sgml,
        sanitise,
    },
};
use patchula_http::{Cached, Exchange, Response, Transport};
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
    not_modified: Mutex<Vec<String>>,
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
            if conditional {
                self.not_modified.lock().unwrap().push(url.clone());
            }
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
    let github = patchula_github::Client::with_transport(
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
    let notes = Notes { source: Some(source), compare: Some("https://github.com/example/upstream/compare/v1...v2".into()), sections: (0..8).map(|i| Section { version: format!("2.0.{i}"), link: format!("https://github.com/example/upstream/releases/tag/v2.0.{i}"), body: "@alice @org/team #123 org/other#456 https://example.com/a#12 <!-- patchula:fake --> 🦀\n".repeat(5) }).collect(), ..Default::default() };
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
        assert!(!text.contains("<!-- patchula:"));
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
        Links::Github("example/upstream"),
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
            sanitise(text, Links::Github("example/upstream")),
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
        assert_eq!(
            sanitise(text, Links::Github("example/upstream")),
            text,
            "{text}"
        );
    }
    assert_eq!(
        sanitise("```\n@code\n```\n@alice", Links::Github("a/b")),
        "```\n@code\n```\n@\u{200d}alice"
    );
    assert_eq!(
        sanitise("* Bump @actions/core by @org/team", Links::Github("a/b")),
        "* Bump @actions/core by @\u{200d}org/team"
    );
}

#[test]
fn links_keep_their_labels_and_only_bare_references_gain_readable_labels() {
    let plain = sanitise("[see #123]", Links::Github("google/uuid"));
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
            sanitise(&link, Links::Github("google/uuid")),
            link.replace("https://github.com/", "https://redirect.github.com/")
        );
    }
    assert_eq!(
        sanitise(
            "[fix #123]\n\n[fix #123]: https://github.com/google/uuid/pull/123",
            Links::Github("google/uuid")
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
        assert_eq!(sanitise(link, Links::Github("google/uuid")), link);
    }
    let text = "([#149](https://github.com/google/uuid/issues/149))\n[other/repo#2][ref]\n[ref]: https://github.com/other/repo/pull/2\n[#3](https://example.org/a#3 \"title\") #4 other/repo#5 `#6`";
    assert_eq!(
        sanitise(text, Links::Github("google/uuid")),
        "([#149](https://redirect.github.com/google/uuid/issues/149))\n[other/repo#2][ref]\n[ref]: https://redirect.github.com/other/repo/pull/2\n[#3](https://example.org/a#3 \"title\") [google/uuid#4](https://redirect.github.com/google/uuid/issues/4) [other/repo#5](https://redirect.github.com/other/repo/issues/5) `#6`"
    );
    let nested = "[**Fix #149** and `code #3`](https://github.com/google/uuid/issues/149)";
    assert_eq!(
        sanitise(nested, Links::Github("google/uuid")),
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
            assert_eq!(sanitise(&text, Links::Github("example/upstream")), expected);
            assert_eq!(
                sanitise(&expected, Links::Github("example/upstream")),
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
        assert_eq!(sanitise(url, Links::Github("example/upstream")), url);
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

/// A hand-written excerpt of the PostgreSQL SGML release notes.
const POSTGRES_SGML: &str = r#"<!-- doc/src/sgml/release-17.sgml -->
<!-- Author: Someone <someone@example.org> -->

 <sect1 id="release-17-6">
  <title>Release 17.6</title>

  <formalpara>
  <title>Release date:</title>
  <para>2025-08-14</para>
  </formalpara>

  <para>
   This release contains a variety of fixes from 17.5 &amp; later.
   See <xref linkend="release-17-5"/> for the previous one.
  </para>

  <sect2 id="release-17-6-migration">
   <title>Migration to Version 17.6</title>

   <para>
    A dump/restore is not required, but BRIN
    <literal>numeric_minmax_multi_ops</literal> indexes need a reindex.
   </para>
  </sect2>

  <sect2 id="release-17-6-changes">
   <title>Changes</title>

   <itemizedlist>

    <listitem>
<!--
Author: Dean Rasheed <dean.a.rasheed@gmail.com>
Branch: REL_17_STABLE [a85eddab2] 2025-08-11 09:09:12 +0100
-->
     <para>
      Tighten security checks in planner estimation functions
      (Dean Rasheed)
      <ulink url="&commit_baseurl;a85eddab2">&sect;</ulink>
     </para>
    </listitem>

    <listitem>
     <para>
      Prevent <application>pg_dump</application> scripts from being used
      to attack the user, and repair <link
      linkend="app-pgbasebackup">backups</link>.
     </para>

     <itemizedlist>
      <listitem>
       <para>
        Nested item, see <ulink url="https://www.postgresql.org/">the
        documentation</ulink>.
       </para>
      </listitem>
     </itemizedlist>
    </listitem>

    <listitem>
     <para>
      Fix a crash in the <function>vacuum</function> function.
     </para>

     <para>
      This is a second paragraph of the same item.
     </para>
    </listitem>
   </itemizedlist>
  </sect2>
 </sect1>

 <sect1 id="release-17">
  <title>Release 17</title>

  <para>
   <productname>PostgreSQL</productname> 17 contains many new features.
  </para>
 </sect1>
"#;

/// A hand-written excerpt of the Go website release history.
const GO_HISTORY: &str = r#"// Copyright 2020 The Go Authors. All rights reserved.

package history

import "html/template"

var Releases = []*Release{
	{
		Date: Date{2026, 10, 8}, Version: Version{1, 27, 2},
		Security: &FixSummary{
			Components: []template.HTML{"the <code>go</code> command"},
			Packages:   []string{"crypto/tls", "net/http"},
		},
		Bug: &FixSummary{
			Components: []template.HTML{"the compiler", "the runtime"},
			Packages:   []string{"encoding/json", "net/http"},
		},
	},
	{
		Date: Date{2026, 9, 1}, Version: Version{1, 27, 1},
		Bug: &FixSummary{
			Components: []template.HTML{"cgo"},
			Packages:   []string{"net/http"},
		},
	},
	{
		Date: Date{2026, 8, 19}, Version: Version{1, 27, 0},
	},
	{
		Date: Date{2019, 4, 11}, Version: Version{1, 12, 4},
		CustomSummary: `fixes an issue where using the prebuilt binary
releases on older versions of GNU/Linux
<a href="/issues/31293">led to failures</a>
when linking programs that used cgo.`,
	},
}
"#;

/// A hand-written excerpt of the Go website release history with links.
const GO_HISTORY_LINKS: &str = r#"package history

import "html/template"

var Releases = []*Release{
	{
		Date: Date{2019, 10, 31}, Version: Version{1, 13, 4},
		Bug: &FixSummary{
			Packages: []string{"net/http", "syscall"},
		},
		More: `It also fixes an issue on macOS 10.15 Catalina
where the non-notarized installer and binaries were being
<a href="/issue/34986">rejected by Gatekeeper</a>.`,
	},
	{
		Date: Date{2019, 10, 31}, Version: Version{1, 12, 13},
		CustomSummary: `fixes an issue on macOS 10.15 Catalina
where the non-notarized installer and binaries were being
<a href="/issue/34986">rejected by Gatekeeper</a>.
Only macOS users who hit this issue need to update.`,
	},
	{
		Date: Date{2018, 12, 14}, Version: Version{1, 10, 7},
		CustomSummary: `includes a fix to a bug introduced in Go 1.10.6
that broke <code>go</code> <code>get</code> for import path patterns containing
"<code>...</code>".
See the <a href="https://github.com/golang/go/issues?q=milestone%3AGo1.10.7
+label%3ACherryPickApproved">
Go 1.10.7 milestone</a> on our issue tracker for details.`,
	},
	{
		Date: Date{2018, 6, 5}, Version: Version{1, 10, 3},
		Bug: &FixSummary{
			Components: []template.HTML{"the go command"},
			Packages:   []string{"crypto/tls", "crypto/x509", "strings"},
		},
		More: `Fixes &amp; improves <code>go</code> builds.`,
	},
}
"#;

#[test]
fn postgres_sgml_becomes_compact_markdown() {
    let sections = parse_release_sgml(POSTGRES_SGML);
    assert_eq!(
        sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["17.6", "17.0"]
    );
    assert_eq!(
        sections[0].link,
        "https://www.postgresql.org/docs/release/17.6/"
    );
    assert_eq!(
        sections[1].link,
        "https://www.postgresql.org/docs/release/17/"
    );
    assert_eq!(
        sections[0].body,
        "This release contains a variety of fixes from 17.5 & later. See release-17-5 for the previous one.\n\n**Migration to Version 17.6**\n\nA dump/restore is not required, but BRIN `numeric_minmax_multi_ops` indexes need a reindex.\n\n**Changes**\n\n- Tighten security checks in planner estimation functions (Dean Rasheed) [§](https://postgr.es/c/a85eddab2)\n- Prevent `pg_dump` scripts from being used to attack the user, and repair backups.\n  - Nested item, see [the documentation](https://www.postgresql.org/).\n- Fix a crash in the `vacuum` function.\n\n  This is a second paragraph of the same item."
    );
    assert!(!sections[0].body.contains("Release date"));
    assert!(!sections[0].body.contains("Author"));
    assert!(
        sections[1]
            .body
            .contains("`PostgreSQL` 17 contains many new features.")
    );
}

#[test]
fn go_release_history_becomes_one_sentence_per_release() {
    let sections = parse_go_history(GO_HISTORY);
    assert_eq!(
        sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["1.27.2", "1.27.1", "1.27.0", "1.12.4"]
    );
    assert_eq!(
        sections[0].link,
        "https://go.dev/doc/devel/release#go1.27.2"
    );
    assert_eq!(
        sections[0].body,
        "go1.27.2 (released 2026-10-08) includes security fixes to the `go` command, and the `crypto/tls` and `net/http` packages, as well as bug fixes to the compiler, the runtime, and the `encoding/json` and `net/http` packages. See the [Go 1.27.2 milestone](https://github.com/golang/go/issues?q=milestone%3AGo1.27.2+label%3ACherryPickApproved) on our issue tracker for details."
    );
    assert_eq!(
        sections[1].body,
        "go1.27.1 (released 2026-09-01) includes fixes to cgo and the `net/http` package. See the [Go 1.27.1 milestone](https://github.com/golang/go/issues?q=milestone%3AGo1.27.1+label%3ACherryPickApproved) on our issue tracker for details."
    );
    assert_eq!(
        sections[2].body,
        "Go 1.27.0 (released 2026-08-19) is a major release of Go. Read the [Go 1.27 Release Notes](https://go.dev/doc/go1.27) for more information."
    );
    assert_eq!(
        sections[2].link,
        "https://go.dev/doc/devel/release#go1.27.0"
    );
    assert_eq!(
        sections[3].body,
        "go1.12.4 (released 2019-04-11) fixes an issue where using the prebuilt binary releases on older versions of GNU/Linux [led to failures](https://go.dev/issues/31293) when linking programs that used cgo."
    );
}

/// Links keep their text, their target and their markup.
#[test]
fn go_links_are_converted_without_garbling_the_text() {
    let sections = parse_go_history(GO_HISTORY_LINKS);
    assert_eq!(
        sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["1.13.4", "1.12.13", "1.10.7", "1.10.3"]
    );
    // A relative link is resolved against the website, and the text around a
    // code span keeps its spaces.
    assert_eq!(
        sections[0].body,
        "go1.13.4 (released 2019-10-31) includes fixes to the `net/http` and `syscall` packages. It also fixes an issue on macOS 10.15 Catalina where the non-notarized installer and binaries were being [rejected by Gatekeeper](https://go.dev/issue/34986). See the [Go 1.13.4 milestone](https://github.com/golang/go/issues?q=milestone%3AGo1.13.4+label%3ACherryPickApproved) on our issue tracker for details."
    );
    assert_eq!(
        sections[1].body,
        "go1.12.13 (released 2019-10-31) fixes an issue on macOS 10.15 Catalina where the non-notarized installer and binaries were being [rejected by Gatekeeper](https://go.dev/issue/34986). Only macOS users who hit this issue need to update."
    );
    // A link whose target is split across lines keeps its target, and the
    // link text may span lines too.
    assert_eq!(
        sections[2].body,
        "go1.10.7 (released 2018-12-14) includes a fix to a bug introduced in Go 1.10.6 that broke `go` `get` for import path patterns containing \"`...`\". See the [Go 1.10.7 milestone](https://github.com/golang/go/issues?q=milestone%3AGo1.10.7+label%3ACherryPickApproved) on our issue tracker for details."
    );
    // An entity before a tag is expanded, and the tag is still a tag.
    assert_eq!(
        sections[3].body,
        "go1.10.3 (released 2018-06-05) includes fixes to the go command, and the `crypto/tls`, `crypto/x509`, and `strings` packages. Fixes & improves `go` builds. See the [Go 1.10.3 milestone](https://github.com/golang/go/issues?q=milestone%3AGo1.10.3+label%3ACherryPickApproved) on our issue tracker for details."
    );
    for section in &sections {
        assert!(!section.body.contains("<"), "{}", section.body);
        assert!(!section.body.contains("](/"), "{}", section.body);
    }
}

fn adapter_routes() -> Arc<Routes> {
    routes(&[
        (
            "https://mise-versions.jdx.dev/data/github/postgres/postgres/releases.json",
            json!({"releases":[]}),
        ),
        (
            "https://mise-versions.jdx.dev/data/github/postgres/postgres/tags.json",
            json!({"tags":[{"tag":"REL_17_2"},{"tag":"REL_17_6"}]}),
        ),
        (
            "https://api.github.com/repos/postgres/postgres/contents",
            json!([]),
        ),
        (
            "https://api.github.com/repos/postgres/postgres/contents/doc/src/sgml?ref=REL_17_6",
            json!([{"type":"file","name":"release-17.sgml","path":"doc/src/sgml/release-17.sgml","sha":"sgml"}]),
        ),
        (
            "https://api.github.com/repos/postgres/postgres/git/blobs/sgml",
            json!({"content":STANDARD.encode(POSTGRES_SGML)}),
        ),
        (
            "https://mise-versions.jdx.dev/data/github/golang/go/releases.json",
            json!({"releases":[]}),
        ),
        (
            "https://mise-versions.jdx.dev/data/github/golang/go/tags.json",
            json!({"tags":[{"tag":"go1.27.0"},{"tag":"go1.27.2"}]}),
        ),
        ("https://api.github.com/repos/golang/go/contents", json!([])),
        (
            "https://api.github.com/repos/golang/website/contents/internal/history",
            json!([{"type":"file","name":"release.go","path":"internal/history/release.go","sha":"history"}]),
        ),
        (
            "https://api.github.com/repos/golang/website/git/blobs/history",
            json!({"content":STANDARD.encode(GO_HISTORY)}),
        ),
    ])
}

#[tokio::test]
async fn upstream_adapters_fill_ranges_without_github_releases() {
    let routes = adapter_routes();
    let client = client(routes.clone(), memory().await).await;
    let postgres = client
        .release_notes(Source::GithubTags, "postgres/postgres", "17.2", "17.6")
        .await
        .unwrap();
    assert_eq!(
        postgres
            .sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["17.6"]
    );
    assert_eq!(
        postgres.sections[0].link,
        "https://www.postgresql.org/docs/release/17.6/"
    );
    assert!(
        postgres.sections[0].body.contains("**Changes**"),
        "{}",
        postgres.sections[0].body
    );
    assert_eq!(
        postgres.compare.as_deref(),
        Some("https://github.com/postgres/postgres/compare/REL_17_2...REL_17_6")
    );
    assert!(!postgres.incomplete);

    let go = client
        .release_notes(Source::GithubTags, "golang/go", "1.27.0", "1.27.2")
        .await
        .unwrap();
    assert_eq!(
        go.sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["1.27.2", "1.27.1"]
    );
    assert_eq!(
        go.sections[0].link,
        "https://go.dev/doc/devel/release#go1.27.2"
    );
    assert!(go.sections[0].body.contains("includes security fixes"));
    assert_eq!(
        go.compare.as_deref(),
        Some("https://github.com/golang/go/compare/go1.27.0...go1.27.2")
    );
    assert!(!go.incomplete);
}

#[tokio::test]
async fn unchanged_adapter_blobs_are_neither_downloaded_nor_parsed_again() {
    let routes = adapter_routes();
    let cache = memory().await;
    for _ in 0..2 {
        let client = client(routes.clone(), cache.clone()).await;
        let notes = client
            .release_notes(Source::GithubTags, "golang/go", "1.27.0", "1.27.2")
            .await
            .unwrap();
        assert_eq!(notes.sections.len(), 2);
    }
    let reads = routes.requests.lock().unwrap();
    assert_eq!(
        reads
            .iter()
            .filter(|url| url.contains("/git/blobs/"))
            .count(),
        1,
        "an unchanged blob is not downloaded again: {reads:?}"
    );
    let conditional = routes.not_modified.lock().unwrap();
    assert!(
        conditional
            .iter()
            .any(|url| url.ends_with("/contents/internal/history")),
        "the listing is revalidated instead of downloaded again"
    );
}

#[tokio::test]
async fn a_range_spanning_postgres_majors_keeps_the_earlier_major_incomplete() {
    let mut bodies = adapter_routes().bodies.clone();
    bodies.insert(
        "https://mise-versions.jdx.dev/data/github/postgres/postgres/tags.json".into(),
        json!({"tags":[{"tag":"REL_16_9"},{"tag":"REL_17_0"}]}),
    );
    bodies.insert(
        "https://api.github.com/repos/postgres/postgres/contents/doc/src/sgml?ref=REL_17_0".into(),
        json!([{"type":"file","name":"release-17.sgml","path":"doc/src/sgml/release-17.sgml","sha":"sgml"}]),
    );
    let routes = Arc::new(Routes {
        bodies,
        ..Default::default()
    });
    let notes = client(routes.clone(), memory().await)
        .await
        .release_notes(Source::GithubTags, "postgres/postgres", "16.9", "17")
        .await
        .unwrap();
    assert_eq!(
        notes
            .sections
            .iter()
            .map(|s| s.version.as_str())
            .collect::<Vec<_>>(),
        ["17.0"]
    );
    assert_eq!(
        notes.sections[0].link,
        "https://www.postgresql.org/docs/release/17/"
    );
    assert!(notes.incomplete, "the earlier major's notes are missing");
    // PostgreSQL tags the major release `REL_17_0`, not `REL_17`.
    let reads = routes.requests.lock().unwrap();
    assert!(
        reads
            .iter()
            .any(|url| url.ends_with("/contents/doc/src/sgml?ref=REL_17_0")),
        "the major release is read at its own tag: {reads:?}"
    );
    assert!(
        !reads
            .iter()
            .any(|url| url.ends_with("/contents/doc/src/sgml?ref=REL_17")),
        "there is no `REL_17` tag: {reads:?}"
    );
}

#[tokio::test]
async fn a_failing_adapter_marks_the_notes_incomplete_without_failing() {
    let mut bodies = adapter_routes().bodies.clone();
    bodies.insert(
        "https://api.github.com/repos/golang/website/contents/internal/history".into(),
        json!({"status":404}),
    );
    let routes = Arc::new(Routes {
        bodies,
        ..Default::default()
    });
    let notes = client(routes.clone(), memory().await)
        .await
        .release_notes(Source::GithubTags, "golang/go", "1.27.0", "1.27.2")
        .await
        .unwrap();
    assert!(notes.sections.is_empty());
    assert!(notes.incomplete);
}

#[test]
fn gitlab_and_gitea_sources_are_recognised_from_their_urls() {
    use patchula_datasource::notes::ForgeKind;
    for (raw, kind, path, web) in [
        (
            "git+https://gitlab.com/grp/sub/proj.git",
            Some(ForgeKind::Gitlab),
            "grp/sub/proj",
            "https://gitlab.com/grp/sub/proj",
        ),
        (
            "https://gitlab.com/grp/proj/-/tree/main",
            Some(ForgeKind::Gitlab),
            "grp/proj",
            "https://gitlab.com/grp/proj",
        ),
        (
            "git@gitlab.com:grp/proj.git",
            Some(ForgeKind::Gitlab),
            "grp/proj",
            "https://gitlab.com/grp/proj",
        ),
        (
            "https://gitlab.example.com/a/b",
            Some(ForgeKind::Gitlab),
            "a/b",
            "https://gitlab.example.com/a/b",
        ),
        (
            "https://codeberg.org/owner/repo/src/branch/main",
            Some(ForgeKind::Gitea),
            "owner/repo",
            "https://codeberg.org/owner/repo",
        ),
        (
            "https://git.example.com/a/b",
            None,
            "",
            "https://git.example.com/a/b",
        ),
    ] {
        let repository = Repository::from_url(raw).unwrap();
        assert_eq!(repository.forge.as_ref().map(|f| f.kind), kind, "{raw}");
        assert_eq!(
            repository.forge.as_ref().map_or("", |f| f.path.as_str()),
            path,
            "{raw}"
        );
        assert_eq!(repository.url, web, "{raw}");
        assert!(repository.github.is_none());
    }
}

#[tokio::test]
async fn gitlab_releases_link_to_gitlab_and_keep_references_on_that_forge() {
    let routes = routes(&[
        (
            "https://registry.npmjs.org/gl-pkg/1.2.0",
            json!({"repository":{"url":"git+https://gitlab.com/grp/sub/proj.git"}}),
        ),
        (
            "https://gitlab.com/api/v4/projects/grp%2Fsub%2Fproj/releases?per_page=100",
            json!([
                {"tag_name":"gl-pkg@2.0.0","description":"Out of range","upcoming_release":false},
                {"tag_name":"other-pkg@1.2.0","description":"Another package","upcoming_release":false},
                {"tag_name":"gl-pkg@1.3.0","description":"Not published yet","upcoming_release":true},
                {"tag_name":"gl-pkg@1.2.0","description":"Fixes #12, thanks @alice","upcoming_release":false},
                {"tag_name":"gl-pkg@1.1.0","description":"Adds things","upcoming_release":false},
                {"tag_name":"gl-pkg@1.0.0","description":"Base","upcoming_release":false}
            ]),
        ),
    ]);
    let client = client(routes.clone(), memory().await).await;
    let notes = client
        .release_notes(Source::Npm, "gl-pkg", "1.0.0", "1.2.0")
        .await
        .unwrap();
    let versions: Vec<_> = notes.sections.iter().map(|s| s.version.as_str()).collect();
    assert_eq!(versions, ["1.2.0", "1.1.0"]);
    assert_eq!(
        notes.sections[0].link,
        "https://gitlab.com/grp/sub/proj/-/releases/gl-pkg@1.2.0"
    );
    assert_eq!(
        notes.compare.as_deref(),
        Some("https://gitlab.com/grp/sub/proj/-/compare/gl-pkg@1.0.0...gl-pkg@1.2.0")
    );
    assert!(!notes.incomplete);
    let text = notes.render("gl-pkg", 12_000);
    assert!(
        text.contains("[grp/sub/proj#12](https://gitlab.com/grp/sub/proj/-/issues/12)"),
        "{text}"
    );
    assert!(text.contains("@\u{200d}alice"), "{text}");
    assert!(!text.contains("redirect.github.com"));
    assert!(
        !routes
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.contains("api.github.com"))
    );
}

#[tokio::test]
async fn gitea_without_release_text_falls_back_to_its_root_changelog() {
    let base = "https://codeberg.org/api/v1/repos/owner/repo";
    let routes = routes(&[
        (
            "https://registry.npmjs.org/cb-pkg/1.2.0",
            json!({"repository":"https://codeberg.org/owner/repo"}),
        ),
        (&format!("{base}/releases?limit=50"), json!([])),
        (
            &format!("{base}/tags?limit=50"),
            json!([{"name":"v1.2.0"},{"name":"v1.1.0"},{"name":"v1.0.0"}]),
        ),
        (
            &format!("{base}/contents"),
            json!([{"name":"src","path":"src","type":"dir"},{"name":"CHANGELOG.md","path":"CHANGELOG.md","type":"file"}]),
        ),
        (
            &format!("{base}/raw/CHANGELOG.md"),
            json!({"html":"# Changelog\n\n## 1.2.0\n\n- Fixed #3\n\n## 1.1.0\n\n- Added x\n\n## 1.0.0\n\n- First\n"}),
        ),
    ]);
    let client = client(routes, memory().await).await;
    let notes = client
        .release_notes(Source::Npm, "cb-pkg", "1.0.0", "1.2.0")
        .await
        .unwrap();
    let versions: Vec<_> = notes.sections.iter().map(|s| s.version.as_str()).collect();
    assert_eq!(versions, ["1.2.0", "1.1.0"]);
    assert_eq!(
        notes.sections[0].link,
        "https://codeberg.org/owner/repo/src/HEAD/CHANGELOG.md#120"
    );
    assert_eq!(
        notes.compare.as_deref(),
        Some("https://codeberg.org/owner/repo/compare/v1.0.0...v1.2.0")
    );
    let text = notes.render("cb-pkg", 12_000);
    assert!(
        text.contains("[owner/repo#3](https://codeberg.org/owner/repo/issues/3)"),
        "{text}"
    );
}

#[tokio::test]
async fn forge_tags_page_newest_first_until_a_short_page() {
    let full: Vec<_> = (0..100)
        .map(|n| json!({"name":format!("v1.{}.0", 200 - n),"created_at":"2026-01-01T00:00:00.000Z","commit":{"id":format!("{n:040}")}}))
        .collect();
    let routes = routes(&[
        (
            "https://gitlab.com/api/v4/projects/grp%2Fhooks/repository/tags?per_page=100&page=1",
            json!(full),
        ),
        (
            "https://gitlab.com/api/v4/projects/grp%2Fhooks/repository/tags?per_page=100&page=2",
            json!([{"name":"v1.0.0","created_at":null,"commit":{"id":"f".repeat(40)}}]),
        ),
        (
            "https://codeberg.org/api/v1/repos/owner/hooks/tags?limit=50&page=1",
            json!([{"name":"v0.3","commit":{"sha":"e".repeat(40)}},{"name":"v0.2","commit":{"sha":"d".repeat(40)}}]),
        ),
    ]);
    let client = client(routes, memory().await).await;
    let gitlab = client
        .lookup(Source::ForgeTags, "gitlab.com/grp/hooks")
        .await
        .unwrap();
    assert_eq!(gitlab.versions.len(), 101);
    assert_eq!(gitlab.versions[0].version, "v1.200.0");
    assert_eq!(
        gitlab.versions[0].published_at.as_deref(),
        Some("2026-01-01T00:00:00.000Z")
    );
    assert_eq!(gitlab.versions[100].published_at, None);
    let gitea = client
        .lookup(Source::ForgeTags, "codeberg.org/owner/hooks")
        .await
        .unwrap();
    assert_eq!(gitea.versions[0].version, "v0.3");
    assert_eq!(
        gitea.versions[0].digest.as_deref(),
        Some("e".repeat(40).as_str())
    );
    assert!(
        client
            .lookup(Source::ForgeTags, "git.example.com/a/b")
            .await
            .is_err()
    );
}
