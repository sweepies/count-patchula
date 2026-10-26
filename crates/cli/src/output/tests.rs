//! Plain output is stable, so it is checked against snapshots. Set
//! PATCHULA_UPDATE_SNAPSHOT=1 to rewrite them after an intended change.
use super::*;
use serde_json::{Value, json};

fn dependency(name: &str, current: &str, updates: Value, advisories: Value) -> Value {
    json!({
        "manager": "npm", "name": name, "package": name,
        "current": {"value": current, "version": current, "digest": null},
        "updates": updates,
        "metadata": {"source": "npm", "kinds": ["dependencies"], "locations": [], "advisories": advisories}
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
        "schemaVersion": 1,
        "repositories": [repository("local/web-chat")]
    }))
    .unwrap();
    let text = report_text(&report);
    assert!(!text.contains('\x1b'), "{text}");
    assert!(text.lines().next().unwrap().starts_with(
        "The Count visited local/web-chat and found 4 dependencies, 5 updates, 1 vulnerability"
    ));
    check("lookup.txt", &text);
}

fn report_text(report: &Report) -> String {
    super::report(report, &Style::PLAIN)
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
        "schemaVersion": 1,
        "repositories": [broken, repository("acme/web")],
        "actions": {
            "schemaVersion": 1,
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
    assert!(
        text.starts_with("The Count visited 2 repositories and found 4 dependencies"),
        "{text}"
    );
    assert!(text.contains("1 repository could not be read."), "{text}");
    assert!(!text.contains("\nUpdates"), "{text}");
    check("run.txt", &text);
}

#[test]
fn a_terminal_fits_rows_to_its_width_but_leaves_links_whole() {
    let report: Report = serde_json::from_value(json!({
        "schemaVersion": 1,
        "repositories": [repository("local/web-chat")]
    }))
    .unwrap();
    let style = Style {
        color: false,
        width: Some(60),
    };
    let text = super::report(&report, &style);
    assert!(text.starts_with("🦇 The Count visited"), "{text}");
    for line in text
        .lines()
        .filter(|line| !line.trim_start().starts_with("https://"))
    {
        let width = console::measure_text_width(line);
        // The headline is wrapped by the terminal itself.
        assert!(width <= 60 || line.starts_with("🦇"), "{width}: {line}");
    }
    assert!(
        text.contains("    https://github.com/acme/template/compare/v0.16.0...v0.23.0\n"),
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
