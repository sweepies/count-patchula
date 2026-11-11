//! Manager-naming conditions the corpus snapshot does not reach, each found
//! by a surviving cargo-mutants mutant: the toolchain support-range rule, the
//! normalized value a tool input is written with, the constraint a toolchain
//! resolves against, and the advisory ecosystem an action answers to.
use patchula_datasource::{Advisory, AdvisoryQuery, Affected, Catalog, Event, Range, Version};
use patchula_lookup::{Evaluator, advisory_query};
use patchula_manager::{Dependency, EditForm, Location, Syntax};
use patchula_words::{DependencyKind, Manager, Scheme, Source};

fn eval() -> Evaluator {
    let config = patchula_config::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus"),
    )
    .unwrap();
    Evaluator::new(&config).unwrap()
}

fn dep(manager: Manager, kind: DependencyKind) -> Dependency {
    Dependency {
        manager,
        file: "file".into(),
        location: Location {
            start: 0,
            end: 1,
            line: 1,
            column: 1,
            pointer: String::new(),
        },
        form: EditForm::Scalar(Syntax::Bare),
        name: "pkg".into(),
        package: "pkg".into(),
        kind,
        current: "1.0.0".into(),
        resolved: Some("1.0.0".into()),
        digest: None,
        source: Source::GithubReleases,
        scheme: Scheme::Semver,
        extract: None,
        included_by: None,
        registry: None,
    }
}

fn catalog(values: &[&str]) -> Catalog {
    Catalog {
        versions: values
            .iter()
            .map(|v| Version {
                version: (*v).into(),
                published_at: None,
                digest: None,
                deprecated: false,
                yanked: false,
            })
            .collect(),
        dist_tags: Default::default(),
    }
}

/// A `required_version` constraint describes supported releases, not an
/// installed pin: releases are observed, never proposed.
#[test]
fn a_terraform_toolchain_constraint_proposes_nothing() {
    let mut dep = dep(Manager::Terraform, DependencyKind::Toolchain);
    dep.scheme = Scheme::Hashicorp;
    dep.current = "~> 1.5".into();
    dep.resolved = None;
    assert_eq!(
        eval()
            .propose(&dep, &catalog(&["1.5.0", "1.6.0", "2.0.0"]))
            .unwrap(),
        Vec::new(),
        "the support range keeps releases out of the proposal lanes"
    );
}

/// The toolchain's resolved value is the newest release the constraint
/// allows — not simply the newest.
#[test]
fn a_toolchain_resolves_to_the_newest_matching_release() {
    let mut dep = dep(Manager::Terraform, DependencyKind::Toolchain);
    dep.scheme = Scheme::Hashicorp;
    dep.current = "~> 1.5".into();
    dep.resolved = None;
    // The newest matching release is yanked, a newer stable release does not
    // satisfy the constraint, and a prerelease never counts.
    let mut catalog = catalog(&["1.6.0", "1.5.9", "9.9.9", "1.7.0-rc.1"]);
    catalog.versions[0].yanked = true;
    assert_eq!(
        eval().resolved_current_version(&dep, &catalog).unwrap(),
        Some("1.5.9".into())
    );
}

/// A tool input inside an action is written with the normalized version, not
/// the raw tag: `version:` wants `2.2.0`, never `v2.2.0`.
#[test]
fn a_tool_input_in_a_workflow_is_written_with_the_normalized_version() {
    let mut dep = dep(Manager::GithubActions, DependencyKind::Tool);
    dep.file = ".github/workflows/ci.yml".into();
    dep.current = "v2.1.6".into();
    dep.resolved = Some("v2.1.6".into());
    dep.scheme = Scheme::Loose;
    dep.extract = Some(r"^v?(?<version>\d+\.\d+\.\d+)(?:-|$)".into());
    let proposals = eval()
        .propose(&dep, &catalog(&["v2.1.6", "v2.2.0"]))
        .unwrap();
    let update = proposals
        .iter()
        .find(|p| p.new_version.is_some())
        .unwrap_or_else(|| panic!("no update for the tool input: {proposals:?}"));
    assert_eq!(update.new_value.as_deref(), Some("2.2.0"));
}

/// Only an action or reusable workflow answers to the GitHub Actions
/// advisory feed; the tool input beside it is not a GitHub-published action.
#[test]
fn the_actions_advisory_feed_names_actions_not_tool_inputs() {
    let mut action = dep(Manager::GithubActions, DependencyKind::Action);
    action.package = "golangci/golangci-lint-action".into();
    assert_eq!(
        advisory_query(&action).map(|q| q.ecosystem),
        Some("GitHub Actions".into())
    );
    let mut tool = dep(Manager::GithubActions, DependencyKind::Tool);
    tool.package = "golangci/golangci-lint".into();
    assert_eq!(advisory_query(&tool).map(|q| q.ecosystem), None);
}

/// `security.lowestFix` also proposes the lowest release that fixes the most
/// advisories — except when a fix candidate's own policy is switched off. A
/// rule that stops every update for the dependency stops the fix too.
#[test]
fn a_disabled_policy_proposes_no_lowest_fix() {
    let config = patchula_config::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/disabled-dependency.pkl"),
    )
    .unwrap();
    let mut dep = dep(Manager::Npm, DependencyKind::Production);
    dep.name = "1password/cli".into();
    dep.source = Source::Npm;
    let query = AdvisoryQuery {
        ecosystem: "npm".into(),
        package: "pkg".into(),
        version: "1.0.0".into(),
    };
    let found = [std::sync::Arc::new(advisory("2.0.0"))];
    let evaluation = Evaluator::new(&config)
        .unwrap()
        .evaluate_with(
            &dep,
            &catalog(&["1.0.0", "2.0.0"]),
            now(),
            &Default::default(),
            Some((&query, &found)),
        )
        .unwrap();
    assert_eq!(evaluation.updates, Vec::new());
}

/// A fix only published as a prerelease stays out of the proposal lanes while
/// the installed release is stable: `lowestFix` does not promote it either.
#[test]
fn a_prerelease_only_fix_is_not_proposed_to_a_stable_baseline() {
    let mut dep = dep(Manager::Npm, DependencyKind::Production);
    dep.source = Source::Npm;
    let query = AdvisoryQuery {
        ecosystem: "npm".into(),
        package: "pkg".into(),
        version: "1.0.0".into(),
    };
    let found = [std::sync::Arc::new(advisory("2.0.0-rc.1"))];
    let evaluation = eval()
        .evaluate_with(
            &dep,
            &catalog(&["1.0.0", "2.0.0-rc.1"]),
            now(),
            &Default::default(),
            Some((&query, &found)),
        )
        .unwrap();
    assert_eq!(evaluation.updates, Vec::new());
}

/// A GitLab CI ref that names a branch, not a tag, has no version to compare:
/// it is never looked up. A version-shaped ref is.
#[test]
fn a_gitlab_forge_branch_ref_needs_no_lookup() {
    for manager in [Manager::Gitlabci, Manager::GitlabciInclude] {
        let mut dep = dep(manager, DependencyKind::Action);
        dep.source = Source::ForgeTags;
        dep.scheme = Scheme::Loose;
        dep.current = "main".into();
        dep.resolved = Some("main".into());
        assert!(
            !eval().needs_lookup(&dep),
            "{manager:?} branch ref should not need a lookup"
        );
        dep.current = "v1.2.0".into();
        dep.resolved = Some("v1.2.0".into());
        assert!(
            eval().needs_lookup(&dep),
            "{manager:?} tag ref should need a lookup"
        );
    }
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// The single advisory every fix test shares: `pkg` is affected from the
/// start of time until `fixed`, the release that resolves it.
fn advisory(fixed: &str) -> Advisory {
    Advisory {
        id: "GHSA-0000-0000".into(),
        modified: "2026-01-01T00:00:00Z".into(),
        aliases: Vec::new(),
        summary: None,
        severity: None,
        affected: vec![Affected {
            ecosystem: "npm".into(),
            package: "pkg".into(),
            ranges: vec![Range {
                kind: "SEMVER".into(),
                events: vec![Event::Introduced("0".into()), Event::Fixed(fixed.into())],
            }],
            versions: Vec::new(),
        }],
    }
}
