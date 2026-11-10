//! Independently simulated public-source metadata, never live HTTP.
use patchula_cache::Cache;
use patchula_datasource::Client;
use patchula_http::{Cached, Exchange, Response, Transport};
use patchula_words::Source;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

struct Steps(Mutex<VecDeque<(String, Response)>>);
impl Transport for Steps {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let url = request.url().to_string();
        let mut steps = self.0.lock().unwrap();
        // Registries may fan out reads in any order; the set of requests is
        // the behavior under test, not their sequence.
        let at = steps
            .iter()
            .position(|(expected, _)| expected == &url)
            .unwrap_or_else(|| panic!("unexpected network read: {url}"));
        let (_, response) = steps.remove(at).unwrap();
        Box::pin(async { Ok(response) })
    }
}
fn response(status: u16, body: &str, headers: &[(&str, &str)]) -> Response {
    Response {
        status: reqwest::StatusCode::from_u16(status).unwrap(),
        body: body.as_bytes().to_vec(),
        headers: headers
            .iter()
            .map(|(k, v)| {
                (
                    reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    v.parse().unwrap(),
                )
            })
            .collect(),
        stale: None,
    }
}
async fn client(transport: Arc<Steps>, dir: &std::path::Path) -> Client {
    let cache = Cache::open(dir.join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        transport.clone(),
    )
    .unwrap();
    Client::new(
        github,
        Cached::with_options(
            cache.clone(),
            transport,
            patchula_http::Options {
                registry_retries: 0,
                ..patchula_datasource::http_options()
            },
        )
        .unwrap(),
        cache,
    )
    .with_versions_host(false)
}

#[tokio::test]
async fn nuget_versions_take_dates_and_flags_from_the_registration() {
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            "https://api.nuget.org/v3-flatcontainer/newtonsoft.json/index.json".into(),
            response(200, r#"{"versions":["13.0.1","13.0.2","12.0.3"]}"#, &[]),
        ),
        (
            "https://api.nuget.org/v3/registration5-gz-semver2/newtonsoft.json/index.json".into(),
            response(
                200,
                r#"{"items":[{"items":[
                    {"catalogEntry":{"version":"13.0.2","published":"2024-03-01T00:00:00Z","listed":true,"deprecation":null}},
                    {"catalogEntry":{"version":"13.0.1","published":"2023-01-01T00:00:00Z","listed":false,"deprecation":{"reason":"bad"}}}
                ]}]}"#,
                &[],
            ),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let catalog = client(steps.clone(), dir.path())
        .await
        .lookup(&patchula_config::PackageId::public(
            Source::Nuget,
            "Newtonsoft.Json",
        ))
        .await
        .unwrap();
    let versions: Vec<_> = catalog
        .versions
        .iter()
        .map(|v| v.version.as_str())
        .collect();
    assert_eq!(versions, ["13.0.2", "13.0.1", "12.0.3"]);
    let at = |version: &str| {
        catalog
            .versions
            .iter()
            .find(|v| v.version == version)
            .unwrap()
    };
    assert_eq!(
        at("13.0.2").published_at.as_deref(),
        Some("2024-03-01T00:00:00Z")
    );
    assert!(!at("13.0.2").yanked && !at("13.0.2").deprecated);
    // Unlisted is yanked; a deprecation object marks deprecated.
    assert!(at("13.0.1").yanked && at("13.0.1").deprecated);
    // A version the registration does not know lists without metadata.
    assert!(at("12.0.3").published_at.is_none() && !at("12.0.3").yanked);
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nuget_registration_follows_only_nugets_own_pages() {
    let page = "https://api.nuget.org/v3/registration5-gz-semver2/spec/page1.json";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            "https://api.nuget.org/v3-flatcontainer/spec/index.json".into(),
            response(200, r#"{"versions":["1.0.0","2.0.0"]}"#, &[]),
        ),
        (
            "https://api.nuget.org/v3/registration5-gz-semver2/spec/index.json".into(),
            // The newest page is a link, not inline entries.
            response(
                200,
                &format!(r#"{{"items":[{{"@id":"{page}","count":2}}]}}"#),
                &[],
            ),
        ),
        (
            page.into(),
            response(
                200,
                r#"{"items":[
                    {"catalogEntry":{"version":"2.0.0","published":"2024-01-01T00:00:00Z","listed":true}},
                    {"catalogEntry":{"version":"1.0.0","published":"2023-01-01T00:00:00Z","listed":true}}
                ]}"#,
                &[],
            ),
        ),
        (
            "https://api.nuget.org/v3-flatcontainer/offsite/index.json".into(),
            response(200, r#"{"versions":["1.0.0"]}"#, &[]),
        ),
        (
            "https://api.nuget.org/v3/registration5-gz-semver2/offsite/index.json".into(),
            // A page link off nuget.org is never requested.
            response(
                200,
                r#"{"items":[{"@id":"https://evil.example.com/page.json","count":1}]}"#,
                &[],
            ),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let sources = client(steps.clone(), dir.path()).await;
    let catalog = sources
        .lookup(&patchula_config::PackageId::public(Source::Nuget, "spec"))
        .await
        .unwrap();
    assert!(catalog.versions.iter().all(|v| v.published_at.is_some()));
    let offsite = sources
        .lookup(&patchula_config::PackageId::public(
            Source::Nuget,
            "offsite",
        ))
        .await
        .unwrap();
    assert_eq!(offsite.versions.len(), 1);
    assert!(offsite.versions[0].published_at.is_none());
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn maven_reads_versions_and_dates_the_newest_from_poms() {
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            "https://maven.acme.dev/com/acme/lib/maven-metadata.xml".into(),
            response(
                200,
                "<metadata><versioning><versions>\
                 <version>1.0.0</version><version>2.1.0-SNAPSHOT</version><version>2.0.0</version>\
                 </versions></versioning></metadata>",
                &[],
            ),
        ),
        (
            "https://maven.acme.dev/com/acme/lib/2.0.0/lib-2.0.0.pom".into(),
            response(
                200,
                "",
                &[("last-modified", "Wed, 01 Jan 2025 00:00:00 GMT")],
            ),
        ),
        (
            "https://maven.acme.dev/com/acme/lib/1.0.0/lib-1.0.0.pom".into(),
            response(
                200,
                "",
                &[("last-modified", "Tue, 02 Jan 2024 00:00:00 GMT")],
            ),
        ),
        (
            "https://repo.maven.apache.org/maven2/com/example/lib/maven-metadata.xml".into(),
            response(
                200,
                "<metadata><versioning><versions><version>3.0.0</version></versions></versioning></metadata>",
                &[],
            ),
        ),
        (
            "https://repo.maven.apache.org/maven2/com/example/lib/3.0.0/lib-3.0.0.pom".into(),
            response(200, "", &[]),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let sources = client(steps.clone(), dir.path()).await;
    // `repository|group:artifact` picks the named repository; SNAPSHOTs are
    // never listed, and each POM's Last-Modified dates its version.
    let catalog = sources
        .lookup(&patchula_config::PackageId::public(
            Source::Maven,
            "https://maven.acme.dev|com.acme:lib",
        ))
        .await
        .unwrap();
    let versions: Vec<_> = catalog
        .versions
        .iter()
        .map(|v| (v.version.as_str(), v.published_at.as_deref()))
        .collect();
    assert_eq!(
        versions,
        [
            ("2.0.0", Some("2025-01-01T00:00:00Z")),
            ("1.0.0", Some("2024-01-02T00:00:00Z")),
        ]
    );
    // Everything else defaults to Maven Central; a POM without
    // Last-Modified leaves the date empty.
    let central = sources
        .lookup(&patchula_config::PackageId::public(
            Source::Maven,
            "com.example:lib",
        ))
        .await
        .unwrap();
    assert_eq!(central.versions[0].version, "3.0.0");
    assert!(central.versions[0].published_at.is_none());
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rubygems_prefers_the_ruby_platforms_date() {
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([(
        "https://rubygems.org/api/v1/versions/rails.json".into(),
        response(
            200,
            r#"[
                {"number":"8.0.0","platform":"x86_64-linux","created_at":"2024-01-02T00:00:00Z"},
                {"number":"8.0.0","platform":"ruby","created_at":"2024-01-01T00:00:00Z"},
                {"number":"7.2.0","platform":"ruby","created_at":"2023-01-01T00:00:00Z"}
            ]"#,
            &[],
        ),
    )]))));
    let dir = tempfile::tempdir().unwrap();
    let catalog = client(steps.clone(), dir.path())
        .await
        .lookup(&patchula_config::PackageId::public(
            Source::Rubygems,
            "rails",
        ))
        .await
        .unwrap();
    let versions: Vec<_> = catalog
        .versions
        .iter()
        .map(|v| (v.version.as_str(), v.published_at.as_deref()))
        .collect();
    assert_eq!(
        versions,
        [
            ("8.0.0", Some("2024-01-01T00:00:00Z")),
            ("7.2.0", Some("2023-01-01T00:00:00Z")),
        ]
    );
    assert!(steps.0.lock().unwrap().is_empty());
}
