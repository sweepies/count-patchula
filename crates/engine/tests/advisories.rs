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
    let config = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"npm\" }\n";
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
    // Warm: conditional repository reads and one OSV query. The catalog would be
    // reused under the release-age wait, but a vulnerable package is read again.
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

#[tokio::test]
async fn a_vulnerable_lockfile_only_crate_is_reported_with_its_fix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spec.json");
    let config = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"cargo\" }\n";
    let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
    let lock = "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"time\"\nversion = \"0.3.20\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"itoa\"\nversion = \"1.0.9\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
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
            {"path":"Cargo.toml","type":"blob","sha":"manifest"},
            {"path":"Cargo.lock","type":"blob","sha":"lock"}
        ]}),
        200,
        "tree",
        false,
    ));
    for (sha, text) in [("config", config), ("manifest", manifest), ("lock", lock)] {
        interactions.push(github(
            &format!("repos/demo/app/git/blobs/{sha}"),
            json!({"encoding":"base64","content":STANDARD.encode(text)}),
            200,
            sha,
            false,
        ));
    }
    interactions.push(exchange(
        "POST",
        "https://api.osv.dev/v1/querybatch",
        &[("accept", "application/json"), ("content-type", "application/json")],
        Body::Json(json!({"queries":[
            {"package":{"ecosystem":"crates.io","name":"itoa"},"version":"1.0.9"},
            {"package":{"ecosystem":"crates.io","name":"time"},"version":"0.3.20"}
        ]})),
        200,
        json!({"results":[{},{"vulns":[{"id":"RUSTSEC-2020-0071","modified":"2026-01-01T00:00:00Z"}]}]}),
        None,
    ));
    interactions.push(osv_record(
        "RUSTSEC-2020-0071",
        json!({
            "id":"RUSTSEC-2020-0071","modified":"2026-01-01T00:00:00Z",
            "summary":"Potential segfault in the time crate","aliases":["CVE-2020-26235","GHSA-wcg3-cvx6-7396"],
            "affected":[{"package":{"ecosystem":"crates.io","name":"time"},
                "ranges":[{"type":"SEMVER","events":[{"introduced":"0.2.23"},{"fixed":"0.3.23"}]}]}]
        }),
    ));
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the GitHub and OSV.dev API documentation; the advisory record is trimmed from OSV.dev".into(),
        interactions,
    };
    patchula_testkit::save(&path, &cassette, &Default::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, replay.clone())
        .unwrap()
        .with_advisories();
    let report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Options::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert!(repo.warnings.is_empty(), "{:?}", repo.warnings);
    assert_eq!(repo.package_files.len(), 1);
    let file = &repo.package_files[0];
    assert_eq!(file.path, "Cargo.lock");
    // Only the vulnerable lockfile package is reported.
    assert_eq!(file.dependencies.len(), 1);
    let time = &file.dependencies[0];
    assert_eq!(time.name, "time");
    assert_eq!(time.metadata.as_ref().unwrap().kinds, ["transitive"]);
    assert_eq!(time.updates.len(), 1);
    let fix = &time.updates[0];
    assert_eq!(fix.new_version.as_deref(), Some("0.3.23"));
    assert_eq!(fix.new_value, None);
    assert_eq!(fix.update_type, "patch");
    assert_eq!(fix.fixes, ["RUSTSEC-2020-0071"]);
    assert!(
        fix.policy
            .as_ref()
            .unwrap()
            .labels
            .contains(&"security".to_owned())
    );
    replay.assert_finished().unwrap();
}

/// One simulated image digest for one architecture: its index, its BuildKit
/// attestation and SBOM.
fn simulated_image(
    repository: &str,
    architecture: &str,
    sources: &[(&str, &str)],
    annotations: serde_json::Value,
) -> (String, Vec<patchula_testkit::Interaction>) {
    use patchula_testkit::{Body, Interaction, RecordedRequest, RecordedResponse};
    let sha = |text: &str| {
        use sha2::Digest as _;
        let hex: String = sha2::Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("sha256:{hex}")
    };
    let packages: Vec<_> = sources
        .iter()
        .map(|(name, version)| {
            serde_json::json!({"name": name, "externalRefs": [{"referenceType": "purl",
                "referenceLocator": format!("pkg:deb/debian/{name}@{version}?os_name=debian&os_version=12")}]})
        })
        .collect();
    let spdx = serde_json::json!({
        "_type": "https://in-toto.io/Statement/v0.1",
        "predicateType": "https://spdx.dev/Document",
        "subject": [],
        "predicate": {"spdxVersion": "SPDX-2.3",
            "creationInfo": {"creators": ["Organization: Docker, Inc", "Tool: docker-scout-1.18.1"]},
            "packages": packages}
    })
    .to_string();
    let child = sha(&format!("{repository}{spdx}"));
    let attestation = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","layers":[{{"mediaType":"application/vnd.in-toto+json","digest":"{}","size":{},"annotations":{{"in-toto.io/predicate-type":"https://spdx.dev/Document"}}}}]}}"#,
        sha(&spdx),
        spdx.len()
    );
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "annotations": annotations,
        "manifests": [
            {"digest": child, "platform": {"os": "linux", "architecture": architecture}},
            {"digest": sha(&attestation), "platform": {"os": "unknown", "architecture": "unknown"},
             "annotations": {"vnd.docker.reference.type": "attestation-manifest", "vnd.docker.reference.digest": child}}
        ]
    })
    .to_string();
    let digest = sha(&index);
    const MANIFEST: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";
    let get = |path: String, accept: &str, body: &str| Interaction {
        request: RecordedRequest {
            method: "GET".into(),
            url: format!("https://ghcr.io/v2/{repository}/{path}"),
            headers: BTreeMap::from([("accept".into(), vec![accept.into()])]),
            body: Body::Text(String::new()),
        },
        response: RecordedResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Body::Text(body.into()),
        },
    };
    let interactions = vec![
        get(format!("manifests/{digest}"), MANIFEST, &index),
        get(
            format!("manifests/{}", sha(&attestation)),
            MANIFEST,
            &attestation,
        ),
        get(format!("blobs/{}", sha(&spdx)), "*/*", &spdx),
    ];
    (digest, interactions)
}

/// Only OSV can mark an image rebuild as a security update. A rebuild whose
/// image metadata claims a security fix, with no OSV match, gets no label and
/// no fixes; its twin with an OSV match gets both, under `security.apply`.
/// The twin is built for arm64 alone, so under the default platforms it is
/// compared on the first platform its index lists.
#[tokio::test]
async fn only_an_osv_match_marks_an_image_rebuild_as_a_security_fix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spec.json");
    let claim = serde_json::json!({
        "org.opencontainers.image.description": "Critical security fix",
        "org.opencontainers.image.title": "security release"
    });
    let none = serde_json::json!({});
    let (fixed_old, mut images) =
        simulated_image("spec/fixed", "arm64", &[("openssl", "1")], none.clone());
    let (fixed_new, more) =
        simulated_image("spec/fixed", "arm64", &[("openssl", "2")], none.clone());
    images.extend(more);
    let (claims_old, more) = simulated_image("spec/claims", "amd64", &[("zlib", "1")], none);
    images.extend(more);
    let (claims_new, more) = simulated_image("spec/claims", "amd64", &[("zlib", "2")], claim);
    images.extend(more);
    let dockerfile = format!(
        "FROM ghcr.io/spec/fixed:1.0@{fixed_old}\nFROM ghcr.io/spec/claims:1.0@{claims_old}\n"
    );
    let config = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"dockerfile\" }\n";
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
            {"path":"Dockerfile","type":"blob","sha":"dockerfile"}
        ]}),
        200,
        "tree",
        false,
    ));
    for (sha, text) in [("config", config), ("dockerfile", dockerfile.as_str())] {
        interactions.push(github(
            &format!("repos/demo/app/git/blobs/{sha}"),
            json!({"encoding":"base64","content":STANDARD.encode(text)}),
            200,
            sha,
            false,
        ));
    }
    for (repository, new) in [("spec/fixed", &fixed_new), ("spec/claims", &claims_new)] {
        interactions.push(exchange(
            "GET",
            &format!("https://ghcr.io/v2/{repository}/tags/list?n=1000"),
            &[("accept", "application/json")],
            Body::Text(String::new()),
            200,
            json!({"tags":["1.0"]}),
            None,
        ));
        let mut head = exchange(
            "HEAD",
            &format!("https://ghcr.io/v2/{repository}/manifests/1.0"),
            &[(
                "accept",
                "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
            )],
            Body::Text(String::new()),
            200,
            Value::Null,
            None,
        );
        head.response.body = Body::Text(String::new());
        head.response
            .headers
            .insert("docker-content-digest".into(), vec![new.to_string()]);
        interactions.push(head);
    }
    interactions.extend(images);
    let query = |name: &str, version: &str| json!({"package":{"ecosystem":"Debian:12","name":name},"version":version});
    interactions.push(exchange(
        "POST",
        "https://api.osv.dev/v1/querybatch",
        &[("accept", "application/json"), ("content-type", "application/json")],
        Body::Json(json!({"queries":[
            query("openssl", "1"), query("openssl", "2"), query("zlib", "1"), query("zlib", "2")
        ]})),
        200,
        json!({"results":[{"vulns":[{"id":"DSA-5999-1","modified":"2026-10-01T00:00:00Z"}]},{},{},{}]}),
        None,
    ));
    interactions.push(osv_record(
        "DSA-5999-1",
        json!({
            "id":"DSA-5999-1","modified":"2026-10-01T00:00:00Z","summary":"openssl security update",
            "aliases":["CVE-2026-0001"],
            "affected":[{"package":{"ecosystem":"Debian:12","name":"openssl"},
                "ranges":[{"type":"ECOSYSTEM","events":[{"introduced":"0"},{"fixed":"2"}]}]}]
        }),
    ));
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the GitHub, OCI distribution and OSV.dev API documentation"
            .into(),
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
    let report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Options::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    let repo = &report.repositories[0];
    assert!(repo.warnings.is_empty(), "{:?}", repo.warnings);
    assert!(
        repo.lookup_failures.is_empty(),
        "{:?}",
        repo.lookup_failures
    );
    let deps = &repo.package_files[0].dependencies;
    let dep = |package: &str| deps.iter().find(|d| d.package == package).unwrap();
    let fixed = dep("ghcr.io/spec/fixed");
    let update = &fixed.updates[0];
    assert_eq!(update.new_digest.as_deref(), Some(fixed_new.as_str()));
    assert_eq!(update.fixes, ["DSA-5999-1"]);
    let policy = update.policy.as_ref().unwrap();
    assert!(policy.labels.contains(&"security".to_owned()));
    assert_eq!(policy.wait_hours, 0);
    let check = update.image.as_ref().unwrap();
    assert_eq!(check.state, "checked");
    assert_eq!(check.platforms, ["linux/arm64"]);
    assert_eq!(check.fixed[0].package, "openssl");
    assert_eq!(check.fixed[0].new.as_deref(), Some("2"));
    let advisories = &fixed.metadata.as_ref().unwrap().advisories;
    assert_eq!(advisories.len(), 1);
    assert_eq!(advisories[0].aliases, ["CVE-2026-0001"]);
    let claims = dep("ghcr.io/spec/claims");
    let update = &claims.updates[0];
    assert_eq!(update.new_digest.as_deref(), Some(claims_new.as_str()));
    assert!(update.fixes.is_empty());
    assert!(
        !update
            .policy
            .as_ref()
            .unwrap()
            .labels
            .contains(&"security".to_owned())
    );
    let check = update.image.as_ref().unwrap();
    assert_eq!(check.state, "checked");
    assert_eq!(check.platforms, ["linux/amd64"]);
    assert!(claims.metadata.as_ref().unwrap().advisories.is_empty());
    replay.assert_finished().unwrap();
}

/// One image rebuild that leaves eight advisories behind and brings in seven.
/// Only the records in `read` are served, and those in `withdrawn` are
/// withdrawn; every one must be read, and nothing else.
async fn rebuild_with_many_advisories(
    withdrawn: &[&str],
    read: &[&str],
) -> patchula_report::Dependency {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spec.json");
    let none = serde_json::json!({});
    let (old, mut images) =
        simulated_image("spec/many", "amd64", &[("openssl", "1")], none.clone());
    let (new, more) = simulated_image("spec/many", "amd64", &[("openssl", "2")], none);
    images.extend(more);
    let dockerfile = format!("FROM ghcr.io/spec/many:1.0@{old}\n");
    let config = "amends \"PatchulaConfig.pkl\"\nmanagers = new Listing { \"dockerfile\" }\nsecurity { imagePlatforms = new Listing { \"linux/amd64\" } }\n";
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
            {"path":"Dockerfile","type":"blob","sha":"dockerfile"}
        ]}),
        200,
        "tree",
        false,
    ));
    for (sha, text) in [("config", config), ("dockerfile", dockerfile.as_str())] {
        interactions.push(github(
            &format!("repos/demo/app/git/blobs/{sha}"),
            json!({"encoding":"base64","content":STANDARD.encode(text)}),
            200,
            sha,
            false,
        ));
    }
    interactions.push(exchange(
        "GET",
        "https://ghcr.io/v2/spec/many/tags/list?n=1000",
        &[("accept", "application/json")],
        Body::Text(String::new()),
        200,
        json!({"tags":["1.0"]}),
        None,
    ));
    let mut head = exchange(
        "HEAD",
        "https://ghcr.io/v2/spec/many/manifests/1.0",
        &[(
            "accept",
            "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json",
        )],
        Body::Text(String::new()),
        200,
        Value::Null,
        None,
    );
    head.response.body = Body::Text(String::new());
    head.response
        .headers
        .insert("docker-content-digest".into(), vec![new.to_string()]);
    interactions.push(head);
    interactions.extend(images);
    let fixed: Vec<String> = std::iter::once("DSA-6000-1".to_owned())
        .chain((1..=7).map(|n| format!("DEBIAN-CVE-2025-{n}")))
        .collect();
    let introduced: Vec<String> = (1..=7).map(|n| format!("DEBIAN-CVE-2026-{n}")).collect();
    let listed = |ids: &[String]| -> Vec<Value> {
        ids.iter()
            .map(|id| json!({"id":id,"modified":"2026-10-01T00:00:00Z"}))
            .collect()
    };
    let query = |version: &str| json!({"package":{"ecosystem":"Debian:12","name":"openssl"},"version":version});
    interactions.push(exchange(
        "POST",
        "https://api.osv.dev/v1/querybatch",
        &[
            ("accept", "application/json"),
            ("content-type", "application/json"),
        ],
        Body::Json(json!({"queries":[query("1"), query("2")]})),
        200,
        json!({"results":[{"vulns":listed(&fixed)},{"vulns":listed(&introduced)}]}),
        None,
    ));
    for id in read {
        let mut record = json!({"id":id,"modified":"2026-10-01T00:00:00Z","summary":format!("{id} summary"),
            "affected":[{"package":{"ecosystem":"Debian:12","name":"openssl"}}]});
        if withdrawn.contains(id) {
            record["withdrawn"] = json!("2026-10-02T00:00:00Z");
        }
        interactions.push(osv_record(id, record));
    }
    let cassette = Cassette {
        schema_version: 1,
        provenance: "Simulated from the GitHub, OCI distribution and OSV.dev API documentation"
            .into(),
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
    let report = engine
        .lookup_at(
            Options {
                repositories: vec!["demo/app".into()],
                ..Options::default()
            },
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    replay.assert_finished().unwrap();
    report.repositories[0].package_files[0].dependencies[0].clone()
}

const INTRODUCED_READ: [&str; 5] = [
    "DEBIAN-CVE-2026-7",
    "DEBIAN-CVE-2026-6",
    "DEBIAN-CVE-2026-5",
    "DEBIAN-CVE-2026-4",
    "DEBIAN-CVE-2026-3",
];

/// Which advisories a rebuild leaves behind comes from OSV's batch answer; only
/// the five most severe it fixes, and the five most severe it brings in, are
/// read in full, and a withdrawn record takes its ID out.
#[tokio::test]
async fn an_image_rebuild_reads_only_the_most_severe_records() {
    let read: Vec<&str> = [
        "DSA-6000-1",
        "DEBIAN-CVE-2025-7",
        "DEBIAN-CVE-2025-6",
        "DEBIAN-CVE-2025-5",
        "DEBIAN-CVE-2025-4",
    ]
    .into_iter()
    .chain(INTRODUCED_READ)
    .collect();
    let dep = rebuild_with_many_advisories(&["DEBIAN-CVE-2025-4"], &read).await;
    let update = &dep.updates[0];
    assert_eq!(update.fixes.len(), 7, "{:?}", update.fixes);
    assert!(!update.fixes.contains(&"DEBIAN-CVE-2025-4".to_owned()));
    let check = update.image.as_ref().unwrap();
    assert_eq!(check.fixed.len(), 7);
    let shown: Vec<_> = dep
        .metadata
        .as_ref()
        .unwrap()
        .advisories
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(shown.len(), 4, "{shown:?}");
    assert!(shown.iter().all(|id| read.contains(id)), "{shown:?}");
    assert_eq!(
        check.introduced[..2],
        ["DEBIAN-CVE-2026-7", "DEBIAN-CVE-2026-6"]
    );
    assert_eq!(check.introduced.len(), 7);
    assert_eq!(check.introduced_details.len(), 5);
}

/// When every fix read is withdrawn, the next are read in rank order until
/// one isn't.
#[tokio::test]
async fn withdrawn_fixes_make_the_count_read_on_until_one_holds() {
    let first = [
        "DSA-6000-1",
        "DEBIAN-CVE-2025-7",
        "DEBIAN-CVE-2025-6",
        "DEBIAN-CVE-2025-5",
        "DEBIAN-CVE-2025-4",
    ];
    let read: Vec<&str> = first
        .into_iter()
        .chain([
            "DEBIAN-CVE-2025-3",
            "DEBIAN-CVE-2025-2",
            "DEBIAN-CVE-2025-1",
        ])
        .chain(INTRODUCED_READ)
        .collect();
    let dep = rebuild_with_many_advisories(&first, &read).await;
    let update = &dep.updates[0];
    assert_eq!(
        update.fixes,
        [
            "DEBIAN-CVE-2025-1",
            "DEBIAN-CVE-2025-2",
            "DEBIAN-CVE-2025-3"
        ]
    );
    assert!(
        update
            .policy
            .as_ref()
            .unwrap()
            .labels
            .contains(&"security".to_owned())
    );
    assert_eq!(dep.metadata.as_ref().unwrap().advisories.len(), 3);
}

/// A rebuild whose every fix turns out withdrawn is no security fix.
#[tokio::test]
async fn a_rebuild_whose_fixes_are_all_withdrawn_is_no_security_fix() {
    let fixes = [
        "DSA-6000-1",
        "DEBIAN-CVE-2025-7",
        "DEBIAN-CVE-2025-6",
        "DEBIAN-CVE-2025-5",
        "DEBIAN-CVE-2025-4",
        "DEBIAN-CVE-2025-3",
        "DEBIAN-CVE-2025-2",
        "DEBIAN-CVE-2025-1",
    ];
    let read: Vec<&str> = fixes.into_iter().chain(INTRODUCED_READ).collect();
    let dep = rebuild_with_many_advisories(&fixes, &read).await;
    let update = &dep.updates[0];
    assert!(update.fixes.is_empty(), "{:?}", update.fixes);
    assert!(
        !update
            .policy
            .as_ref()
            .unwrap()
            .labels
            .contains(&"security".to_owned())
    );
    assert!(update.image.as_ref().unwrap().fixed.is_empty());
    assert!(dep.metadata.as_ref().unwrap().advisories.is_empty());
}
