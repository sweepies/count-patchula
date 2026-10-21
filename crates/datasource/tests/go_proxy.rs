//! Independent Go proxy contract fixtures; no network client is constructed.
use patchula_cache::Cache;
use patchula_config::Source;
use patchula_datasource::Client;
use patchula_http::{Cached, Exchange, Response, Transport};
use std::sync::{Arc, Mutex};

/// A module listing thirty releases, one of which the proxy cannot describe.
struct Proxy {
    infos: Mutex<Vec<String>>,
}
impl Transport for Proxy {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let path = request.url().path().to_string();
            let (status, body) = if path.ends_with("/@v/list") {
                let list: Vec<String> = (0..30).map(|n| format!("v1.{n}.0")).collect();
                (200, list.join("\n"))
            } else if let Some(version) = path
                .strip_prefix("/example.com/mod/@v/")
                .and_then(|p| p.strip_suffix(".info"))
            {
                self.infos.lock().unwrap().push(version.to_string());
                if version == "v1.28.0" {
                    (404, "not found: unknown revision".to_string())
                } else {
                    (
                        200,
                        format!(r#"{{"Version":"{version}","Time":"2026-01-01T00:00:00Z"}}"#),
                    )
                }
            } else {
                panic!("unexpected request {path}");
            };
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: body.into_bytes(),
            })
        })
    }
}

#[tokio::test]
async fn undescribed_versions_are_dropped_and_cold_lookups_date_only_recent_releases() {
    let proxy = Arc::new(Proxy {
        infos: Mutex::new(Vec::new()),
    });
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        proxy.clone(),
    )
    .unwrap();
    let client = Client::new(
        github,
        Cached::new(cache.clone(), proxy.clone(), 4).unwrap(),
        cache,
    )
    .with_versions_host(false);
    let catalog = client.lookup(Source::Go, "example.com/mod").await.unwrap();
    let versions: Vec<_> = catalog
        .versions
        .iter()
        .map(|v| v.version.as_str())
        .collect();
    assert_eq!(versions.len(), 29, "{versions:?}");
    assert!(!versions.contains(&"v1.28.0"));
    assert_eq!(versions[0], "v1.29.0");
    let infos = proxy.infos.lock().unwrap();
    assert_eq!(infos.len(), 20, "{infos:?}");
    assert!(catalog.versions[0].published_at.is_some());
    assert!(catalog.versions.last().unwrap().published_at.is_none());
}
