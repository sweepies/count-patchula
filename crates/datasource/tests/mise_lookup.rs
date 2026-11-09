//! Mise tools through a replayed `mise ls-remote --json`. The listings in
//! `fixtures/mise/listings.json` are what the pinned mise printed; nothing
//! here starts mise or reaches the network.
use patchula_cache::Cache;
use patchula_datasource::{Client, Error};
use patchula_http::{Exchange, Response, Transport};
use patchula_mise::Lookup;
use patchula_testkit::{MiseReplay, Outcome, Run};
use patchula_words::Source;
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
                headers: reqwest::header::HeaderMap::default(),
                body: br#"{"repository":{"type":"git","url":"git+https://github.com/prettier/prettier.git"}}"#.to_vec(),
                stale: None,
            })
        })
    }
}

fn replay() -> Arc<MiseReplay> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mise/listings.json");
    Arc::new(MiseReplay::open(&path).unwrap())
}
async fn memory() -> Cache {
    Cache::open(":memory:", patchula_cache::Options::default())
        .await
        .unwrap()
}
fn client(replay: Option<&Arc<MiseReplay>>, cache: Cache, token: &str) -> Client {
    let transport = Arc::new(Npm);
    let github = patchula_github::Client::with_transport(
        token,
        cache.clone(),
        patchula_github::Options::default(),
        transport.clone(),
    )
    .unwrap();
    let client = Client::new(
        github,
        patchula_datasource::cached(cache.clone(), transport, 2).unwrap(),
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
        .lookup(&patchula_config::PackageId::public(
            Source::Mise,
            "packslip:jdx/hk",
        ))
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
        .lookup(&patchula_config::PackageId::public(
            Source::Mise,
            "go:github.com/golangci/golangci-lint/cmd/golangci-lint",
        ))
        .await
        .unwrap();
    assert!(catalog.versions.iter().any(|v| v.published_at.is_none()));
    assert!(catalog.versions.iter().any(|v| v.published_at.is_some()));
}

#[tokio::test]
async fn prereleases_stay_in_the_catalog_for_the_policy_to_decide() {
    let source = client(Some(&replay()), memory().await, "a-token");
    // npm reports `prerelease` for these; the Count does not drop them.
    let prettier = source
        .lookup(&patchula_config::PackageId::public(
            Source::Mise,
            "prettier",
        ))
        .await
        .unwrap();
    for version in ["3.0.0-alpha.0", "3.0.0-alpha.11", "1.11.0-rc.1"] {
        assert!(
            prettier.versions.iter().any(|v| v.version == version),
            "{version}"
        );
    }
    // GitHub-backed tools mark them too, with versions that look stable.
    let jq = source
        .lookup(&patchula_config::PackageId::public(Source::Mise, "jq"))
        .await
        .unwrap();
    assert!(jq.versions.iter().any(|v| v.version == "1.7rc2"));
}

#[tokio::test]
async fn node_takes_its_lts_line_from_mise() {
    let replay = replay();
    for name in ["node", "core:node"] {
        let catalog = client(Some(&replay), memory().await, "a-token")
            .lookup(&patchula_config::PackageId::public(Source::Mise, name))
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
    first
        .lookup(&patchula_config::PackageId::public(Source::Mise, "jq"))
        .await
        .unwrap();
    assert_eq!(lookups(), 1);
    // Another client over the same cache reads what the first stored.
    let again = client(Some(&replay), cache.clone(), "a-token");
    let catalog = again
        .lookup(&patchula_config::PackageId::public(Source::Mise, "jq"))
        .await
        .unwrap();
    assert_eq!(lookups(), 1);
    assert_eq!(catalog.versions[0].version, "1.8.2");
    // A manual retry asks mise again.
    again
        .revalidating()
        .lookup(&patchula_config::PackageId::public(Source::Mise, "jq"))
        .await
        .unwrap();
    assert_eq!(lookups(), 2);
    // Without the versions host, mise reads upstream: a separate listing.
    again
        .clone()
        .with_versions_host(false)
        .lookup(&patchula_config::PackageId::public(Source::Mise, "jq"))
        .await
        .unwrap();
    assert_eq!(lookups(), 3);
    let calls = replay.calls();
    let last = calls.last().unwrap();
    assert_eq!(last.env["MISE_USE_VERSIONS_HOST"], "0");
    // What one token may see is not shared with another.
    client(Some(&replay), cache, "another-token")
        .lookup(&patchula_config::PackageId::public(Source::Mise, "jq"))
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
                .lookup(&patchula_config::PackageId::public(
                    Source::Mise,
                    "somethingunknownxyz"
                ))
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
        let error = source
            .lookup(&patchula_config::PackageId::public(Source::Mise, tool))
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Mise(_)), "{tool}");
        assert!(error.to_string().contains(expected), "{tool}: {error}");
    }
}

#[tokio::test]
async fn a_tool_that_installs_from_a_git_ref_has_no_releases_to_update_to() {
    let error = client(Some(&replay()), memory().await, "a-token")
        .lookup(&patchula_config::PackageId::public(
            Source::Mise,
            "cargo:https://github.com/BurntSushi/ripgrep",
        ))
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("no release versions"), "{message}");
    assert!(message.contains("`HEAD`"), "{message}");
}

#[tokio::test]
async fn a_client_without_the_tool_store_cannot_look_up_mise_tools() {
    let error = client(None, memory().await, "a-token")
        .lookup(&patchula_config::PackageId::public(Source::Mise, "jq"))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::NoMise));
}

/// Listings the tests write themselves, for what no recording has: a run
/// that mise failed, and versions that do not order.
fn run(tool: &[&str], outcome: Outcome) -> Run {
    let mut args = ["ls-remote", "--json", "--prerelease", "--"]
        .map(String::from)
        .to_vec();
    args.extend(tool.iter().map(|a| (*a).to_owned()));
    Run {
        args,
        result: outcome,
    }
}
fn listing(versions: &[&str]) -> Outcome {
    Outcome::Output(serde_json::Value::Array(
        versions
            .iter()
            .map(|v| serde_json::json!({ "version": v }))
            .collect(),
    ))
}

#[tokio::test]
async fn node_keeps_its_catalog_when_the_lts_listing_fails() {
    let replay = Arc::new(MiseReplay::new(vec![
        run(&["node"], listing(&["22.1.0", "24.2.0", "26.1.0"])),
        run(
            &["node", "lts"],
            Outcome::Failure(patchula_process::Failure::Exited {
                status: "exit status: 1".into(),
                stdout: String::new(),
                stderr: "mise ERROR no such release line".into(),
            }),
        ),
    ]));
    let catalog = client(Some(&replay), memory().await, "a-token")
        .lookup(&patchula_config::PackageId::public(Source::Mise, "node"))
        .await
        .unwrap();
    assert_eq!(catalog.versions.len(), 3);
    assert_eq!(catalog.versions[0].version, "26.1.0");
    assert!(catalog.dist_tags.is_empty());
    // An empty `lts` listing is the same: no evidence, and no failure.
    let replay = Arc::new(MiseReplay::new(vec![
        run(&["node"], listing(&["22.1.0", "24.2.0"])),
        run(&["node", "lts"], listing(&[])),
    ]));
    let catalog = client(Some(&replay), memory().await, "a-token")
        .lookup(&patchula_config::PackageId::public(Source::Mise, "node"))
        .await
        .unwrap();
    assert_eq!(catalog.versions.len(), 2);
    assert!(catalog.dist_tags.is_empty());
}

#[tokio::test]
async fn versions_that_do_not_order_are_not_taken_for_a_git_ref() {
    let replay = Arc::new(MiseReplay::new(vec![
        run(&["nightlies"], listing(&["nightly", "stable"])),
        run(&["mixed"], listing(&["HEAD", "nightly"])),
        run(&["pinned"], listing(&["HEAD"])),
    ]));
    let source = client(Some(&replay), memory().await, "a-token");
    for tool in ["nightlies", "mixed"] {
        let catalog = source
            .lookup(&patchula_config::PackageId::public(Source::Mise, tool))
            .await
            .unwrap();
        assert!(!catalog.versions.is_empty(), "{tool}");
    }
    let error = source
        .lookup(&patchula_config::PackageId::public(Source::Mise, "pinned"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Git ref"), "{error}");
}
