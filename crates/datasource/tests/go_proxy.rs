//! Independent Go proxy contract fixtures; no network client is constructed.
use patchula_cache::Cache;
use patchula_datasource::{Candidates, Catalog, Client};
use patchula_http::{Exchange, Response, Transport};
use patchula_words::{Scheme, Source};
use std::sync::{Arc, Mutex};

/// A module proxy whose listing, undescribable (404) and failing (500)
/// versions a test sets, recording every `.info` request.
struct Proxy {
    list: Mutex<Vec<String>>,
    missing: Vec<String>,
    failing: Mutex<Vec<String>>,
    infos: Mutex<Vec<String>>,
    /// The `Cache-Control` every answer carries.
    cache_control: &'static str,
}
impl Proxy {
    fn new(list: Vec<String>, missing: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            list: Mutex::new(list),
            missing: missing.iter().map(|v| (*v).to_owned()).collect(),
            failing: Mutex::new(Vec::new()),
            infos: Mutex::new(Vec::new()),
            cache_control: "no-store",
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
                headers: [(
                    reqwest::header::CACHE_CONTROL,
                    self.cache_control.parse().unwrap(),
                )]
                .into_iter()
                .collect(),
                body: body.into_bytes(),
                stale: None,
            })
        })
    }
}
async fn lookup(proxy: &Arc<Proxy>, cache: &Cache) -> Catalog {
    lookup_for(proxy, cache, Candidates::all()).await
}
async fn lookup_for(proxy: &Arc<Proxy>, cache: &Cache, candidates: Candidates) -> Catalog {
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
    .with_candidates(candidates)
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

/// Releases newer than the baseline are dated, a patch on the current line
/// included; older ones and unwanted prereleases are listed unchecked and
/// never asked about. A later lookup with an older baseline asks only about
/// the versions it adds.
#[tokio::test]
async fn only_candidates_are_dated_and_an_older_baseline_adds_only_its_own() {
    let list = [
        "v1.0.0",
        "v1.2.0",
        "v1.2.3",
        "v1.2.4",
        "v1.3.0",
        "v2.0.0-rc.1",
        "v2.0.0",
    ];
    let proxy = Proxy::new(list.iter().map(|v| (*v).to_owned()).collect(), &[]);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let after =
        |floor: &str, prereleases| Candidates::after(Scheme::Go, Source::Go, floor, prereleases);
    let catalog = lookup_for(&proxy, &cache, after("v1.2.3", false)).await;
    assert_eq!(proxy.infos(), ["v1.2.4", "v1.3.0", "v2.0.0"]);
    for dated in ["v1.2.4", "v1.3.0", "v2.0.0"] {
        let dated = version(&catalog, dated);
        assert!(dated.published_at.is_some() && !dated.unchecked);
    }
    for undated in ["v1.0.0", "v1.2.0", "v1.2.3", "v2.0.0-rc.1"] {
        let undated = version(&catalog, undated);
        assert!(
            undated.unchecked && undated.published_at.is_none(),
            "{}",
            undated.version
        );
    }
    // An older baseline that also wants prereleases asks only about what
    // the first lookup did not.
    let catalog = lookup_for(&proxy, &cache, after("v1.0.0", true)).await;
    assert_eq!(proxy.infos(), ["v1.2.0", "v1.2.3", "v2.0.0-rc.1"]);
    assert!(version(&catalog, "v1.0.0").unchecked);
    assert!(!version(&catalog, "v1.2.0").unchecked);
    // Every version, and everything is known by now.
    let catalog = lookup(&proxy, &cache).await;
    assert_eq!(proxy.infos(), ["v1.0.0"]);
    assert!(catalog.versions.iter().all(|v| !v.unchecked));
}

/// A `.info` answer is kept as release-dates state only: it is never
/// written to the HTTP cache, even when the proxy lets it be cached.
#[tokio::test]
async fn date_requests_never_enter_the_http_cache() {
    let proxy = Arc::new(Proxy {
        cache_control: "public, max-age=600",
        ..Arc::into_inner(Proxy::new(vec!["v1.0.0".into()], &[])).unwrap()
    });
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    lookup(&proxy, &cache).await;
    assert_eq!(proxy.infos(), ["v1.0.0"]);
    let key = |url: &str| {
        patchula_http::conditional::key("GET", url, &[("accept", "application/json")], None, None)
            .unwrap()
    };
    assert!(
        cache
            .http_get(&key(
                "https://proxy.golang.org/example.com/mod/@v/v1.0.0.info"
            ))
            .await
            .unwrap()
            .is_none()
    );
}
