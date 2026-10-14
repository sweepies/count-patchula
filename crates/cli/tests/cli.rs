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
fn phase_one_stubs_are_runtime_errors() {
    for command in ["lookup", "run"] {
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
