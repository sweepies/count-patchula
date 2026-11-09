//! Public source evidence. Normal execution is strictly offline replay.
#![forbid(unsafe_code)]
use patchula_cache::Cache;
use patchula_datasource::Client;
use patchula_http::Network;
use patchula_testkit::{Options, Redactor};
use patchula_words::Source;
use std::{path::Path, sync::Arc};

#[tokio::test]
async fn public_note_sources_and_release_range() {
    let recording = patchula_testkit::recording();
    let token = if recording {
        std::env::var(patchula_testkit::GH_TOKEN_VAR).expect("recording requires GH_TOKEN")
    } else {
        "offline-public-token".into()
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/notes/public-notes.json");
    let transport = patchula_testkit::from_env(
        &path,
        Options {
            redactor: Redactor::default().with_literals([token.clone()]),
            ..Options::default()
        },
        || Ok(Arc::new(Network::new()?)),
    )
    .unwrap();
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
    let http = patchula_datasource::cached(cache.clone(), transport, 4).unwrap();
    let client =
        patchula_notes::Client::new(Client::new(github, http, cache).with_versions_host(false));
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
            .source_repository(
                &patchula_config::PackageId::public(source, package),
                version,
            )
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
    for _ in 0..2 {
        let notes = client
            .release_notes(
                &patchula_config::PackageId::public(Source::Go, "github.com/google/uuid"),
                "v1.5.0",
                "v1.6.0",
            )
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
    }
}

#[tokio::test]
async fn public_sandbox_note_ranges() {
    let recording = patchula_testkit::recording();
    let token = if recording {
        std::env::var(patchula_testkit::GH_TOKEN_VAR).expect("recording requires GH_TOKEN")
    } else {
        "offline-public-token".into()
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/notes");
    // One cassette per upstream keeps refreshes and offline failures focused.
    for (source, package, current, targets, fixture) in [
        (
            Source::Docker,
            "redis",
            "7.2.5-alpine",
            vec![
                ("7.4.11-alpine", Some("7.4.11")),
                ("8.10.2-alpine", Some("8.10.2")),
            ],
            "redis",
        ),
        (
            Source::Docker,
            "nginx",
            "1.27.1-alpine",
            vec![("1.31.7-alpine", Some("1.31.7"))],
            "nginx",
        ),
        (
            Source::Docker,
            "postgres",
            "16.4",
            vec![("18.6", None)],
            "postgres",
        ),
        (
            Source::Npm,
            "hono",
            "4.6.0",
            vec![("4.13.13", Some("4.13.13"))],
            "hono",
        ),
        (
            Source::Npm,
            "zod",
            "3.23.8",
            vec![("3.25.76", Some("3.25.76")), ("4.6.7", Some("4.6.7"))],
            "zod",
        ),
        (
            Source::Npm,
            "typescript",
            "5.6.2",
            vec![("5.9.3", None), ("7.0.2", None)],
            "typescript",
        ),
        (
            Source::Mise,
            "node",
            "22.9.0",
            vec![("22.23.3", Some("22.23.3")), ("26.12.0", Some("26.12.0"))],
            "node",
        ),
        (Source::Mise, "go", "1.22", vec![("1.27.2", None)], "go"),
        (
            Source::GithubReleases,
            "jqlang/jq",
            "1.7.1",
            vec![("1.8.2", Some("1.8.2"))],
            "jq",
        ),
        (
            Source::GithubTags,
            "actions/checkout",
            "v4.1.7",
            vec![("v7.0.1", Some("7.0.1"))],
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
        let http = patchula_datasource::cached(cache.clone(), transport, 4).unwrap();
        let client =
            patchula_notes::Client::new(Client::new(github, http, cache).with_versions_host(false));
        for (target, newest) in targets {
            let notes = client
                .release_notes(
                    &patchula_config::PackageId::public(source, package),
                    current,
                    target,
                )
                .await
                .unwrap();
            assert!(notes.source.is_some(), "{package} source");
            assert_eq!(
                notes.sections.first().map(|s| s.version.as_str()),
                newest,
                "{package} {current}..{target} newest section"
            );
            let text = notes.render(package, 12_000);
            assert!(text.len() <= 12_000);
            assert!(!text.trim().is_empty(), "{package} rendered notes");
            if let Some(version) = newest {
                assert!(
                    text.lines()
                        .any(|line| line.starts_with("#### ") && line.contains(version)),
                    "{package} {current}..{target}: no heading for {version}\n{text}"
                );
            }
            if recording {
                std::fs::write(
                    std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
                        .join(format!("patchula-notes-{fixture}-{target}.txt")),
                    text,
                )
                .unwrap();
            }
        }
    }
}
