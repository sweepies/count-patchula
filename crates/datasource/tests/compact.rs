//! Compact catalogs retain conditional HTTP semantics across process handles.
use reqwest::{StatusCode, header::HeaderMap};
use rustovate_cache::{Cache, HttpKey};
use rustovate_config::Source;
use rustovate_datasource::Client;
use rustovate_http::{Cached, Exchange, Response, Transport};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

type Step = (Option<&'static str>, Response);
struct Queue(Mutex<VecDeque<Step>>);
impl Transport for Queue {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let (validator, response) = self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected network read");
            assert_eq!(
                request
                    .headers()
                    .get("if-none-match")
                    .map(|v| v.to_str().unwrap()),
                validator
            );
            assert_eq!(request.headers()["accept"], "application/json");
            Ok(response)
        })
    }
}
fn response(status: u16, etag: &str, control: &str, body: Value) -> Response {
    Response {
        status: StatusCode::from_u16(status).unwrap(),
        headers: HeaderMap::from_iter([
            (reqwest::header::ETAG, etag.parse().unwrap()),
            (reqwest::header::CACHE_CONTROL, control.parse().unwrap()),
        ]),
        body: if status == 304 {
            Vec::new()
        } else {
            serde_json::to_vec(&body).unwrap()
        },
    }
}
fn client(cache: Cache, transport: Arc<Queue>) -> Client {
    Client::new(
        rustovate_github::Client::with_transport(
            "offline",
            cache.clone(),
            Default::default(),
            transport.clone(),
        )
        .unwrap(),
        Cached::new(cache.clone(), transport, 4).unwrap(),
        cache,
    )
    .with_versions_host(false)
}
fn document(version: &str, deprecated: &str) -> Value {
    json!({"dist-tags":{"latest":version},"time":{version:"2026-10-14T00:00:00Z","modified":"2026-10-15T00:00:00Z"},
        "versions":{version:{"deprecated":deprecated,"readme":"discard me".repeat(100_000),"dist":{"integrity":"unused"}}}})
}
#[tokio::test]
async fn fresh_reopen_304_changed_200_and_invalid_200_preserve_catalog_semantics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.sqlite");
    let cache = Cache::open(&path, Default::default()).await.unwrap();
    let transport = Arc::new(Queue(Mutex::new(VecDeque::from([
        (
            None,
            response(200, "v1", "no-cache", document("1.0.0", "old")),
        ),
        (Some("v1"), response(304, "v2", "max-age=600", Value::Null)),
        (
            Some("v2"),
            response(200, "v3", "no-cache", document("2.0.0", "")),
        ),
        (
            Some("v3"),
            response(200, "invalid", "no-cache", json!({"versions":{}})),
        ),
        (Some("v3"), response(304, "v3", "max-age=600", Value::Null)),
    ]))));
    let first = client(cache.clone(), transport.clone())
        .lookup(Source::Npm, "fixture")
        .await
        .unwrap();
    assert!(first.versions[0].deprecated);
    let second = client(cache.clone(), transport.clone())
        .lookup(Source::Npm, "fixture")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap()
    );
    let reopened = Cache::open(&path, Default::default()).await.unwrap();
    let third = client(reopened, transport.clone())
        .lookup(Source::Npm, "fixture")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(third).unwrap()
    );
    assert_eq!(
        transport.0.lock().unwrap().len(),
        3,
        "fresh reopen performs no request"
    );
    let key = HttpKey::new(
        "GET",
        "https://registry.npmjs.org/fixture",
        BTreeMap::from([("accept".into(), "application/json".into())]),
    )
    .unwrap()
    .representation("npm-catalog-v1");
    let mut entry = cache.http_get(&key).await.unwrap().unwrap();
    assert!(
        entry.body.len() < 500,
        "README and integrity must not survive compaction"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&entry.body).unwrap()["modified"],
        "2026-10-15T00:00:00Z"
    );
    entry.expires_at = Some(0);
    cache.http_put(&key, &entry).await.unwrap();
    let changed = client(cache.clone(), transport.clone())
        .lookup(Source::Npm, "fixture")
        .await
        .unwrap();
    assert_eq!(changed.versions.len(), 1, "unpublished versions disappear");
    assert_eq!(changed.versions[0].version, "2.0.0");
    assert!(!changed.versions[0].deprecated);
    assert_eq!(changed.dist_tags["latest"], "2.0.0");
    assert_eq!(
        changed.versions[0].published_at.as_deref(),
        Some("2026-10-14T00:00:00Z")
    );
    assert!(
        client(cache.clone(), transport.clone())
            .lookup(Source::Npm, "fixture")
            .await
            .is_err()
    );
    let final_catalog = client(cache, transport.clone())
        .lookup(Source::Npm, "fixture")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(changed).unwrap(),
        serde_json::to_value(final_catalog).unwrap()
    );
    assert!(transport.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn uncacheable_derived_responses_remove_body_and_validators() {
    for (control, vary) in [("no-store", None), ("max-age=600", Some("x-unknown"))] {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        let mut uncacheable = response(200, "v2", control, document("2.0.0", ""));
        if let Some(vary) = vary {
            uncacheable
                .headers
                .insert(reqwest::header::VARY, vary.parse().unwrap());
        }
        let transport = Arc::new(Queue(Mutex::new(VecDeque::from([
            (None, response(200, "v1", "no-cache", document("1.0.0", ""))),
            (Some("v1"), uncacheable),
            (
                None,
                response(200, "v3", "max-age=600", document("3.0.0", "")),
            ),
        ]))));
        let client = client(cache, transport.clone());
        for version in ["1.0.0", "2.0.0", "3.0.0"] {
            assert_eq!(
                client
                    .lookup(Source::Npm, "fixture")
                    .await
                    .unwrap()
                    .versions[0]
                    .version,
                version
            );
        }
        assert!(transport.0.lock().unwrap().is_empty());
    }
}
