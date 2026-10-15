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
