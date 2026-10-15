use rustovate_config::{DependencyKind, Manager, Scheme, Source};
use rustovate_datasource::{Catalog, Version};
use rustovate_lookup::Evaluator;
use rustovate_manager::{Dependency, Location};
fn config(name: &str) -> rustovate_config::Config {
    rustovate_config::load(
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
fn rules_replace_lists_in_order_and_windows_are_reported() {
    let mut config = config("renovate-runner");
    let mut rule = config.rules[0].clone();
    rule.select=serde_json::from_value(serde_json::json!({"packages":["plain"],"dependencies":[],"managers":[],"files":[],"updates":["minor"],"sources":[],"kinds":[],"current":null})).unwrap();
    rule.apply.group = Some("early".into());
    rule.apply.wait_hours = Some(12);
    rule.apply.permitted = Some("semver:<2.0.0".into());
    rule.apply.windows = Some(vec![rustovate_config::Window {
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
    let files = rustovate_manager::Files::from([(
        ".github/workflows/test.yml".into(),
        "jobs:\n  build:\n    uses: sweepies/ops-workflows/.github/workflows/build.yml@main\n"
            .into(),
    )]);
    let extracted = rustovate_manager::Scanner::new(&config)
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
    assert_eq!(result.held_back.len(), 2);
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
    let deps = rustovate_manager::Scanner::new(&config)
        .unwrap()
        .extract(&rustovate_manager::Files::from([(
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
    let deps = rustovate_manager::Scanner::new(&cfg)
        .unwrap()
        .extract(&rustovate_manager::Files::from([(
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
            rustovate_lookup::resolved_tool_version(dep, &catalog).as_deref(),
            Some(expected)
        );
    }
}
