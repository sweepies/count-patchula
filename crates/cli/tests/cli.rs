#![forbid(unsafe_code)]

use std::process::{Command, Output};

/// A GitHub CLI config directory with no sign-in, so token resolution never
/// picks up the developer's own `gh` login.
const SIGNED_OUT_GH: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/gh-signed-out");

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_patchula"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn help_version_and_bundled_schema() {
    for args in [vec!["--help"], vec!["--version"], vec!["config", "--help"]] {
        let output = cli(&args);
        assert!(output.status.success(), "{output:?}");
        assert!(!output.stdout.is_empty());
    }
    let output = cli(&["config", "schema"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        patchula_config::SCHEMA
    );
    let output = cli(&["config", "server-schema"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        patchula_server::settings::SCHEMA
    );
}

#[test]
fn operator_settings_are_checked_offline_and_secret_diagnostics_are_sanitized() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.pkl");
    std::fs::write(&path, "amends \"PatchulaServer.pkl\"\nconcurrency = 2\n").unwrap();
    let output = cli(&["config", "check-server", path.to_str().unwrap(), "--json"]);
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["valid"],
        true
    );
    std::fs::write(
        &path,
        "amends \"PatchulaServer.pkl\"\nprivateKey = \"do-not-echo-test-secret\"\n",
    )
    .unwrap();
    let output = cli(&["config", "check-server", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("do-not-echo-test-secret"));
}

#[test]
fn settings_diagnostics_reach_check_and_server_startup() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("server.pkl");
    std::fs::write(
        &settings,
        "amends \"PatchulaServer.pkl\"\nconcurrency = 0\n",
    )
    .unwrap();
    let checked = cli(&["config", "check-server", settings.to_str().unwrap()]);
    assert_eq!(checked.status.code(), Some(2));
    let diagnostic = String::from_utf8(checked.stderr).unwrap();
    assert!(diagnostic.contains("server.pkl") && diagnostic.contains("concurrency"));
    assert!(diagnostic.contains('│'), "{diagnostic}");
    let output = Command::new(env!("CARGO_BIN_EXE_patchula"))
        .args(["serve", "--app-id", "1", "--settings"])
        .arg(&settings)
        .env(
            "PATCHULA_APP_PRIVATE_KEY",
            "test-private-key-must-not-appear",
        )
        .env(
            "PATCHULA_WEBHOOK_SECRET",
            "test-webhook-secret-must-not-appear",
        )
        .env_remove("PATCHULA_APP_PRIVATE_KEY_FILE")
        .env_remove("PATCHULA_WEBHOOK_SECRET_FILE")
        .env_remove("PATCHULA_RECHECK")
        .env_remove("PATCHULA_LOG")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("must-not-appear"));
    let log: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    let diagnostic = log["fields"]["error"].as_str().unwrap();
    assert!(diagnostic.contains("server.pkl") && diagnostic.contains("concurrency"));
    assert!(diagnostic.contains('│'), "{diagnostic}");
}

#[test]
fn server_startup_errors_are_json_and_never_echo_secret_values() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("server.pkl");
    std::fs::write(&settings, "amends \"PatchulaServer.pkl\"\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_patchula"))
        .args([
            "serve",
            "--app-id",
            "1",
            "--listen",
            "127.0.0.1:0",
            "--shutdown-timeout-secs",
            "1",
            "--settings",
        ])
        .arg(settings)
        .arg("--cache")
        .arg(dir.path().join("cache.sqlite"))
        .env(
            "PATCHULA_APP_PRIVATE_KEY",
            "test-private-key-must-not-appear",
        )
        .env(
            "PATCHULA_WEBHOOK_SECRET",
            "test-webhook-secret-must-not-appear",
        )
        .env_remove("PATCHULA_APP_PRIVATE_KEY_FILE")
        .env_remove("PATCHULA_WEBHOOK_SECRET_FILE")
        .env_remove("PATCHULA_RECHECK")
        .env_remove("PATCHULA_LOG")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("must-not-appear"));
    let log: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(log["level"], "ERROR");
    assert_eq!(log["fields"]["error"], "invalid GitHub App credentials");
}

#[test]
fn run_requires_only_the_standard_token_and_accepts_write_options() {
    for dry_run in [false, true] {
        let mut args = vec!["run", "--json", "--allow-lockfile-pending", "owner/repo"];
        if dry_run {
            args.push("--dry-run");
        }
        let output = Command::new(env!("CARGO_BIN_EXE_patchula"))
            .args(args)
            .env_remove("PATCHULA_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .env("GH_CONFIG_DIR", SIGNED_OUT_GH)
            .output()
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(json["exitCode"], 1);
        assert_eq!(json["schemaVersion"], 1);
        let error = json["error"].as_str().unwrap();
        assert!(
            error.contains("PATCHULA_TOKEN") && error.contains("gh auth login"),
            "{error}"
        );
    }
}

#[test]
fn lookup_requires_token_without_network_and_validates_concurrency() {
    let output = Command::new(env!("CARGO_BIN_EXE_patchula"))
        .args([
            "lookup",
            "--config",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../examples/renovate-migration.pkl"
            ),
            "--json",
            "acme/runner",
        ])
        .env_remove("GITHUB_TOKEN")
        .env_remove("PATCHULA_TOKEN")
        .env_remove("GH_TOKEN")
        .env("GH_CONFIG_DIR", SIGNED_OUT_GH)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["error"].as_str().unwrap().contains("PATCHULA_TOKEN"));
    let output = cli(&["lookup", "--concurrency", "0", "--json", "owner/repo"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn invalid_arguments_exit_two() {
    for args in [
        vec![],
        vec!["config", "check"],
        vec!["--color", "purple", "lookup"],
        vec!["--unknown"],
    ] {
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn config_validation_and_json_results_use_documented_exit_codes() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/renovate-migration.pkl"
    );
    let output = cli(&["config", "check", path, "--json"]);
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["valid"].as_bool(), Some(true));
    assert!(output.stderr.is_empty());

    let directory = tempfile::tempdir().unwrap();
    let invalid = directory.path().join("patchula.pkl");
    std::fs::write(
        &invalid,
        "amends \"PatchulaConfig.pkl\"\nunknownPolicy = true\n",
    )
    .unwrap();
    let invalid = invalid.to_str().unwrap();
    let output = cli(&["config", "check", invalid, "--json"]);
    assert_eq!(output.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["exitCode"], 2);
    assert!(json["error"].as_str().unwrap().contains("unknownPolicy"));
    assert!(output.stderr.is_empty());

    let output = cli(&["config", "check", invalid, "--color", "never"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("unknownPolicy") && stderr.contains("patchula.pkl"));
    assert!(!stderr.contains("\x1b["));

    let output = cli(&["config", "schema", "--json"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, patchula_config::SCHEMA.as_bytes());
}

#[test]
fn lookup_config_and_config_dir_are_mutually_exclusive() {
    let output = cli(&[
        "lookup",
        "--config",
        "missing.pkl",
        "--config-dir",
        "missing",
        "owner/repo",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains("--config") && diagnostic.contains("--config-dir"),
        "{diagnostic}"
    );
}

#[test]
fn config_check_directory_uses_bundled_nested_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("patchula.pkl"),
        "amends \"PatchulaConfig.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\n",
    )
    .unwrap();
    let output = cli(&["config", "check", dir.path().to_str().unwrap(), "--json"]);
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["valid"].as_bool(), Some(true));
}

#[test]
fn lookup_config_dir_validates_all_matches_before_repository_reads() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("first.pkl"),
        "amends \"PatchulaConfig.pkl\"\n",
    )
    .unwrap();
    let bad = dir.path().join("second.pkl");
    std::fs::write(
        &bad,
        "amends \"PatchulaConfig.pkl\"\nunknownPolicy = true\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_patchula"))
        .args([
            "lookup",
            "--config-dir",
            dir.path().to_str().unwrap(),
            "--cache",
            dir.path().join("cache.sqlite").to_str().unwrap(),
            "--json",
            "demo/first",
            "demo/second",
        ])
        .env("PATCHULA_TOKEN", "offline")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let error = json["error"].as_str().unwrap();
    assert!(
        error.contains("demo/second")
            && error.contains(bad.to_str().unwrap())
            && error.contains("unknownPolicy"),
        "{error}"
    );
}

#[test]
fn recheck_flag_overrides_environment_and_invalid_environment_is_a_usage_error() {
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_patchula"));
        command
            .args(["run", "--json", "--dry-run", "owner/repo"])
            .env_remove("PATCHULA_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .env("GH_CONFIG_DIR", SIGNED_OUT_GH)
            .env("PATCHULA_RECHECK", "invalid");
        command
    };
    let output = command().output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("PATCHULA_RECHECK")
    );
    for mode in ["now", "next-run"] {
        let output = command().args(["--recheck", mode]).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("PATCHULA_TOKEN")
        );
    }
}

#[test]
fn repositories_default_to_the_current_github_remote() {
    let git = |dir: &std::path::Path, args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    let lookup = |dir: &std::path::Path, args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_patchula"))
            .arg("lookup")
            .args(args)
            .arg("--json")
            .current_dir(dir)
            .env_remove("GITHUB_TOKEN")
            .env_remove("PATCHULA_TOKEN")
            .env_remove("GH_TOKEN")
            .env("GH_CONFIG_DIR", SIGNED_OUT_GH)
            .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
            .output()
            .unwrap()
    };
    let error = |output: &std::process::Output| {
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        json["error"].as_str().unwrap().to_owned()
    };

    let dir = tempfile::tempdir().unwrap();
    let output = lookup(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(error(&output).contains("not a Git checkout"));

    git(dir.path(), &["init", "--quiet"]);
    let output = lookup(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(error(&output).contains("no Git remote"));

    git(
        dir.path(),
        &["remote", "add", "origin", "https://gitlab.com/demo/app.git"],
    );
    let output = lookup(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(error(&output).contains("origin remote is not a github.com repository"));

    // A GitHub remote resolves, so the run gets as far as needing a token.
    git(
        dir.path(),
        &["remote", "set-url", "origin", "git@github.com:demo/app.git"],
    );
    let output = lookup(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(error(&output).contains("PATCHULA_TOKEN"));

    let output = lookup(dir.path(), &["--all-repos", "demo/app"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(error(&output).contains("--all-repos cannot be combined"));

    let elsewhere = tempfile::tempdir().unwrap();
    let output = lookup(elsewhere.path(), &["--all-repos"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(error(&output).contains("PATCHULA_TOKEN"));
}
