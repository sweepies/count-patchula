#![forbid(unsafe_code)]
use patchula_conformance::{Classification as Class, Rules, compare, parse_logs};
use patchula_report::Report;
use serde_json::{Value, json};

fn rules() -> Rules {
    Rules::from_json(include_bytes!("../departures.json")).unwrap()
}
fn snapshot(config: Value) -> Report {
    parse_logs(
        &json!({"repository":"example/demo","msg":"packageFiles with updates","time":"2026-10-15T00:00:00Z","config":config})
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

#[test]
fn stable_node_departures_require_identity_major_order_and_stable_policy() {
    for (manager, file, name, package, source, actual_source, kind, current) in [
        (
            "mise",
            "mise.toml",
            "node",
            "node",
            "node-version",
            "mise",
            "tool",
            "24.1.0",
        ),
        (
            "github-actions",
            "ci.yml",
            "node",
            "actions/node-versions",
            "github-releases",
            "github-releases",
            "tool",
            "22.1.0",
        ),
        (
            "npm",
            "package.json",
            "@types/node",
            "@types/node",
            "npm",
            "npm",
            "development",
            "22.1.0",
        ),
    ] {
        let mut node = dep(name, source, current);
        node["packageName"] = json!(package);
        node["updates"] = if manager == "mise" {
            json!([])
        } else {
            json!([{"newValue":"24","newVersion":"24.1.0","updateType":"major"}])
        };
        let baseline = snapshot(json!({(manager):[{"packageFile":file,"deps":[node]}]}));
        let mut actual = baseline.clone();
        let d = &mut actual.repositories[0].package_files[0].dependencies[0];
        d.metadata.as_mut().unwrap().source = actual_source.into();
        d.metadata.as_mut().unwrap().kinds = vec![kind.into()];
        d.updates = vec![update("26.1.0", "major")];
        let triage = compare(&baseline, &actual, &rules()).unwrap();
        assert!(
            triage.differences.iter().all(|d| d
                .departure
                .as_ref()
                .is_some_and(|d| d.id == "release-channels")),
            "{manager}: {:?}",
            triage.differences
        );
        d_policy(&mut actual).channel = Some("lts".into());
        assert!(
            compare(&baseline, &actual, &rules())
                .unwrap()
                .differences
                .iter()
                .all(|d| d.departure.is_none())
        );
        d_policy(&mut actual).channel = Some("stable".into());
        actual.repositories[0].package_files[0].dependencies[0].updates[0].new_digest =
            Some("a".repeat(40));
        // npm integrity is independently normalized; peers must still remain
        // outside the Node channel exemption.
        if manager == "npm" {
            actual.repositories[0].package_files[0].dependencies[0]
                .metadata
                .as_mut()
                .unwrap()
                .kinds = vec!["peer".into()];
        }
        assert!(
            compare(&baseline, &actual, &rules())
                .unwrap()
                .differences
                .iter()
                .any(|d| d.departure.is_none())
        );
        if manager == "mise" {
            let mut baseline = baseline;
            baseline.repositories[0].package_files[0].dependencies[0]
                .updates
                .clear();
            actual.repositories[0].package_files[0].dependencies[0].updates[0].new_digest = None;
            assert!(
                compare(&baseline, &actual, &rules())
                    .unwrap()
                    .differences
                    .iter()
                    .all(|d| d
                        .departure
                        .as_ref()
                        .is_some_and(|d| d.id == "release-channels"))
            );
        }
    }
}

fn d_policy(report: &mut Report) -> &mut patchula_report::UpdatePolicy {
    report.repositories[0].package_files[0].dependencies[0].updates[0]
        .policy
        .as_mut()
        .unwrap()
}

#[test]
fn declared_go_floor_requires_provenance_and_limits_targets_to_observed_compiler() {
    let mut go = dep("go", "golang-version", "1.26");
    go["currentVersion"] = json!("1.27.2");
    go["depType"] = json!("golang");
    let baseline = snapshot(json!({"gomod":[{"packageFile":"go.mod","deps":[go]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.current.version = Some("1.26".into());
    d.metadata.as_mut().unwrap().source = "mise".into();
    d.metadata.as_mut().unwrap().kinds = vec!["go-directive".into()];
    d.metadata.as_mut().unwrap().resolution =
        Some(serde_json::from_value(json!({"kind":"declared","version":"1.26"})).unwrap());
    d.updates = vec![update("1.26.9", "patch"), update("1.27.2", "minor")];
    let triage = compare(&baseline, &actual, &rules()).unwrap();
    assert_eq!(triage.differences.len(), 3);
    assert!(triage.differences.iter().all(|d| {
        d.departure
            .as_ref()
            .is_some_and(|d| d.id == "go-language-floor")
    }));
    actual.repositories[0].package_files[0].dependencies[0].updates[1] = update("1.28.0", "minor");
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.departure.is_none())
    );
    actual.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .resolution = None;
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );
}

#[test]
fn mise_migrations_keep_distinct_identities_and_reject_other_sources_or_targets() {
    for (name, old, new, source) in [
        ("prettier", "prettier/prettier", "prettier", "npm"),
        (
            "bitwarden-secrets-manager",
            "bitwarden/sdk",
            "bitwarden/sdk-sm",
            "github-releases",
        ),
    ] {
        let mut tool = dep(name, "github-releases", "latest");
        tool["packageName"] = json!(old);
        tool["currentVersion"] = Value::Null;
        tool["depType"] = json!("tools");
        let baseline = snapshot(json!({"mise":[{"packageFile":"mise.toml","deps":[tool]}]}));
        let mut actual = baseline.clone();
        let d = &mut actual.repositories[0].package_files[0].dependencies[0];
        d.package = new.into();
        d.metadata.as_mut().unwrap().source = source.into();
        d.metadata.as_mut().unwrap().kinds = vec!["tool".into()];
        let triage = compare(&baseline, &actual, &rules()).unwrap();
        assert_eq!(triage.differences.len(), 1);
        assert_eq!(
            triage.differences[0].departure.as_ref().unwrap().id,
            "mise-backends"
        );
        assert_ne!(
            triage.differences[0].renovate,
            triage.differences[0].patchula
        );
        actual.repositories[0].package_files[0].dependencies[0].updates =
            vec![update("2.0.0", "pin")];
        assert!(
            compare(&baseline, &actual, &rules())
                .unwrap()
                .differences
                .iter()
                .find(|d| d.field == "package")
                .unwrap()
                .departure
                .is_none()
        );
    }
}

#[test]
fn action_alias_rule_requires_identical_full_version_and_commit() {
    let mut action = dep("example/action", "github-tags", "v4");
    action["updates"] = json!([{"newValue":"v4.4.0","newVersion":"v4.4.0","newDigest":"a".repeat(40),"updateType":"pinDigest"}]);
    let baseline = snapshot(json!({"github-actions":[{"packageFile":"ci.yml","deps":[action]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.metadata.as_mut().unwrap().kinds = vec!["action".into()];
    d.updates[0].new_value = Some("v4".into());
    d.updates[0].new_version = Some("v4.4.0".into());
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d
                .departure
                .as_ref()
                .is_some_and(|d| d.id == "action-ref-precision"))
    );
    for (version, digest) in [("v4.5.0", "a".repeat(40)), ("v4.4.0", "b".repeat(40))] {
        let u = &mut actual.repositories[0].package_files[0].dependencies[0].updates[0];
        u.new_version = Some(version.into());
        u.new_digest = Some(digest);
        assert!(
            compare(&baseline, &actual, &rules())
                .unwrap()
                .differences
                .iter()
                .all(|d| d.departure.is_none())
        );
    }
}

#[test]
fn cargo_resolved_targets_require_the_identical_compatibility_interval() {
    let mut lib = dep("lib", "crate", "0.8");
    lib["currentVersion"] = json!("0.8.19");
    lib["updates"] = json!([{"newValue":"0.15","newVersion":"0.15.0","updateType":"minor"}]);
    let baseline = snapshot(json!({"cargo":[{"packageFile":"Cargo.toml","deps":[lib]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.metadata.as_mut().unwrap().source = "crates".into();
    d.updates = vec![update("0.15.2", "major")];
    d.updates[0].new_value = Some("0.15".into());
    d.updates[0].policy.as_mut().unwrap().versions = Some("preserve".into());
    let triage = compare(&baseline, &actual, &rules()).unwrap();
    assert_eq!(triage.differences.len(), 2);
    assert!(triage.differences.iter().all(|d| {
        d.departure
            .as_ref()
            .is_some_and(|d| d.id == "cargo-target-resolution")
    }));
    for (target, range) in [("0.16.0", "0.15"), ("0.15.2", "0.16"), ("0.15.0", "0.15")] {
        let d = &mut actual.repositories[0].package_files[0].dependencies[0];
        d.updates[0].new_version = Some(target.into());
        d.updates[0].new_value = Some(range.into());
        assert!(
            compare(&baseline, &actual, &rules())
                .unwrap()
                .differences
                .iter()
                .any(|d| d
                    .departure
                    .as_ref()
                    .is_none_or(|d| d.id != "cargo-target-resolution"))
        );
    }
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.updates[0].new_version = Some("0.15.2".into());
    d.metadata
        .as_mut()
        .unwrap()
        .stale
        .insert("catalog".into(), 1);
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );
}

#[test]
fn exact_peer_pins_are_no_longer_a_departure() {
    let mut peer = dep("lib", "npm", "^5");
    peer["depType"] = json!("peerDependencies");
    peer["updates"] =
        json!([{"newValue":"^5 || ^7.0.0","newVersion":"7.0.2","updateType":"major"}]);
    let baseline = snapshot(json!({"npm":[{"packageFile":"package.json","deps":[peer]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.metadata.as_mut().unwrap().kinds = vec!["peer".into()];
    d.updates = vec![update("7.0.2", "major")];
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );
}

#[test]
fn retired_cargo_floor_exemption_stays_actionable_and_npm_pins_require_the_same_target() {
    let mut cargo = dep("lib", "crate", "0.30");
    cargo["updates"] = json!([{"newValue":"1.0","newVersion":"1.1.0","updateType":"major"}]);
    let baseline = snapshot(json!({"cargo":[{"packageFile":"Cargo.toml","deps":[cargo]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.metadata.as_mut().unwrap().source = "crates".into();
    d.updates[0].new_value = Some("1.1".into());
    let mut p: patchula_report::UpdatePolicy = serde_json::from_value(policy()).unwrap();
    p.versions = Some("preserve".into());
    d.updates[0].policy = Some(p);
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0].new_version =
        Some("1.2.0".into());
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );

    let mut npm = dep("types", "npm", "^22.20.4");
    npm["updates"] = json!([{"newValue":"^22.20.4","newVersion":"22.20.5","updateType":"patch"}]);
    let baseline = snapshot(json!({"npm":[{"packageFile":"package.json","deps":[npm]}]}));
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0].updates =
        vec![update("22.20.5", "pin")];
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d
                .departure
                .as_ref()
                .is_some_and(|d| d.id == "npm-range-edits"))
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0] = update("22.20.6", "pin");
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.classification == Class::PatchulaMissing)
    );
}

#[test]
fn action_age_digest_does_not_excuse_equal_version_hash_differences() {
    let mut action = dep("example/action", "github-tags", "v3");
    action["updates"] = json!([{"newValue":"v5","newVersion":"v5.1.0","newDigest":"a".repeat(40),"updateType":"major"}]);
    let baseline = snapshot(json!({"github-actions":[{"packageFile":"ci.yml","deps":[action]}]}));
    let mut actual = baseline.clone();
    let update = &mut actual.repositories[0].package_files[0].dependencies[0].updates[0];
    update.new_version = Some("v5.1.1".into());
    update.new_digest = Some("b".repeat(40));
    update.policy = Some(serde_json::from_value(policy()).unwrap());
    update.release_age = Some(patchula_report::ReleaseAge {
        published_at: "2026-10-13T00:00:00Z".into(),
        evaluated_at: "2026-10-15T00:00:00Z".into(),
    });
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d
                .departure
                .as_ref()
                .is_some_and(|d| d.id == "minimum-release-age"))
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0].new_version =
        Some("v5.1.0".into());
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );
}

#[test]
fn provider_pins_do_not_excuse_target_changes_or_support_constraints() {
    let mut provider = dep("aws", "terraform-provider", "~> 6.0");
    provider["updates"] = json!([{"newValue":"~> 6.0","newVersion":"6.69.0","updateType":"minor"}]);
    let baseline = snapshot(json!({"terraform":[{"packageFile":"versions.tf","deps":[provider]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.metadata.as_mut().unwrap().source = "terraform".into();
    d.metadata.as_mut().unwrap().kinds = vec!["provider".into()];
    d.updates[0] = update("6.69.0", "minor");
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );
    for kind in ["module", "toolchain"] {
        actual.repositories[0].package_files[0].dependencies[0]
            .metadata
            .as_mut()
            .unwrap()
            .kinds = vec![kind.into()];
        assert!(
            compare(&baseline, &actual, &rules())
                .unwrap()
                .differences
                .iter()
                .all(|d| d.departure.is_none())
        );
    }
}

#[test]
fn held_minor_patch_alternatives_require_target_and_clock_evidence() {
    let mut lib = dep("lib", "npm", "1.2.0");
    lib["updates"] = json!([{"newValue":"1.3.0","newVersion":"1.3.0","updateType":"minor"}]);
    let baseline = snapshot(json!({"regex":[{"packageFile":"versions.txt","deps":[lib]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.updates = vec![update("1.2.1", "patch")];
    d.metadata.as_mut().unwrap().lookup = Some(
        serde_json::from_value(
            json!({"evaluatedAt":"2026-10-15T00:00:00Z","disabledUpdates":[],"rejected":[]}),
        )
        .unwrap(),
    );
    d.metadata.as_mut().unwrap().held_back = vec![serde_json::from_value(json!({"version":"1.3.0","versions":["1.3.0"],"count":1,"updateType":"minor","publishedAt":"2026-10-14T12:00:00Z","eligibleAt":"2026-10-15T12:00:00Z"})).unwrap()];
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_some())
    );
    actual.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .lookup
        .as_mut()
        .unwrap()
        .evaluated_at = "2026-10-16T00:00:00Z".into();
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );
}
fn update(value: &str, kind: &str) -> patchula_report::Update {
    serde_json::from_value(json!({"newValue":value,"newVersion":value,"newDigest":null,"updateType":kind,"group":null,"policy":policy()})).unwrap()
}

#[test]
fn cargo_noop_departure_requires_unchanged_satisfying_nonexact_manifest() {
    for (current, next, target, source, expected) in [
        ("1", "1", "1.9.2", "crate", true),
        ("0.3", "0.3", "0.3.9", "crate", true),
        ("1", "2", "2.0.0", "crate", false),
        ("1", "1", "2.0.0", "crate", false),
        ("=1.0.0", "=1.0.0", "1.0.0", "crate", false),
        ("1", "1", "1.9.2", "npm", false),
    ] {
        let mut d = dep("lib", source, current);
        d["updates"] =
            json!([{"newValue":next,"newVersion":target,"newDigest":null,"updateType":"patch"}]);
        let baseline = snapshot(json!({"cargo":[{"packageFile":"Cargo.toml","deps":[d]}]}));
        let mut actual = baseline.clone();
        let dep = &mut actual.repositories[0].package_files[0].dependencies[0];
        dep.updates.clear();
        dep.metadata.as_mut().unwrap().source = "crates".into();
        let triage = compare(&baseline, &actual, &rules()).unwrap();
        assert_eq!(
            triage.differences[0].classification == Class::DeliberateDeparture,
            expected,
            "{current}/{next}/{target}/{source}"
        );
    }
}

#[test]
fn digest_rate_limit_attribution_does_not_excuse_missing_major_policy() {
    let mut d = dep("image", "docker", "1");
    d["updates"] = json!([
        {"newValue":"1","newDigest":"sha256:changed","updateType":"digest"},
        {"newValue":"2","newVersion":"2","newDigest":"sha256:new","updateType":"major"}
    ]);
    let baseline = snapshot(json!({"kubernetes":[{"packageFile":"deploy.yaml","deps":[d]}]}));
    let mut actual = baseline.clone();
    actual.repositories[0].package_files[0].dependencies[0]
        .updates
        .clear();
    actual.repositories[0]
        .warnings
        .push("image: registry rate limit reached; retry after 60 seconds".into());
    let triage = compare(&baseline, &actual, &rules()).unwrap();
    assert_eq!(
        triage.differences.iter().filter(|d| d.rate_limited).count(),
        1
    );
    assert!(
        triage
            .differences
            .iter()
            .all(|d| d.classification == Class::PatchulaMissing)
    );
    actual.repositories[0].warnings[0] = "image: registry HTTP 429".into();
    assert_eq!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .filter(|d| d.rate_limited)
            .count(),
        1
    );
    actual.repositories[0].warnings[0] = "other-image: registry rate limit reached".into();
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| !d.rate_limited)
    );
}

#[test]
fn mise_cargo_git_url_normalization_retains_other_sources_and_repository_changes() {
    let mut d = dep(
        "cargo:https://github.com/example/tool",
        "git-tags",
        "v1.0.0",
    );
    d["packageName"] = json!("https://github.com/example/tool");
    let baseline = snapshot(json!({"mise":[{"packageFile":"mise.toml","deps":[d]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.package = "example/tool".into();
    d.metadata.as_mut().unwrap().source = "github-tags".into();
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .is_empty()
    );
    actual.repositories[0].package_files[0].dependencies[0].package = "example/other".into();
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .any(|d| d.field == "package")
    );
}

#[test]
fn disabled_rejected_and_age_departures_require_new_report_evidence() {
    let mut d = dep("lib", "npm", "1.0.0");
    d["updates"] = json!([{"newValue":"1.1.0","newVersion":"1.1.0","updateType":"minor"}]);
    let baseline = snapshot(json!({"npm":[{"packageFile":"package.json","deps":[d]}]}));
    for (lookup, held, expected) in [
        (
            json!({"evaluatedAt":"2026-10-15T00:00:00Z","disabledUpdates":["minor"],"rejected":[]}),
            json!([]),
            Some("disabled-update-types"),
        ),
        (
            json!({"evaluatedAt":"2026-10-15T00:00:00Z","disabledUpdates":["major"],"rejected":[]}),
            json!([]),
            None,
        ),
        (
            json!({"evaluatedAt":"2026-10-15T00:00:00Z","disabledUpdates":[],"rejected":[{"version":"1.1.0","reason":"deprecated"}]}),
            json!([]),
            Some("rejected-releases"),
        ),
        (
            json!({"evaluatedAt":"2026-10-15T00:00:00Z","disabledUpdates":[],"rejected":[{"version":"1.2.0","reason":"deprecated"}]}),
            json!([]),
            None,
        ),
        (
            json!({"evaluatedAt":"2026-10-15T00:00:00Z","disabledUpdates":[],"rejected":[]}),
            json!([{"version":"1.1.0","updateType":"minor","publishedAt":"2026-10-14T12:00:00Z","eligibleAt":"2026-10-15T12:00:00Z"}]),
            Some("minimum-release-age"),
        ),
        (
            json!({"evaluatedAt":"2026-10-16T00:00:00Z","disabledUpdates":[],"rejected":[]}),
            json!([{"version":"1.1.0","updateType":"minor","publishedAt":"2026-10-14T12:00:00Z","eligibleAt":"2026-10-15T12:00:00Z"}]),
            None,
        ),
    ] {
        let mut actual = baseline.clone();
        let d = &mut actual.repositories[0].package_files[0].dependencies[0];
        d.updates.clear();
        d.metadata.as_mut().unwrap().lookup = Some(serde_json::from_value(lookup).unwrap());
        d.metadata.as_mut().unwrap().held_back = serde_json::from_value(held).unwrap();
        let triage = compare(&baseline, &actual, &rules()).unwrap();
        assert_eq!(
            triage.differences[0]
                .departure
                .as_ref()
                .map(|d| d.id.as_str()),
            expected
        );
        actual.repositories[0].package_files[0].dependencies[0]
            .metadata
            .as_mut()
            .unwrap()
            .lookup = None;
        assert!(
            compare(&baseline, &actual, &rules()).unwrap().differences[0]
                .departure
                .is_none()
        );
    }
}

#[test]
fn current_resolution_departure_does_not_hide_lock_or_digest_changes() {
    let mut d = dep("lib", "npm", "^1.0.0");
    d["currentVersion"] = json!("1.2.0");
    let baseline = snapshot(json!({"npm":[{"packageFile":"package.json","deps":[d]}]}));
    let mut actual = baseline.clone();
    let d = &mut actual.repositories[0].package_files[0].dependencies[0];
    d.current.version = Some("1.0.0".into());
    d.metadata.as_mut().unwrap().resolution =
        Some(serde_json::from_value(json!({"kind":"lockfile","version":"1.0.0"})).unwrap());
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0]
            .departure
            .as_ref()
            .unwrap()
            .id,
        "current-versions"
    );
    actual.repositories[0].package_files[0].dependencies[0]
        .current
        .digest = Some("changed".into());
    assert!(
        compare(&baseline, &actual, &rules())
            .unwrap()
            .differences
            .iter()
            .all(|d| d.departure.is_none())
    );
    actual.repositories[0].package_files[0].dependencies[0]
        .current
        .digest = None;
    actual.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .resolution = None;
    assert!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0]
            .departure
            .is_none()
    );
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
        patchula_report::Status::Complete
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
    assert_eq!(triage.differences[0].classification, Class::PatchulaExtra);
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
    assert!(
        triage
            .exclusions
            .iter()
            .any(|e| e.departure == "kubernetes-api-versions")
    );
    assert!(
        triage
            .exclusions
            .iter()
            .any(|e| e.departure == "yaml-image-ownership")
    );
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
            .id,
        "floating-selectors"
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0]
        .policy
        .as_mut()
        .unwrap()
        .versions = Some("preserve".into());
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].classification,
        Class::PatchulaExtra
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
            .id,
        "indirect-go-requirements"
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
        Class::PatchulaExtra
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
            .id,
        "compose-test-services"
    );
    actual.repositories[0].package_files[0].path = "production/docker-compose.demo.yml".into();
    assert_eq!(
        compare(&empty, &actual, &rules()).unwrap().differences[0].classification,
        Class::PatchulaExtra
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
            .id,
        "npm-range-edits"
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
            .id,
        "npm-range-edits"
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[0] = update("2.0.0", "pin");
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].classification,
        Class::PatchulaExtra
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
            .id,
        "patch-track"
    );
    actual.repositories[0].package_files[0].dependencies[0].updates[1] = update("1.2.0", "patch");
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].classification,
        Class::PatchulaExtra
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
            .id,
        "go-major-versions"
    );
    let mut incompatible = baseline.clone();
    incompatible.repositories[0].package_files[0].dependencies[0].updates[0] =
        update("v2.0.0+incompatible", "major");
    assert_eq!(
        compare(&incompatible, &actual, &rules())
            .unwrap()
            .differences[0]
            .classification,
        Class::PatchulaMissing
    );
    actual.repositories[0].package_files[0].dependencies[0]
        .metadata
        .as_mut()
        .unwrap()
        .source = "github-releases".into();
    assert_eq!(
        compare(&baseline, &actual, &rules()).unwrap().differences[0].classification,
        Class::PatchulaMissing
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
    let run = std::process::Command::new(env!("CARGO_BIN_EXE_patchula-conformance"))
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

#[test]
fn floating_action_target_requires_observed_alias_commit_and_matching_release_line() {
    for (current, alias, old, new) in [
        ("v3", "v7", "v7.0.0", "v7.1.0"),
        ("v3.0", "v3.1", "v3.1.0", "v3.1.2"),
    ] {
        let mut action = dep("example/action", "github-tags", current);
        action["currentDigest"] = json!("c".repeat(40));
        action["updates"] = json!([{"newValue":alias,"newVersion":old,"newDigest":"a".repeat(40),"updateType":"major"}]);
        let baseline =
            snapshot(json!({"github-actions":[{"packageFile":"ci.yml","deps":[action]}]}));
        let mut actual = baseline.clone();
        let d = &mut actual.repositories[0].package_files[0].dependencies[0];
        d.metadata.as_mut().unwrap().kinds = vec!["action".into()];
        d.metadata
            .as_mut()
            .unwrap()
            .tag_digests
            .insert(alias.into(), "b".repeat(40));
        d.updates[0].new_version = Some(new.into());
        d.updates[0].new_digest = Some("b".repeat(40));
        let diffs = compare(&baseline, &actual, &rules()).unwrap().differences;
        assert_eq!(diffs.len(), 2);
        assert!(diffs.iter().all(|d| {
            d.departure
                .as_ref()
                .is_some_and(|d| d.id == "action-ref-precision")
        }));
        for (field, value) in [
            ("evidence", ""),
            ("evidence", "a"),
            ("target", "v8.1.0"),
            ("target", old),
            ("value", "v7.1.0"),
            ("current", "v3.0.0"),
            ("source", "github-releases"),
            ("kind", "tool"),
            ("type", "minor"),
        ] {
            let mut changed = actual.clone();
            let d = &mut changed.repositories[0].package_files[0].dependencies[0];
            match field {
                "evidence" => {
                    d.metadata
                        .as_mut()
                        .unwrap()
                        .tag_digests
                        .insert(alias.into(), value.repeat(40));
                }
                "target" => d.updates[0].new_version = Some(value.into()),
                "value" => d.updates[0].new_value = Some(value.into()),
                "current" => d.current.value = Some(value.into()),
                "source" => d.metadata.as_mut().unwrap().source = value.into(),
                "kind" => d.metadata.as_mut().unwrap().kinds = vec![value.into()],
                "type" => d.updates[0].update_type = value.into(),
                _ => unreachable!(),
            }
            assert!(
                compare(&baseline, &changed, &rules())
                    .unwrap()
                    .differences
                    .iter()
                    .any(|d| d.departure.is_none()),
                "{field}={value}"
            );
        }
    }
}
