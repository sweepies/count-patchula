//! The built-in Pkl repository group: a Pkl package and the other
//! dependencies its GitHub repository ships share a group named after the
//! repository, unless anything set a group for one of them.
use patchula_config::{Config, Rule};
use patchula_datasource::{Catalog, Version};
use patchula_lookup::{Context, Evaluator};
use patchula_manager::{Dependency, EditForm, Syntax};
use patchula_words::{DependencyKind, Manager, Scheme, Source, Update};

fn config() -> Config {
    patchula_config::load(std::path::Path::new("../../PatchulaConfig.pkl")).unwrap()
}
fn dep(manager: Manager, source: Source, file: &str, package: &str, current: &str) -> Dependency {
    Dependency {
        file: file.into(),
        form: EditForm::Scalar(Syntax::Bare),
        name: package.rsplit('/').next().unwrap().into(),
        package: package.into(),
        current: current.into(),
        resolved: Some(current.into()),
        scheme: if source == Source::Go {
            Scheme::Go
        } else {
            Scheme::Semver
        },
        ..patchula_manager::testing::dependency(manager, DependencyKind::Production, source)
    }
}
fn hk_tool() -> Dependency {
    dep(Manager::Mise, Source::Mise, "mise.toml", "hk", "2.10.0")
}
fn hk_package() -> Dependency {
    dep(
        Manager::Pkl,
        Source::Pkl,
        "hk.pkl",
        "pkg.pkl-lang.org/github.com/jdx/hk/hk",
        "2.10.0",
    )
}
fn pkl_go_module() -> Dependency {
    dep(
        Manager::Go,
        Source::Go,
        "go.mod",
        "github.com/apple/pkl-go",
        "v0.9.0",
    )
}
fn pkl_go_package() -> Dependency {
    dep(
        Manager::Pkl,
        Source::Pkl,
        "pkl/PklProject",
        "pkg.pkl-lang.org/pkl-go/pkl.golang",
        "0.9.0",
    )
}
fn rule(select: serde_json::Value, apply: serde_json::Value) -> Rule {
    let mut full = serde_json::json!({
        "packages": [], "dependencies": [], "managers": [], "files": [],
        "updates": [], "sources": [], "kinds": [], "current": null,
    });
    full.as_object_mut()
        .unwrap()
        .extend(select.as_object().unwrap().clone());
    serde_json::from_value(serde_json::json!({"note": "test", "select": full, "apply": apply}))
        .unwrap()
}
fn evaluator(config: &Config, deps: &[Dependency]) -> Evaluator {
    Evaluator::new(
        config,
        Context {
            dependencies: deps.iter().collect(),
            ..Default::default()
        },
    )
    .unwrap()
}
fn catalog(versions: &[&str]) -> Catalog {
    Catalog {
        versions: versions
            .iter()
            .map(|v| Version {
                version: (*v).into(),
                published_at: None,
                digest: None,
                deprecated: false,
                yanked: false,
                prerelease: false,
                unchecked: false,
            })
            .collect(),
        dist_tags: Default::default(),
    }
}
/// The group of `dep`'s minor update.
fn group(eval: &Evaluator, dep: &Dependency) -> Option<String> {
    let (next, current) = if dep.source == Source::Go {
        ("v0.10.0", "v0.9.0")
    } else if dep.current.starts_with("0.") {
        ("0.10.0", "0.9.0")
    } else {
        ("2.11.0", "2.10.0")
    };
    let proposals = eval.propose(dep, &catalog(&[current, next])).unwrap();
    let minor = proposals
        .iter()
        .find(|p| p.update_type == Update::Minor)
        .unwrap_or_else(|| panic!("{}: {proposals:?}", dep.package));
    minor.group.clone()
}
fn groups(eval: &Evaluator, deps: &[Dependency]) -> Vec<Option<String>> {
    deps.iter().map(|d| group(eval, d)).collect()
}
const HK: Option<&str> = Some("jdx/hk");

#[test]
fn a_pkl_package_groups_with_its_repositorys_mise_tool() {
    let deps = [hk_tool(), hk_package()];
    let eval = evaluator(&config(), &deps);
    assert_eq!(groups(&eval, &deps), vec![HK.map(String::from); 2]);
}

#[test]
fn a_pkl_package_groups_with_its_repositorys_go_module() {
    let deps = [pkl_go_module(), pkl_go_package()];
    let eval = evaluator(&config(), &deps);
    assert_eq!(
        groups(&eval, &deps),
        [
            Some("apple/pkl-go".to_owned()),
            Some("apple/pkl-go".to_owned())
        ]
    );
}

#[test]
fn the_go_code_generator_tool_joins_through_its_module_path() {
    let generator = dep(
        Manager::Mise,
        Source::Mise,
        "mise.toml",
        "go:github.com/apple/pkl-go/cmd/pkl-gen-go",
        "0.9.0",
    );
    let deps = [generator, pkl_go_package()];
    let eval = evaluator(&config(), &deps);
    assert_eq!(
        groups(&eval, &deps),
        [
            Some("apple/pkl-go".to_owned()),
            Some("apple/pkl-go".to_owned())
        ]
    );
}

#[test]
fn pkl_packages_alone_or_other_dependencies_alone_form_no_group() {
    let pantry = |name: &str| {
        dep(
            Manager::Pkl,
            Source::Pkl,
            "PklProject",
            &format!("pkg.pkl-lang.org/pkl-pantry/{name}"),
            "1.0.0",
        )
    };
    let deps = [pantry("pkl.toml"), pantry("k8s.contrib")];
    let eval = evaluator(&config(), &deps);
    let mut a = deps[0].clone();
    a.current = "2.10.0".into();
    a.resolved = a.current.clone().into();
    assert_eq!(group(&eval, &a), None);

    let tool = dep(
        Manager::Mise,
        Source::Mise,
        "mise.toml",
        "go:github.com/apple/pkl-go/cmd/pkl-gen-go",
        "0.9.0",
    );
    let deps = [pkl_go_module(), tool];
    let eval = evaluator(&config(), &deps);
    assert_eq!(groups(&eval, &deps), [None, None]);
}

#[test]
fn an_empty_group_on_either_side_frees_both() {
    for sources in [["pkl"], ["mise"]] {
        let mut config = config();
        config.rules.push(rule(
            serde_json::json!({"sources": sources}),
            serde_json::json!({"group": ""}),
        ));
        let deps = [hk_tool(), hk_package()];
        let eval = evaluator(&config, &deps);
        assert_eq!(groups(&eval, &deps), [None, None], "{sources:?}");
    }
}

#[test]
fn a_default_group_wins_whether_empty_or_not() {
    for (default, expected) in [("", None), ("all", Some("all".to_owned()))] {
        let mut config = config();
        config.defaults.group = Some(default.into());
        let deps = [hk_tool(), hk_package()];
        let eval = evaluator(&config, &deps);
        assert_eq!(
            groups(&eval, &deps),
            [expected.clone(), expected],
            "{default:?}"
        );
    }
}

#[test]
fn a_member_grouped_elsewhere_leaves_its_partner_alone() {
    let mut config = config();
    config.rules.push(rule(
        serde_json::json!({"managers": ["mise"]}),
        serde_json::json!({"group": "tools"}),
    ));
    let deps = [hk_tool(), hk_package()];
    let eval = evaluator(&config, &deps);
    assert_eq!(groups(&eval, &deps), [Some("tools".to_owned()), None]);
}

#[test]
fn a_user_group_of_the_same_name_joins_the_group() {
    let mut config = config();
    let lint = dep(
        Manager::Npm,
        Source::Npm,
        "package.json",
        "eslint",
        "2.10.0",
    );
    config.rules.push(rule(
        serde_json::json!({"packages": ["eslint"]}),
        serde_json::json!({"group": "jdx/hk"}),
    ));
    let deps = [hk_tool(), hk_package(), lint];
    let eval = evaluator(&config, &deps);
    assert_eq!(groups(&eval, &deps), vec![HK.map(String::from); 3]);
}

#[test]
fn an_inactive_or_ignored_partner_leaves_no_group() {
    let mut inactive = config();
    inactive.rules.push(rule(
        serde_json::json!({"sources": ["pkl"]}),
        serde_json::json!({"active": false}),
    ));
    let mut ignored = config();
    ignored.ignore =
        patchula_config::Patterns::compile(&["pkg.pkl-lang.org/github.com/jdx/hk/hk".into()])
            .unwrap();
    for config in [inactive, ignored] {
        let deps = [hk_tool(), hk_package()];
        let eval = evaluator(&config, &deps);
        assert_eq!(group(&eval, &deps[0]), None);
    }
}

#[test]
fn a_registry_qualified_pkl_package_does_not_join() {
    let mut private = hk_package();
    private.registry = Some("https://pkl.example.com".into());
    let deps = [hk_tool(), private];
    let eval = evaluator(&config(), &deps);
    assert_eq!(groups(&eval, &deps), [None, None]);
}

#[test]
fn a_mise_tool_with_options_does_not_join() {
    let tool = dep(
        Manager::Mise,
        Source::Mise,
        "mise.toml",
        "github:jdx/hk[bin=hk]",
        "2.10.0",
    );
    let deps = [tool, hk_package()];
    let eval = evaluator(&config(), &deps);
    assert_eq!(groups(&eval, &deps), [None, None]);
}

#[test]
fn mismatched_commit_prefixes_do_not_join() {
    let mut config = config();
    config.rules.push(rule(
        serde_json::json!({"sources": ["pkl"]}),
        serde_json::json!({"commitPrefix": "build(deps):"}),
    ));
    let deps = [hk_tool(), hk_package()];
    let eval = evaluator(&config, &deps);
    assert_eq!(groups(&eval, &deps), [None, None]);
}

#[test]
fn a_group_is_per_update_kind() {
    let mut config = config();
    config.rules.push(rule(
        serde_json::json!({"sources": ["pkl"], "updates": ["major"]}),
        serde_json::json!({"group": ""}),
    ));
    let deps = [hk_tool(), hk_package()];
    let eval = evaluator(&config, &deps);
    assert_eq!(groups(&eval, &deps), vec![HK.map(String::from); 2]);
    let major = eval
        .propose(&deps[0], &catalog(&["2.10.0", "3.0.0"]))
        .unwrap();
    let major = major
        .iter()
        .find(|p| p.update_type == Update::Major)
        .unwrap();
    assert_eq!(major.group, None);
}

#[test]
fn an_advisory_fix_keeps_its_own_pull_request() {
    use patchula_datasource::{Advisory, Affected, Event, Range};
    let deps = [pkl_go_module(), pkl_go_package()];
    let eval = evaluator(&config(), &deps);
    let query = patchula_lookup::advisory_query(&deps[0]).unwrap();
    let found = vec![std::sync::Arc::new(Advisory {
        id: "GHSA-test".into(),
        modified: "2026-10-01T00:00:00Z".into(),
        aliases: vec![],
        summary: None,
        severity: None,
        affected: vec![Affected {
            ecosystem: "Go".into(),
            package: "github.com/apple/pkl-go".into(),
            ranges: vec![Range {
                kind: "SEMVER".into(),
                events: vec![Event::Introduced("0".into()), Event::Fixed("0.10.0".into())],
            }],
            versions: vec![],
        }],
    })];
    let now = chrono::Utc::now();
    let evaluation = eval
        .evaluate_with(
            &deps[0],
            &catalog(&["v0.9.0", "v0.10.0"]),
            now,
            &Default::default(),
            Some((&query, &found)),
        )
        .unwrap();
    let fix = evaluation
        .updates
        .iter()
        .find(|u| !u.fixes.is_empty())
        .unwrap_or_else(|| panic!("{:?}", evaluation.updates));
    assert_eq!(fix.group, None);
    assert_eq!(group(&eval, &deps[1]).as_deref(), Some("apple/pkl-go"));
}

/// Minors, patches and digests share one branch, so a dependency joins only
/// when it may for all three, with one commit prefix across them; anything
/// else would hold the branch for conflicting prefixes.
#[test]
fn a_prefix_that_differs_across_a_track_forms_no_group() {
    let mut config = config();
    config.rules.push(rule(
        serde_json::json!({"updates": ["patch"]}),
        serde_json::json!({"commitPrefix": "fix(deps):"}),
    ));
    let mut package = hk_package();
    package.current = "2.11.0".into();
    package.resolved = Some("2.11.0".into());
    let deps = [hk_tool(), package];
    let eval = evaluator(&config, &deps);
    let release = catalog(&["2.10.0", "2.11.0", "2.11.1"]);
    for dep in &deps {
        for update in eval.propose(dep, &release).unwrap() {
            assert_eq!(update.group, None, "{} {update:?}", dep.package);
        }
    }
}

#[test]
fn members_with_two_prefixes_form_no_group() {
    let mut config = config();
    config.rules.push(rule(
        serde_json::json!({"files": ["b.pkl", "mise.toml"]}),
        serde_json::json!({"commitPrefix": "build(deps):"}),
    ));
    let mut a = pkl_go_package();
    a.file = "a.pkl".into();
    let mut b = pkl_go_package();
    b.file = "b.pkl".into();
    let generator = dep(
        Manager::Mise,
        Source::Mise,
        "mise.toml",
        "go:github.com/apple/pkl-go/cmd/pkl-gen-go",
        "0.9.0",
    );
    let deps = [a, pkl_go_module(), b, generator];
    let eval = evaluator(&config, &deps);
    assert_eq!(groups(&eval, &deps), [None, None, None, None]);
}

/// A dependency that may not take one kind on the track is no member for
/// any of them, so its branch does not change between a patch and a minor.
#[test]
fn membership_holds_for_the_whole_track() {
    let mut config = config();
    config.rules.push(rule(
        serde_json::json!({"sources": ["mise"], "updates": ["minor"]}),
        serde_json::json!({"active": false}),
    ));
    let deps = [hk_tool(), hk_package()];
    let eval = evaluator(&config, &deps);
    let patch = eval
        .propose(&deps[1], &catalog(&["2.10.0", "2.10.1"]))
        .unwrap();
    assert_eq!(patch[0].update_type, Update::Patch);
    assert_eq!(patch[0].group, None);
    assert_eq!(group(&eval, &deps[1]), None);
}

#[test]
fn pins_never_group() {
    let mut latest = hk_tool();
    latest.kind = DependencyKind::Tool;
    latest.current = "latest".into();
    latest.resolved = None;
    let deps = [latest.clone(), hk_package()];
    let eval = evaluator(&config(), &deps);
    let mut versions = catalog(&["2.10.0", "2.11.0"]);
    versions.dist_tags.insert("latest".into(), "2.11.0".into());
    let pins = eval.propose(&latest, &versions).unwrap();
    assert!(
        pins.iter().any(|p| p.update_type == Update::Pin),
        "{pins:?}"
    );
    assert!(pins.iter().all(|p| p.group.is_none()), "{pins:?}");

    let action = Dependency {
        file: ".github/workflows/ci.yml".into(),
        form: EditForm::Scalar(Syntax::Yaml),
        name: "jdx/hk".into(),
        package: "jdx/hk".into(),
        current: "v2.10.0".into(),
        resolved: Some("v2.10.0".into()),
        scheme: Scheme::Loose,
        ..patchula_manager::testing::dependency(
            Manager::GithubActions,
            DependencyKind::Action,
            Source::GithubTags,
        )
    };
    let deps = [action.clone(), hk_package()];
    let eval = evaluator(&config(), &deps);
    let pin = eval
        .digest_proposal(&action, &"a".repeat(40))
        .unwrap()
        .expect("a digest pin");
    assert_eq!(pin.update_type, Update::Pin);
    assert_eq!(pin.group, None);
    // The action is still a partner for the Pkl package's version updates.
    assert_eq!(group(&eval, &deps[1]).as_deref(), HK);
}

/// A Copier template is reported, never edited, so it can share no PR.
#[test]
fn a_report_only_dependency_is_no_partner() {
    let template = Dependency {
        file: ".copier-answers.yml".into(),
        ..dep(
            Manager::Copier,
            Source::GithubTags,
            ".copier-answers.yml",
            "jdx/hk",
            "v2.10.0",
        )
    };
    let template = Dependency {
        kind: DependencyKind::Template,
        ..template
    };
    let deps = [template, hk_package()];
    let eval = evaluator(&config(), &deps);
    assert_eq!(group(&eval, &deps[1]), None);
}
