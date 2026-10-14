#![forbid(unsafe_code)]

use rustovate_conformance::{Classification as Class, Rules, compare, parse_logs};
use rustovate_report::{Report, Status};
use serde_json::json;
use std::{path::Path, process::Command};

const LOG: &str = include_str!("fixtures/renovate.jsonl");
const REPORT: &[u8] = include_bytes!("fixtures/report.json");

fn rules() -> Rules {
    Rules::from_json(include_bytes!("../departures.json")).unwrap()
}

#[test]
fn hand_written_output_classifies_all_four_cases_and_keeps_registry_identity() {
    let renovate = parse_logs(LOG).unwrap();
    let report = Report::from_json(REPORT).unwrap();
    let triage = compare(&renovate, &report, &rules()).unwrap();
    for (class, count) in [
        (Class::RustovateMissing, 1),
        (Class::RustovateExtra, 1),
        (Class::FieldMismatch, 2),
        (Class::DeliberateDeparture, 2),
    ] {
        assert_eq!(
            triage
                .differences
                .iter()
                .filter(|d| d.classification == class)
                .count(),
            count,
            "{class:?}"
        );
    }
    assert!(triage.markdown().contains("docs/departures.md#departure-7"));
    assert!(
        triage
            .differences
            .iter()
            .filter_map(|d| d.departure.as_ref())
            .all(|p| p.row == 7)
    );
    let npm = renovate.repositories[0]
        .package_files
        .iter()
        .find(|f| f.path == "package.json")
        .unwrap();
    assert_eq!(npm.dependencies[0].package, "actual-library");
    assert_eq!(
        npm.dependencies[0].updates[0].group.as_deref(),
        Some("javascript")
    );
    assert_eq!(
        Report::from_json(&serde_json::to_vec(&report).unwrap()).unwrap(),
        report
    );
}

#[test]
fn equal_proposals_are_order_independent_and_never_paired_by_position() {
    let mut a = parse_logs(LOG).unwrap();
    let dep = &mut a.repositories[0].package_files[0].dependencies[0];
    let mut u = rustovate_report::Update {
        new_value: Some("5".into()),
        new_version: Some("5".into()),
        new_digest: None,
        update_type: "major".into(),
        group: None,
    };
    dep.updates.push(u.clone());
    u.new_value = Some("6".into());
    u.new_version = Some("6".into());
    dep.updates.push(u);
    let mut b = a.clone();
    b.repositories[0].package_files[0].dependencies[0]
        .updates
        .reverse();
    assert!(compare(&a, &b, &rules()).unwrap().differences.is_empty());
    b.repositories[0].package_files[0].dependencies[0]
        .updates
        .retain(|u| u.new_value.as_deref() != Some("6"));
    let triage = compare(&a, &b, &rules()).unwrap();
    assert_eq!(triage.differences.len(), 1);
    assert_eq!(
        triage.differences[0].classification,
        Class::RustovateMissing
    );
}

#[test]
fn departures_do_not_hide_version_changes_or_other_managers() {
    let a = parse_logs(LOG).unwrap();
    let mut b = a.clone();
    let dep = &mut b.repositories[0].package_files[0].dependencies[0];
    dep.updates.push(rustovate_report::Update {
        new_value: None,
        new_version: Some("99".into()),
        new_digest: Some("digest".into()),
        update_type: "pin".into(),
        group: None,
    });
    assert_eq!(
        compare(&a, &b, &rules()).unwrap().differences[0].classification,
        Class::RustovateExtra
    );
    let mut value: serde_json::Value =
        serde_json::from_slice(include_bytes!("../departures.json")).unwrap();
    value["rules"][0]["departure"] = json!(999);
    assert!(Rules::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut b = a.clone();
    let npm = b.repositories[0]
        .package_files
        .iter_mut()
        .find(|f| f.path == "package.json")
        .unwrap();
    npm.dependencies[0].updates[0].new_digest = Some("digest".into());
    assert_eq!(
        compare(&a, &b, &rules()).unwrap().differences[0].classification,
        Class::FieldMismatch
    );
    let npm = b.repositories[0]
        .package_files
        .iter_mut()
        .find(|f| f.path == "package.json")
        .unwrap();
    npm.dependencies[0].updates[0].new_digest = None;
    npm.dependencies[0].updates[0].update_type = "patch".into();
    let triage = compare(&a, &b, &rules()).unwrap();
    assert_eq!(triage.differences[0].field, "updates.updateType");
}

#[test]
fn unsupported_or_incomplete_inputs_fail_without_false_success() {
    for logs in [
        "nothing",
        "{broken",
        r#"{"msg":"packageFiles with updates","repository":"x/y"}"#,
        r#"{"msg":"packageFiles with updates","repository":"x/y","packageFiles":{"npm":[{"packageFile":"package.json","deps":[{"currentValue":"1"}]}]}}"#,
    ] {
        assert!(parse_logs(logs).is_err());
    }
    assert!(parse_logs(&format!("{LOG}\n{{\"level\":50,\"msg\":\"failed\"}}")).is_err());
    assert!(parse_logs(&format!("Unsupported node environment\n{LOG}")).is_ok());
    let mut report = parse_logs(LOG).unwrap();
    report.repositories[0].status = Status::Error;
    report.repositories[0].reason = Some("failed extraction".into());
    assert!(compare(&parse_logs(LOG).unwrap(), &report, &rules()).is_err());
}

#[test]
fn aliases_taxonomy_latest_snapshot_and_empty_repo_coverage_are_explicit() {
    let log = r#"{"repository":"x/y","msg":"packageFiles with updates","packageFiles":{"gomod":[{"packageFile":"go.mod","deps":[{"depName":"example.com/module","currentValue":"1","updates":[{"newDigest":"abc","updateType":"pinDigest"}]}]}]}}
{"repository":"empty/repo","msg":"packageFiles with updates","packageFiles":{}}"#;
    let report = parse_logs(log).unwrap();
    assert_eq!(
        report.repositories[1].package_files[0].dependencies[0].manager,
        "go"
    );
    assert_eq!(
        report.repositories[1].package_files[0].dependencies[0].updates[0].update_type,
        "pin"
    );
    let empty = Report {
        schema_version: 1,
        repositories: vec![],
    };
    assert_eq!(
        compare(&report, &empty, &rules())
            .unwrap()
            .differences
            .len(),
        2
    );
    let latest = parse_logs(&format!(
        "{LOG}\n{{\"repository\":\"demo/rustovate-sample\",\"msg\":\"packageFiles with updates\",\"packageFiles\":{{}}}}"
    ));
    // A stale branch summary cannot enrich a newer extraction snapshot.
    assert!(latest.unwrap().repositories[0].package_files.is_empty());
}

#[test]
fn cli_writes_both_outputs_offline_and_rejects_missing_requested_repository() {
    let dir = tempfile::tempdir().unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report_path = root.join("tests/fixtures/report.json");
    let log_path = root.join("tests/fixtures/renovate.jsonl");
    let args = [
        "--report",
        report_path.to_str().unwrap(),
        "--renovate-log",
        log_path.to_str().unwrap(),
    ];
    let run = Command::new(env!("CARGO_BIN_EXE_rustovate-conformance"))
        .args(args)
        .arg("--output")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(dir.path().join("triage.md").exists());
    let output: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("triage.json")).unwrap()).unwrap();
    assert_eq!(output["schemaVersion"], 1);
    assert_eq!(output["differences"].as_array().unwrap().len(), 6);
    let run = Command::new(env!("CARGO_BIN_EXE_rustovate-conformance"))
        .args(args)
        .arg("absent/repo")
        .arg("--output")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!run.status.success());
}
