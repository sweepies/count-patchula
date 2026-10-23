//! Mise tools through a replayed `mise ls-remote --json`. The listings in
//! `fixtures/mise/listings.json` are what the pinned mise printed; nothing
//! here starts mise or reaches the network.
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::{Client, Error};
use patchula_http::{Cached, Exchange, Response, Transport};
use patchula_mise::Lookup;
use patchula_testkit::MiseReplay;
use std::{path::Path, sync::Arc};

/// Answers the one public registry read the notes tests need.
struct Npm;
impl Transport for Npm {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            assert_eq!(
                request.url().as_str(),
                "https://registry.npmjs.org/prettier/3.9.10"
            );
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: Default::default(),
                body: br#"{"repository":{"type":"git","url":"git+https://github.com/prettier/prettier.git"}}"#.to_vec(),
            })
        })
    }
}

fn replay() -> Arc<MiseReplay> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mise/listings.json");
    Arc::new(MiseReplay::open(&path).unwrap())
}
async fn memory() -> Cache {
    Cache::open(":memory:", Default::default()).await.unwrap()
}
fn client(replay: Option<&Arc<MiseReplay>>, cache: Cache, token: &str) -> Client {
    let transport = Arc::new(Npm);
    let github = patchula_github::Client::with_transport(
        token,
        cache.clone(),
        Default::default(),
        transport.clone(),
    )
    .unwrap();
    let client = Client::new(
        github,
        Cached::new(cache.clone(), transport, 2).unwrap(),
        cache,
    );
    match replay {
        Some(replay) => client.with_mise(Arc::new(Lookup::new(
            replay.clone(),
            "mise".into(),
            "/var/lib/patchula/tools".into(),
            "2026.10.11".into(),
        ))),
        None => client,
    }
}

#[tokio::test]
async fn a_packslip_tool_lists_versions_with_dates_newest_first() {
    let replay = replay();
    let catalog = client(Some(&replay), memory().await, "a-token")
        .lookup(Source::Mise, "packslip:jdx/hk")
        .await
        .unwrap();
    assert_eq!(catalog.versions.len(), 13);
    assert_eq!(catalog.versions[0].version, "2.8.0");
    assert_eq!(catalog.versions[12].version, "1.58.1");
    // mise prints `19:46:14.0Z`; the Count keeps whole-second RFC 3339.
    assert_eq!(
        catalog.versions[0].published_at.as_deref(),
        Some("2026-10-20T19:23:14Z")
    );
    assert!(catalog.dist_tags.is_empty());
    assert_eq!(
        replay.lookups(),
        [[
            "ls-remote",
            "--json",
            "--prerelease",
            "--",
            "packslip:jdx/hk"
        ]]
    );
}

#[tokio::test]
async fn versions_a_backend_cannot_date_stay_undated() {
    let catalog = client(Some(&replay()), memory().await, "a-token")
        .lookup(
            Source::Mise,
            "go:github.com/golangci/golangci-lint/cmd/golangci-lint",
        )
        .await
        .unwrap();
    assert!(catalog.versions.iter().any(|v| v.published_at.is_none()));
    assert!(catalog.versions.iter().any(|v| v.published_at.is_some()));
}

#[tokio::test]
async fn prereleases_stay_in_the_catalog_for_the_policy_to_decide() {
    let source = client(Some(&replay()), memory().await, "a-token");
    // npm reports `prerelease` for these; the Count does not drop them.
    let prettier = source.lookup(Source::Mise, "prettier").await.unwrap();
    for version in ["3.0.0-alpha.0", "3.0.0-alpha.11", "1.11.0-rc.1"] {
        assert!(
            prettier.versions.iter().any(|v| v.version == version),
            "{version}"
        );
    }
    // GitHub-backed tools mark them too, with versions that look stable.
    let jq = source.lookup(Source::Mise, "jq").await.unwrap();
    assert!(jq.versions.iter().any(|v| v.version == "1.7rc2"));
}

#[tokio::test]
async fn node_takes_its_lts_line_from_mise() {
    let replay = replay();
    for name in ["node", "core:node"] {
        let catalog = client(Some(&replay), memory().await, "a-token")
            .lookup(Source::Mise, name)
            .await
            .unwrap();
        assert_eq!(catalog.dist_tags["lts"], "24.22.1", "{name}");
        assert!(catalog.versions.len() > 800);
        // Node publishes a date and no time.
        let newest = &catalog.versions[0];
        assert_eq!(newest.version, "26.12.1");
        assert_eq!(newest.published_at.as_deref(), Some("2026-10-21T00:00:00Z"));
    }
    assert_eq!(
        replay.lookups(),
        [
            vec!["ls-remote", "--json", "--prerelease", "--", "node"],
            vec!["ls-remote", "--json", "--prerelease", "--", "node", "lts"],
            vec!["ls-remote", "--json", "--prerelease", "--", "core:node"],
            vec![
                "ls-remote",
                "--json",
                "--prerelease",
                "--",
                "core:node",
                "lts"
            ],
        ]
    );
}

#[tokio::test]
async fn a_stored_listing_is_reused_until_it_is_scoped_differently_or_retried() {
    let replay = replay();
    let cache = memory().await;
    let lookups = || replay.lookups().len();
    let first = client(Some(&replay), cache.clone(), "a-token");
    first.lookup(Source::Mise, "jq").await.unwrap();
    assert_eq!(lookups(), 1);
    // Another client over the same cache reads what the first stored.
    let again = client(Some(&replay), cache.clone(), "a-token");
    let catalog = again.lookup(Source::Mise, "jq").await.unwrap();
    assert_eq!(lookups(), 1);
    assert_eq!(catalog.versions[0].version, "1.8.2");
    // A manual retry asks mise again.
    again
        .revalidating()
        .lookup(Source::Mise, "jq")
        .await
        .unwrap();
    assert_eq!(lookups(), 2);
    // Without the versions host, mise reads upstream: a separate listing.
    again
        .clone()
        .with_versions_host(false)
        .lookup(Source::Mise, "jq")
        .await
        .unwrap();
    assert_eq!(lookups(), 3);
    let calls = replay.calls();
    let last = calls.last().unwrap();
    assert_eq!(last.env["MISE_USE_VERSIONS_HOST"], "0");
    // What one token may see is not shared with another.
    client(Some(&replay), cache, "another-token")
        .lookup(Source::Mise, "jq")
        .await
        .unwrap();
    assert_eq!(lookups(), 4);
    assert_eq!(
        replay.calls().last().unwrap().env["MISE_GITHUB_TOKEN"],
        "another-token"
    );
}

#[tokio::test]
async fn failed_lookups_are_not_stored() {
    let replay = replay();
    let source = client(Some(&replay), memory().await, "a-token");
    for _ in 0..2 {
        assert!(
            source
                .lookup(Source::Mise, "somethingunknownxyz")
                .await
                .is_err()
        );
    }
    assert_eq!(replay.lookups().len(), 2);
}

#[tokio::test]
async fn lookup_failures_name_the_tool_and_say_why() {
    let replay = replay();
    let source = client(Some(&replay), memory().await, "a-token");
    for (tool, expected) in [
        ("somethingunknownxyz", "not found in mise tool registry"),
        ("aapt2", "none of its backends"),
        ("packslip:packslip.dev", "nothing pins its signer"),
        ("1password", "lists no versions of `1password`"),
        (
            "packslip:github.com/jdx/hk/tools/lint",
            "lists no versions of `packslip:github.com/jdx/hk/tools/lint`",
        ),
    ] {
        let error = source.lookup(Source::Mise, tool).await.unwrap_err();
        assert!(matches!(error, Error::Mise(_)), "{tool}");
        assert!(error.to_string().contains(expected), "{tool}: {error}");
    }
}

#[tokio::test]
async fn a_tool_that_installs_from_a_git_ref_has_no_releases_to_update_to() {
    let error = client(Some(&replay()), memory().await, "a-token")
        .lookup(Source::Mise, "cargo:https://github.com/BurntSushi/ripgrep")
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("no release versions"), "{message}");
    assert!(message.contains("`HEAD`"), "{message}");
}

#[tokio::test]
async fn asdf_and_vfox_tools_are_refused_before_mise_starts() {
    let replay = replay();
    let source = client(Some(&replay), memory().await, "a-token");
    for tool in ["asdf:mise-plugins/asdf-tiny", "vfox:version-fox/vfox-cmake"] {
        let error = source.lookup(Source::Mise, tool).await.unwrap_err();
        assert!(
            error.to_string().contains("runs plugin code"),
            "{tool}: {error}"
        );
        assert!(
            source
                .source_repository(Source::Mise, tool, "1.0.0")
                .await
                .is_err()
        );
    }
    assert!(replay.calls().is_empty());
}

#[tokio::test]
async fn a_client_without_the_tool_store_cannot_look_up_mise_tools() {
    let error = client(None, memory().await, "a-token")
        .lookup(Source::Mise, "jq")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::NoMise));
}

#[tokio::test]
async fn release_notes_find_their_repository_from_the_release_url() {
    let replay = replay();
    let source = client(Some(&replay), memory().await, "a-token");
    let jq = source
        .source_repository(Source::Mise, "jq", "1.8.2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(jq.github.as_deref(), Some("jqlang/jq"));
    let hk = source
        .source_repository(Source::Mise, "packslip:jdx/hk", "2.8.0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hk.github.as_deref(), Some("jdx/hk"));
    // Core tools keep their fixed upstream and need no listing.
    let before = replay.lookups().len();
    for (tool, repo) in [("node", "nodejs/node"), ("core:node", "nodejs/node")] {
        let found = source
            .source_repository(Source::Mise, tool, "26.12.1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.github.as_deref(), Some(repo));
    }
    assert_eq!(replay.lookups().len(), before);
}

#[tokio::test]
async fn release_notes_follow_an_npm_backend_when_the_listing_has_no_release_url() {
    let replay = replay();
    let found = client(Some(&replay), memory().await, "a-token")
        .source_repository(Source::Mise, "prettier", "3.9.10")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.github.as_deref(), Some("prettier/prettier"));
    assert_eq!(
        replay.lookups(),
        [
            vec!["ls-remote", "--json", "--prerelease", "--", "prettier"],
            vec!["tool", "--json", "--", "prettier"],
        ]
    );
}
