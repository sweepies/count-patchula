use patchula_config::{DependencyKind, Manager, Scheme, Source};
use patchula_datasource::{Catalog, Version};
use patchula_lookup::Evaluator;
use patchula_manager::{Dependency, Location};
fn config(name: &str) -> patchula_config::Config {
    patchula_config::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../examples/{name}.pkl")),
    )
    .unwrap()
}
fn dependency(name: &str) -> Dependency {
    Dependency {
        manager: Manager::Npm,
        file: "package.json".into(),
        location: Location {
            start: 0,
            end: 1,
            line: 1,
            column: 1,
            pointer: String::new(),
        },
        name: name.into(),
        package: name.into(),
        kind: DependencyKind::Production,
        current: "1.0.0".into(),
        resolved: Some("1.0.0".into()),
        digest: None,
        source: Source::Npm,
        scheme: Scheme::Semver,
        extract: None,
    }
}
fn catalog(values: &[&str]) -> Catalog {
    Catalog {
        versions: values
            .iter()
            .map(|v| Version {
                version: (*v).into(),
                published_at: None,
                digest: Some("a".repeat(40)),
                deprecated: false,
                yanked: false,
            })
            .collect(),
        dist_tags: Default::default(),
    }
}

#[test]
fn peer_support_ranges_widen_without_pins_while_installed_dependencies_remain_exact() {
    let eval = Evaluator::new(&config("benchmark-public")).unwrap();
    for (kind, current, target, expected) in [
        (DependencyKind::Peer, "^5", "7.0.2", "^5 || ^7.0.0"),
        (
            DependencyKind::Peer,
            "^5 || ^7.0.0",
            "8.1.2",
            "^5 || ^7.0.0 || ^8.0.0",
        ),
        (DependencyKind::Peer, "2026.9.8", "2026.10.1", "2026.10.1"),
        (DependencyKind::Production, "^5", "7.0.2", "7.0.2"),
        (DependencyKind::Development, "^5", "7.0.2", "7.0.2"),
    ] {
        let mut dep = dependency("lib");
        dep.kind = kind;
        dep.current = current.into();
        dep.resolved = Some(
            if current.starts_with('^') {
                "5.0.0"
            } else {
                current
            }
            .into(),
        );
        let proposals = eval
            .propose(&dep, &catalog(&["5.0.0", "5.9.3", target]))
            .unwrap();
        let selected = proposals
            .iter()
            .find(|p| p.new_version.as_deref() == Some(target))
            .unwrap();
        assert_eq!(selected.new_value.as_deref(), Some(expected));
        if kind == DependencyKind::Peer {
            assert!(proposals.iter().all(|p| p.update_type != "pin"));
        }
    }
    let mut dep = dependency("lib");
    dep.kind = DependencyKind::Peer;
    dep.current = "^5".into();
    dep.resolved = Some("5.9.3".into());
    assert!(eval.propose(&dep, &catalog(&["5.9.3"])).unwrap().is_empty());
}

#[test]
fn satisfying_provider_constraints_and_cargo_floors_are_retained() {
    let cfg = config("fnox");
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("azure_core");
    dep.manager = Manager::Cargo;
    dep.source = Source::Crates;
    dep.current = "0.30".into();
    dep.resolved = Some("0.30.1".into());
    assert_eq!(
        eval.propose(&dep, &catalog(&["1.1.0"])).unwrap()[0]
            .new_value
            .as_deref(),
        Some("1.0")
    );
    dep.current = "1.0".into();
    dep.resolved = Some("1.0.0".into());
    assert!(eval.propose(&dep, &catalog(&["1.1.0"])).unwrap().is_empty());

    let eval = Evaluator::new(&config("benchmark-public")).unwrap();
    dep.manager = Manager::Terraform;
    dep.kind = DependencyKind::Provider;
    dep.source = Source::Terraform;
    dep.scheme = Scheme::Hashicorp;
    dep.current = "~> 6.0".into();
    dep.resolved = Some("6.68.0".into());
    let proposals = eval.propose(&dep, &catalog(&["6.69.0", "7.1.0"])).unwrap();
    assert_eq!(
        proposals
            .iter()
            .find(|p| p.new_version.as_deref() == Some("6.69.0"))
            .unwrap()
            .new_value
            .as_deref(),
        Some("~> 6.0")
    );
    assert_eq!(
        proposals
            .iter()
            .find(|p| p.new_version.as_deref() == Some("7.1.0"))
            .unwrap()
            .new_value
            .as_deref(),
        Some("7.1.0")
    );
}

#[test]
fn node_lts_is_explicit_uses_catalog_evidence_and_respects_ordered_rules() {
    use patchula_config::ReleaseChannel;
    let mut cfg = config("benchmark-public");
    let mut dep = dependency("node");
    dep.manager = Manager::Mise;
    dep.kind = DependencyKind::Tool;
    dep.source = Source::Mise;
    dep.scheme = Scheme::Loose;
    dep.current = "22.0.0".into();
    dep.resolved = Some(dep.current.clone());
    let mut versions = catalog(&["22.1.0", "24.2.0", "26.1.0"]);
    versions.dist_tags.insert("lts".into(), "24.2.0".into());
    let stable = Evaluator::new(&cfg)
        .unwrap()
        .propose(&dep, &versions)
        .unwrap();
    assert!(
        stable
            .iter()
            .any(|p| p.new_version.as_deref() == Some("26.1.0"))
    );
    let rule = serde_json::from_value(serde_json::json!({"note":"Node LTS", "select":{"packages":["node"],"dependencies":[],"managers":["mise"],"files":[],"updates":[],"sources":[],"kinds":[],"current":null},"apply":{"channel":"lts"}})).unwrap();
    cfg.rules.push(rule);
    let lts = Evaluator::new(&cfg)
        .unwrap()
        .propose(&dep, &versions)
        .unwrap();
    assert_eq!(lts.len(), 1);
    assert_eq!(lts[0].new_version.as_deref(), Some("24.2.0"));
    assert_eq!(
        lts[0].policy.as_ref().unwrap().channel.as_deref(),
        Some("lts")
    );
    versions.dist_tags.clear();
    assert!(
        Evaluator::new(&cfg)
            .unwrap()
            .propose(&dep, &versions)
            .unwrap_err()
            .contains("no LTS channel evidence")
    );
    cfg.rules[0].apply.channel = Some(ReleaseChannel::Stable);
    assert!(
        Evaluator::new(&cfg)
            .unwrap()
            .propose(&dep, &versions)
            .is_ok()
    );
    cfg.defaults.channel = ReleaseChannel::Lts;
    cfg.rules.clear();
    dep = dependency("arbitrary-npm-package");
    assert!(
        Evaluator::new(&cfg)
            .unwrap()
            .propose(&dep, &catalog(&["2.0.0"]))
            .unwrap_err()
            .contains("supported only for Node")
    );
}

#[test]
fn equal_precedence_candidates_and_age_boundaries_are_order_independent() {
    let cfg = config("benchmark-public");
    let eval = Evaluator::new(&cfg).unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T05:51:26Z")
        .unwrap()
        .to_utc();
    let mut versions = catalog(&["1.0.1+a", "1.0.1+b", "1.0.2"]);
    versions.versions[0].published_at = Some("2026-10-10T00:00:00Z".into());
    versions.versions[1].published_at = Some("2026-10-10T00:00:00Z".into());
    versions.versions[2].published_at = Some("2026-10-14T05:51:27Z".into());
    for current in ["1.0.0", "^1.0.0", "latest"] {
        let mut dep = dependency("lib");
        dep.current = current.into();
        if current == "latest" {
            dep.resolved = None;
            versions.dist_tags.insert("latest".into(), "1.0.2".into());
        }
        let expected = eval.evaluate_at(&dep, &versions, now).unwrap();
        assert_eq!(expected.updates[0].new_version.as_deref(), Some("1.0.1+b"));
        for _ in 0..versions.versions.len() {
            versions.versions.rotate_left(1);
            let result = eval.evaluate_at(&dep, &versions, now).unwrap();
            assert_eq!(result.updates, expected.updates);
            assert_eq!(result.held_back, expected.held_back);
        }
        assert_eq!(
            eval.evaluate_at(&dep, &versions, now + chrono::Duration::seconds(1))
                .unwrap()
                .updates[0]
                .new_version
                .as_deref(),
            Some("1.0.2")
        );
    }
}

#[test]
fn action_digest_targets_resolve_the_new_commit_without_expanding_ref_precision() {
    let eval = Evaluator::new(&config("benchmark-public")).unwrap();
    let mut dep = dependency("example/action");
    dep.manager = Manager::GithubActions;
    dep.kind = DependencyKind::Action;
    dep.source = Source::GithubTags;
    dep.scheme = Scheme::Loose;
    dep.current = "v2".into();
    dep.resolved = Some("v2".into());
    let mut versions = catalog(&["v2", "v2.1.0", "v2.2.0"]);
    versions.versions[1].digest = Some("b".repeat(40));
    for digest in [None, Some("b".repeat(40))] {
        dep.digest = digest;
        let p = eval
            .digest_proposal_with_catalog(&dep, &"a".repeat(40), Some(&versions))
            .unwrap();
        assert_eq!(p.new_value.as_deref(), Some("v2"));
        assert_eq!(p.new_version.as_deref(), Some("v2.2.0"));
        assert_eq!(
            p.update_type,
            if dep.digest.is_some() {
                "digest"
            } else {
                "pin"
            }
        );
    }
}

#[test]
fn prefixed_mise_product_releases_pin_latest_without_sdk_tags() {
    let cfg = config("benchmark-public");
    let dependencies = patchula_manager::Scanner::new(&cfg)
        .unwrap()
        .extract(&patchula_manager::Files::from([(
            "mise.toml".into(),
            "[tools]\njq = \"latest\"\nbitwarden-secrets-manager = \"latest\"\n".into(),
        )]))
        .unwrap()
        .dependencies;
    let eval = Evaluator::new(&cfg).unwrap();
    for dep in dependencies {
        let (tags, target) = if dep.name == "jq" {
            (vec!["jq-1.8.2", "jq-1.8.1"], "1.8.2")
        } else {
            (vec!["python-v9.0.0", "bws-v2.1.0", "rust-v9.0.0"], "2.1.0")
        };
        let proposals = eval.propose(&dep, &catalog(&tags)).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].update_type, "pin");
        assert_eq!(proposals[0].new_version.as_deref(), Some(target));
    }
}

#[test]
fn docker_policy_keeps_variants_and_python_epochs_use_pep440_order() {
    let mut cfg = config("renovate-runner");
    cfg.defaults.wait_hours = 0;
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("image");
    dep.manager = Manager::Compose;
    dep.kind = DependencyKind::ContainerImage;
    dep.source = Source::Docker;
    dep.scheme = Scheme::Docker;
    dep.current = "1.2-alpine".into();
    dep.resolved = Some(dep.current.clone());
    let proposals = eval
        .propose(
            &dep,
            &catalog(&["1.3-alpine", "1.9-bookworm", "2.0-bookworm", "2.1-alpine"]),
        )
        .unwrap();
    assert_eq!(proposals.len(), 2);
    assert!(
        proposals
            .iter()
            .all(|p| p.new_version.as_deref().unwrap().ends_with("-alpine"))
    );
    dep.manager = Manager::Requirements;
    dep.kind = DependencyKind::Production;
    dep.source = Source::Pypi;
    dep.scheme = Scheme::Pep440;
    dep.current = "==1.0".into();
    dep.resolved = None;
    let proposals = eval
        .propose(&dep, &catalog(&["1.0rc1", "1.0.post1", "1!0.1"]))
        .unwrap();
    assert!(
        proposals
            .iter()
            .any(|p| p.new_value.as_deref() == Some("==1!0.1") && p.update_type == "major")
    );
    assert!(
        proposals
            .iter()
            .all(|p| !p.new_version.as_deref().unwrap().contains("rc"))
    );
}
#[test]
fn grouping_ops_workflow_opt_out_and_opaque_refs() {
    let eval = Evaluator::new(&config("renovate-runner")).unwrap();
    for name in [
        "alchemy",
        "@alchemy.run/core",
        "effect",
        "@effect/platform",
        "@sveltejs/kit",
    ] {
        let p = eval
            .propose(&dependency(name), &catalog(&["1.0.1", "1.2.0", "2.0.0"]))
            .unwrap();
        assert_eq!(p.len(), 3);
        assert!(
            p.iter()
                .all(|p| p.group.as_deref() == Some("alchemy, effect and sveltekit"))
        );
    }
    let mut dep = dependency("sweepies/ops-workflows");
    dep.manager = Manager::GithubActions;
    dep.kind = DependencyKind::ReusableWorkflow;
    dep.source = Source::GithubTags;
    dep.scheme = Scheme::Loose;
    dep.current = "main".into();
    dep.resolved = Some("main".into());
    assert!(
        eval.propose(&dep, &catalog(&["v2.0.0"]))
            .unwrap()
            .is_empty()
    );
    assert!(eval.digest_proposal(&dep, &"a".repeat(40)).is_none());
    dep.name = "actions/checkout".into();
    dep.package = dep.name.clone();
    dep.kind = DependencyKind::Action;
    assert!(eval.digest_proposal(&dep, &"a".repeat(40)).is_some());
}
#[test]
fn exact_preserve_nightly_and_deprecated_yanked_filters() {
    let mut dep = dependency("serde");
    dep.current = "^1.0.0".into();
    let eval = Evaluator::new(&config("fnox")).unwrap();
    let p = eval.propose(&dep, &catalog(&["1.2.0", "2.0.0"])).unwrap();
    assert!(p.iter().any(|p| p.new_value.as_deref() == Some("^1.0.0")));
    assert!(p.iter().any(|p| p.new_value.as_deref() == Some("^2.0.0")));
    let eval = Evaluator::new(&config("renovate-runner")).unwrap();
    let p = eval.propose(&dep, &catalog(&["1.0.0"])).unwrap();
    assert_eq!(p[0].update_type, "pin");
    assert_eq!(p[0].new_value.as_deref(), Some("1.0.0"));
    let mut versions = catalog(&["1.1.0", "2.0.0", "3.0.0-beta.1"]);
    versions.versions[0].deprecated = true;
    versions.versions[1].yanked = true;
    assert!(
        eval.propose(&dependency("plain"), &versions)
            .unwrap()
            .is_empty()
    );
    let eval = Evaluator::new(&config("remote-agent")).unwrap();
    dep.name = "pingdotgg/t3code".into();
    dep.package = dep.name.clone();
    dep.manager = Manager::Regex;
    dep.source = Source::GithubReleases;
    dep.current = "0.1.0-nightly.1".into();
    dep.resolved = None;
    let p = eval
        .propose(
            &dep,
            &catalog(&["0.1.0-nightly.2", "0.1.0", "0.2.0-nightly.1"]),
        )
        .unwrap();
    assert_eq!(p.len(), 2);
    assert!(
        p.iter()
            .all(|p| p.new_version.as_ref().unwrap().contains("nightly.") && p.group.is_none())
    );
}

#[test]
fn lookup_evidence_records_rejected_versions_and_per_class_policy() {
    let eval = Evaluator::new(&config("maccrae-infra")).unwrap();
    let mut dep = dependency("plain");
    let mut versions = catalog(&["0.9.0", "1.1.0", "1.2.0", "1.3.0"]);
    versions.versions[0].deprecated = true;
    versions.versions[1].deprecated = true;
    versions.versions[2].yanked = true;
    let evidence = eval.lookup_evidence(
        &dep,
        Some(&versions),
        chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z")
            .unwrap()
            .into(),
    );
    assert_eq!(evidence.rejected.len(), 2);
    assert_eq!(evidence.rejected[0].version, "1.1.0");
    assert_eq!(evidence.rejected[0].reason, "deprecated");
    dep.manager = Manager::Kubernetes;
    dep.source = Source::Docker;
    dep.package = "postgres".into();
    dep.file = "apps/notifuse/deployment-database.yaml".into();
    assert_eq!(eval.disabled_updates(&dep), ["major"]);
}
#[test]
fn rules_replace_lists_in_order_and_windows_are_reported() {
    let mut config = config("renovate-runner");
    let mut rule = config.rules[0].clone();
    rule.select=serde_json::from_value(serde_json::json!({"packages":["plain"],"dependencies":[],"managers":[],"files":[],"updates":["minor"],"sources":[],"kinds":[],"current":null})).unwrap();
    rule.apply.group = Some("early".into());
    rule.apply.wait_hours = Some(12);
    rule.apply.permitted = Some("semver:<2.0.0".into());
    rule.apply.windows = Some(vec![patchula_config::Window {
        cron: vec!["0 9 * * 1".into()],
        zone: "UTC".into(),
    }]);
    rule.apply.labels = Some(vec!["first".into()]);
    config.rules = vec![rule.clone()];
    rule.apply.group = Some(String::new());
    rule.apply.labels = Some(vec![]);
    config.rules.push(rule);
    let eval = Evaluator::new(&config).unwrap();
    let p = eval
        .propose(&dependency("plain"), &catalog(&["1.2.0"]))
        .unwrap();
    assert_eq!(p.len(), 1);
    let policy = p[0].policy.as_ref().unwrap();
    assert!(p[0].group.is_none() && policy.labels.is_empty());
    assert_eq!(policy.wait_hours, 12);
    assert_eq!(policy.windows.len(), 1);
    config.ignore = vec!["plain".into()];
    assert!(
        Evaluator::new(&config)
            .unwrap()
            .propose(&dependency("plain"), &catalog(&["1.2.0"]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn negated_ignore_exceptions_match_manifest_or_registry_alias() {
    let mut config = config("renovate-runner");
    config.ignore = vec!["!alias".into()];
    let eval = Evaluator::new(&config).unwrap();
    let mut dep = dependency("alias");
    dep.package = "real".into();
    assert!(!eval.ignored(&dep));
    assert!(eval.ignored(&dependency("other")));
}

#[test]
fn floating_dist_tags_and_latest_tools_can_be_pinned() {
    let eval = Evaluator::new(&config("renovate-runner")).unwrap();
    let mut dep = dependency("plain");
    dep.current = "latest".into();
    dep.resolved = None;
    let mut versions = catalog(&["1.0.0", "2.0.0"]);
    versions.dist_tags.insert("latest".into(), "2.0.0".into());
    let p = eval.propose(&dep, &versions).unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].update_type, "pin");
    assert_eq!(p[0].new_value.as_deref(), Some("2.0.0"));
    dep.manager = Manager::Mise;
    dep.source = Source::GithubReleases;
    dep.scheme = Scheme::Loose;
    dep.kind = DependencyKind::Tool;
    let p = eval
        .propose(&dep, &catalog(&["v2.0.0", "v3.0.0-beta.1"]))
        .unwrap();
    assert_eq!(p[0].new_value.as_deref(), Some("2.0.0"));
}

#[test]
fn extracted_reusable_workflow_matches_runner_digest_opt_out() {
    let config = config("renovate-runner");
    let files = patchula_manager::Files::from([(
        ".github/workflows/test.yml".into(),
        "jobs:\n  build:\n    uses: sweepies/ops-workflows/.github/workflows/build.yml@main\n"
            .into(),
    )]);
    let extracted = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&files)
        .unwrap();
    let dep = &extracted.dependencies[0];
    assert_eq!(dep.name, "sweepies/ops-workflows");
    let eval = Evaluator::new(&config).unwrap();
    assert!(!eval.needs_lookup(dep));
    assert!(eval.propose(dep, &catalog(&["v1.0.0"])).unwrap().is_empty());
    assert!(eval.digest_proposal(dep, &"a".repeat(40)).is_none());
}

fn instant(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&chrono::Utc)
}
#[test]
fn held_back_versions_share_the_normalized_proposal_identity() {
    let cfg = config("renovate-runner");
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("plain");
    dep.extract = Some(r"^release-(?<version>.*)$".into());
    let mut versions = catalog(&["release-1.0.1", "release-1.0.2"]);
    versions.versions[0].published_at = Some("2026-10-01T00:00:00Z".into());
    versions.versions[1].published_at = Some("2026-10-15T00:00:00Z".into());
    let result = eval
        .evaluate_at(&dep, &versions, instant("2026-10-15T12:00:00Z"))
        .unwrap();
    assert_eq!(result.updates[0].new_version.as_deref(), Some("1.0.1"));
    assert_eq!(result.held_back[0].version, "1.0.2");
}

#[test]
fn held_back_summarizes_newest_but_keeps_earliest_eligibility() {
    let eval = Evaluator::new(&config("renovate-runner")).unwrap();
    let mut versions = catalog(&["1.0.2", "1.0.4", "1.0.3"]);
    for (v, date) in versions.versions.iter_mut().zip([
        "2026-10-14T13:00:00Z",
        "2026-10-15T11:00:00Z",
        "2026-10-14T18:00:00Z",
    ]) {
        v.published_at = Some(date.into());
    }
    let result = eval
        .evaluate_at(
            &dependency("plain"),
            &versions,
            chrono::DateTime::parse_from_rfc3339("2026-10-15T12:00:00Z")
                .unwrap()
                .to_utc(),
        )
        .unwrap();
    let held = &result.held_back[0];
    assert_eq!(held.version, "1.0.4");
    assert_eq!(held.count, 3);
    assert_eq!(held.versions, ["1.0.2", "1.0.3", "1.0.4"]);
    assert_eq!(held.eligible_at, "2026-10-15T13:00:00Z");
    assert_eq!(
        held.target_eligible_at.as_deref(),
        Some("2026-10-16T11:00:00Z")
    );
}

#[test]
fn manual_age_override_selects_only_its_version_and_class_and_preserves_disabled_policy() {
    let mut cfg = config("renovate-runner");
    let dep = dependency("plain");
    let mut versions = catalog(&["1.0.2", "1.0.3", "2.0.0"]);
    for version in &mut versions.versions {
        version.published_at = Some("2026-10-15T11:00:00Z".into());
    }
    let now = instant("2026-10-15T12:00:00Z");
    let overrides = std::collections::BTreeSet::from([("1.0.2".into(), "patch".into())]);
    let result = Evaluator::new(&cfg)
        .unwrap()
        .evaluate_with_age_overrides(&dep, &versions, now, &overrides)
        .unwrap();
    assert!(
        result
            .updates
            .iter()
            .any(|u| u.new_version.as_deref() == Some("1.0.2"))
    );
    assert!(
        result
            .updates
            .iter()
            .all(|u| u.new_version.as_deref() != Some("1.0.3")
                && u.new_version.as_deref() != Some("2.0.0"))
    );
    assert!(!result.held_back.is_empty());
    cfg.defaults.active = false;
    assert!(
        Evaluator::new(&cfg)
            .unwrap()
            .evaluate_with_age_overrides(&dep, &versions, now, &overrides)
            .unwrap()
            .updates
            .is_empty()
    );
}
#[test]
fn release_age_falls_back_and_reports_eligibility_without_hiding_current() {
    let config = config("renovate-runner");
    let eval = Evaluator::new(&config).unwrap();
    let now = instant("2026-10-15T12:00:00Z");
    let mut versions = catalog(&["1.0.0", "1.0.1", "1.0.2", "2.0.0", "3.0.0"]);
    for (v, date) in versions.versions.iter_mut().zip([
        "2026-10-15T11:00:00Z",      // current remains a valid baseline
        "2026-10-14T12:00:00Z",      // eligible at exactly 24h
        "2026-10-14T12:00:01Z",      // held by one second
        "2026-10-15T13:00:00+01:00", // equivalent to now
        "2026-10-01T12:00:00Z",
    ]) {
        v.published_at = Some(date.into());
    }
    let result = eval
        .evaluate_at(&dependency("plain"), &versions, now)
        .unwrap();
    assert_eq!(result.updates.len(), 2);
    assert!(
        result
            .updates
            .iter()
            .any(|p| p.new_version.as_deref() == Some("1.0.1"))
    );
    assert!(
        result
            .updates
            .iter()
            .any(|p| p.new_version.as_deref() == Some("3.0.0"))
    );
    assert_eq!(result.held_back.len(), 1); // 2.0.0 is superseded by eligible 3.0.0
    assert_eq!(result.held_back[0].version, "1.0.2");
    assert_eq!(result.held_back[0].eligible_at, "2026-10-15T12:00:01Z");
    let patch = result
        .updates
        .iter()
        .find(|p| p.update_type == "patch")
        .unwrap();
    assert_eq!(
        patch.release_age.as_ref().unwrap().evaluated_at,
        "2026-10-15T12:00:00Z"
    );
    let mut cfg = config;
    cfg.defaults.wait_hours = 0;
    let result = Evaluator::new(&cfg)
        .unwrap()
        .evaluate_at(&dependency("plain"), &versions, now)
        .unwrap();
    assert!(result.held_back.is_empty());
    assert!(
        result
            .updates
            .iter()
            .any(|p| p.new_version.as_deref() == Some("1.0.2"))
    );
}

#[test]
fn missing_dates_all_held_and_rule_overrides_are_explicit() {
    let mut cfg = config("renovate-runner");
    let now = instant("2026-10-15T12:00:00Z");
    let mut versions = catalog(&["1.0.1", "1.0.2"]);
    for version in &mut versions.versions {
        version.published_at = Some("2026-10-15T00:00:00Z".into());
    }
    let eval = Evaluator::new(&cfg).unwrap();
    let result = eval
        .evaluate_at(&dependency("plain"), &versions, now)
        .unwrap();
    assert!(result.updates.is_empty());
    assert_eq!(result.held_back.len(), 1);
    assert_eq!(result.held_back[0].count, 2);
    versions.versions[0].published_at = None;
    assert_eq!(
        eval.evaluate_at(&dependency("plain"), &versions, now)
            .unwrap()
            .updates[0]
            .new_version
            .as_deref(),
        Some("1.0.1")
    );
    let mut rule = cfg.rules[0].clone();
    rule.select.packages = vec!["plain".into()];
    rule.apply.wait_hours = Some(12);
    cfg.rules = vec![rule];
    assert_eq!(
        Evaluator::new(&cfg)
            .unwrap()
            .evaluate_at(&dependency("plain"), &versions, now)
            .unwrap()
            .updates[0]
            .new_version
            .as_deref(),
        Some("1.0.2")
    );
}

#[test]
fn floating_targets_fall_back_and_existing_lock_pins_survive_age_gate() {
    let eval = Evaluator::new(&config("renovate-runner")).unwrap();
    let now = instant("2026-10-15T12:00:00Z");
    let mut dep = dependency("plain");
    dep.current = "latest".into();
    dep.resolved = None;
    let mut versions = catalog(&["1.0.0", "1.0.1"]);
    versions.versions[1].published_at = Some("2026-10-15T11:00:00Z".into());
    versions.dist_tags.insert("latest".into(), "1.0.1".into());
    let result = eval.evaluate_at(&dep, &versions, now).unwrap();
    assert_eq!(result.updates[0].new_value.as_deref(), Some("1.0.0"));
    assert_eq!(result.held_back.len(), 1);
    dep.current = "^1.0.0".into();
    dep.resolved = Some("1.0.1".into());
    let result = eval.evaluate_at(&dep, &versions, now).unwrap();
    assert_eq!(result.updates[0].update_type, "pin");
    assert_eq!(result.updates[0].new_value.as_deref(), Some("1.0.1"));
    assert!(result.held_back.is_empty());
}

#[test]
fn runner_latest_is_floating_and_versioned_images_never_get_digests() {
    let config = config("renovate-runner");
    let deps = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&patchula_manager::Files::from([(
            ".github/workflows/ci.yml".into(),
            "jobs:\n  a:\n    runs-on: ubuntu-latest\n  b:\n    runs-on: ubuntu-22.04\n".into(),
        )]))
        .unwrap()
        .dependencies;
    let eval = Evaluator::new(&config).unwrap();
    assert!(!eval.needs_lookup(&deps[0]));
    let p = eval
        .propose(&deps[1], &catalog(&["24.04", "26.04"]))
        .unwrap();
    assert_eq!(p[0].new_value.as_deref(), Some("26.04"));
    assert!(!eval.needs_digest(&deps[1], &p[0]));
}

#[test]
fn mise_current_versions_follow_backend_tag_identity_without_proposal_filters() {
    let cfg = config("renovate-runner");
    let deps = patchula_manager::Scanner::new(&cfg)
        .unwrap()
        .extract(&patchula_manager::Files::from([(
            "mise.toml".into(),
            "[tools]\nage = \"1.2.3\"\nfnox = \"2.3.4\"\nbun = \"3.4.5\"\n".into(),
        )]))
        .unwrap()
        .dependencies;
    for (name, source_version, expected) in [
        ("age", "v1.2.3", "1.2.3"),
        ("fnox", "v2.3.4", "v2.3.4"),
        ("bun", "bun-v3.4.5", "3.4.5"),
    ] {
        let dep = deps.iter().find(|d| d.name == name).unwrap();
        let mut catalog = catalog(&[source_version]);
        catalog.versions[0].published_at = Some("2099-01-01T00:00:00Z".into());
        catalog.versions[0].yanked = true;
        assert_eq!(
            patchula_lookup::resolved_tool_version(dep, &catalog).as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn floating_action_aliases_resolve_by_commit_and_keep_comment_precision() {
    let eval = Evaluator::new(&config("renovate-runner")).unwrap();
    let mut dep = dependency("example/action");
    dep.manager = Manager::GithubActions;
    dep.source = Source::GithubTags;
    dep.kind = DependencyKind::Action;
    dep.scheme = Scheme::Loose;
    dep.current = "v2".into();
    dep.resolved = Some("v2".into());
    dep.digest = Some("a".repeat(40));
    let mut versions = catalog(&["v2", "v2.0.1", "v2.4.2", "v2.4.3", "v3", "v3.2.1"]);
    versions.versions[1].digest = Some("b".repeat(40));
    versions.versions[3].digest = Some("c".repeat(40));
    versions.versions[4].digest = Some("d".repeat(40));
    versions.versions[5].digest = Some("d".repeat(40));
    assert_eq!(
        patchula_lookup::resolved_current_version(&dep, &versions).as_deref(),
        Some("v2.4.2")
    );
    let proposals = eval.propose(&dep, &versions).unwrap();
    assert!(
        !proposals
            .iter()
            .any(|p| p.new_version.as_deref() == Some("v2.0.1"))
    );
    let major = proposals.iter().find(|p| p.update_type == "major").unwrap();
    assert_eq!(major.new_value.as_deref(), Some("v3"));
    assert_eq!(major.new_version.as_deref(), Some("v3.2.1"));
    dep.digest = Some("e".repeat(40));
    assert!(patchula_lookup::resolved_current_version(&dep, &versions).is_none());
}

#[test]
fn docker_alias_precision_prevents_patch_and_date_tag_proposals() {
    let eval = Evaluator::new(&config("renovate-runner")).unwrap();
    let mut dep = dependency("image");
    dep.manager = Manager::Kubernetes;
    dep.kind = DependencyKind::ContainerImage;
    dep.source = Source::Docker;
    dep.scheme = Scheme::Docker;
    dep.current = "3.20".into();
    dep.resolved = Some("3.20".into());
    let p = eval
        .propose(&dep, &catalog(&["3.20.9", "3.21", "3.21.1", "20260101"]))
        .unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].new_value.as_deref(), Some("3.21"));
    assert_eq!(p[0].update_type, "minor");
}

#[test]
fn cargo_noop_ranges_are_not_updates_and_both_breaking_tracks_survive() {
    let eval = Evaluator::new(&config("fnox")).unwrap();
    let mut dep = dependency("example");
    dep.manager = Manager::Cargo;
    dep.source = Source::Crates;
    dep.current = "0.2".into();
    dep.resolved = Some("0.2.3".into());
    let p = eval
        .propose(&dep, &catalog(&["0.2.4", "0.3.1", "1.2.3"]))
        .unwrap();
    assert_eq!(p.len(), 2);
    assert!(p.iter().all(|p| p.update_type == "major"));
    assert!(p.iter().any(|p| p.new_value.as_deref() == Some("0.3")));
    assert!(p.iter().any(|p| p.new_value.as_deref() == Some("1.0")));
}

#[test]
fn satisfying_npm_range_upgrade_is_one_pin_even_with_an_older_lock() {
    let eval = Evaluator::new(&config("renovate-runner")).unwrap();
    let mut dep = dependency("plain");
    dep.current = "^1.0.0".into();
    dep.resolved = Some("1.0.0".into());
    let p = eval
        .propose(&dep, &catalog(&["1.0.1", "1.2.0", "2.0.0"]))
        .unwrap();
    let pins: Vec<_> = p.iter().filter(|p| p.update_type == "pin").collect();
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].new_value.as_deref(), Some("1.2.0"));
    assert!(
        p.iter()
            .all(|p| matches!(p.update_type.as_str(), "pin" | "major"))
    );
}

#[test]
fn numeric_mise_selectors_resolve_the_release_line_without_fake_zero_patches() {
    let mut dep = dependency("node");
    dep.manager = Manager::Mise;
    dep.kind = DependencyKind::Tool;
    dep.source = Source::Mise;
    dep.scheme = Scheme::Loose;
    dep.current = "24".into();
    dep.resolved = Some("24".into());
    let versions = catalog(&["24.0.1", "24.6.2", "26.1.0"]);
    assert_eq!(
        patchula_lookup::resolved_current_version(&dep, &versions).as_deref(),
        Some("24.6.2")
    );
    let p = Evaluator::new(&config("renovate-runner"))
        .unwrap()
        .propose(&dep, &versions)
        .unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].new_version.as_deref(), Some("26.1.0"));
}

#[test]
fn runtime_support_intervals_are_observed_without_spurious_exact_pins() {
    let cfg = config("renovate-runner");
    let files = patchula_manager::Files::from([(
        "package.json".into(),
        r#"{"engines":{"node":">=22 <27"}}"#.into(),
    )]);
    let deps = patchula_manager::Scanner::new(&cfg)
        .unwrap()
        .extract(&files)
        .unwrap()
        .dependencies;
    let dep = &deps[0];
    assert_eq!(dep.kind, DependencyKind::Toolchain);
    assert_eq!(dep.location.pointer, "/engines/node");
    let versions = catalog(&["22.1.0", "24.2.0", "26.3.0", "27.0.0"]);
    assert_eq!(
        patchula_lookup::resolved_current_version(dep, &versions).as_deref(),
        Some("26.3.0")
    );
    assert!(
        Evaluator::new(&cfg)
            .unwrap()
            .propose(dep, &versions)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn pyproject_pins_move_poetry_ranges_keep_form_and_satisfied_ranges_wait() {
    let mut cfg = config("renovate-runner");
    cfg.defaults.wait_hours = 0;
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("ruff");
    dep.manager = Manager::Pyproject;
    dep.kind = DependencyKind::Production;
    dep.source = Source::Pypi;
    dep.scheme = Scheme::Pep440;
    dep.current = "==0.6.0".into();
    dep.resolved = Some("0.6.0".into());
    let releases = catalog(&["0.6.0", "0.6.9", "0.7.1"]);
    let values: Vec<_> = eval
        .propose(&dep, &releases)
        .unwrap()
        .into_iter()
        .filter_map(|p| p.new_value)
        .collect();
    assert_eq!(values, ["==0.6.9", "==0.7.1"]);
    // A release inside an open range needs no manifest edit; lock maintenance
    // refreshes it.
    dep.current = ">=0.6".into();
    let mut preserve = cfg.clone();
    preserve.defaults.versions = patchula_config::VersionPolicy::Preserve;
    let eval = Evaluator::new(&preserve).unwrap();
    let open = eval.propose(&dep, &releases).unwrap();
    assert!(
        open.is_empty(),
        "{:?}",
        open.iter()
            .map(|p| (&p.new_value, &p.new_version, &p.update_type))
            .collect::<Vec<_>>()
    );
    dep.scheme = Scheme::Poetry;
    dep.current = "^0.6.0".into();
    let values: Vec<_> = eval
        .propose(&dep, &releases)
        .unwrap()
        .into_iter()
        .filter_map(|p| p.new_value)
        .collect();
    assert_eq!(values, ["^0.7.1"]);
}
#[test]
fn maven_variants_are_kept_and_ordered_by_maven_rules() {
    let mut cfg = config("renovate-runner");
    cfg.defaults.wait_hours = 0;
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("com.google.guava:guava");
    dep.manager = Manager::Maven;
    dep.kind = DependencyKind::Production;
    dep.source = Source::Maven;
    dep.scheme = Scheme::Maven;
    dep.current = "33.4.8-android".into();
    dep.resolved = Some(dep.current.clone());
    let releases = catalog(&[
        "33.4.8-android",
        "33.4.8-jre",
        "33.7.2-android",
        "33.7.2-jre",
        "34.0.0-jre",
        "33.8.0-rc1-android",
    ]);
    let values: Vec<_> = eval
        .propose(&dep, &releases)
        .unwrap()
        .into_iter()
        .filter_map(|p| p.new_value)
        .collect();
    assert_eq!(values, ["33.7.2-android"]);
}
/// Without a lockfile, Composer and RubyGems ranges classify their updates from
/// the range's lowest version instead of failing.
#[test]
fn unlocked_composer_and_gem_ranges_classify_from_their_floor() {
    let mut cfg = config("renovate-runner");
    cfg.defaults.wait_hours = 0;
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("symfony/console");
    dep.manager = Manager::Composer;
    dep.kind = DependencyKind::Production;
    dep.source = Source::Packagist;
    dep.scheme = Scheme::Composer;
    dep.resolved = None;
    for current in ["^6.4 || ^7.0", ">=6.4 <7.0", "^6.4"] {
        dep.current = current.into();
        let proposals = eval
            .propose(&dep, &catalog(&["6.4.0", "7.2.0", "8.0.1"]))
            .unwrap();
        assert!(
            proposals.iter().any(|p| p.update_type == "major"),
            "{current}: {proposals:?}"
        );
    }
    let mut gem = dependency("wdm");
    gem.manager = Manager::Bundler;
    gem.kind = DependencyKind::Production;
    gem.source = Source::Rubygems;
    gem.scheme = Scheme::Rubygems;
    gem.resolved = None;
    gem.current = ">= 0.1.0".into();
    assert!(
        !eval
            .propose(&gem, &catalog(&["0.1.0", "0.2.0"]))
            .unwrap()
            .is_empty()
    );
}
