//! Independently simulated registry metadata from ecosystem specifications.
use rustovate_cache::Cache;
use rustovate_config::Source;
use rustovate_datasource::{Client, escape_go};
use rustovate_http::Cached;
use rustovate_testkit::{Body, Cassette, Interaction, RecordedRequest, RecordedResponse, Replay};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
#[tokio::test]
async fn registry_flags_tags_and_sparse_layout_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metadata.json");
    let interactions=[("https://registry.npmjs.org/spec-package","application/vnd.npm.install-v1+json",Body::Json(json!({"dist-tags":{"latest":"2.0.0","next":"3.0.0-beta.1"},"versions":{"1.0.0":{"deprecated":"use new API"},"2.0.0":{},"3.0.0-beta.1":{}}}))),
        ("https://index.crates.io/sp/ec/spec-package","text/plain",Body::Text("{\"vers\":\"1.0.0\",\"yanked\":false}\n{\"vers\":\"2.0.0\",\"yanked\":true}\n".into()))].into_iter().map(|(url,accept,body)|Interaction{request:RecordedRequest{method:"GET".into(),url:url.into(),headers:BTreeMap::from([("accept".into(),vec![accept.into()])]),body:Body::Text(String::new())},response:RecordedResponse{status:200,headers:Default::default(),body}}).collect();
    rustovate_testkit::save(&path,&Cassette{schema_version:1,provenance:"Simulated from npm metadata and Cargo sparse-index specifications, independently written".into(),interactions},&Default::default()).unwrap();
    let replay = Arc::new(Replay::open(&path, Default::default()).unwrap());
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let github = rustovate_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        replay.clone(),
    )
    .unwrap();
    let client = Client::new(
        github,
        Cached::new(cache.clone(), replay.clone(), 4).unwrap(),
        cache,
    );
    let npm = client.lookup(Source::Npm, "spec-package").await.unwrap();
    assert_eq!(npm.dist_tags["next"], "3.0.0-beta.1");
    assert!(
        npm.versions
            .iter()
            .find(|v| v.version == "1.0.0")
            .unwrap()
            .deprecated
    );
    let crates = client.lookup(Source::Crates, "spec-package").await.unwrap();
    assert!(crates.versions[0].yanked);
    assert!(!crates.versions[1].yanked);
    replay.assert_finished().unwrap();
}
#[test]
fn go_case_escape_follows_proxy_specification() {
    assert_eq!(
        escape_go("github.com/Azure/SDK").unwrap(),
        "github.com/!azure/!s!d!k"
    );
    assert!(escape_go("x/y?bad").is_err());
}
