//! A made-up report for `--demo`, the headless scenes and the tests: a small
//! web service with every kind of row the window draws. It goes through the
//! same parser as a real `patchula lookup --json`.
use crate::{
    cli::{Applied, Patch, Written},
    model::Update,
};
use patchula_report::Report;
use patchula_words::Manager;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The moment the demo report was "evaluated", which the demo uses as now.
pub const NOW: &str = "2026-11-14T12:00:00Z";

fn bare(value: &str) -> &str {
    value.trim_start_matches(['^', '~', 'v'])
}

/// An update to `value`, published at `published`.
fn update(kind: &str, value: &str, published: &str) -> Value {
    json!({
        "newValue": value,
        "newVersion": bare(value),
        "newDigest": null,
        "updateType": kind,
        "releaseAge": {"publishedAt": published, "evaluatedAt": NOW},
    })
}

/// A digest move: a pin of the current tag, or a new build of it.
fn digest(kind: &str, value: Option<&str>, digest: &str, published: &str) -> Value {
    json!({
        "newValue": value,
        "newVersion": value.map(bare),
        "newDigest": digest,
        "updateType": kind,
        "releaseAge": {"publishedAt": published, "evaluatedAt": NOW},
    })
}

fn fixing(mut update: Value, ids: &[&str]) -> Value {
    update["fixes"] = json!(ids);
    update
}

fn dependency(manager: &str, name: &str, current: &str, updates: Vec<Value>) -> Value {
    json!({
        "manager": manager,
        "name": name,
        "package": name,
        "current": {"value": current, "version": bare(current), "digest": null},
        "updates": updates,
    })
}

/// `dependency` with metadata: where it came from, and whatever `extra` adds.
fn with(mut dependency: Value, source: &str, kind: &str, extra: Value) -> Value {
    let mut metadata = json!({"source": source, "kinds": [kind], "locations": []});
    if let (Some(metadata), Some(extra)) = (metadata.as_object_mut(), extra.as_object()) {
        metadata.extend(extra.clone());
    }
    dependency["metadata"] = metadata;
    dependency
}

fn held(version: &str, kind: &str, published: &str, eligible: &str, count: usize) -> Value {
    json!({
        "version": version,
        "count": count,
        "updateType": kind,
        "publishedAt": published,
        "eligibleAt": eligible,
    })
}

fn advisory(id: &str, cve: &str, severity: &str, summary: &str, fixed: &str) -> Value {
    json!({
        "id": id,
        "aliases": [cve],
        "severity": severity,
        "summary": summary,
        "url": format!("https://github.com/advisories/{id}"),
        "fixedIn": fixed,
    })
}

fn value() -> Value {
    let sha = |seed: &str| {
        format!(
            "sha256:{}",
            seed.repeat(64).chars().take(64).collect::<String>()
        )
    };
    let npm = vec![
        dependency(
            "npm",
            "react",
            "18.3.1",
            vec![update("major", "19.2.1", "2026-09-30T16:02:11Z")],
        ),
        dependency(
            "npm",
            "react-dom",
            "18.3.1",
            vec![update("major", "19.2.1", "2026-09-30T16:02:40Z")],
        ),
        with(
            dependency(
                "npm",
                "vite",
                "^6.4.1",
                vec![
                    fixing(
                        update("patch", "^6.4.3", "2026-10-21T09:14:52Z"),
                        &["GHSA-93m4-6634-74q7"],
                    ),
                    update("major", "^7.2.2", "2026-11-03T13:40:09Z"),
                ],
            ),
            "npm",
            "development",
            json!({"advisories": [advisory(
                "GHSA-93m4-6634-74q7",
                "CVE-2026-62522",
                "high",
                "The dev server's fs.deny check can be bypassed with a backslash on Windows",
                "6.4.2",
            )]}),
        ),
        with(
            dependency(
                "npm",
                "typescript",
                "5.9.2",
                vec![update("patch", "5.9.3", "2026-10-01T18:22:30Z")],
            ),
            "npm",
            "development",
            json!({"heldBack": [held("6.0.2", "major", "2026-11-11T17:05:00Z", "2026-11-18T17:05:00Z", 2)]}),
        ),
        dependency(
            "npm",
            "@types/node",
            "22.18.0",
            vec![update("minor", "22.19.1", "2026-11-02T04:31:18Z")],
        ),
        dependency(
            "npm",
            "vitest",
            "3.2.4",
            vec![update("major", "4.0.8", "2026-10-28T11:09:44Z")],
        ),
        dependency(
            "npm",
            "zod",
            "3.25.76",
            vec![update("major", "4.1.12", "2026-10-06T08:55:03Z")],
        ),
        dependency(
            "npm",
            "tailwindcss",
            "4.1.13",
            vec![update("patch", "4.1.17", "2026-11-05T15:47:21Z")],
        ),
        with(
            dependency(
                "npm",
                "lodash",
                "4.17.20",
                vec![fixing(
                    update("patch", "4.17.21", "2021-02-20T15:42:16Z"),
                    &["GHSA-35jh-r3h4-6jhm", "GHSA-29mw-wpgm-hmr9"],
                )],
            ),
            "npm",
            "production",
            json!({
                "resolution": {"kind": "lockfile", "version": "4.17.20"},
                "advisories": [
                    advisory("GHSA-35jh-r3h4-6jhm", "CVE-2021-23337", "high", "Command injection through the template function", "4.17.21"),
                    advisory("GHSA-29mw-wpgm-hmr9", "CVE-2020-28500", "moderate", "Regular expression denial of service in toNumber, trim and trimEnd", "4.17.21"),
                ],
            }),
        ),
        with(
            dependency("npm", "eslint", "9.36.0", vec![]),
            "npm",
            "development",
            json!({"heldBack": [held("9.39.1", "minor", "2026-11-12T20:11:37Z", "2026-11-15T20:11:37Z", 1)]}),
        ),
        dependency(
            "npm",
            "@tanstack/react-query",
            "5.90.2",
            vec![update("minor", "5.90.10", "2026-11-07T22:16:05Z")],
        ),
    ];
    let cargo = vec![
        dependency(
            "cargo",
            "serde",
            "1.0.219",
            vec![update("patch", "1.0.228", "2026-09-27T05:12:48Z")],
        ),
        dependency(
            "cargo",
            "tokio",
            "1.47.1",
            vec![update("minor", "1.48.0", "2026-10-14T12:38:27Z")],
        ),
        dependency(
            "cargo",
            "reqwest",
            "0.12.23",
            vec![update("minor", "0.13.5", "2026-11-01T19:20:13Z")],
        ),
        dependency(
            "cargo",
            "clap",
            "4.5.47",
            vec![update("patch", "4.5.51", "2026-10-29T14:03:55Z")],
        ),
        dependency(
            "cargo",
            "anyhow",
            "1.0.99",
            vec![update("patch", "1.0.100", "2026-09-19T21:44:02Z")],
        ),
        with(
            dependency(
                "cargo",
                "rustls",
                "0.23.31",
                vec![fixing(
                    update("patch", "0.23.35", "2026-10-30T10:27:39Z"),
                    &["RUSTSEC-2026-0087"],
                )],
            ),
            "crates",
            "production",
            json!({"advisories": [advisory(
                "RUSTSEC-2026-0087",
                "CVE-2026-58190",
                "moderate",
                "A fragmented handshake message can make the server spin",
                "0.23.34",
            )]}),
        ),
        dependency("cargo", "tracing", "0.1.41", vec![]),
        dependency("cargo", "thiserror", "2.0.17", vec![]),
    ];
    let mut checkout = dependency(
        "github-actions",
        "actions/checkout",
        "v4",
        vec![
            digest(
                "major",
                Some("v5"),
                &sha("08c6903cd8c0fde9"),
                "2026-08-11T13:55:20Z",
            ),
            digest(
                "pin",
                None,
                &sha("34e114876b0b11c3"),
                "2026-08-02T09:10:00Z",
            ),
        ],
    );
    checkout["updates"][1]["newValue"] = json!("v4");
    checkout["updates"][1]["newVersion"] = json!("4");
    let actions = vec![
        checkout,
        dependency(
            "github-actions",
            "actions/setup-node",
            "v4.4.0",
            vec![update("major", "v6.0.0", "2026-10-13T16:48:51Z")],
        ),
        dependency(
            "github-actions",
            "jdx/mise-action",
            "v3.2.0",
            vec![update("minor", "v3.4.0", "2026-11-04T23:02:36Z")],
        ),
        dependency(
            "github-actions",
            "docker/build-push-action",
            "v6.18.0",
            vec![],
        ),
    ];
    let docker = vec![
        dependency(
            "dockerfile",
            "node",
            "24.9.0-alpine",
            vec![update("minor", "24.11.1-alpine", "2026-11-06T02:18:44Z")],
        ),
        dependency(
            "dockerfile",
            "nginx",
            "1.29.3-alpine",
            vec![digest(
                "digest",
                None,
                &sha("b3c656d55d7ad751"),
                "2026-11-13T06:30:12Z",
            )],
        ),
    ];
    let mise = vec![
        dependency(
            "mise",
            "node",
            "24.9.0",
            vec![update("minor", "24.11.1", "2026-11-06T02:18:44Z")],
        ),
        dependency(
            "mise",
            "aqua:cli/cli",
            "2.81.0",
            vec![update("minor", "2.83.1", "2026-11-10T18:07:59Z")],
        ),
        with(
            dependency("mise", "python", "3.14.0", vec![]),
            "mise",
            "tool",
            json!({"heldBack": [held("3.14.1", "patch", "2026-11-13T14:00:00Z", "2026-11-16T14:00:00Z", 1)]}),
        ),
    ];
    let go = vec![
        with(
            dependency(
                "go",
                "golang.org/x/net",
                "v0.44.0",
                vec![fixing(
                    update("minor", "v0.47.0", "2026-11-09T17:26:40Z"),
                    &["GO-2026-4102"],
                )],
            ),
            "go",
            "production",
            json!({"advisories": [advisory(
                "GO-2026-4102",
                "CVE-2026-47912",
                "critical",
                "HTTP/2 CONTINUATION frames are read without a bound",
                "0.46.0",
            )]}),
        ),
        dependency(
            "go",
            "github.com/spf13/cobra",
            "v1.10.1",
            vec![update("patch", "v1.10.2", "2026-10-25T08:41:15Z")],
        ),
        dependency("go", "github.com/stretchr/testify", "v1.11.1", vec![]),
    ];
    let file =
        |path: &str, dependencies: Vec<Value>| json!({"path": path, "dependencies": dependencies});
    json!({
        "schemaVersion": patchula_report::SCHEMA_VERSION,
        "repositories": [{
            "repository": "local/castle-web",
            "status": "complete",
            "reason": null,
            "packageFiles": [
                file("web/package.json", npm),
                file("Cargo.toml", cargo),
                file(".github/workflows/ci.yml", actions),
                file("Dockerfile", docker),
                file("mise.toml", mise),
                file("tools/go.mod", go),
            ],
            "warnings": ["compose.yaml: the image tag `latest` floats, so the Count cannot say which release it is"],
            "notices": [
                {
                    "manager": "copier",
                    "path": ".copier-answers.yml",
                    "name": "gh:acme/service-template",
                    "package": "gh:acme/service-template",
                    "current": "v2.3.0",
                    "available": "v2.5.1",
                    "compareUrl": "https://github.com/acme/service-template/compare/v2.3.0...v2.5.1",
                },
                {
                    "manager": "compose",
                    "path": "compose.yaml",
                    "name": "postgres",
                    "package": "postgres",
                    "current": "16.10",
                    "available": "18.1",
                    "reason": "major updates are off for this dependency",
                },
            ],
            "lookupFailures": [
                {
                    "manager": "npm",
                    "path": "web/package.json",
                    "name": "@acme/design-tokens",
                    "package": "@acme/design-tokens",
                    "datasource": "npm",
                    "kind": "auth",
                    "message": "npm.acme.example answered 401: the registry wants a token the Count was not given",
                },
                {
                    "manager": "github-actions",
                    "path": ".github/workflows/release.yml",
                    "name": "acme/deploy-action",
                    "package": "acme/deploy-action",
                    "datasource": "github-tags",
                    "kind": "rate-limit",
                    "message": "GitHub's rate limit is spent until 12:41 UTC",
                },
            ],
            "defaultBranch": "local",
        }],
        "stats": {
            "requests": 214,
            "notModified": 163,
            "cacheHits": 388,
            "extractionHits": 9,
            "lookups": 61,
            "phaseMicros": {"extract": 84_300, "lookup": 1_312_000, "resolve": 211_500, "report": 9_100},
        },
    })
}

/// What `patchula apply --dry-run` would answer for these picks if the demo
/// were a tree: each picked declaration before and after, one hunk apiece.
pub fn plan(updates: &[Update], picked: &BTreeSet<usize>) -> Applied {
    let mut applied = Applied::default();
    let mut files = BTreeMap::<&str, String>::new();
    for (order, update) in picked.iter().filter_map(|i| updates.get(*i)).enumerate() {
        let line = |version: &str| match update.manager {
            Manager::Npm => format!("    \"{}\": \"{version}\",", update.name),
            Manager::GithubActions => format!("      - uses: {}@{version}", update.name),
            Manager::Dockerfile => format!("FROM {}:{version}", update.name),
            Manager::Go => format!("\t{} {version}", update.name),
            _ => format!("{} = \"{version}\"", update.name),
        };
        let at = 7 + order * 4;
        let diff = files
            .entry(&update.file)
            .or_insert_with(|| format!("--- a/{0}\n+++ b/{0}\n", update.file));
        diff.push_str(&format!(
            "@@ -{at},1 +{at},1 @@\n-{}\n+{}\n",
            line(&update.current),
            line(&update.target())
        ));
        applied.updates.push(Written {
            manager: update.manager,
            file: update.file.clone(),
            name: update.name.clone(),
            current: update.current.clone(),
            kind: update.kind,
            value: update.value.clone(),
            version: update.version.clone(),
            digest: update.digest.clone(),
            reason: None,
        });
    }
    applied.files = files
        .into_iter()
        .map(|(path, diff)| Patch {
            path: path.to_owned(),
            diff,
        })
        .collect();
    applied
}

/// The demo report, parsed as a real one is.
pub fn report() -> Report {
    serde_json::from_value(value()).expect("the demo report follows the report schema")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_demo_report_is_one_the_cli_could_have_written() {
        let json = serde_json::to_vec(&super::value()).unwrap();
        patchula_report::Report::from_json(&json).unwrap();
    }
}
