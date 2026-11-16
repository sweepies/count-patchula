//! Independent Go proxy contract fixtures; no network client is constructed.
use patchula_cache::Cache;
use patchula_datasource::{Catalog, Client};
use patchula_http::{Exchange, Response, Transport};
use patchula_words::Source;
use std::sync::{Arc, Mutex};

/// A module proxy whose listing, undescribable (404) and failing (500)
/// versions a test sets, recording every `.info` request.
struct Proxy {
    list: Mutex<Vec<String>>,
    missing: Vec<String>,
    failing: Mutex<Vec<String>>,
    infos: Mutex<Vec<String>>,
}
impl Proxy {
    fn new(list: Vec<String>, missing: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            list: Mutex::new(list),
            missing: missing.iter().map(|v| (*v).to_owned()).collect(),
            failing: Mutex::new(Vec::new()),
            infos: Mutex::new(Vec::new()),
        })
    }
    fn infos(&self) -> Vec<String> {
        let mut infos = std::mem::take(&mut *self.infos.lock().unwrap());
        infos.sort();
        infos
    }
}
impl Transport for Proxy {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            let path = request.url().path().to_string();
            let (status, body) = if path.ends_with("/@v/list") {
                (200, self.list.lock().unwrap().join("\n"))
            } else if let Some(version) = path
                .strip_prefix("/example.com/mod/@v/")
                .and_then(|p| p.strip_suffix(".info"))
            {
                self.infos.lock().unwrap().push(version.to_string());
                if self.missing.iter().any(|v| v == version) {
                    (404, "not found: unknown revision".to_string())
                } else if self.failing.lock().unwrap().iter().any(|v| v == version) {
                    (500, "unavailable".to_string())
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
                headers: [(reqwest::header::CACHE_CONTROL, "no-store".parse().unwrap())]
                    .into_iter()
                    .collect(),
                body: body.into_bytes(),
                stale: None,
            })
        })
    }
}
async fn lookup(proxy: &Arc<Proxy>, cache: &Cache) -> Catalog {
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        proxy.clone(),
    )
    .unwrap();
    Client::new(
        github,
        patchula_datasource::cached(cache.clone(), proxy.clone(), 4).unwrap(),
        cache.clone(),
    )
    .with_versions_host(false)
    .lookup(&patchula_config::PackageId::public(
        Source::Go,
        "example.com/mod",
    ))
    .await
    .unwrap()
}
fn version<'a>(catalog: &'a Catalog, version: &str) -> &'a patchula_datasource::Version {
    catalog
        .versions
        .iter()
        .find(|v| v.version == version)
        .unwrap_or_else(|| panic!("{version} is listed"))
}

/// Every listed version is dated once, however many there are; versions
/// the proxy cannot describe are dropped.
#[tokio::test]
async fn every_version_is_dated_once_and_undescribed_versions_are_dropped() {
    let list: Vec<String> = (0..30).map(|n| format!("v1.{n}.0")).collect();
    let proxy = Proxy::new(list, &["v1.28.0"]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let catalog = lookup(&proxy, &cache).await;
    let versions: Vec<_> = catalog
        .versions
        .iter()
        .map(|v| v.version.as_str())
        .collect();
    assert_eq!(versions.len(), 29, "{versions:?}");
    assert!(!versions.contains(&"v1.28.0"));
    assert_eq!(versions[0], "v1.29.0");
    assert_eq!(proxy.infos().len(), 30);
    assert!(
        catalog
            .versions
            .iter()
            .all(|v| v.published_at.is_some() && !v.unchecked)
    );
    // A warm lookup asks about nothing it has already dated, not even the
    // version the proxy could not describe.
    lookup(&proxy, &cache).await;
    assert_eq!(proxy.infos(), Vec::<String>::new());
}

/// A new patch release on an older line is dated, though newer versions
/// were dated before it appeared.
#[tokio::test]
async fn a_new_release_below_the_newest_known_is_dated() {
    let proxy = Proxy::new(vec!["v0.29.3".into(), "v0.30.0".into()], &[]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    lookup(&proxy, &cache).await;
    assert_eq!(proxy.infos(), ["v0.29.3", "v0.30.0"]);
    proxy.list.lock().unwrap().push("v0.29.4".into());
    let catalog = lookup(&proxy, &cache).await;
    assert_eq!(proxy.infos(), ["v0.29.4"]);
    assert_eq!(
        version(&catalog, "v0.29.4").published_at.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
}

/// A version whose `.info` request fails is unchecked, not undated, and is
/// asked about again by the next lookup.
#[tokio::test]
async fn a_failed_date_request_leaves_the_version_unchecked_and_is_retried() {
    let proxy = Proxy::new(vec!["v1.0.0".into(), "v1.0.1".into()], &[]);
    proxy.failing.lock().unwrap().push("v1.0.1".into());
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let catalog = lookup(&proxy, &cache).await;
    let failed = version(&catalog, "v1.0.1");
    assert!(failed.unchecked && failed.published_at.is_none());
    assert!(!version(&catalog, "v1.0.0").unchecked);
    proxy.infos();
    proxy.failing.lock().unwrap().clear();
    let catalog = lookup(&proxy, &cache).await;
    assert_eq!(proxy.infos(), ["v1.0.1"]);
    let dated = version(&catalog, "v1.0.1");
    assert!(!dated.unchecked && dated.published_at.is_some());
}
