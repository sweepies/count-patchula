//! OSV.dev advisories in a lookup, from offline responses written to the OSV API
//! documentation; the advisory records are trimmed copies of real ones.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_testkit::{Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Replay};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

fn exchange(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Body,
    status: u16,
    response: Value,
    etag: Option<&str>,
) -> Interaction {
    Interaction {
        request: RecordedRequest {
            method: method.into(),
            url: url.into(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).into(), vec![(*v).into()]))
                .collect(),
            body,
        },
        response: RecordedResponse {
            status,
            headers: etag
                .map(|e| BTreeMap::from([("etag".into(), vec![e.into()])]))
                .unwrap_or_default(),
            body: Body::Json(response),
        },
    }
}
const GITHUB: &[(&str, &str)] = &[
    ("accept", "application/vnd.github+json"),
    ("x-github-api-version", "2022-11-28"),
];
fn github(path: &str, value: Value, status: u16, etag: &str, conditional: bool) -> Interaction {
    let mut headers = GITHUB.to_vec();
    if conditional {
        headers.push(("if-none-match", etag));
    }
    exchange(
        "GET",
        &format!("https://api.github.com/{path}"),
        &headers,
        Body::Text(String::new()),
        status,
        value,
        Some(etag),
    )
}
fn osv_batch() -> Interaction {
    exchange(
        "POST",
        "https://api.osv.dev/v1/querybatch",
        &[
            ("accept", "application/json"),
            ("content-type", "application/json"),
        ],
        Body::Json(
            json!({"queries":[{"package":{"ecosystem":"npm","name":"lodash"},"version":"4.17.11"}]}),
        ),
        200,
        json!({"results":[{"vulns":[
            {"id":"GHSA-35jh-r3h4-6jhm","modified":"2026-10-17T02:55:38.085813Z"},
            {"id":"GHSA-jf85-cpcp-j695","modified":"2026-09-10T03:47:55.238868631Z"}
        ]}]}),
        None,
    )
}
fn osv_record(id: &str, record: Value) -> Interaction {
    exchange(
        "GET",
        &format!("https://api.osv.dev/v1/vulns/{id}"),
        &[("accept", "application/json")],
        Body::Text(String::new()),
        200,
        record,
        None,
    )
}

#[tokio::test]
async fn advisories_mark_fixing_updates_and_warm_runs_reuse_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spec.json");
    let config = "amends \"PatchulaConfig.pkl\"\ndefaults { waitHours = 0 }\nmanagers = new Listing { \"npm\" }\n";
    let manifest = "{\"dependencies\":{\"lodash\":\"4.17.11\"}}";
    let mut interactions = vec![exchange(
        "POST",
        "https://api.github.com/graphql",
        &[GITHUB[0], GITHUB[1], ("content-type", "application/json")],
        Body::Json(
            json!({"query":patchula_engine::REPOSITORY_HEAD_QUERY,"variables":{"owner":"demo","name":"app"}}),
        ),
        200,
        json!({"data":{"repository":{"isPrivate":false,"defaultBranchRef":{"name":"main","target":{"oid":"head","tree":{"oid":"tree"}}}}}}),
        None,
    )];
    interactions.push(github(
        "repos/demo/app/git/trees/tree?recursive=1",
        json!({"truncated":false,"tree":[
            {"path":"patchula.pkl","type":"blob","sha":"config"},
            {"path":"package.json","type":"blob","sha":"manifest"}
        ]}),
        200,
        "tree",
        false,
    ));
    for (sha, text) in [("config", config), ("manifest", manifest)] {
        interactions.push(github(
            &format!("repos/demo/app/git/blobs/{sha}"),
            json!({"encoding":"base64","content":STANDARD.encode(text)}),
            200,
            sha,
            false,
        ));
    }
    let npm = &[("accept", "application/json")];
    interactions.push(exchange(
        "GET",
        "https://registry.npmjs.org/lodash",
        npm,
        Body::Text(String::new()),
        200,
        json!({"dist-tags":{"latest":"4.17.21"},"versions":{"4.17.11":{},"4.17.12":{},"4.17.21":{}}}),
        Some("npm"),
    ));
    interactions.push(osv_batch());
    interactions.push(osv_record(
        "GHSA-jf85-cpcp-j695",
        json!({
            "id":"GHSA-jf85-cpcp-j695","modified":"2026-09-10T03:47:55.238868631Z",
            "summary":"Prototype Pollution in lodash","aliases":["CVE-2019-10744","SNYK-JS-LODASH-450202"],
            "database_specific":{"severity":"CRITICAL","github_reviewed":true},
            "affected":[{"package":{"ecosystem":"npm","name":"lodash"},
                "ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"4.17.12"}]}]}]
        }),
    ));
    interactions.push(osv_record(
        "GHSA-35jh-r3h4-6jhm",
        json!({
            "id":"GHSA-35jh-r3h4-6jhm","modified":"2026-10-17T02:55:38.085813Z",
            "summary":"Command Injection in lodash","aliases":["CVE-2021-23337"],
            "database_specific":{"severity":"HIGH"},
            "affected":[{"package":{"ecosystem":"npm","name":"lodash"},
                "ranges":[{"type":"SEMVER","events":[{"introduced":"0"},{"fixed":"4.17.21"}]}]}]
        }),
    ));
    // Warm: conditional repository reads, a 304 catalog, and one OSV query.
    interactions.push(github(
        "repos/demo/app",
        json!({"default_branch":"main"}),
        200,
        "meta",
        false,
    ));
    interactions.push(github(
        "repos/demo/app/branches/main",
        json!({"commit":{"sha":"head","commit":{"tree":{"sha":"tree"}}}}),
        200,
        "head",
        false,
    ));
    interactions.push(exchange(
        "GET",
        "https://registry.npmjs.org/lodash",
        &[("accept", "application/json"), ("if-none-match", "npm")],
        Body::Text(String::new()),
        304,
        Value::Null,
        Some("npm"),
    ));
    interactions.push(osv_batch());
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the GitHub, npm and OSV.dev API documentation; advisory records trimmed from OSV.dev".into(),
        interactions,
    };
    patchula_testkit::save(&path, &cassette, &Default::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone())
        .unwrap()
        .with_versions_host(false)
        .with_advisories();
    let options = || Options {
        repositories: vec!["demo/app".into()],
        ..Options::default()
    };
    let now = chrono::Utc::now();
    let cold = engine.lookup_at(options(), now).await.unwrap();
    let warm = engine.lookup_at(options(), now).await.unwrap();
    assert_eq!(cold.repositories, warm.repositories);
    let repo = &cold.repositories[0];
    assert!(repo.warnings.is_empty(), "{:?}", repo.warnings);
    let dep = &repo.package_files[0].dependencies[0];
    let advisories = &dep.metadata.as_ref().unwrap().advisories;
    let summary: Vec<_> = advisories
        .iter()
        .map(|a| (a.id.as_str(), a.severity.as_deref(), a.fixed_in.as_deref()))
        .collect();
    assert_eq!(
        summary,
        [
            ("GHSA-35jh-r3h4-6jhm", Some("high"), Some("4.17.21")),
            ("GHSA-jf85-cpcp-j695", Some("critical"), Some("4.17.12")),
        ]
    );
    assert!(advisories[1].aliases.contains(&"CVE-2019-10744".to_owned()));
    let update = &dep.updates[0];
    assert_eq!(update.new_version.as_deref(), Some("4.17.21"));
    assert_eq!(update.fixes, ["GHSA-35jh-r3h4-6jhm", "GHSA-jf85-cpcp-j695"]);
    assert_eq!(
        update.policy.as_ref().unwrap().labels,
        ["dependencies", "security"]
    );
    replay.assert_finished().unwrap();
}
