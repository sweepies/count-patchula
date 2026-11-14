//! Mise tools through a replayed `mise ls-remote --json`. The listings in
//! `fixtures/mise/listings.json` are what the pinned mise printed; nothing
//! here starts mise or reaches the network.
use patchula_cache::Cache;
use patchula_datasource::{Client, Error, MiseAccess, MiseRepository, mise_scope_rule};
use patchula_http::{Exchange, Response, Transport};
use patchula_mise::Lookup;
use patchula_testkit::{MiseReplay, Outcome, Run};
use patchula_words::{Source, Trust};
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

/// Installation credentials: each repository's scoped token names it and
/// the permissions it was minted with, and each name maps to an id a test
/// can move, as a rename and a new repository under the old name do.
#[derive(Default)]
struct Installation {
    ids: std::sync::Mutex<std::collections::BTreeMap<String, u64>>,
}
impl Installation {
    fn rename(&self, repository: &str, id: u64) {
        self.ids.lock().unwrap().insert(repository.into(), id);
    }
}
impl patchula_github::Credentials for Installation {
    fn scope(&self) -> &str {
        "installation"
    }
    fn token(&self) -> patchula_github::Token<'_> {
        Box::pin(async { Ok("installation-token".to_owned()) })
    }
    fn mints_scoped_tokens(&self) -> bool {
        true
    }
    fn scoped_token(
        &self,
        repository: &str,
        permissions: patchula_github::Permissions,
    ) -> Option<patchula_github::Token<'_>> {
        let token = format!("scoped-{repository}-{permissions:?}");
        Some(Box::pin(async move { Ok(token) }))
    }
    fn repository_id(&self, repository: &str) -> Option<patchula_github::RepositoryId<'_>> {
        let mut ids = self.ids.lock().unwrap();
        let next = 100 + ids.len() as u64;
        let id = *ids.entry(repository.to_ascii_lowercase()).or_insert(next);
        Some(Box::pin(async move { Ok(id) }))
    }
}
fn installation(replay: &Arc<MiseReplay>, cache: Cache, credentials: Arc<Installation>) -> Client {
    let transport = Arc::new(Npm);
    let github = patchula_github::Client::with_credentials(
        credentials,
        cache.clone(),
        patchula_github::Options::default(),
        transport.clone(),
    )
    .unwrap();
    Client::new(
        github,
        patchula_datasource::cached(cache.clone(), transport, 2).unwrap(),
        cache,
    )
    .with_mise(Arc::new(Lookup::new(
        replay.clone(),
        "mise".into(),
        "/var/lib/patchula/tools".into(),
        "2026.10.11".into(),
    )))
}
fn token(replay: &MiseReplay) -> String {
    replay.calls().last().unwrap().env["MISE_GITHUB_TOKEN"].clone()
}
/// An untrusted repository.
fn repository(name: &str, private: Option<bool>) -> MiseRepository {
    MiseRepository {
        name: name.into(),
        private,
        trust: Trust::Untrusted,
    }
}
fn trusted(name: &str) -> MiseRepository {
    MiseRepository {
        trust: Trust::Trusted,
        ..repository(name, Some(true))
    }
}

/// A tool released from another repository, or from no GitHub repository,
/// lists public data; one released from the repository itself, or from
/// somewhere the Count cannot tell in a repository that may be private,
/// is that repository's own, named in lowercase.
#[test]
fn the_scope_rule() {
    let own = || MiseAccess::Repository("acme/app".into());
    for (tool, private, access) in [
        ("github:acme/tool", Some(true), MiseAccess::Public),
        ("aqua:jqlang/jq", Some(true), MiseAccess::Public),
        ("jq", Some(true), MiseAccess::Public),
        ("npm:prettier", Some(true), MiseAccess::Public),
        ("core:node", None, MiseAccess::Public),
        ("github:acme/app", Some(false), own()),
        ("ubi:ACME/APP", Some(true), own()),
        ("go:github.com/acme/tool", Some(true), own()),
        ("go:github.com/acme/tool", None, own()),
        ("go:github.com/acme/tool", Some(false), MiseAccess::Public),
        ("http:tool", Some(false), MiseAccess::Public),
    ] {
        assert_eq!(
            mise_scope_rule(tool, &repository("Acme/App", private)),
            access,
            "{tool} {private:?}"
        );
    }
}

/// mise gets a token scoped to the repository a lookup is made for, never
/// the installation's: `contents: read` for the repository's own listing,
/// `metadata: read` for a public one, which reads no repository's private
/// releases (not even its own under an old name). The run's own token only
/// without a repository.
#[tokio::test]
async fn mise_gets_a_token_scoped_to_the_repository_with_the_access_permissions() {
    let replay = replay();
    let jq = patchula_config::PackageId::public(Source::Mise, "jq");
    let source = installation(&replay, memory().await, Default::default());
    // jq is released from jqlang/jq: its own listing there, public elsewhere.
    source
        .clone()
        .with_mise_repository(Some(repository("jqlang/jq", Some(false))))
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(token(&replay), "scoped-jqlang/jq-ContentsRead");
    source
        .clone()
        .with_mise_repository(Some(repository("acme/site", Some(true))))
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(token(&replay), "scoped-acme/site-MetadataRead");
    source.lookup(&jq).await.unwrap();
    assert_eq!(token(&replay), "installation-token");
    // A trusted repository's lookups get the run's token.
    source
        .clone()
        .with_mise_repository(Some(trusted("acme/app")))
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(token(&replay), "installation-token");
}

/// A token run cannot mint a scoped token: a trusted repository's lookups
/// get the run's token, and an untrusted one's get none while nothing shows
/// the run's reaches no further than the repository (its reach unprobed). Untrusted listings are public and shared;
/// the run's listing is never served to them.
#[tokio::test]
async fn a_token_run_hands_its_token_only_to_trusted_repositories() {
    let jq = patchula_config::PackageId::public(Source::Mise, "jq");
    let replay = self::replay();
    let lookups = || replay.lookups().len();
    let source = client(Some(&replay), memory().await, "run-token");
    let as_repo = |repository| source.clone().with_mise_repository(Some(repository));
    as_repo(trusted("acme/app")).lookup(&jq).await.unwrap();
    assert_eq!(token(&replay), "run-token");
    assert_eq!(lookups(), 1);
    as_repo(repository("acme/app", Some(true)))
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(lookups(), 2);
    assert!(
        !replay
            .calls()
            .last()
            .unwrap()
            .env
            .contains_key("MISE_GITHUB_TOKEN")
    );
    as_repo(repository("jqlang/jq", Some(false)))
        .lookup(&jq)
        .await
        .unwrap();
    as_repo(trusted("acme/other")).lookup(&jq).await.unwrap();
    assert_eq!(lookups(), 2);
}

/// Answers `installation/repositories`, a fixed token's reach probe, with
/// one status and body.
struct Reach(reqwest::StatusCode, &'static str);
impl Transport for Reach {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        assert_eq!(request.url().path(), "/installation/repositories");
        Box::pin(async move {
            Ok(Response {
                status: self.0,
                headers: reqwest::header::HeaderMap::default(),
                body: self.1.as_bytes().to_vec(),
                stale: None,
            })
        })
    }
}
/// A token run whose token's reach GitHub answers with `status` and `body`,
/// probed.
async fn probed(
    replay: &Arc<MiseReplay>,
    status: reqwest::StatusCode,
    body: &'static str,
) -> Client {
    let cache = memory().await;
    let transport = Arc::new(Reach(status, body));
    let github = patchula_github::Client::with_transport(
        "run-token",
        cache.clone(),
        patchula_github::Options::default(),
        transport.clone(),
    )
    .unwrap();
    github.probe_reach().await;
    Client::new(
        github,
        patchula_datasource::cached(cache.clone(), transport, 2).unwrap(),
        cache,
    )
    .with_mise(Arc::new(Lookup::new(
        replay.clone(),
        "mise".into(),
        "/var/lib/patchula/tools".into(),
        "2026.10.11".into(),
    )))
}

/// A token GitHub reports reaching only the repository being read is that
/// repository's scoped token: an untrusted repository's lookups get it, each
/// listed for the repository alone, since its permissions cannot be narrowed
/// to public data. A token that reaches further, or whose reach cannot be
/// read, gives an untrusted repository none.
#[tokio::test]
async fn a_token_reaching_only_the_repository_is_its_scoped_token() {
    let jq = patchula_config::PackageId::public(Source::Mise, "jq");
    let replay = self::replay();
    let lookups = || replay.lookups().len();
    let source = probed(
        &replay,
        reqwest::StatusCode::OK,
        r#"{"total_count":1,"repositories":[{"id":7,"full_name":"Acme/App"}]}"#,
    )
    .await;
    let app = repository("acme/app", Some(true));
    assert_eq!(
        source.mise_access(Some(&app), "jq"),
        MiseAccess::Repository("acme/app".into())
    );
    assert_eq!(
        source.mise_access(Some(&repository("acme/other", Some(false))), "jq"),
        MiseAccess::Anonymous
    );
    source
        .clone()
        .with_mise_repository(Some(app.clone()))
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(token(&replay), "run-token");
    assert_eq!(lookups(), 1);
    // Another repository the token does not reach shares nothing with it.
    source
        .clone()
        .with_mise_repository(Some(repository("acme/other", Some(false))))
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(lookups(), 2);
    assert!(
        !replay
            .calls()
            .last()
            .unwrap()
            .env
            .contains_key("MISE_GITHUB_TOKEN")
    );
    source
        .clone()
        .with_mise_repository(Some(app))
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(lookups(), 2);

    for (status, body) in [
        (
            reqwest::StatusCode::OK,
            r#"{"total_count":2,"repositories":[{"id":7,"full_name":"acme/app"},{"id":8,"full_name":"acme/api"}]}"#,
        ),
        (
            reqwest::StatusCode::FORBIDDEN,
            r#"{"message":"You must authenticate with an installation access token in order to list repositories for an installation."}"#,
        ),
    ] {
        let replay = self::replay();
        let source = probed(&replay, status, body).await;
        let app = repository("acme/app", Some(true));
        assert_eq!(
            source.mise_access(Some(&app), "jq"),
            MiseAccess::Anonymous,
            "{status}"
        );
        source
            .with_mise_repository(Some(app))
            .lookup(&jq)
            .await
            .unwrap();
        assert!(
            !replay
                .calls()
                .last()
                .unwrap()
                .env
                .contains_key("MISE_GITHUB_TOKEN"),
            "{status}"
        );
    }
}

/// A batch key's access must be the one its repository gives: a `Public`
/// key with no repository to mint for, or a key another repository's
/// lookup would not have, fails without starting mise.
#[tokio::test]
async fn a_batch_lookup_without_its_access_fails_closed() {
    let replay = replay();
    let jq = patchula_config::PackageId::public(Source::Mise, "jq");
    let source = installation(&replay, memory().await, Default::default());
    for (access, via) in [
        (MiseAccess::Public, None),
        (
            MiseAccess::Public,
            Some(repository("jqlang/jq", Some(false))),
        ),
        (
            MiseAccess::Repository("acme/app".into()),
            Some(repository("acme/site", Some(true))),
        ),
    ] {
        let error = source
            .clone()
            .with_mise_access(access.clone(), via.clone())
            .lookup(&jq)
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::Github(_)),
            "{access:?} {via:?}: {error}"
        );
    }
    assert!(replay.lookups().is_empty());
    source
        .clone()
        .with_mise_access(
            MiseAccess::Public,
            Some(repository("acme/site", Some(true))),
        )
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(token(&replay), "scoped-acme/site-MetadataRead");
}

/// A public listing one repository's lookup stored serves another's; a
/// repository's own listing serves no other repository, nor a public or
/// run-token lookup, nor a new repository under its old name.
#[tokio::test]
async fn public_listings_are_shared_and_repository_listings_are_not() {
    let replay = replay();
    let cache = memory().await;
    let jq = patchula_config::PackageId::public(Source::Mise, "jq");
    let lookups = || replay.lookups().len();
    let credentials = Arc::new(Installation::default());
    let source = installation(&replay, cache.clone(), credentials.clone());
    let as_repo = |name: &str, private| {
        source
            .clone()
            .with_mise_repository(Some(repository(name, Some(private))))
    };
    // A go: tool's source is unknown: public only from a public repository.
    let go = patchula_config::PackageId::public(
        Source::Mise,
        "go:github.com/golangci/golangci-lint/cmd/golangci-lint",
    );
    as_repo("acme/site", false).lookup(&go).await.unwrap();
    as_repo("acme/docs", false).lookup(&go).await.unwrap();
    assert_eq!(lookups(), 1);
    as_repo("acme/app", true).lookup(&go).await.unwrap();
    assert_eq!(lookups(), 2);
    assert_eq!(token(&replay), "scoped-acme/app-ContentsRead");
    as_repo("ACME/App", true).lookup(&go).await.unwrap();
    assert_eq!(lookups(), 2);
    as_repo("acme/api", true).lookup(&go).await.unwrap();
    assert_eq!(lookups(), 3);
    // acme/app was renamed and a new repository took the name.
    credentials.rename("acme/app", 2);
    as_repo("acme/app", true).lookup(&go).await.unwrap();
    assert_eq!(lookups(), 4);
    // jq is public to both; the run's token keeps its own listing.
    as_repo("acme/app", true).lookup(&jq).await.unwrap();
    as_repo("acme/api", true).lookup(&jq).await.unwrap();
    assert_eq!(lookups(), 5);
    source.lookup(&jq).await.unwrap();
    assert_eq!(lookups(), 6);
    // A trusted repository's lookups share the run's listing.
    source
        .clone()
        .with_mise_repository(Some(trusted("acme/app")))
        .lookup(&jq)
        .await
        .unwrap();
    assert_eq!(lookups(), 6);
}
