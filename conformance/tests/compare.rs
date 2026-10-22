#![forbid(unsafe_code)]

use patchula_conformance::{
    Classification as Class, Rules, compare, parse_logs, parse_logs_with_diagnostics,
};
use patchula_report::{Report, Status};
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
        (Class::PatchulaMissing, 1),
        (Class::PatchulaExtra, 1),
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
    assert!(
        triage
            .markdown()
            .contains("docs/src/content/docs/renovate/departures.md#explicit-defaults")
    );
    assert!(
        triage
            .differences
            .iter()
            .filter_map(|d| d.departure.as_ref())
            .all(|p| p.id == "explicit-defaults")
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
    let mut u = patchula_report::Update {
        new_value: Some("5".into()),
        new_version: Some("5".into()),
        new_digest: None,
        update_type: "major".into(),
        group: None,
        policy: None,
        release_age: None,
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
    assert_eq!(triage.differences[0].classification, Class::PatchulaMissing);
}

#[test]
fn departures_do_not_hide_version_changes_or_other_managers() {
    let a = parse_logs(LOG).unwrap();
    let mut b = a.clone();
    let dep = &mut b.repositories[0].package_files[0].dependencies[0];
    dep.updates.push(patchula_report::Update {
        new_value: None,
        new_version: Some("99".into()),
        new_digest: Some("digest".into()),
        update_type: "pin".into(),
        group: None,
        policy: None,
        release_age: None,
    });
    assert_eq!(
        compare(&a, &b, &rules()).unwrap().differences[0].classification,
        Class::PatchulaExtra
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
    assert!(compare(&a, &b, &rules()).unwrap().differences.is_empty());
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
fn npm_integrity_unknown_groups_and_empty_files_are_counted_without_hiding_real_changes() {
    let log = r#"{"repository":"x/y","msg":"packageFiles with updates","config":{"custom.regex":[{"packageFile":"versions.ts","deps":[{"depName":"spec","datasource":"npm","currentValue":"1.0.0","updates":[{"newValue":"1.0.1","newVersion":"1.0.1","newDigest":"sha512-example","updateType":"patch"}]}]}],"npm":[{"packageFile":"empty/package.json","deps":[]}]}}"#;
    let a = parse_logs(log).unwrap();
    let mut b = a.clone();
    b.repositories[0]
        .package_files
        .retain(|f| !f.dependencies.is_empty());
    let dep = &mut b.repositories[0].package_files[0].dependencies[0];
    dep.updates[0].new_digest = None;
    dep.updates[0].group = Some("grouped later".into());
    let triage = compare(&a, &b, &rules()).unwrap();
    assert!(triage.differences.is_empty());
    assert_eq!(triage.normalizations["npm-newDigest-ignored"], 1);
    assert_eq!(triage.normalizations["groups-unavailable-skipped"], 1);
    assert_eq!(triage.normalizations["empty-package-file-ignored"], 1);
    let mut grouped = a.clone();
    grouped.repositories[0]
        .package_files
        .iter_mut()
        .find(|f| !f.dependencies.is_empty())
        .unwrap()
        .dependencies[0]
        .updates[0]
        .group = Some("expected".into());
    assert_eq!(
        compare(&grouped, &b, &rules()).unwrap().differences[0].field,
        "updates.group"
    );
    let dep = &mut b.repositories[0].package_files[0].dependencies[0];
    dep.updates[0].new_value = Some("9.0.0".into());
    assert_eq!(
        compare(&a, &b, &rules()).unwrap().differences[0].classification,
        Class::FieldMismatch
    );
    let mut non_npm = a.clone();
    non_npm.repositories[0]
        .package_files
        .iter_mut()
        .find(|f| !f.dependencies.is_empty())
        .unwrap()
        .dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .source = "github-releases".into();
    assert!(
        compare(&non_npm, &b, &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.field == "updates.newDigest")
    );
}

fn policy() -> patchula_report::UpdatePolicy {
    serde_json::from_value(json!({"versions":"exact","waitHours":24,"windows":[],"merge":false,"labels":[],"commitPrefix":"chore(deps):","releaseNotes":true,"bodyNotes":[]})).unwrap()
}

#[test]
fn renovate_snapshot_retains_its_own_evaluation_time() {
    let mut record = json!({"repository":"example/demo","msg":"packageFiles with updates","time":"2026-10-15T06:40:00Z","config":{"npm":[{"packageFile":"package.json","deps":[{"depName":"example","datasource":"npm","currentValue":"1.0.0","updates":[]}]}]}});
    for time in ["2026-10-15T06:40:00Z", "malformed"] {
        record["time"] = json!(time);
        let report = parse_logs(&record.to_string()).unwrap();
        assert_eq!(
            report.repositories[0].package_files[0].dependencies[0]
                .metadata
                .as_ref()
                .unwrap()
                .lookup
                .as_ref()
                .unwrap()
                .evaluated_at,
            time
        );
    }
    record.as_object_mut().unwrap().remove("time");
    let report = parse_logs(&record.to_string()).unwrap();
    assert!(
        report.repositories[0].package_files[0].dependencies[0]
            .metadata
            .as_ref()
            .unwrap()
            .lookup
            .is_none()
    );
}

#[test]
fn release_age_departure_requires_newer_target_and_timestamp_evidence_inside_window() {
    let mut a = parse_logs(LOG).unwrap();
    let dep = &mut a.repositories[0].package_files[0].dependencies[0];
    dep.manager = "npm".into();
    dep.current.value = Some("1.0.0".into());
    dep.updates = vec![serde_json::from_value(json!({"newValue":"1.0.1","newVersion":"1.0.1","newDigest":null,"updateType":"patch","group":null})).unwrap()];
    dep.metadata = Some(serde_json::from_value(json!({"source":"npm","kinds":[],"locations":[],"lookup":{"evaluatedAt":"2026-10-15T12:00:00Z","disabledUpdates":[],"rejected":[]}})).unwrap());
    let mut b = a.clone();
    let update = &mut b.repositories[0].package_files[0].dependencies[0].updates[0];
    update.new_value = Some("1.0.2".into());
    update.new_version = Some("1.0.2".into());
    update.policy = Some(policy());
    update.release_age = Some(patchula_report::ReleaseAge {
        published_at: "2026-10-14T00:00:00Z".into(),
        evaluated_at: "2026-10-15T12:00:00Z".into(),
    });
    let diffs = compare(&a, &b, &rules()).unwrap().differences;
    assert_eq!(diffs.len(), 2);
    assert!(
        diffs
            .iter()
            .all(|d| d.classification == Class::DeliberateDeparture
                && d.departure.as_ref().unwrap().id == "minimum-release-age")
    );
    for date in ["2026-10-15T01:00:00Z", "2026-10-11T12:00:00Z", "malformed"] {
        let mut changed = b.clone();
        changed.repositories[0].package_files[0].dependencies[0].updates[0]
            .release_age
            .as_mut()
            .unwrap()
            .published_at = date.into();
        assert!(
            compare(&a, &changed, &rules())
                .unwrap()
                .differences
                .iter()
                .all(|d| d.classification == Class::FieldMismatch)
        );
    }
    // Observed single-repository rerun: unocss 66.11.0 was still held at 72 h 17 min.
    let mut observed = b.clone();
    observed.repositories[0].package_files[0].dependencies[0].updates[0]
        .release_age
        .as_mut()
        .unwrap()
        .published_at = "2026-10-12T06:41:36Z".into();
    let mut baseline = a.clone();
    baseline.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .lookup
        .as_mut()
        .unwrap()
        .evaluated_at = "2026-10-15T06:59:00Z".into();
    let diffs = compare(&baseline, &observed, &rules()).unwrap().differences;
    assert_eq!(diffs.len(), 2);
    assert!(
        diffs
            .iter()
            .all(|d| d.classification == Class::DeliberateDeparture
                && d.departure.as_ref().unwrap().id == "minimum-release-age")
    );
    assert!(
        compare(&b, &a, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.classification == Class::FieldMismatch)
    );
    // The target crosses 96 h between the independent evaluations.
    b.repositories[0].package_files[0].dependencies[0].updates[0]
        .release_age
        .as_mut()
        .unwrap()
        .evaluated_at = "2026-10-18T00:00:00Z".into();
    for (time, expected) in [
        (Some("2026-10-17T23:59:59Z"), Class::DeliberateDeparture),
        (Some("2026-10-18T00:00:00Z"), Class::FieldMismatch),
        (Some("2026-10-14T23:59:59Z"), Class::FieldMismatch),
        (Some("malformed"), Class::FieldMismatch),
        (None, Class::FieldMismatch),
    ] {
        a.repositories[0].package_files[0].dependencies[0]
            .metadata
            .as_mut()
            .unwrap()
            .lookup = time.map(|time| patchula_report::LookupEvidence {
            catalog: None,
            evaluated_at: time.into(),
            disabled_updates: vec![],
            rejected: vec![],
        });
        assert!(
            compare(&a, &b, &rules())
                .unwrap()
                .differences
                .iter()
                .all(|d| d.classification == expected),
            "{time:?}"
        );
    }
    b.repositories[0].package_files[0].dependencies[0].updates[0].release_age = None;
    assert!(
        compare(&a, &b, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.classification == Class::FieldMismatch)
    );
}

#[test]
fn older_patchula_target_requires_the_exact_held_back_release_and_24_hour_evidence() {
    let mut a = parse_logs(LOG).unwrap();
    let dep = &mut a.repositories[0].package_files[0].dependencies[0];
    dep.manager = "npm".into();
    dep.updates = vec![serde_json::from_value(json!({"newValue":"1.0.2","newVersion":"1.0.2","newDigest":null,"updateType":"patch","group":null})).unwrap()];
    let mut b = a.clone();
    let dep = &mut b.repositories[0].package_files[0].dependencies[0];
    dep.updates[0].new_value = Some("1.0.1".into());
    dep.updates[0].new_version = Some("1.0.1".into());
    dep.updates[0].policy = Some(policy());
    dep.updates[0].release_age = Some(patchula_report::ReleaseAge {
        published_at: "2026-10-01T00:00:00Z".into(),
        evaluated_at: "2026-10-15T12:00:00Z".into(),
    });
    dep.metadata = Some(serde_json::from_value(json!({"source":"npm","kinds":[],"locations":[],"heldBack":[{"version":"1.0.2","updateType":"patch","publishedAt":"2026-10-14T19:00:00Z","eligibleAt":"2026-10-15T19:00:00Z"}]})).unwrap());
    let diffs = compare(&a, &b, &rules()).unwrap().differences;
    assert_eq!(diffs.len(), 2);
    assert!(
        diffs
            .iter()
            .all(|d| d.classification == Class::DeliberateDeparture
                && d.departure.as_ref().unwrap().id == "minimum-release-age")
    );
    let mut summarized = b.clone();
    let held = &mut summarized.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .held_back[0];
    held.version = "1.0.4".into();
    held.versions = vec!["1.0.2".into(), "1.0.3".into(), "1.0.4".into()];
    held.count = 3;
    assert!(
        compare(&a, &summarized, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.classification == Class::DeliberateDeparture)
    );
    for (field, value) in [
        ("version", "1.0.3"),
        ("updateType", "minor"),
        ("publishedAt", "malformed"),
        ("publishedAt", "2026-10-15T13:00:00Z"),
        ("eligibleAt", "malformed"),
        ("eligibleAt", "2026-10-15T12:00:00Z"),
        ("eligibleAt", "2026-10-17T19:00:00Z"),
    ] {
        let mut changed = serde_json::to_value(&b).unwrap();
        changed["repositories"][0]["packageFiles"][0]["dependencies"][0]["metadata"]["heldBack"]
            [0][field] = json!(value);
        let changed: patchula_report::Report = serde_json::from_value(changed).unwrap();
        assert!(
            compare(&a, &changed, &rules())
                .unwrap()
                .differences
                .iter()
                .all(|d| d.classification == Class::FieldMismatch),
            "{field}={value}"
        );
    }
    for hours in [0, 72] {
        let mut changed = b.clone();
        changed.repositories[0].package_files[0].dependencies[0].updates[0]
            .policy
            .as_mut()
            .unwrap()
            .wait_hours = hours;
        assert!(
            compare(&a, &changed, &rules())
                .unwrap()
                .differences
                .iter()
                .all(|d| d.classification == Class::FieldMismatch)
        );
    }
    let mut ranged = a.clone();
    ranged.repositories[0].package_files[0].dependencies[0].updates[0].new_value =
        Some("^1.0.2".into());
    let diffs = compare(&ranged, &b, &rules()).unwrap().differences;
    assert_eq!(
        diffs
            .iter()
            .find(|d| d.field == "updates.newValue")
            .unwrap()
            .classification,
        Class::FieldMismatch
    );
    b.repositories[0].package_files[0].dependencies[0].metadata = None;
    assert!(
        compare(&a, &b, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.classification == Class::FieldMismatch)
    );
}

#[test]
fn npm_exact_major_departure_requires_identical_resolved_targets_and_explicit_policy() {
    let mut a = parse_logs(LOG).unwrap();
    let dep = &mut a.repositories[0].package_files[0].dependencies[0];
    dep.manager = "npm".into();
    dep.current.value = Some("^5.9.3".into());
    dep.updates = vec![serde_json::from_value(json!({"newValue":"^7.0.0","newVersion":"7.0.2","newDigest":null,"updateType":"major","group":null})).unwrap()];
    let mut b = a.clone();
    let update = &mut b.repositories[0].package_files[0].dependencies[0].updates[0];
    update.new_value = Some("7.0.2".into());
    update.policy = Some(policy());
    let diff = compare(&a, &b, &rules()).unwrap().differences;
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].departure.as_ref().unwrap().id, "npm-range-edits");
    b.repositories[0].package_files[0].dependencies[0].updates[0].new_version =
        Some("7.0.3".into());
    assert!(
        compare(&a, &b, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.classification == Class::FieldMismatch)
    );
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
        actions: None,
        schema_version: patchula_report::SCHEMA_VERSION,
        repositories: vec![],
        stats: None,
    };
    assert_eq!(
        compare(&report, &empty, &rules())
            .unwrap()
            .differences
            .len(),
        2
    );
    let latest = parse_logs(&format!(
        "{LOG}\n{{\"repository\":\"demo/patchula-sample\",\"msg\":\"packageFiles with updates\",\"packageFiles\":{{}}}}"
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
    let run = Command::new(env!("CARGO_BIN_EXE_patchula-conformance"))
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
    let run = Command::new(env!("CARGO_BIN_EXE_patchula-conformance"))
        .args(args)
        .arg("absent/repo")
        .arg("--output")
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!run.status.success());
}

#[test]
fn real_lookup_shape_retains_skipped_and_digest_only_deps_and_audits_exclusion() {
    let parsed = parse_logs_with_diagnostics(include_str!("fixtures/lookup.jsonl")).unwrap();
    let repo = &parsed.report.repositories[0];
    assert_eq!(repo.package_files.len(), 4);
    assert_eq!(
        repo.package_files
            .iter()
            .map(|f| f.dependencies.len())
            .sum::<usize>(),
        6
    );
    let cargo = repo
        .package_files
        .iter()
        .find(|f| f.path == "Cargo.toml")
        .unwrap();
    assert_eq!(cargo.dependencies.len(), 1);
    assert_eq!(cargo.dependencies[0].updates.len(), 1);
    let actions = repo
        .package_files
        .iter()
        .find(|f| f.path.ends_with("ci.yml"))
        .unwrap();
    assert_eq!(actions.dependencies[0].current.value, None);
    assert_eq!(
        actions.dependencies[0].current.digest.as_deref(),
        Some("aaaa")
    );
    assert_eq!(
        actions.dependencies[0].updates[0].new_digest.as_deref(),
        Some("bbbb")
    );
    assert_eq!(
        actions.dependencies[0].updates[0].group.as_deref(),
        Some("actions")
    );
    assert!(actions.dependencies[1].updates.is_empty());
    let mise = repo
        .package_files
        .iter()
        .find(|f| f.path == "mise.toml")
        .unwrap();
    assert_eq!(mise.dependencies[0].package, "example/tool");
    assert_eq!(
        mise.dependencies[0].updates[0].group.as_deref(),
        Some("tools")
    );
    assert_eq!(parsed.exclusions.len(), 1);
    let excluded = &parsed.exclusions[0];
    assert_eq!(
        (
            excluded.package_files,
            excluded.dependencies,
            excluded.updates
        ),
        (1, 1, 0)
    );
    assert_eq!(
        excluded.link,
        "docs/src/content/docs/renovate/departures.md#configuration-dependencies"
    );
    assert_eq!(
        parsed
            .warnings
            .iter()
            .filter(|w| w.field.ends_with("skipReason"))
            .count(),
        2
    );
    let empty = Report {
        actions: None,
        schema_version: patchula_report::SCHEMA_VERSION,
        repositories: vec![],
        stats: None,
    };
    let triage = compare(&parsed.report, &empty, &rules()).unwrap();
    assert_eq!(triage.differences.len(), 6);
    assert!(
        triage
            .differences
            .iter()
            .all(|d| d.classification == Class::PatchulaMissing && d.field == "dependency")
    );
    let reverse = compare(&empty, &parsed.report, &rules()).unwrap();
    assert!(reverse.differences.is_empty());
    assert_eq!(reverse.no_baseline.len(), 1);
    let mut missing_file = parsed.report.clone();
    missing_file.repositories[0]
        .package_files
        .retain(|f| f.path != "Cargo.toml");
    let triage = compare(&parsed.report, &missing_file, &rules()).unwrap();
    assert_eq!(triage.differences.len(), 1);
    assert_eq!(
        triage.differences[0].dependency.as_deref(),
        Some("sample-crate")
    );
}

#[test]
fn optional_output_changes_warn_without_echoing_ignored_payloads() {
    let log = json!({"repository":"example/optional","msg":"packageFiles with updates","config":{
        "future-manager":[{"packageFile":"manifest","futureFile":{"private":"not-echoed"},"deps":[
            {"packageName":"fallback","currentValue":42,"currentVersion":false,"currentDigest":{},
             "groupName":[],"datasource":{},"futureDep":"not-echoed","updates":[
                {"newValue":"2","newVersion":7,"newDigest":false,"groupName":{},"futureUpdate":"not-echoed"},
                {"newDigest":"abc","updateType":"future-type"}, {}, null
             ]},
            {"depName":"without-updates"}, {"depName":"bad-updates","updates":{}},
            {"depName":"null-updates","updates":null}
        ]}]
    }}).to_string();
    let summaries = r#"
{"repository":"example/optional","branch":{"upgrades":42}}
{"branch":{"upgrades":[{"depName":"fallback"}]}}
{"repository":"example/optional","branches":[{"upgrades":[{}, {"depName":"unextracted"}, {"depName":"fallback","updates":null}]}]}
"#;
    let parsed = parse_logs_with_diagnostics(&format!("{log}\n{summaries}")).unwrap();
    let deps = &parsed.report.repositories[0].package_files[0].dependencies;
    assert_eq!(deps.len(), 4);
    let fallback = deps.iter().find(|d| d.name == "fallback").unwrap();
    assert_eq!(fallback.manager, "future-manager");
    assert_eq!(fallback.updates.len(), 2);
    assert!(fallback.updates.iter().any(|u| u.update_type == "unknown"));
    assert!(
        fallback
            .updates
            .iter()
            .any(|u| u.update_type == "future-type")
    );
    for field in [
        "futureFile",
        "futureDep",
        "futureUpdate",
        "datasource",
        "currentValue",
        "currentVersion",
        "currentDigest",
        "newVersion",
        "newDigest",
        "updateType",
        "upgrades",
    ] {
        assert!(
            parsed.warnings.iter().any(|w| w.field.ends_with(field)),
            "{field}"
        );
    }
    let mut triage = compare(&parsed.report, &parsed.report, &rules()).unwrap();
    triage.warnings = parsed.warnings;
    let markdown = triage.markdown();
    assert!(markdown.contains("Parser warnings:"));
    assert!(!markdown.contains("not-echoed"));
    assert!(
        !serde_json::to_string(&triage)
            .unwrap()
            .contains("not-echoed")
    );
}

#[test]
fn required_snapshot_structure_and_ambiguous_dependency_identities_still_fail() {
    for config in [
        json!([]),
        json!({"npm":null}),
        json!({"npm":[{"packageFile":"package.json"}]}),
        json!({"npm":[{"packageFile":"package.json","deps":{}}]}),
        json!({"npm":[{"deps":[]}]}),
        json!({"npm":[{"packageFile":"package.json","deps":[{},null]}]}),
    ] {
        assert!(
            parse_logs(
                &json!({"repository":"x/y","msg":"packageFiles with updates","config":config})
                    .to_string()
            )
            .is_err()
        );
    }
}

#[test]
fn cli_accepts_one_sided_repo_coverage_and_writes_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("empty report.json");
    std::fs::write(&report, r#"{"schemaVersion":2,"repositories":[]}"#).unwrap();
    let run = Command::new(env!("CARGO_BIN_EXE_patchula-conformance"))
        .arg("--report")
        .arg(report)
        .arg("--renovate-log")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lookup.jsonl"))
        .arg("--output")
        .arg(dir.path())
        .arg("example/lookup")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let triage: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("triage.json")).unwrap()).unwrap();
    assert_eq!(triage["differences"].as_array().unwrap().len(), 6);
    assert!(!triage["warnings"].as_array().unwrap().is_empty());
    assert_eq!(
        triage["exclusions"][0]["departure"],
        "configuration-dependencies"
    );
    let md = std::fs::read_to_string(dir.path().join("triage.md")).unwrap();
    assert!(md.contains("configuration-dependencies"));
    assert!(md.contains("Parser warnings:"));
}

#[test]
fn ambiguous_optional_summaries_warn_and_new_snapshots_replace_exclusions() {
    let log = r#"{"repository":"x/y","msg":"packageFiles with updates","config":{"npm":[{"packageFile":"one.json","deps":[{"depName":"same","updates":[{"updateType":"major","newValue":"2"}]}]},{"packageFile":"two.json","deps":[{"depName":"same","updates":[]}]}],"renovate-config":[{"packageFile":"renovate.json","deps":[{"depName":"config-only"}]}]}}
{"repository":"x/y","branches":[{"upgrades":[{"depName":"same","newValue":"2","updateType":"major"},{"manager":"renovate-config","depName":"config-only","newValue":"3","updateType":"major"}]}]}"#;
    let parsed = parse_logs_with_diagnostics(log).unwrap();
    assert!(
        parsed
            .warnings
            .iter()
            .any(|w| w.message == "ambiguous branch summary; enrichment ignored")
    );
    assert!(
        !parsed
            .warnings
            .iter()
            .any(|w| w.message == "branch summary does not match an extracted dependency; ignored")
    );
    assert_eq!(
        parsed.report.repositories[0].package_files[0].dependencies[0]
            .updates
            .len(),
        1
    );
    assert_eq!(parsed.exclusions.len(), 1);
    let parsed = parse_logs_with_diagnostics(&format!(
        "{log}\n{}",
        r#"{"repository":"x/y","msg":"packageFiles with updates","config":{}}"#
    ))
    .unwrap();
    assert!(parsed.exclusions.is_empty());
    assert!(parsed.report.repositories[0].package_files.is_empty());
}

#[test]
fn distinct_occurrences_survive_and_compare_without_losing_rows() {
    let config = json!({"npm":[{"packageFile":"package.json","deps":[
        {"depName":"same","currentValue":"1","updates":[]},
        {"depName":"same","currentValue":"2","updates":[]},
        {"depName":"same","currentValue":"1","updates":[]}
    ]}]});
    let report = parse_logs(
        &json!({"repository":"x/y","msg":"packageFiles with updates","config":config}).to_string(),
    )
    .unwrap();
    let deps = &report.repositories[0].package_files[0].dependencies;
    assert_eq!(deps.len(), 2);
    assert_eq!(deps[0].occurrence.as_deref(), Some("1"));
    assert_eq!(deps[1].occurrence.as_deref(), Some("2"));
    assert!(
        compare(&report, &report, &rules())
            .unwrap()
            .differences
            .is_empty()
    );
    let mut changed = report.clone();
    changed.repositories[0].package_files[0].dependencies[1]
        .current
        .value = Some("3".into());
    let differences = compare(&report, &changed, &rules()).unwrap().differences;
    assert_eq!(differences.len(), 1);
    assert_eq!(differences[0].field, "current.value");
}

#[test]
fn cargo_pre1_departure_is_limited_to_identical_breaking_targets() {
    let mut a = parse_logs(LOG).unwrap();
    let dep = &mut a.repositories[0].package_files[0].dependencies[0];
    dep.manager = "cargo".into();
    dep.current.version = Some("0.2.3".into());
    dep.updates = vec![patchula_report::Update {
        new_value: Some("0.3.0".into()),
        new_version: Some("0.3.0".into()),
        new_digest: None,
        update_type: "minor".into(),
        group: None,
        policy: None,
        release_age: None,
    }];
    let mut b = a.clone();
    b.repositories[0].package_files[0].dependencies[0].updates[0].update_type = "major".into();
    let diff = compare(&a, &b, &rules()).unwrap().differences;
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].classification, Class::DeliberateDeparture);
    assert_eq!(
        diff[0].departure.as_ref().unwrap().id,
        "cargo-compatibility"
    );
    b.repositories[0].package_files[0].dependencies[0].updates[0].new_version =
        Some("0.4.0".into());
    assert!(
        compare(&a, &b, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.classification != Class::DeliberateDeparture)
    );
    a.repositories[0].package_files[0].dependencies[0]
        .current
        .version = Some("1.2.3".into());
    b = a.clone();
    b.repositories[0].package_files[0].dependencies[0].updates[0].update_type = "major".into();
    assert_eq!(
        compare(&a, &b, &rules()).unwrap().differences[0].classification,
        Class::FieldMismatch
    );
}
