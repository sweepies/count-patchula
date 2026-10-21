//! Public source evidence. Normal execution is strictly offline replay.
#![forbid(unsafe_code)]
mod support;
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::Client;
use patchula_http::{Cached, Exchange, Network, Response, Transport};
use patchula_testkit::{Options, Redactor};
use std::{path::Path, sync::Arc};

/// Existing captures remain verbatim REST evidence. During offline replay,
/// project their release fields into simulated GraphQL responses; other reads
/// still use fail-closed cassette matching. Recording uses actual GraphQL.
struct ReleaseReplay {
    inner: Arc<dyn Transport>,
    releases: std::collections::BTreeMap<String, serde_json::Value>,
}
impl Transport for ReleaseReplay {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        if request.url().path() != "/graphql" {
            return self.inner.send(request);
        }
        Box::pin(async move {
            let request: serde_json::Value =
                serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
            let body = support::releases(&request, |repo, tag| {
                let mut url = reqwest::Url::parse(&format!(
                    "https://api.github.com/repos/{repo}/releases/tags/"
                ))
                .unwrap();
                url.path_segments_mut().unwrap().pop_if_empty().push(tag);
                self.releases.get(&url.to_string()).cloned()
            });
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: Default::default(),
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    }
}
fn release_replay(inner: Arc<dyn Transport>, path: &Path, recording: bool) -> Arc<dyn Transport> {
    if recording {
        return inner;
    }
    let cassette: patchula_testkit::Cassette =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    if cassette
        .interactions
        .iter()
        .any(|entry| entry.request.url.ends_with("/graphql"))
    {
        // Refreshed cassettes replay real GraphQL without the REST projection.
        return inner;
    }
    let mut releases = std::collections::BTreeMap::new();
    for entry in cassette.interactions {
        if entry.request.url.contains("/releases/tags/")
            && entry.response.status == 200
            && let patchula_testkit::Body::Json(body) = entry.response.body
        {
            releases.entry(entry.request.url).or_insert(body);
        }
    }
    Arc::new(ReleaseReplay { inner, releases })
}

#[tokio::test]
async fn public_note_sources_and_release_range() {
    let recording = std::env::var("PATCHULA_RECORD").as_deref() == Ok("1");
    let token = if recording {
        std::env::var("GH_TOKEN").expect("recording requires GH_TOKEN")
    } else {
        "offline-public-token".into()
    };
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/datasource/public-notes.json");
    let transport = patchula_testkit::from_env(
        &path,
        Options {
            redactor: Redactor::default().with_literals([token.clone()]),
            ..Options::default()
        },
        || Ok(Arc::new(Network::new()?)),
    )
    .unwrap();
    let transport = release_replay(transport, &path, recording);
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let github = patchula_github::Client::with_transport(
        &token,
        cache.clone(),
        Default::default(),
        transport.clone(),
    )
    .unwrap();
    let http = Cached::new(cache.clone(), transport, 4).unwrap();
    let client = Client::new(github.clone(), http.clone(), cache).with_versions_host(false);
    for (source, package, version, repo) in [
        (Source::Npm, "@babel/code-frame", "7.27.1", "babel/babel"),
        (Source::Crates, "heck", "0.5.0", "withoutboats/heck"),
        (Source::Go, "rsc.io/quote", "v1.5.2", "rsc/quote"),
        (Source::Pypi, "idna", "3.10", "kjd/idna"),
        (Source::Docker, "ghcr.io/jqlang/jq", "1.8.1", "jqlang/jq"),
        (Source::Docker, "redis", "7.4.11-alpine", "redis/redis"),
        (Source::Mise, "node", "22.23.3", "nodejs/node"),
    ] {
        let found = client
            .source_repository(source, package, version)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.github.as_deref(), Some(repo), "{package}");
        if source == Source::Npm {
            assert_eq!(
                found.directory.as_deref(),
                Some("packages/babel-code-frame")
            );
        }
        if package == "ghcr.io/jqlang/jq" {
            assert!(found.image_version.is_some());
        }
    }
    for run in 0..2 {
        let before = github.stats().await.requests;
        let started = std::time::Instant::now();
        let notes = client
            .release_notes(Source::Go, "github.com/google/uuid", "v1.5.0", "v1.6.0")
            .await
            .unwrap();
        assert_eq!(
            notes
                .sections
                .iter()
                .map(|s| s.version.as_str())
                .collect::<Vec<_>>(),
            ["1.6.0"]
        );
        assert!(
            notes
                .compare
                .as_ref()
                .unwrap()
                .ends_with("/compare/v1.5.0...v1.6.0")
        );
        let text = notes.render("github.com/google/uuid", 12_000);
        assert!(text.contains("1.6.0"));
        println!(
            "public notes run={run} elapsed={:?} github_requests={} public_requests={}",
            started.elapsed(),
            github.stats().await.requests - before,
            http.stats().requests
        );
    }
}

#[tokio::test]
async fn public_sandbox_note_ranges() {
    let recording = std::env::var("PATCHULA_RECORD").as_deref() == Ok("1");
    let token = if recording {
        std::env::var("GH_TOKEN").expect("recording requires GH_TOKEN")
    } else {
        "offline-public-token".into()
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/datasource");
    // One cassette per upstream keeps refreshes and offline failures focused.
    for (source, package, current, targets, fixture) in [
        (
            Source::Docker,
            "redis",
            "7.2.5-alpine",
            vec!["7.4.11-alpine", "8.10.2-alpine"],
            "redis",
        ),
        (
            Source::Docker,
            "nginx",
            "1.27.1-alpine",
            vec!["1.31.7-alpine"],
            "nginx",
        ),
        (Source::Docker, "postgres", "16.4", vec!["18.6"], "postgres"),
        (Source::Npm, "hono", "4.6.0", vec!["4.13.13"], "hono"),
        (
            Source::Npm,
            "zod",
            "3.23.8",
            vec!["3.25.76", "4.6.7"],
            "zod",
        ),
        (
            Source::Npm,
            "typescript",
            "5.6.2",
            vec!["5.9.3", "7.0.2"],
            "typescript",
        ),
        (
            Source::Mise,
            "node",
            "22.9.0",
            vec!["22.23.3", "26.12.0"],
            "node",
        ),
        (Source::Mise, "go", "1.22", vec!["1.27.2"], "go"),
        (
            Source::GithubReleases,
            "jqlang/jq",
            "1.7.1",
            vec!["1.8.2"],
            "jq",
        ),
        (
            Source::GithubTags,
            "actions/checkout",
            "v4.1.7",
            vec!["v7.0.1"],
            "checkout",
        ),
    ] {
        let path = root.join(format!("notes-{fixture}.json"));
        let transport = patchula_testkit::from_env(
            &path,
            Options {
                redactor: Redactor::default().with_literals([token.clone()]),
                ..Options::default()
            },
            || Ok(Arc::new(Network::new()?)),
        )
        .unwrap();
        let transport = release_replay(transport, &path, recording);
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        let github = patchula_github::Client::with_transport(
            &token,
            cache.clone(),
            Default::default(),
            transport.clone(),
        )
        .unwrap();
        let http = Cached::new(cache.clone(), transport, 4).unwrap();
        let client = Client::new(github.clone(), http.clone(), cache).with_versions_host(false);
        for target in targets {
            let started = std::time::Instant::now();
            let notes = client
                .release_notes(source, package, current, target)
                .await
                .unwrap();
            assert!(notes.source.is_some(), "{package} source");
            let text = notes.render(package, 12_000);
            assert!(text.len() <= 12_000);
            println!(
                "sandbox {package} {current}..{target}: source={} sections={} newest={} compare={} incomplete={} rendered={} elapsed={:?} github_requests={} public_requests={}",
                notes.source.as_ref().unwrap().url,
                notes.sections.len(),
                notes
                    .sections
                    .first()
                    .map_or("none", |s| s.version.as_str()),
                notes.compare.as_deref().unwrap_or("none"),
                notes.incomplete,
                text.len(),
                started.elapsed(),
                github.stats().await.requests,
                http.stats().requests
            );
            if recording {
                std::fs::write(format!("/tmp/patchula-notes-{fixture}-{target}.txt"), text)
                    .unwrap();
            }
        }
    }
}
