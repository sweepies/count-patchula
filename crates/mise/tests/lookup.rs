//! Lookups through a replayed mise: the commands the Count runs and what it
//! makes of the recorded answers. Nothing here starts mise.
use patchula_mise::{Error, Lookup, Query};
use patchula_testkit::MiseReplay;
use std::{path::Path, sync::Arc};

const TOKEN: &str = "offline-token-value";

fn replay() -> Arc<MiseReplay> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/mise/listings.json");
    Arc::new(MiseReplay::open(&path).unwrap())
}
fn lookup(replay: &Arc<MiseReplay>) -> Lookup {
    Lookup::new(
        replay.clone(),
        "/usr/local/bin/mise".into(),
        "/var/lib/patchula/tools".into(),
        "2026.10.11".into(),
    )
}
fn query<'a>(tool: &'a str, token: Option<&'a str>) -> Query<'a> {
    Query {
        tool,
        prefix: None,
        token,
        versions_host: true,
    }
}

#[tokio::test]
async fn short_and_registry_names_list_what_mise_printed() {
    let replay = replay();
    let lookup = lookup(&replay);
    let hk = lookup
        .ls_remote(&query("packslip:jdx/hk", None))
        .await
        .unwrap();
    assert_eq!(hk.len(), 13);
    assert_eq!(hk[0].version, "1.58.1");
    assert_eq!(hk[0].created_at.as_deref(), Some("2026-09-05T19:46:14.0Z"));
    assert_eq!(
        hk[0].release_url.as_deref(),
        Some("https://github.com/jdx/hk/releases/tag/v1.58.1")
    );
    assert_eq!(hk[0].prerelease, Some(false));
    // Node lists a version and a date and nothing else.
    let node = lookup.ls_remote(&query("node", None)).await.unwrap();
    assert!(node.iter().all(|r| r.release_url.is_none()));
    // Some backends list versions without dates.
    let go = lookup
        .ls_remote(&query(
            "go:github.com/golangci/golangci-lint/cmd/golangci-lint",
            None,
        ))
        .await
        .unwrap();
    assert!(go.iter().any(|r| r.created_at.is_none()));
    let jq = lookup.ls_remote(&query("jq", None)).await.unwrap();
    assert!(jq.iter().all(|r| {
        r.release_url
            .as_deref()
            .is_some_and(|u| u.contains("/jqlang/jq/releases/tag/jq-"))
    }));
}

#[tokio::test]
async fn a_prefix_selects_a_line_and_follows_the_tool() {
    let replay = replay();
    let lookup = lookup(&replay);
    let lts = lookup
        .ls_remote(&Query {
            prefix: Some("lts"),
            ..query("node", None)
        })
        .await
        .unwrap();
    assert!(lts.len() < 100);
    assert_eq!(
        replay.lookups(),
        [["ls-remote", "--json", "--prerelease", "--", "node", "lts"]]
    );
}

#[tokio::test]
async fn mise_runs_pinned_in_an_empty_sandbox_with_only_the_given_token() {
    let replay = replay();
    let lookup = lookup(&replay);
    lookup
        .ls_remote(&query("packslip:jdx/hk", Some(TOKEN)))
        .await
        .unwrap();
    let calls = replay.calls();
    // The pinned mise is installed first, then asked.
    assert_eq!(calls.len(), 2);
    let install = &calls[0];
    assert_eq!(
        install.args,
        ["--no-config", "install", "aqua:jdx/mise@2026.10.11"]
    );
    assert_eq!(install.timeout.as_secs(), 1800);
    let ask = &calls[1];
    assert_eq!(
        ask.args,
        [
            "--no-config",
            "exec",
            "aqua:jdx/mise@2026.10.11",
            "--",
            "mise",
            "ls-remote",
            "--json",
            "--prerelease",
            "--",
            "packslip:jdx/hk"
        ]
    );
    assert_eq!(ask.timeout.as_secs(), 180);
    for call in &calls {
        assert_eq!(call.program, Path::new("/usr/local/bin/mise"));
        // A fresh directory with nothing in it, apart from the home beside it.
        assert!(call.cwd.starts_with(std::env::temp_dir()));
        assert!(call.cwd.ends_with("work"));
        let env = &call.env;
        let home = env["HOME"].clone();
        assert!(home.starts_with(std::env::temp_dir().to_str().unwrap()));
        assert!(home.ends_with("home"));
        for key in ["XDG_CONFIG_HOME", "XDG_CACHE_HOME"] {
            assert!(env[key].starts_with(&home), "{key}");
        }
        assert_eq!(env["MISE_DATA_DIR"], "/var/lib/patchula/tools");
        // No config of any kind, no env templates, no hooks, no prompts.
        for key in ["MISE_NO_CONFIG", "MISE_NO_ENV", "MISE_NO_HOOKS", "MISE_YES"] {
            assert_eq!(env[key], "1", "{key}");
        }
        // Plugin backends are off, and nothing installs behind a lookup.
        assert_eq!(env["MISE_DISABLE_BACKENDS"], "asdf,vfox");
        assert_eq!(env["MISE_AUTO_INSTALL"], "0");
        // The token reaches mise by one name and nothing else supplies one.
        assert_eq!(env["MISE_GITHUB_TOKEN"], TOKEN);
        assert_eq!(env["MISE_GITHUB_GH_CLI_TOKENS"], "0");
        assert!(!call.args.iter().any(|a| a.contains(TOKEN)));
        // Only these names are set; the operator's environment is not copied.
        let allowed = [
            "PATH",
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "LANG",
            "CI",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_CONFIG_GLOBAL",
            "GIT_TERMINAL_PROMPT",
            "GIT_ALLOW_PROTOCOL",
            "NPM_CONFIG_USERCONFIG",
            "NPM_CONFIG_GLOBALCONFIG",
            "NPM_CONFIG_IGNORE_SCRIPTS",
            "BUN_INSTALL_CACHE_DIR",
            "CARGO_HOME",
            "GOPATH",
            "GOWORK",
            "GOTOOLCHAIN",
            "GOENV",
            "TF_IN_AUTOMATION",
            "TF_INPUT",
            "TF_CLI_CONFIG_FILE",
            "MISE_COLOR",
            "MISE_DATA_DIR",
            "MISE_NO_CONFIG",
            "MISE_NO_ENV",
            "MISE_NO_HOOKS",
            "MISE_YES",
            "MISE_DISABLE_BACKENDS",
            "MISE_AUTO_INSTALL",
            "MISE_USE_VERSIONS_HOST",
            "MISE_GITHUB_TOKEN",
            "MISE_GITHUB_GH_CLI_TOKENS",
        ];
        for key in env.keys() {
            assert!(allowed.contains(&key.as_str()), "unexpected variable {key}");
        }
        assert_eq!(env["PATH"], "/usr/local/bin:/usr/bin:/bin");
    }
}

#[tokio::test]
async fn without_a_token_none_is_set_and_the_versions_host_follows_the_query() {
    let replay = replay();
    let lookup = lookup(&replay);
    lookup
        .ls_remote(&Query {
            versions_host: false,
            ..query("packslip:jdx/hk", None)
        })
        .await
        .unwrap();
    let call = &replay.calls()[1];
    assert!(!call.env.contains_key("MISE_GITHUB_TOKEN"));
    assert_eq!(call.env["MISE_USE_VERSIONS_HOST"], "0");
    assert!(call.env.contains_key("MISE_GITHUB_GH_CLI_TOKENS"));
}

#[tokio::test]
async fn the_pinned_mise_is_installed_once_for_every_lookup() {
    let replay = replay();
    let lookup = lookup(&replay);
    for tool in ["jq", "prettier", "packslip:jdx/hk"] {
        lookup.ls_remote(&query(tool, None)).await.unwrap();
    }
    let installs = replay
        .calls()
        .iter()
        .filter(|call| call.args.iter().any(|a| a == "install"))
        .count();
    assert_eq!(installs, 1);
    assert_eq!(replay.lookups().len(), 3);
}

#[tokio::test]
async fn plugin_backends_and_option_syntax_never_reach_mise() {
    let replay = replay();
    let lookup = lookup(&replay);
    for tool in [
        "asdf:mise-plugins/asdf-tiny",
        "vfox:version-fox/vfox-cmake",
        "github:jdx/mise[api_url=http://127.0.0.1:9]",
        "pypi:black[extras=jupyter]",
        "-x",
        "--all",
        "npm:--registry=https://example.invalid",
        "go:-modfile=x",
        "node lts",
        "",
        "tool\nname",
    ] {
        let error = lookup
            .ls_remote(&query(tool, Some(TOKEN)))
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Refused(_)), "{tool}: {error}");
        assert!(!error.to_string().contains(TOKEN));
    }
    assert!(matches!(
        lookup.backend("asdf:mise-plugins/asdf-tiny", None).await,
        Err(Error::Refused(_))
    ));
    // Not even the pinned mise was started for any of them.
    assert!(replay.calls().is_empty());
    let message = lookup
        .ls_remote(&query("vfox:version-fox/vfox-cmake", None))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("vfox backend runs plugin code"),
        "{message}"
    );
}

#[tokio::test]
async fn a_backend_the_count_does_not_know_is_left_to_mise_to_refuse() {
    let replay = replay();
    let lookup = lookup(&replay);
    // mise adds backends over time, so no list of them is kept here. One it
    // does not have fails in mise, which runs no plugin and installs nothing.
    let error = lookup
        .ls_remote(&query("nosuch:foo", None))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Failed(_)), "{error}");
    assert!(
        error
            .to_string()
            .contains("nosuch is not a valid plugin name"),
        "{error}"
    );
    assert_eq!(
        replay.lookups(),
        [["ls-remote", "--json", "--prerelease", "--", "nosuch:foo"]]
    );
    let call = &replay.calls()[1];
    assert_eq!(call.env["MISE_DISABLE_BACKENDS"], "asdf,vfox");
    assert_eq!(call.env["MISE_AUTO_INSTALL"], "0");
}

#[tokio::test]
async fn names_for_release_backends_are_passed_through_unchanged() {
    let replay = replay();
    let lookup = lookup(&replay);
    for tool in [
        "packslip:github.com/jdx/hk/tools/lint",
        "cargo:https://github.com/BurntSushi/ripgrep",
    ] {
        let _ = lookup.ls_remote(&query(tool, None)).await;
    }
    assert_eq!(
        replay.lookups(),
        [
            [
                "ls-remote",
                "--json",
                "--prerelease",
                "--",
                "packslip:github.com/jdx/hk/tools/lint"
            ],
            [
                "ls-remote",
                "--json",
                "--prerelease",
                "--",
                "cargo:https://github.com/BurntSushi/ripgrep"
            ]
        ]
    );
}

#[tokio::test]
async fn failures_carry_what_mise_said_and_empty_lists_are_errors() {
    let replay = replay();
    let lookup = lookup(&replay);
    let unknown = lookup
        .ls_remote(&query("somethingunknownxyz", None))
        .await
        .unwrap_err();
    assert!(matches!(unknown, Error::Failed(_)));
    assert!(
        unknown
            .to_string()
            .contains("somethingunknownxyz not found in mise tool registry")
    );
    // A shorthand whose only backend is a plugin.
    let plugin_only = lookup.ls_remote(&query("aapt2", None)).await.unwrap_err();
    assert!(plugin_only.to_string().contains("none of its backends"));
    // A publisher-domain packslip source needs signer options the Count lacks.
    let domain = lookup
        .ls_remote(&query("packslip:packslip.dev", None))
        .await
        .unwrap_err();
    assert!(domain.to_string().contains("nothing pins its signer"));
    // A shorthand that falls through to a backend with nothing to list.
    assert!(matches!(
        lookup.ls_remote(&query("1password", None)).await,
        Err(Error::Empty(tool)) if tool == "1password"
    ));
    assert!(matches!(
        lookup
            .ls_remote(&query("packslip:github.com/jdx/hk/tools/lint", None))
            .await,
        Err(Error::Empty(_))
    ));
}

#[tokio::test]
async fn a_tools_backend_comes_from_mise() {
    let replay = replay();
    let lookup = lookup(&replay);
    assert_eq!(
        lookup.backend("prettier", None).await.unwrap(),
        "npm:prettier"
    );
    assert_eq!(replay.lookups(), [["tool", "--json", "--", "prettier"]]);
}

#[tokio::test]
async fn a_stuck_install_is_reported_and_retried_by_the_next_lookup() {
    struct Failing(std::sync::atomic::AtomicUsize);
    impl patchula_process::Executor for Failing {
        fn run(
            &self,
            _: patchula_process::CommandSpec,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, String>> + Send + '_>>
        {
            Box::pin(async move {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("tool exceeded 1800s time limit".into())
            })
        }
    }
    let executor = Arc::new(Failing(Default::default()));
    let lookup = Lookup::new(
        executor.clone(),
        "mise".into(),
        "/tmp/tools".into(),
        "1".into(),
    );
    for attempt in 1..=2 {
        let error = lookup.ls_remote(&query("jq", None)).await.unwrap_err();
        assert!(error.to_string().contains("time limit"), "{error}");
        assert_eq!(
            executor.0.load(std::sync::atomic::Ordering::SeqCst),
            attempt
        );
    }
}
