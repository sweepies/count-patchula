use patchula_config::{DependencyKind, Manager, Scheme, Source};
use patchula_datasource::{Advisory, AdvisoryQuery, Affected, Event, Range};
use patchula_lookup::{advisory_query, affects, fixes, report_advisories};
use patchula_manager::{Dependency, Location};
use patchula_versioning::Ecosystem;
use std::sync::Arc;

fn dependency(source: Source, package: &str, current: &str, resolved: Option<&str>) -> Dependency {
    let (manager, scheme) = match source {
        Source::Npm => (Manager::Npm, Scheme::Semver),
        Source::Crates => (Manager::Cargo, Scheme::Semver),
        Source::Go => (Manager::Go, Scheme::Go),
        Source::Pypi => (Manager::Requirements, Scheme::Pep440),
        _ => (Manager::GithubActions, Scheme::Loose),
    };
    Dependency {
        manager,
        file: "manifest".into(),
        location: Location {
            start: 0,
            end: 1,
            line: 1,
            column: 1,
            pointer: String::new(),
        },
        name: package.into(),
        package: package.into(),
        kind: if manager == Manager::GithubActions {
            DependencyKind::Action
        } else {
            DependencyKind::Production
        },
        current: current.into(),
        resolved: resolved.map(Into::into),
        digest: None,
        source,
        scheme,
        extract: None,
    }
}

fn advisory(
    ecosystem: &str,
    package: &str,
    ranges: Vec<Vec<Event>>,
    versions: &[&str],
) -> Advisory {
    Advisory {
        id: "GHSA-test".into(),
        modified: "2026-10-01T00:00:00Z".into(),
        aliases: vec!["CVE-2026-0001".into()],
        summary: Some("Prototype pollution".into()),
        severity: Some("high".into()),
        affected: vec![Affected {
            ecosystem: ecosystem.into(),
            package: package.into(),
            ranges: ranges
                .into_iter()
                .map(|events| Range {
                    kind: "ECOSYSTEM".into(),
                    events,
                })
                .collect(),
            versions: versions.iter().map(|v| (*v).into()).collect(),
        }],
    }
}

fn query(ecosystem: &str, package: &str, version: &str) -> AdvisoryQuery {
    AdvisoryQuery {
        ecosystem: ecosystem.into(),
        package: package.into(),
        version: version.into(),
    }
}

#[test]
fn only_exactly_known_versions_are_queried_in_osv_naming() {
    let npm = dependency(Source::Npm, "lodash", "^4.17.0", Some("4.17.11"));
    assert_eq!(
        advisory_query(&npm),
        Some(query("npm", "lodash", "4.17.11"))
    );
    let exact = dependency(Source::Npm, "lodash", "4.17.11", None);
    assert_eq!(
        advisory_query(&exact),
        Some(query("npm", "lodash", "4.17.11"))
    );
    // A range's floor is not what is installed.
    assert_eq!(
        advisory_query(&dependency(Source::Npm, "lodash", "^4.17.0", None)),
        None
    );
    assert_eq!(
        advisory_query(&dependency(Source::Crates, "smallvec", "1.6", None)),
        None
    );
    assert_eq!(
        advisory_query(&dependency(
            Source::Crates,
            "smallvec",
            "1.6",
            Some("1.6.0")
        )),
        Some(query("crates.io", "smallvec", "1.6.0"))
    );
    assert_eq!(
        advisory_query(&dependency(
            Source::Go,
            "golang.org/x/net",
            "v0.1.0",
            Some("v0.1.0")
        )),
        Some(query("Go", "golang.org/x/net", "0.1.0"))
    );
    // A floating action tag names a release line, not a release.
    let floating = dependency(
        Source::GithubTags,
        "tj-actions/changed-files",
        "v45",
        Some("v45"),
    );
    assert_eq!(advisory_query(&floating), None);
    let pinned = dependency(
        Source::GithubTags,
        "tj-actions/changed-files",
        "abc",
        Some("v45.0.7"),
    );
    assert_eq!(
        advisory_query(&pinned),
        Some(query(
            "GitHub Actions",
            "tj-actions/changed-files",
            "45.0.7"
        ))
    );
    let mut toolchain = dependency(Source::Mise, "go", "1.22.3", Some("1.22.3"));
    toolchain.manager = Manager::Go;
    toolchain.kind = DependencyKind::Toolchain;
    toolchain.scheme = Scheme::Loose;
    assert_eq!(
        advisory_query(&toolchain),
        Some(query("Go", "stdlib", "1.22.3"))
    );
    let mut private = dependency(Source::Npm, "lodash", "4.17.11", None);
    private.package = "https://npm.acme.dev|lodash".into();
    assert_eq!(advisory_query(&private), None);
}

#[test]
fn ranges_follow_osv_event_order_across_introduced_fixed_and_last_affected() {
    let q = query("npm", "lib", "1.0.0");
    let two_lines = advisory(
        "npm",
        "lib",
        vec![
            vec![Event::Introduced("0".into()), Event::Fixed("1.0.5".into())],
            vec![
                Event::Introduced("2.0.0".into()),
                Event::Fixed("2.0.3".into()),
            ],
        ],
        &[],
    );
    for (version, expected) in [
        ("1.0.0", true),
        ("1.0.5", false),
        ("1.9.0", false),
        ("2.0.0", true),
        ("2.0.2", true),
        ("2.0.3", false),
    ] {
        assert_eq!(
            affects(&two_lines, &q, Ecosystem::Npm, version),
            Some(expected),
            "{version}"
        );
    }
    let last = advisory(
        "npm",
        "lib",
        vec![vec![
            Event::Introduced("1.0.0".into()),
            Event::LastAffected("1.2.0".into()),
        ]],
        &[],
    );
    assert_eq!(affects(&last, &q, Ecosystem::Npm, "1.2.0"), Some(true));
    assert_eq!(affects(&last, &q, Ecosystem::Npm, "1.2.1"), Some(false));
    assert_eq!(affects(&last, &q, Ecosystem::Npm, "0.9.0"), Some(false));
    let listed = advisory("npm", "lib", Vec::new(), &["3.1.0"]);
    assert_eq!(affects(&listed, &q, Ecosystem::Npm, "3.1.0"), Some(true));
    assert_eq!(affects(&listed, &q, Ecosystem::Npm, "3.1.1"), Some(false));
    // Another package's ranges say nothing about this one.
    assert_eq!(
        affects(
            &listed,
            &query("npm", "other", "1.0.0"),
            Ecosystem::Npm,
            "3.1.1"
        ),
        None
    );
    let unreadable = advisory(
        "npm",
        "lib",
        vec![vec![
            Event::Introduced("0".into()),
            Event::Fixed("not-a-version".into()),
        ]],
        &[],
    );
    assert_eq!(affects(&unreadable, &q, Ecosystem::Npm, "9.0.0"), None);
}

#[test]
fn updates_fix_only_what_their_target_leaves_behind() {
    let dep = dependency(Source::Npm, "lodash", "4.17.11", None);
    let q = advisory_query(&dep).unwrap();
    let fixed_early = Arc::new(advisory(
        "npm",
        "lodash",
        vec![vec![
            Event::Introduced("0".into()),
            Event::Fixed("4.17.12".into()),
        ]],
        &[],
    ));
    let mut fixed_late = advisory(
        "npm",
        "lodash",
        vec![vec![
            Event::Introduced("0".into()),
            Event::Fixed("4.17.21".into()),
        ]],
        &[],
    );
    fixed_late.id = "GHSA-late".into();
    let mut unreadable = advisory(
        "npm",
        "lodash",
        vec![vec![
            Event::Introduced("0".into()),
            Event::Fixed("soon".into()),
        ]],
        &[],
    );
    unreadable.id = "GHSA-unknown".into();
    let found = vec![fixed_early, Arc::new(fixed_late), Arc::new(unreadable)];
    assert_eq!(fixes(&dep, &q, &found, "4.17.12"), ["GHSA-test"]);
    assert_eq!(
        fixes(&dep, &q, &found, "4.17.21"),
        ["GHSA-test", "GHSA-late"]
    );
    let reported = report_advisories(&dep, &q, &found);
    assert_eq!(reported.len(), 3);
    let early = reported.iter().find(|a| a.id == "GHSA-test").unwrap();
    assert_eq!(early.fixed_in.as_deref(), Some("4.17.12"));
    assert_eq!(early.url, "https://osv.dev/vulnerability/GHSA-test");
    assert_eq!(early.aliases, ["CVE-2026-0001"]);
    assert!(
        reported
            .iter()
            .any(|a| a.id == "GHSA-unknown" && a.fixed_in.is_none())
    );
}

#[test]
fn go_versions_without_their_prefix_and_pypi_names_match() {
    let go = dependency(Source::Go, "golang.org/x/net", "v0.1.0", Some("v0.1.0"));
    let q = advisory_query(&go).unwrap();
    let found = vec![Arc::new(advisory(
        "Go",
        "golang.org/x/net",
        vec![vec![
            Event::Introduced("0".into()),
            Event::Fixed("0.7.0".into()),
        ]],
        &[],
    ))];
    assert!(fixes(&go, &q, &found, "v0.6.0").is_empty());
    assert_eq!(fixes(&go, &q, &found, "v0.7.0"), ["GHSA-test"]);
    let py = dependency(Source::Pypi, "Django", "==4.2.0", Some("4.2.0"));
    let q = advisory_query(&py).unwrap();
    let found = vec![Arc::new(advisory(
        "PyPI",
        "django",
        vec![vec![
            Event::Introduced("4.2".into()),
            Event::Fixed("4.2.8".into()),
        ]],
        &[],
    ))];
    assert_eq!(fixes(&py, &q, &found, "4.2.8"), ["GHSA-test"]);
}
