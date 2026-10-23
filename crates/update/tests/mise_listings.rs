//! Records what the pinned `mise ls-remote --json` prints for a representative
//! set of tools. Replay tests elsewhere read `fixtures/mise/listings.json`.
//! Recording needs the network and a `mise` on PATH:
//! `PATCHULA_RECORD=1 mise run fixtures:record-mise`.
use patchula_mise::Query;
use patchula_process::SystemExecutor;
use patchula_testkit::MiseRecorder;
use patchula_update::lockfiles::Runner;
use std::{path::Path, sync::Arc};

/// A registry shorthand, backends, a core tool and its LTS prefix, and the
/// ways a lookup fails. The prefix, when there is one, follows the tool.
const LISTINGS: &[&[&str]] = &[
    &["packslip:jdx/hk"],
    &["packslip:github.com/jdx/hk/tools/lint"],
    &["jq"],
    &["prettier"],
    &["npm:prettier"],
    &["cargo:ripgrep"],
    &["go:github.com/golangci/golangci-lint/cmd/golangci-lint"],
    &["node"],
    &["node", "lts"],
    &["core:node"],
    &["core:node", "lts"],
    &["cargo:https://github.com/BurntSushi/ripgrep"],
    &["1password"],
    &["aapt2"],
    &["packslip:packslip.dev"],
    &["somethingunknownxyz"],
    &["nosuch:foo"],
];
const BACKENDS: &[&str] = &["prettier"];
const RECORD_VAR: &str = "PATCHULA_RECORD";
const TOKEN_VAR: &str = "GH_TOKEN";

#[tokio::test]
async fn record_mise_listings() {
    if std::env::var(RECORD_VAR).as_deref() != Ok("1") {
        return;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mise/listings.json");
    let cache = tempfile::tempdir().unwrap();
    let probe = Runner::new(cache.path().join("cache.sqlite"), "");
    let version = probe.mise_lookup().version().to_owned();
    let recorder = Arc::new(MiseRecorder::new(
        &path,
        Arc::new(SystemExecutor),
        &version,
        &format!(
            "Recorded from the pinned mise {version} with `mise run fixtures:record-mise`; \
             each run is what `mise ls-remote --json` or `mise tool --json` printed in the Count's sandbox"
        ),
    ));
    let lookup = Runner::new(cache.path().join("cache.sqlite"), "")
        .with_executor(recorder)
        .mise_lookup();
    let token = std::env::var(TOKEN_VAR).ok();
    for listing in LISTINGS {
        let result = lookup
            .ls_remote(&Query {
                tool: listing[0],
                prefix: listing.get(1).copied(),
                token: token.as_deref(),
                versions_host: true,
            })
            .await;
        eprintln!(
            "{listing:?}: {}",
            result.map_or_else(|e| e.to_string(), |r| format!("{} versions", r.len()))
        );
    }
    for tool in BACKENDS {
        eprintln!("{tool}: {:?}", lookup.backend(tool, token.as_deref()).await);
    }
}
