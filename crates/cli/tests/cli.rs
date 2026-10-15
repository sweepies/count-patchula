#![forbid(unsafe_code)]

use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rustovate"))
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
        rustovate_config::SCHEMA
    );
}

#[test]
fn update_stub_is_a_runtime_error() {
    {
        let command = "run";
        let output = cli(&[command]);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("not implemented yet (Phase 1)"));
        let output = cli(&[command, "--json"]);
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(json["exitCode"], 1);
        assert_eq!(json["schemaVersion"], 1);
    }
}

#[test]
fn lookup_requires_token_without_network_and_validates_concurrency() {
    let output = Command::new(env!("CARGO_BIN_EXE_rustovate"))
        .args([
            "lookup",
            "--config",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../examples/renovate-runner.pkl"
            ),
            "--json",
            "sweepies/renovate-runner",
        ])
        .env_remove("GITHUB_TOKEN")
        .env_remove("RUSTOVATE_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["error"].as_str().unwrap().contains("RUSTOVATE_TOKEN"));
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
        "/../../examples/renovate-runner.pkl"
    );
    let output = cli(&["config", "check", path, "--json"]);
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["valid"], true);
    assert!(output.stderr.is_empty());

    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("Rustovate.pkl"),
        rustovate_config::SCHEMA,
    )
    .unwrap();
    let invalid = directory.path().join("rustovate.pkl");
    std::fs::write(&invalid, "amends \"Rustovate.pkl\"\nunknownPolicy = true\n").unwrap();
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
    assert!(stderr.contains("unknownPolicy") && stderr.contains("rustovate.pkl"));
    assert!(!stderr.contains("\x1b["));

    let output = cli(&["config", "schema", "--json"]);
    assert!(output.status.success());
    assert_eq!(output.stdout, rustovate_config::SCHEMA.as_bytes());
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
    std::fs::create_dir(dir.path().join(".github")).unwrap();
    std::fs::write(
        dir.path().join(".github/rustovate.pkl"),
        "amends \"../Rustovate.pkl\"\nprs { dashboard = true }\ndefaults { waitHours = 0 }\n",
    )
    .unwrap();
    for vendored in [false, true] {
        if vendored {
            std::fs::write(dir.path().join("Rustovate.pkl"), rustovate_config::SCHEMA).unwrap();
        }
        let output = cli(&["config", "check", dir.path().to_str().unwrap(), "--json"]);
        assert!(output.status.success(), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["valid"], true);
    }
}

#[test]
fn lookup_config_dir_validates_all_matches_before_repository_reads() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Rustovate.pkl"), rustovate_config::SCHEMA).unwrap();
    std::fs::write(dir.path().join("first.pkl"), "amends \"Rustovate.pkl\"\n").unwrap();
    let bad = dir.path().join("second.pkl");
    std::fs::write(&bad, "amends \"Rustovate.pkl\"\nunknownPolicy = true\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rustovate"))
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
        .env("RUSTOVATE_TOKEN", "offline")
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
