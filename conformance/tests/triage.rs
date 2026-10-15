#![forbid(unsafe_code)]
use rustovate_conformance::{Classification as Class, Rules, compare, parse_logs};
use rustovate_report::Report;
use serde_json::{Value, json};

fn rules() -> Rules {
    Rules::from_json(include_bytes!("../departures.json")).unwrap()
}
fn snapshot(config: Value) -> Report {
    parse_logs(
        &json!({"repository":"example/demo","msg":"packageFiles with updates","config":config})
            .to_string(),
    )
    .unwrap()
}
fn dep(name: &str, source: &str, current: &str) -> Value {
    json!({"depName":name,"packageName":name,"datasource":source,"currentValue":current,"currentVersion":current,"updates":[]})
}
fn policy() -> Value {
    json!({"versions":"exact","waitHours":24,"windows":[],"merge":false,"labels":[],"commitPrefix":"chore(deps):","releaseNotes":true,"bodyNotes":[]})
}
fn update(value: &str, kind: &str) -> rustovate_report::Update {
    serde_json::from_value(json!({"newValue":value,"newVersion":value,"newDigest":null,"updateType":kind,"group":null,"policy":policy()})).unwrap()
}

#[test]
fn invalid_or_absent_baseline_is_coverage_and_empty_success_is_comparable() {
    let failed = parse_logs(r#"{"repository":"example/demo","msg":"Repository finished","result":"config-validation","exitCode":5}"#).unwrap();
    let actual =
        snapshot(json!({"npm":[{"packageFile":"package.json","deps":[dep("lib","npm","1.0.0")]}]}));
    let triage = compare(&failed, &actual, &rules()).unwrap();
    assert!(triage.differences.is_empty());
    assert_eq!(triage.no_baseline.len(), 1);
    assert!(triage.markdown().contains("No baseline: example/demo"));
    let recovered = parse_logs(
        r#"{"repository":"example/demo","msg":"Repository finished","result":"config-validation"}
{"repository":"example/demo","msg":"packageFiles with updates","config":{}}"#,
    )
    .unwrap();
    assert_eq!(recovered.repositories.len(), 1);
    assert_eq!(
        recovered.repositories[0].status,
        rustovate_report::Status::Complete
    );
    let mut absent = actual.clone();
    absent.repositories.clear();
    assert_eq!(
        compare(&absent, &actual, &rules())
            .unwrap()
            .no_baseline
            .len(),
        1
    );
    let empty = snapshot(json!({}));
    let triage = compare(&empty, &actual, &rules()).unwrap();
    assert!(triage.no_baseline.is_empty());
    assert_eq!(triage.differences[0].classification, Class::RustovateExtra);
}

#[test]
fn bun_alias_is_manifest_and_source_scoped_and_preserves_changes() {
    let baseline =
        snapshot(json!({"bun":[{"packageFile":"package.json","deps":[dep("lib","npm","1.0.0")]}]}));
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0].manager = "npm".into();
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .is_empty()
    );
    actual.repositories[0].package_files[0].dependencies[0]
        .current
        .value = Some("2.0.0".into());
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.field == "current.value")
    );
    let other = snapshot(
        json!({"bun":[{"packageFile":"package.json","deps":[dep("lib","other-source","1.0.0")]}]}),
    );
    assert!(
        compare(&other, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.field == "dependency")
    );
}

#[test]
fn api_inventory_and_identical_yaml_duplicates_are_audited_but_proposals_survive() {
    let image = dep("example/image", "docker", "1.2.3");
    let api = dep("Deployment", "kubernetes-api", "apps/v1");
    let baseline = snapshot(
        json!({"kubernetes":[{"packageFile":"deploy.yaml","deps":[image.clone(),api.clone()]}],"helm-values":[{"packageFile":"deploy.yaml","deps":[image.clone()]}]}),
    );
    let actual = snapshot(json!({"kubernetes":[{"packageFile":"deploy.yaml","deps":[image]}]}));
    let triage = compare(&baseline, &actual, &rules()).unwrap();
    assert!(triage.differences.is_empty());
    assert_eq!(
        triage
            .exclusions
            .iter()
            .map(|e| e.dependencies)
            .sum::<usize>(),
        2
    );
    assert!(triage.exclusions.iter().any(|e| e.departure == 26));
    assert!(triage.exclusions.iter().any(|e| e.departure == 27));
    let mut changed = baseline.clone();
    let deps = &mut changed.repositories[0].package_files[0].dependencies;
    deps.iter_mut()
        .find(|d| d.name == "Deployment")
        .unwrap()
        .updates
        .push(update("apps/v2", "major"));
    deps.iter_mut()
        .find(|d| d.manager == "helm-values")
        .unwrap()
        .current
        .digest = Some("sha256:different".into());
    let triage = compare(&changed, &actual, &rules()).unwrap();
    assert_eq!(triage.differences.len(), 2);
    assert!(triage.exclusions.is_empty());
    assert!(
        compare(&baseline, &snapshot(json!({})), &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.manager.as_deref() == Some("helm-values"))
    );
}

#[test]
fn helm_registry_and_checksum_adapter_does_not_hide_registry_changes() {
    let mut chart = dep("chart", "helm", "1.0.0");
    chart["registryUrls"] = json!(["https://charts.example.org"]);
    chart["updates"] = json!([{"newValue":"1.0.1","newVersion":"1.0.1","newDigest":"archive-hash","updateType":"patch"}]);
    let baseline = snapshot(json!({"flux":[{"packageFile":"release.yaml","deps":[chart]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    assert_eq!(d.package, "https://charts.example.org|chart");
    d.updates[0].new_digest = None;
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .is_empty()
    );
    actual.repositories[0].package_files[0].dependencies[0].package =
        "https://different.example.org|chart".into();
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].field,
        "package"
    );
}

#[test]
fn unknown_current_and_digest_metadata_are_not_target_exemptions() {
    let baseline = snapshot(
        json!({"github-actions":[{"packageFile":"ci.yml","deps":[{"depName":"example/action","currentValue":"v2","updates":[{"newValue":"v2","newDigest":"aaa","updateType":"pinDigest"}]}]}]}),
    );
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.current.version = Some("v2.4.0".into());
    d.updates[0].new_version = Some("v2".into());
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .is_empty()
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0].new_digest =
        Some("bbb".into());
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.field == "updates.newDigest" && d.classification == Class::FieldMismatch)
    );
}

#[test]
fn latest_pin_departure_requires_exact_policy_and_literal_latest() {
    let baseline = snapshot(
        json!({"mise":[{"packageFile":"mise.toml","deps":[dep("tool","github-releases","latest")]}]}),
    );
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0]
        .updates
        .push(update("1.2.3", "pin"));
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0]
            .departure
            .as_ref()
            .unwrap()
            .row,
        29
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0]
        .policy
        .as_mut()
        .unwrap()
        .versions = Some("preserve".into());
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].classification,
        Class::RustovateExtra
    );
    let mut numeric = baseline.clone();
    numeric.repositories[0].package_files[0].dependencies[0]
        .current
        .value = Some("1".into());
    assert!(
        compare(&numeric, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.classification != Class::DeliberateDeparture)
    );
}

#[test]
fn indirect_go_and_test_compose_departures_do_not_extend_to_direct_or_production() {
    let mut indirect = dep("example.org/lib", "go", "v1.2.3");
    indirect["depType"] = json!("indirect");
    let baseline = snapshot(json!({"gomod":[{"packageFile":"go.mod","deps":[indirect]}]}));
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0]
        .updates
        .push(update("v1.3.0", "minor"));
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0]
            .departure
            .as_ref()
            .unwrap()
            .row,
        30
    );
    let mut direct = baseline.clone();
    direct.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .kinds
        .clear();
    assert_eq!(
        compare(&direct, &actual, &rules()).unwrap().differences[0].classification,
        Class::RustovateExtra
    );
    let empty = snapshot(json!({}));
    let mut actual = snapshot(
        json!({"compose":[{"packageFile":"test/docker-compose.demo.yml","deps":[dep("image","docker","1.2.3")]}]}),
    );
    actual.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .kinds = vec!["container-image".into()];
    assert_eq!(
        compare(&empty, &actual, &rules()).unwrap().differences[0]
            .departure
            .as_ref()
            .unwrap()
            .row,
        31
    );
    actual.repositories[0].package_files[0].path = "production/docker-compose.demo.yml".into();
    assert_eq!(
        compare(&empty, &actual, &rules()).unwrap().differences[0].classification,
        Class::RustovateExtra
    );
}

#[test]
fn npm_exact_range_rules_require_the_same_satisfying_target_and_policy() {
    let mut lib = dep("lib", "npm", "^1.2.0");
    lib["currentVersion"] = json!("1.2.0");
    lib["updates"] = json!([{"newValue":"^1.3.0","newVersion":"1.3.1","updateType":"minor"}]);
    let baseline = snapshot(json!({"npm":[{"packageFile":"package.json","deps":[lib]}]}));
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0].updates[0] = update("1.3.1", "minor");
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0]
            .departure
            .as_ref()
            .unwrap()
            .row,
        25
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0] = update("1.3.2", "minor");
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.classification == Class::FieldMismatch)
    );
    let baseline = snapshot(
        json!({"npm":[{"packageFile":"package.json","deps":[dep("lib","npm","^1.2.0")]}]}),
    );
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0]
        .updates
        .push(update("1.2.3", "pin"));
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0]
            .departure
            .as_ref()
            .unwrap()
            .row,
        25
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0] = update("2.0.0", "pin");
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].classification,
        Class::RustovateExtra
    );
}

#[test]
fn patch_track_departure_requires_an_identical_minor_and_a_newer_manifest_edit() {
    let mut lib = dep("lib", "npm", "1.2.0");
    lib["updates"] = json!([{"newValue":"1.3.0","newVersion":"1.3.0","updateType":"minor"}]);
    let baseline = snapshot(json!({"npm":[{"packageFile":"package.json","deps":[lib]}]}));
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0]
        .updates
        .push(update("1.2.1", "patch"));
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0]
            .departure
            .as_ref()
            .unwrap()
            .row,
        33
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[1] = update("1.2.0", "patch");
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].classification,
        Class::RustovateExtra
    );
    actual.repositories[0].package_files[0].dependencies[0].updates =
        vec![update("1.2.1", "patch")];
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.classification != Class::DeliberateDeparture)
    );
}

#[test]
fn go_major_path_rule_preserves_incompatible_releases_and_other_sources() {
    let mut lib = dep("github.com/example/lib", "go", "v1.2.3");
    lib["updates"] = json!([{"newValue":"v2.0.0","newVersion":"v2.0.0","updateType":"major"}]);
    let baseline = snapshot(json!({"gomod":[{"packageFile":"go.mod","deps":[lib]}]}));
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0]
        .updates
        .clear();
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0]
            .departure
            .as_ref()
            .unwrap()
            .row,
        34
    );
    let mut incompatible = baseline.clone();
    incompatible.repositories[0].package_files[0].dependencies[0].updates[0] =
        update("v2.0.0+incompatible", "major");
    assert_eq!(
        compare(&incompatible, &actual, &rules())
            .unwrap()
            .differences[0]
            .classification,
        Class::RustovateMissing
    );
    actual.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .source = "github-releases".into();
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].classification,
        Class::RustovateMissing
    );
}

#[test]
fn cli_handles_a_requested_repo_present_only_in_the_actual_report() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("actual.json");
    std::fs::write(&report, serde_json::to_vec(&snapshot(json!({}))).unwrap()).unwrap();
    let log = dir.path().join("baseline.jsonl");
    std::fs::write(
        &log,
        r#"{"repository":"example/other","msg":"packageFiles with updates","config":{}}"#,
    )
    .unwrap();
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_rustovate-conformance"))
        .arg("--report")
        .arg(report)
        .arg("--renovate-log")
        .arg(log)
        .arg("--output")
        .arg(dir.path())
        .arg("example/demo")
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let output: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("triage.json")).unwrap()).unwrap();
    assert_eq!(output["noBaseline"][0]["repository"], "example/demo");
    assert!(output["differences"].as_array().unwrap().is_empty());
}
