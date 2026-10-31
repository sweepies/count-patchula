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
        included_by: None,
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
    let cfg = config("preserve-ranges");
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
    dep.resolved = None;
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
    cfg.rules.last_mut().unwrap().apply.channel = Some(ReleaseChannel::Stable);
    // A catalog without LTS evidence, as when the `lts` listing failed, still
    // serves the stable channel.
    let stable = Evaluator::new(&cfg)
        .unwrap()
        .propose(&dep, &versions)
        .unwrap();
    assert!(
        stable
            .iter()
            .any(|p| p.new_version.as_deref() == Some("26.1.0"))
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
fn mise_latest_pins_the_newest_listed_version() {
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
        // mise lists versions without their tag prefixes (`jq-1.8.2`, `bws-v2.1.0`).
        let (versions, target) = if dep.name == "jq" {
            (vec!["1.8.1", "1.8.2"], "1.8.2")
        } else {
            (vec!["2.0.0", "2.1.0"], "2.1.0")
        };
        let proposals = eval.propose(&dep, &catalog(&versions)).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].update_type, "pin");
        assert_eq!(proposals[0].new_version.as_deref(), Some(target));
    }
}

#[test]
fn docker_policy_keeps_variants_and_python_epochs_use_pep440_order() {
    let mut cfg = config("renovate-migration");
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
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
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
    let mut dep = dependency("acme/shared-workflows");
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
    dep.package.clone_from(&dep.name);
    dep.kind = DependencyKind::Action;
    assert!(eval.digest_proposal(&dep, &"a".repeat(40)).is_some());
}
#[test]
fn exact_preserve_nightly_and_deprecated_yanked_filters() {
    let mut dep = dependency("serde");
    dep.current = "^1.0.0".into();
    let eval = Evaluator::new(&config("preserve-ranges")).unwrap();
    let p = eval.propose(&dep, &catalog(&["1.2.0", "2.0.0"])).unwrap();
    assert!(p.iter().any(|p| p.new_value.as_deref() == Some("^1.0.0")));
    assert!(p.iter().any(|p| p.new_value.as_deref() == Some("^2.0.0")));
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
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
    let eval = Evaluator::new(&config("nightly-pins")).unwrap();
    dep.name = "pingdotgg/t3code".into();
    dep.package.clone_from(&dep.name);
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
    let eval = Evaluator::new(&config("kubernetes-infra")).unwrap();
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
    let mut config = config("renovate-migration");
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
    let mut config = config("renovate-migration");
    config.ignore = vec!["!alias".into()];
    let eval = Evaluator::new(&config).unwrap();
    let mut dep = dependency("alias");
    dep.package = "real".into();
    assert!(!eval.ignored(&dep));
    assert!(eval.ignored(&dependency("other")));
}

#[test]
fn floating_dist_tags_and_latest_tools_can_be_pinned() {
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
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
    let config = config("renovate-migration");
    let files = patchula_manager::Files::from([(
        ".github/workflows/test.yml".into(),
        "jobs:\n  build:\n    uses: acme/shared-workflows/.github/workflows/build.yml@main\n"
            .into(),
    )]);
    let extracted = patchula_manager::Scanner::new(&config)
        .unwrap()
        .extract(&files)
        .unwrap();
    let dep = &extracted.dependencies[0];
    assert_eq!(dep.name, "acme/shared-workflows");
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
    let cfg = config("renovate-migration");
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
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
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
    let mut cfg = config("renovate-migration");
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
    let config = config("renovate-migration");
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
    let mut cfg = config("renovate-migration");
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
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
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
    let config = config("renovate-migration");
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
fn mise_current_versions_resolve_from_the_listing_without_proposal_filters() {
    let cfg = config("renovate-migration");
    let deps = patchula_manager::Scanner::new(&cfg)
        .unwrap()
        .extract(&patchula_manager::Files::from([(
            "mise.toml".into(),
            "[tools]\nage = \"1.2.3\"\nfnox = \"2.3.4\"\nbun = \"3.4.5\"\n".into(),
        )]))
        .unwrap()
        .dependencies;
    for (name, listed, expected) in [
        ("age", "1.2.3", "1.2.3"),
        ("fnox", "2.3.4", "2.3.4"),
        ("bun", "3.4.5", "3.4.5"),
    ] {
        let dep = deps.iter().find(|d| d.name == name).unwrap();
        let mut catalog = catalog(&[listed]);
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
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
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
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
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
    let eval = Evaluator::new(&config("preserve-ranges")).unwrap();
    let mut dep = dependency("example");
    dep.manager = Manager::Cargo;
    dep.source = Source::Crates;
    dep.current = "0.2".into();
    dep.resolved = None;
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
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
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
    let p = Evaluator::new(&config("renovate-migration"))
        .unwrap()
        .propose(&dep, &versions)
        .unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].new_version.as_deref(), Some("26.1.0"));
}

#[test]
fn runtime_support_intervals_are_observed_without_spurious_exact_pins() {
    let cfg = config("renovate-migration");
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
    let mut cfg = config("renovate-migration");
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
    // A release inside an open range needs no manifest edit: with a lock it
    // moves the lock, without one there is nothing to change.
    dep.current = ">=0.6".into();
    let mut preserve = cfg.clone();
    preserve.defaults.versions = patchula_config::VersionPolicy::Preserve;
    let eval = Evaluator::new(&preserve).unwrap();
    let open = eval.propose(&dep, &releases).unwrap();
    assert_eq!(
        open.iter()
            .map(|p| (&p.new_value, p.new_version.as_deref()))
            .collect::<Vec<_>>(),
        [(&None, Some("0.6.9")), (&None, Some("0.7.1"))]
    );
    dep.resolved = None;
    assert!(eval.propose(&dep, &releases).unwrap().is_empty());
    dep.resolved = Some("0.6.0".into());
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
    let mut cfg = config("renovate-migration");
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
    let mut cfg = config("renovate-migration");
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

/// Reference data, such as a CA root bundle, skips the release-age wait and
/// merges once checks pass through the schema's built-in rule, while the same
/// release of an ordinary package still waits.
#[test]
fn reference_data_is_fast_tracked_by_the_built_in_rule() {
    let eval = Evaluator::new(&config("defaults")).unwrap();
    let mut versions = catalog(&["2026.10.20"]);
    versions.versions[0].published_at = Some("2026-10-22T11:00:00Z".into());
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-22T12:00:00Z")
        .unwrap()
        .to_utc();
    let mut certifi = dependency("certifi");
    certifi.manager = Manager::Requirements;
    certifi.file = "requirements.txt".into();
    certifi.source = Source::Pypi;
    certifi.scheme = Scheme::Pep440;
    certifi.current = "2025.1.31".into();
    certifi.resolved = Some("2025.1.31".into());
    assert_eq!(
        certifi.kinds(),
        [DependencyKind::Production, DependencyKind::ReferenceData]
    );
    let result = eval.evaluate_at(&certifi, &versions, now).unwrap();
    assert!(result.held_back.is_empty());
    assert_eq!(result.updates.len(), 1);
    let update = &result.updates[0];
    assert_eq!(update.new_version.as_deref(), Some("2026.10.20"));
    let policy = update.policy.as_ref().unwrap();
    assert!(policy.merge);
    assert_eq!(policy.wait_hours, 0);
    assert!(policy.windows.is_empty());
    assert!(
        update.group.as_deref().is_none_or(str::is_empty),
        "{update:?}"
    );

    let mut requests = certifi.clone();
    requests.name = "requests".into();
    requests.package = "requests".into();
    assert_eq!(requests.kinds(), [DependencyKind::Production]);
    let result = eval.evaluate_at(&requests, &versions, now).unwrap();
    assert!(result.updates.is_empty());
    assert_eq!(result.held_back.len(), 1);
}

/// Versions as `mise ls-remote --json --prerelease` lists prettier: the
/// prereleases are in the catalog and the policy decides who is offered one.
fn prettier_tool(current: &str) -> Dependency {
    let mut dep = dependency("npm:prettier");
    dep.manager = Manager::Mise;
    dep.source = Source::Mise;
    dep.scheme = Scheme::Loose;
    dep.kind = DependencyKind::Tool;
    dep.current = current.into();
    dep.resolved = Some(current.into());
    dep
}
const PRETTIER: &[&str] = &[
    "2.8.8",
    "3.0.0-alpha.0",
    "3.0.0-alpha.1",
    "3.0.0-alpha.10",
    "3.0.0-alpha.11",
    "3.0.0-beta.1",
    "3.0.0",
];

#[test]
fn a_stable_mise_pin_is_not_offered_a_prerelease_from_the_listing() {
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
    let proposals = eval
        .propose(
            &prettier_tool("2.8.8"),
            &catalog(&PRETTIER[..PRETTIER.len() - 1]),
        )
        .unwrap();
    assert!(proposals.is_empty(), "{proposals:?}");
    let proposals = eval
        .propose(&prettier_tool("2.8.8"), &catalog(PRETTIER))
        .unwrap();
    assert!(!proposals.is_empty());
    assert!(
        proposals
            .iter()
            .all(|p| p.new_version.as_deref() == Some("3.0.0")),
        "{proposals:?}"
    );
}

#[test]
fn a_prerelease_mise_pin_is_offered_a_newer_prerelease() {
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
    let proposals = eval
        .propose(
            &prettier_tool("3.0.0-alpha.1"),
            &catalog(&PRETTIER[..PRETTIER.len() - 1]),
        )
        .unwrap();
    assert_eq!(proposals.len(), 1, "{proposals:?}");
    assert_eq!(proposals[0].new_version.as_deref(), Some("3.0.0-beta.1"));
}

#[test]
fn gitlab_refs_update_to_the_exact_tag_name() {
    let cfg = config("benchmark-public");
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("grp/proj");
    dep.manager = Manager::GitlabciInclude;
    dep.package = "gitlab.com/grp/proj".into();
    dep.kind = DependencyKind::ReusableWorkflow;
    dep.source = Source::ForgeTags;
    dep.scheme = Scheme::Loose;
    dep.current = "1.0".into();
    dep.resolved = Some("1.0".into());
    let proposals = eval
        .propose(&dep, &catalog(&["1.0", "1.1", "v2.0"]))
        .unwrap();
    let mut values: Vec<_> = proposals
        .iter()
        .map(|p| (p.update_type.as_str(), p.new_value.as_deref()))
        .collect();
    values.sort();
    assert_eq!(values, [("major", Some("v2.0")), ("minor", Some("1.1"))]);
}
#[test]
fn pre_commit_revs_update_to_the_exact_tag_name() {
    let cfg = config("benchmark-public");
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("org/hooks");
    dep.manager = Manager::PreCommit;
    dep.file = ".pre-commit-config.yaml".into();
    dep.kind = DependencyKind::Tool;
    dep.source = Source::GithubTags;
    dep.scheme = Scheme::Loose;
    dep.current = "v1.0".into();
    dep.resolved = Some("v1.0".into());
    let proposals = eval
        .propose(&dep, &catalog(&["v1.0", "v1.1", "2.0"]))
        .unwrap();
    let mut values: Vec<_> = proposals
        .iter()
        .map(|p| (p.update_type.as_str(), p.new_value.as_deref()))
        .collect();
    values.sort();
    assert_eq!(values, [("major", Some("2.0")), ("minor", Some("v1.1"))]);
}

/// A flake input's only update is the new revision its reference resolves to,
/// held back until that commit is as old as the release-age wait.
#[test]
fn flake_inputs_get_digest_updates_after_the_release_age_wait() {
    let mut cfg = config("benchmark-public");
    cfg.defaults.wait_hours = 24;
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("nixpkgs");
    dep.manager = Manager::Nix;
    dep.kind = DependencyKind::FlakeInput;
    dep.source = Source::Nix;
    dep.scheme = Scheme::Loose;
    dep.package = "https://github.com/NixOS/nixpkgs".into();
    dep.current = "github:NixOS/nixpkgs/nixos-unstable".into();
    dep.resolved = None;
    dep.digest = Some("1".repeat(40));
    assert!(eval.needs_lookup(&dep));
    let resolved = |digest: String| Catalog {
        versions: vec![Version {
            version: digest.clone(),
            published_at: Some("2026-10-20T00:00:00Z".into()),
            digest: Some(digest),
            deprecated: false,
            yanked: false,
        }],
        dist_tags: Default::default(),
    };
    let moved = resolved("2".repeat(40));
    let early = eval
        .evaluate_at(&dep, &moved, "2026-10-20T12:00:00Z".parse().unwrap())
        .unwrap();
    assert!(early.updates.is_empty());
    assert_eq!(early.held_back[0].update_type, "digest");
    assert_eq!(early.held_back[0].eligible_at, "2026-10-21T00:00:00Z");
    let later = eval
        .evaluate_at(&dep, &moved, "2026-10-22T00:00:00Z".parse().unwrap())
        .unwrap();
    let [update] = later.updates.as_slice() else {
        panic!("one update expected: {:?}", later.updates);
    };
    assert_eq!(update.update_type, "digest");
    assert_eq!(update.new_digest.as_deref(), Some("2".repeat(40).as_str()));
    assert_eq!(update.new_value, None);
    assert!(update.release_age.is_some());
    let unchanged = eval
        .evaluate_at(
            &dep,
            &resolved("1".repeat(40)),
            "2026-10-22T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
    assert!(unchanged.updates.is_empty() && unchanged.held_back.is_empty());
}

#[test]
fn apm_short_tags_are_fixed_tags_not_floating_aliases() {
    let eval = Evaluator::new(&config("renovate-migration")).unwrap();
    let mut dep = dependency("example/package");
    dep.manager = Manager::Apm;
    dep.file = "apm.yml".into();
    dep.source = Source::GithubTags;
    dep.scheme = Scheme::Loose;
    dep.current = "v1.0".into();
    dep.resolved = Some("v1.0".into());
    let versions = catalog(&["v1.0", "v1.0.1", "v1.1"]);
    assert!(patchula_lookup::resolved_current_version(&dep, &versions).is_none());
    let proposals = eval.propose(&dep, &versions).unwrap();
    let patch = proposals.iter().find(|p| p.update_type == "patch").unwrap();
    assert_eq!(patch.new_value.as_deref(), Some("v1.0.1"));
    let minor = proposals.iter().find(|p| p.update_type == "minor").unwrap();
    assert_eq!(minor.new_value.as_deref(), Some("v1.1"));
}

fn image(digest: &str) -> Dependency {
    Dependency {
        manager: Manager::Dockerfile,
        kind: DependencyKind::ContainerImage,
        file: "Dockerfile".into(),
        name: "ghcr.io/acme/app".into(),
        package: "ghcr.io/acme/app".into(),
        current: "1.0".into(),
        resolved: None,
        digest: Some(digest.into()),
        source: Source::Docker,
        scheme: Scheme::Loose,
        ..dependency("unused")
    }
}

#[test]
fn an_image_rebuild_is_secured_only_as_security_apply_allows() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("patchula.pkl");
    let secured = |overrides: &str, tag: Option<&str>| {
        std::fs::write(
            &path,
            format!("amends \"PatchulaConfig.pkl\"\nsecurity {{ apply {{ {overrides} }} }}\n"),
        )
        .unwrap();
        let eval = Evaluator::new(&patchula_config::load(&path).unwrap()).unwrap();
        let dep = image(&format!("sha256:{}", "a".repeat(64)));
        let mut update = eval
            .digest_proposal(&dep, &format!("sha256:{}", "b".repeat(64)))
            .unwrap();
        if let Some(tag) = tag {
            update.update_type = "minor".into();
            update.new_version = Some(tag.into());
        }
        let applied = eval.secure(&dep, &mut update);
        // A refused rebuild keeps the policy it had.
        assert_eq!(
            update.policy.as_ref().unwrap().wait_hours == 0,
            applied && overrides.contains("waitHours")
        );
        // A fixed rebuild ships as a fix, like a package fix.
        assert_eq!(
            update.policy.as_ref().unwrap().commit_prefix,
            if applied {
                "fix(deps):"
            } else {
                "chore(deps):"
            }
        );
        applied
    };
    assert!(secured("waitHours = 0", None));
    assert!(!secured("waitHours = 0; active = false", None));
    assert!(!secured("waitHours = 0; digest = false", None));
    // `permitted` filters the tag a rebuild moves to, not a same-tag rebuild.
    assert!(secured("waitHours = 0; permitted = \"semver:<2\"", None));
    assert!(secured(
        "waitHours = 0; permitted = \"semver:<2\"",
        Some("1.5.0")
    ));
    assert!(!secured(
        "waitHours = 0; permitted = \"semver:<2\"",
        Some("3.0.0")
    ));
}

/// Cargo, pyproject, Bundler and Composer keep one version per package in a
/// lockfile, so under `preserve` a release the declaration already allows moves
/// that lock and leaves the manifest as it is.
#[test]
fn in_range_releases_are_lock_only_updates_when_a_lockfile_holds_the_version() {
    let mut cfg = config("preserve-ranges");
    cfg.defaults.wait_hours = 0;
    let eval = Evaluator::new(&cfg).unwrap();
    for (manager, source, scheme, current, locked, newer) in [
        (
            Manager::Cargo,
            Source::Crates,
            Scheme::Semver,
            "1",
            "1.39.0",
            "1.40.0",
        ),
        (
            Manager::Pyproject,
            Source::Pypi,
            Scheme::Pep440,
            ">=2.28",
            "2.28.0",
            "2.32.3",
        ),
        (
            Manager::Pyproject,
            Source::Pypi,
            Scheme::Poetry,
            "^2.28",
            "2.28.0",
            "2.32.3",
        ),
        (
            Manager::Bundler,
            Source::Rubygems,
            Scheme::Rubygems,
            "~> 7.1",
            "7.1.2",
            "7.2.0",
        ),
        (
            Manager::Composer,
            Source::Packagist,
            Scheme::Composer,
            "^3.0",
            "3.0.0",
            "3.2.0",
        ),
    ] {
        let mut dep = dependency("example");
        dep.manager = manager;
        dep.source = source;
        dep.scheme = scheme;
        dep.current = current.into();
        dep.resolved = Some(locked.into());
        let releases = catalog(&[locked, newer]);
        let proposals = eval.propose(&dep, &releases).unwrap();
        assert_eq!(proposals.len(), 1, "{manager:?}: {proposals:?}");
        assert_eq!(proposals[0].new_value, None, "{manager:?}");
        assert_eq!(proposals[0].new_version.as_deref(), Some(newer));
        assert_eq!(proposals[0].update_type, "minor", "{manager:?}");
        // The locked version is the baseline, not the range's floor: a lock
        // already at the newest release has nothing to move to.
        dep.resolved = Some(newer.into());
        assert!(eval.propose(&dep, &releases).unwrap().is_empty());
        // Without a committed lockfile there is nothing to change.
        dep.resolved = None;
        assert!(eval.propose(&dep, &releases).unwrap().is_empty());
    }
}

#[test]
fn lock_only_updates_keep_the_wait_the_permitted_filter_and_exact_policy() {
    let mut cfg = config("preserve-ranges");
    cfg.defaults.wait_hours = 24;
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("tokio");
    dep.manager = Manager::Cargo;
    dep.source = Source::Crates;
    dep.current = "1".into();
    dep.resolved = Some("1.39.0".into());
    let mut releases = catalog(&["1.39.0", "1.40.0", "1.41.0"]);
    for (release, date) in releases.versions.iter_mut().zip([
        "2026-09-01T00:00:00Z",
        "2026-10-01T00:00:00Z",
        "2026-10-15T00:00:00Z",
    ]) {
        release.published_at = Some(date.into());
    }
    // 1.41.0 is still inside the wait, so the lock moves to 1.40.0.
    let result = eval
        .evaluate_at(&dep, &releases, instant("2026-10-15T12:00:00Z"))
        .unwrap();
    assert_eq!(result.updates.len(), 1);
    assert_eq!(result.updates[0].new_value, None);
    assert_eq!(result.updates[0].new_version.as_deref(), Some("1.40.0"));
    assert_eq!(result.held_back[0].version, "1.41.0");
    // `permitted` filters a lock-only release like any other: 1.41.0 and 1.42.0
    // are neither proposed nor held back, and 1.40.0 stays.
    let mut filtered = cfg.clone();
    filtered.defaults.permitted = Some("semver:<1.41.0".into());
    let mut later = releases.clone();
    later.versions.push(patchula_datasource::Version {
        version: "1.42.0".into(),
        published_at: Some("2026-10-15T11:00:00Z".into()),
        digest: None,
        deprecated: false,
        yanked: false,
    });
    let result = Evaluator::new(&filtered)
        .unwrap()
        .evaluate_at(&dep, &later, instant("2026-10-15T12:00:00Z"))
        .unwrap();
    assert_eq!(
        result
            .updates
            .iter()
            .map(|u| (u.new_value.as_deref(), u.new_version.as_deref()))
            .collect::<Vec<_>>(),
        [(None, Some("1.40.0"))]
    );
    assert!(result.held_back.is_empty(), "{:?}", result.held_back);
    // Exact policy still pins the declaration.
    cfg.defaults.versions = patchula_config::VersionPolicy::Exact;
    cfg.rules.retain(|rule| rule.apply.versions.is_none());
    let exact = Evaluator::new(&cfg)
        .unwrap()
        .evaluate_at(&dep, &releases, instant("2026-10-15T12:00:00Z"))
        .unwrap();
    assert_eq!(exact.updates[0].new_value.as_deref(), Some("=1.40.0"));
}

/// Out of range, `preserve` shifts the declaration as before.
#[test]
fn out_of_range_releases_still_shift_the_declaration_under_preserve() {
    let mut cfg = config("preserve-ranges");
    cfg.defaults.wait_hours = 0;
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("tokio");
    dep.manager = Manager::Cargo;
    dep.source = Source::Crates;
    dep.current = "1".into();
    dep.resolved = Some("1.39.0".into());
    let proposals = eval
        .propose(&dep, &catalog(&["1.39.0", "1.40.0", "2.1.0"]))
        .unwrap();
    let by_version = |v: &str| {
        proposals
            .iter()
            .find(|p| p.new_version.as_deref() == Some(v))
            .unwrap()
            .new_value
            .clone()
    };
    assert_eq!(by_version("1.40.0"), None);
    assert_eq!(by_version("2.1.0").as_deref(), Some("2"));
}

/// The schema's second built-in rule makes ecosystems with one version per
/// package and a lockfile preserve their ranges; pins stay the default
/// elsewhere.
#[test]
fn cargo_pyproject_bundler_and_composer_preserve_by_default_and_the_rest_pin() {
    let eval = Evaluator::new(&config("defaults")).unwrap();
    for (manager, source, scheme, current, locked, newer, versions) in [
        (
            Manager::Cargo,
            Source::Crates,
            Scheme::Semver,
            "1",
            "1.39.0",
            "1.40.0",
            "preserve",
        ),
        (
            Manager::Pyproject,
            Source::Pypi,
            Scheme::Pep440,
            ">=2",
            "2.0.0",
            "2.1.0",
            "preserve",
        ),
        (
            Manager::Bundler,
            Source::Rubygems,
            Scheme::Rubygems,
            "~> 7.1",
            "7.1.0",
            "7.2.0",
            "preserve",
        ),
        (
            Manager::Composer,
            Source::Packagist,
            Scheme::Composer,
            "^3.0",
            "3.0.0",
            "3.1.0",
            "preserve",
        ),
        (
            Manager::Requirements,
            Source::Pypi,
            Scheme::Pep440,
            "==2.0.0",
            "2.0.0",
            "2.1.0",
            "exact",
        ),
        (
            Manager::Npm,
            Source::Npm,
            Scheme::Semver,
            "^2.0.0",
            "2.0.0",
            "2.1.0",
            "exact",
        ),
    ] {
        let mut dep = dependency("example");
        dep.manager = manager;
        dep.source = source;
        dep.scheme = scheme;
        dep.current = current.into();
        dep.resolved = Some(locked.into());
        let mut releases = catalog(&[locked, newer]);
        releases.versions[1].published_at = Some("2020-01-01T00:00:00Z".into());
        let proposals = eval.propose(&dep, &releases).unwrap();
        assert_eq!(proposals.len(), 1, "{manager:?}");
        assert_eq!(
            proposals[0].policy.as_ref().unwrap().versions.as_deref(),
            Some(versions),
            "{manager:?}"
        );
    }
}

/// The lookup worked; the declaration is what cannot be rewritten. That is a
/// release to report, not an error that drops the dependency's other updates.
#[test]
fn constraints_that_cannot_be_rewritten_are_reported_not_failed() {
    let mut cfg = config("preserve-ranges");
    cfg.defaults.wait_hours = 0;
    let eval = Evaluator::new(&cfg).unwrap();
    for (manager, source, scheme, current, locked) in [
        (
            Manager::Bundler,
            Source::Rubygems,
            Scheme::Rubygems,
            ">= 1.2, < 2",
            "1.5.0",
        ),
        (
            Manager::Composer,
            Source::Packagist,
            Scheme::Composer,
            "^1 | ^2",
            "2.5.0",
        ),
    ] {
        let mut dep = dependency("example");
        dep.manager = manager;
        dep.source = source;
        dep.scheme = scheme;
        dep.current = current.into();
        dep.resolved = Some(locked.into());
        let now = instant("2026-02-01T00:00:00Z");
        let result = eval
            .evaluate_at(&dep, &catalog(&[locked, "1.6.0", "3.0.0"]), now)
            .unwrap_or_else(|e| panic!("{manager:?}: {e}"));
        assert_eq!(
            result
                .not_proposed
                .iter()
                .map(|n| n.version.as_str())
                .collect::<Vec<_>>(),
            ["3.0.0"],
            "{manager:?}"
        );
        assert!(result.not_proposed[0].reason.contains(current));
        // The in-range release is still an update.
        assert!(
            result
                .updates
                .iter()
                .all(|u| u.new_version.as_deref() != Some("3.0.0")),
            "{manager:?}"
        );
    }
}

/// When the newest release of a track needs a rewrite the Count will not guess,
/// the older release of the same track that the declaration allows is still
/// proposed, and only the newest is reported.
#[test]
fn an_unrewritable_newest_release_does_not_hide_an_older_one_the_range_allows() {
    let mut cfg = config("preserve-ranges");
    cfg.defaults.wait_hours = 0;
    let eval = Evaluator::new(&cfg).unwrap();
    let mut dep = dependency("example");
    dep.manager = Manager::Composer;
    dep.source = Source::Packagist;
    dep.scheme = Scheme::Composer;
    dep.current = ">=1.4.0 <1.4.5".into();
    dep.resolved = Some("1.4.0".into());
    let result = eval
        .evaluate_at(
            &dep,
            &catalog(&["1.4.0", "1.4.3", "1.4.4", "1.4.5"]),
            instant("2026-02-01T00:00:00Z"),
        )
        .unwrap();
    assert_eq!(
        result
            .updates
            .iter()
            .map(|u| (u.new_value.as_deref(), u.new_version.as_deref()))
            .collect::<Vec<_>>(),
        [(None, Some("1.4.4"))]
    );
    assert_eq!(
        result
            .not_proposed
            .iter()
            .map(|n| n.version.as_str())
            .collect::<Vec<_>>(),
        ["1.4.5"]
    );
}

/// A Maven or NuGet interval has no resolved version, so it starts at its first
/// bound: `widen` extends its upper bound, `preserve` reports the release it
/// cannot reach, and a release the interval already allows is not an update.
#[test]
fn bracketed_intervals_start_at_their_first_bound_and_widen() {
    let mut cfg = config("preserve-ranges");
    cfg.defaults.wait_hours = 0;
    let now = instant("2026-02-01T00:00:00Z");
    for (manager, source, scheme, current, versions, widened) in [
        (
            Manager::Maven,
            Source::Maven,
            Scheme::Maven,
            "[1.0,2.0)",
            ["1.0", "1.5", "3.1"],
            "[1.0,3.1]",
        ),
        (
            Manager::Maven,
            Source::Maven,
            Scheme::Maven,
            "(,2.0)",
            ["1.0", "1.5", "3.1"],
            "(,3.1]",
        ),
        (
            Manager::Nuget,
            Source::Nuget,
            Scheme::Nuget,
            "[1.0.0,2.0.0)",
            ["1.0.0", "1.5.0", "3.1.0"],
            "[1.0.0,3.1.0]",
        ),
    ] {
        let mut dep = dependency("example");
        dep.manager = manager;
        dep.source = source;
        dep.scheme = scheme;
        dep.current = current.into();
        dep.resolved = None;
        let releases = catalog(&versions);
        cfg.defaults.versions = patchula_config::VersionPolicy::Widen;
        let widen = Evaluator::new(&cfg)
            .unwrap()
            .evaluate_at(&dep, &releases, now)
            .unwrap_or_else(|e| panic!("{current}: {e}"));
        assert_eq!(
            widen
                .updates
                .iter()
                .map(|u| (u.new_value.as_deref(), u.new_version.as_deref()))
                .collect::<Vec<_>>(),
            [(Some(widened), Some(versions[2]))],
            "{current}"
        );
        cfg.defaults.versions = patchula_config::VersionPolicy::Preserve;
        let preserve = Evaluator::new(&cfg)
            .unwrap()
            .evaluate_at(&dep, &releases, now)
            .unwrap();
        assert!(
            preserve.updates.is_empty(),
            "{current}: {:?}",
            preserve.updates
        );
        assert_eq!(
            preserve
                .not_proposed
                .iter()
                .map(|n| n.version.as_str())
                .collect::<Vec<_>>(),
            [versions[2]],
            "{current}"
        );
    }
}
