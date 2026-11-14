//! Plain output is stable, so it is checked against snapshots. Set
//! PATCHULA_UPDATE_SNAPSHOT=1 to rewrite them after an intended change.
use super::*;
use serde_json::{Value, json};

fn dependency(name: &str, current: &str, updates: Value, advisories: Value) -> Value {
    json!({
        "manager": "npm", "name": name, "package": name,
        "current": {"value": current, "version": current, "digest": null},
        "updates": updates,
        "metadata": {"source": "npm", "kinds": ["production"], "locations": [], "advisories": advisories}
    })
}

fn update(kind: &str, version: &str) -> Value {
    json!({"newValue": version, "newVersion": version, "newDigest": null, "updateType": kind, "group": null})
}

fn repository(name: &str) -> Value {
    json!({
        "repository": name,
        "status": "complete",
        "reason": null,
        "packageFiles": [
            {"path": ".github/workflows/ci.yml", "dependencies": [{
                "manager": "github-actions", "name": "actions/checkout", "package": "actions/checkout",
                "current": {"value": "v4.1.0", "version": "v4.1.0", "digest": null},
                "updates": [update("major", "v5.0.0")]
            }]},
            {"path": "package.json", "dependencies": [
                dependency("lodash", "4.17.20", json!([update("patch", "4.17.21")]), json!([{
                    "id": "GHSA-35jh-r3h4-6jhm", "aliases": ["CVE-2021-23337"], "severity": "HIGH",
                    "summary": "Command Injection in lodash", "url": "https://osv.dev/GHSA-35jh-r3h4-6jhm",
                    "fixedIn": "4.17.21"
                }])),
                dependency("ms", "2.1.2", json!([update("patch", "2.1.3")]), json!([])),
                dependency("typescript", "5.4.5", json!([update("minor", "5.9.3"), update("major", "6.0.2")]), json!([]))
            ]}
        ],
        "warnings": ["advisory lookup on OSV.dev failed: the service answered 503"],
        "skipped": [{"manager": "dockerfile", "path": "Dockerfile", "line": 3, "value": "node:${NODE}", "reason": "the image tag is a build argument"}],
        "notices": [{
            "manager": "copier", "path": ".copier-answers.yml", "name": "gh:acme/template",
            "package": "gh:acme/template", "current": "v0.16.0", "available": "v0.23.0",
            "compareUrl": "https://github.com/acme/template/compare/v0.16.0...v0.23.0"
        }],
        "lookupFailures": [{
            "manager": "mise", "path": "mise.toml", "name": "pipx:acme/tidy", "package": "pipx:acme/tidy",
            "datasource": "mise", "kind": "not-found",
            "message": "mise lists no versions of pipx:acme/tidy"
        }]
    })
}

fn check(name: &str, actual: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/output/snapshots")
        .join(name);
    if std::env::var_os("PATCHULA_UPDATE_SNAPSHOT").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        actual, expected,
        "{name} changed; rerun with PATCHULA_UPDATE_SNAPSHOT=1 if that was intended"
    );
}

#[test]
fn a_lookup_reads_plain_with_counts_first() {
    let report: Report = serde_json::from_value(json!({
        "schemaVersion": 8,
        "repositories": [repository("local/web-chat")]
    }))
    .unwrap();
    let text = report_text(&report);
    assert!(!text.contains('\x1b'), "{text}");
    assert!(summary(&text).starts_with(
        "The Count visited local/web-chat and found 4 dependencies, 5 updates, 1 vulnerability"
    ));
    check("lookup.txt", &text);
}

fn report_text(report: &Report) -> String {
    super::report(report, &Style::PLAIN)
}

/// The summary paragraph on one line, after checking that a pipe gets it
/// wrapped at word boundaries to 80 columns.
fn summary(text: &str) -> String {
    let paragraph = text.split("\n\n").next().unwrap();
    for line in paragraph.lines() {
        assert!(line.chars().count() <= 80, "{line}");
        assert_eq!(line, line.trim(), "{line}");
    }
    paragraph.replace('\n', " ")
}

#[test]
fn a_run_lists_branches_and_pull_requests_per_repository() {
    let mut broken = repository("acme/api");
    broken["status"] = json!("error");
    broken["reason"] = json!("no patchula.pkl on the default branch");
    broken["packageFiles"] = json!([]);
    for key in ["warnings", "skipped", "notices", "lookupFailures"] {
        broken[key] = json!([]);
    }
    let report: Report = serde_json::from_value(json!({
        "schemaVersion": 8,
        "repositories": [broken, repository("acme/web")],
        "actions": {
            "schemaVersion": 3,
            "dryRun": true,
            "branches": [
                {"repository": "acme/web", "branch": "patchula/npm-lodash-4.x", "baseCommit": "abc",
                 "status": "planned", "reason": null, "message": "fix(deps): update lodash to 4.17.21",
                 "previousCommit": null, "commit": null, "verified": null, "lockfilePending": true,
                 "files": [{"path": "package.json", "blobSha": "def",
                            "diff": "--- a/package.json\n+++ b/package.json\n@@ -1 +1 @@\n-\"lodash\": \"4.17.20\"\n+\"lodash\": \"4.17.21\"\n"}]},
                {"repository": "acme/web", "branch": "patchula/actions-checkout-5.x", "baseCommit": "abc",
                 "status": "skipped", "reason": "the branch was edited by someone else, so the Count leaves it",
                 "message": "chore(deps): update actions/checkout to v5", "previousCommit": null, "commit": null,
                 "verified": null, "lockfilePending": false, "files": []}
            ],
            "pullRequests": [
                {"repository": "acme/web", "branch": "patchula/npm-lodash-4.x", "number": 12,
                 "url": "https://github.com/acme/web/pull/12", "status": "updated", "reason": null}
            ]
        }
    }))
    .unwrap();
    let text = report_text(&report);
    let summary = summary(&text);
    assert!(
        summary.starts_with("The Count visited 2 repositories and found 4 dependencies"),
        "{text}"
    );
    assert!(
        summary.contains("1 repository could not be read."),
        "{text}"
    );
    assert!(!text.contains("\nUpdates"), "{text}");
    check("run.txt", &text);
}

/// A raced repository warns with its `stale` reason, and the headline
/// still describes the visit rather than a failure to read.
#[test]
fn a_stale_repository_warns_with_the_race_and_keeps_the_headline() {
    let mut raced = repository("acme/api");
    raced["status"] = json!("stale");
    raced["reason"] = json!("default branch changed during the run; run again");
    raced["packageFiles"] = json!([]);
    for key in ["warnings", "skipped", "notices", "lookupFailures"] {
        raced[key] = json!([]);
    }
    let report: Report = serde_json::from_value(json!({
        "schemaVersion": 8,
        "repositories": [raced, repository("acme/web")],
        "actions": {
            "schemaVersion": 3,
            "dryRun": false,
            "branches": [
                {"repository": "acme/api", "branch": "patchula/npm-lodash-4.x", "baseCommit": "abc",
                 "status": "stale", "reason": "default branch moved since extraction; stale blob refusal, rerun",
                 "message": "fix(deps): update lodash to 4.17.21", "previousCommit": null, "commit": null,
                 "verified": null, "lockfilePending": false, "files": []}
            ],
            "pullRequests": []
        }
    }))
    .unwrap();
    let text = report_text(&report);
    assert!(!text.contains('\x1b'), "{text}");
    assert!(
        text.contains("stale: default branch changed during the run; run again"),
        "{text}"
    );
    assert!(!text.contains("could not be read"), "{text}");
    check("stale.txt", &text);
}

#[test]
fn a_terminal_fits_rows_to_its_width_but_leaves_links_whole() {
    let report: Report = serde_json::from_value(json!({
        "schemaVersion": 8,
        "repositories": [repository("local/web-chat")]
    }))
    .unwrap();
    let style = Style {
        color: false,
        width: Some(60),
    };
    let text = super::report(&report, &style);
    assert!(text.starts_with("🦇 The Count visited"), "{text}");
    // Paths are cut to fit, and messages and the headline wrap.
    assert!(
        text.contains("  major  actions/checkout  v4.1.0 → v5.0.0    .github/workf…\n"),
        "{text}"
    );
    for line in text.lines().filter(|line| {
        !line.contains("GHSA-") && !line.contains("https://") && !line.contains("template ")
    }) {
        let width = console::measure_text_width(line);
        assert!(width <= 60, "{width}: {line}");
    }
    // A notice keeps its template and versions; its path gives way.
    assert!(
        text.contains("  template gh:acme/template: v0.16.0 → v0.23.0 available  .copier…\n"),
        "{text}"
    );
    // A row too wide even with its flexible column cut keeps every version.
    assert!(
        text.contains(
            "lodash 4.17.20  GHSA-35jh-r3h4-6jhm (CVE-2021-23337)  fixed in 4.17.21  package…\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("    https://github.com/acme/template/compare/v0.16.0...v0.23.0\n"),
        "{text}"
    );
}

#[test]
fn one_advisory_on_one_version_in_two_manifests_is_one_vulnerability() {
    let rsa = |version: &str| {
        let mut dep = dependency(
            "rsa",
            version,
            json!([]),
            json!([{
                "id": "RUSTSEC-2023-0071", "aliases": ["CVE-2023-49092"], "severity": "moderate",
                "summary": "Marvin Attack", "url": "https://osv.dev/RUSTSEC-2023-0071", "fixedIn": null
            }]),
        );
        dep["manager"] = json!("cargo");
        dep
    };
    let report: Report = serde_json::from_value(json!({
        "schemaVersion": 8,
        "repositories": [{
            "repository": "acme/bat", "status": "complete", "reason": null,
            "packageFiles": [
                {"path": "crates/github/Cargo.toml", "dependencies": [rsa("0.9.10")]},
                {"path": "crates/server/Cargo.toml", "dependencies": [rsa("0.9.10")]},
                {"path": "legacy/Cargo.toml", "dependencies": [rsa("0.8.0")]},
                {"path": "a/Cargo.toml", "dependencies": [rsa("0.7.0")]},
                {"path": "b/Cargo.toml", "dependencies": [rsa("0.7.0")]},
                {"path": "c/Cargo.toml", "dependencies": [rsa("0.7.0")]}
            ],
            "warnings": [], "skipped": [], "notices": [], "lookupFailures": []
        }]
    }))
    .unwrap();
    let text = report_text(&report);
    assert!(summary(&text).contains("3 vulnerabilities"), "{text}");
    assert!(text.contains("Vulnerabilities (3)\n"), "{text}");
    assert!(
        text.contains(
            "  moderate  rsa 0.9.10  RUSTSEC-2023-0071 (CVE-2023-49092)  no fix yet  \
             crates/github/Cargo.toml, crates/server/Cargo.toml\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("  moderate  rsa 0.8.0   RUSTSEC-2023-0071 (CVE-2023-49092)  no fix yet  legacy/Cargo.toml\n"),
        "{text}"
    );
    // Past two manifests, they're counted.
    assert!(
        text.contains(
            "  moderate  rsa 0.7.0   RUSTSEC-2023-0071 (CVE-2023-49092)  no fix yet  3 manifests\n"
        ),
        "{text}"
    );
}

#[test]
fn colour_needs_a_terminal_unless_forced() {
    assert!(colored(cli::Color::Always, false));
    assert!(!colored(cli::Color::Never, true));
    assert!(!colored(cli::Color::Auto, false));
    let style = Style {
        color: true,
        width: None,
    };
    assert_eq!(style.bold("x"), "\x1b[1mx\x1b[0m");
    assert_eq!(Style::PLAIN.bold("x"), "x");
}

#[test]
fn advisory_severities_paint_by_weight() {
    use patchula_words::Severity;
    assert!(matches!(severity_paint(Severity::Critical), Paint::Error));
    assert!(matches!(severity_paint(Severity::High), Paint::Error));
    assert!(matches!(severity_paint(Severity::Moderate), Paint::Warning));
    assert!(matches!(severity_paint(Severity::Low), Paint::Dim));
}
