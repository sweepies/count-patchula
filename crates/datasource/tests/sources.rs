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
    // A version the registration does not describe yet is unchecked, which
    // policy holds, rather than listed and undated.
    assert!(at("12.0.3").unchecked && at("12.0.3").published_at.is_none());
    assert!(!at("13.0.2").unchecked);
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
            // A page link off nuget.org is never requested, and fails the
            // lookup rather than leave its versions looking listed.
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
        .await;
    assert!(offsite.is_err());
    assert!(steps.0.lock().unwrap().is_empty());
}

/// Every registration page is read: an older page holds an unlisted patch
/// release on an older line, which must not look listed and undated.
#[tokio::test]
async fn nuget_reads_every_registration_page() {
    let older = "https://api.nuget.org/v3/registration5-gz-semver2/lib/page-6.json";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            "https://api.nuget.org/v3-flatcontainer/lib/index.json".into(),
            response(200, r#"{"versions":["6.0.1","6.0.5","7.0.0"]}"#, &[]),
        ),
        (
            "https://api.nuget.org/v3/registration5-gz-semver2/lib/index.json".into(),
            response(
                200,
                &format!(
                    r#"{{"items":[
                        {{"@id":"{older}","count":2,"lower":"6.0.1","upper":"6.0.5"}},
                        {{"count":1,"lower":"7.0.0","upper":"7.0.0","items":[
                            {{"catalogEntry":{{"version":"7.0.0","published":"2024-06-01T00:00:00Z","listed":true}}}}
                        ]}}
                    ]}}"#
                ),
                &[],
            ),
        ),
        (
            older.into(),
            response(
                200,
                r#"{"items":[
                    {"catalogEntry":{"version":"6.0.1","published":"2023-01-01T00:00:00Z","listed":true}},
                    {"catalogEntry":{"version":"6.0.5+build.7","published":"2024-07-01T00:00:00Z","listed":false}}
                ]}"#,
                &[],
            ),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let catalog = client(steps.clone(), dir.path())
        .await
        .lookup(&patchula_config::PackageId::public(Source::Nuget, "lib"))
        .await
        .unwrap();
    let patch = catalog
        .versions
        .iter()
        .find(|v| v.version == "6.0.5")
        .unwrap();
    assert!(patch.yanked && !patch.unchecked);
    assert_eq!(patch.published_at.as_deref(), Some("2024-07-01T00:00:00Z"));
    assert!(catalog.versions.iter().all(|v| !v.unchecked));
    assert!(steps.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn maven_reads_versions_and_dates_each_from_its_pom() {
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
    assert!(!central.versions[0].unchecked);
    assert!(steps.0.lock().unwrap().is_empty());
}

/// Every Maven version is dated once, not just the newest ten; a later
/// lookup asks only about versions it has not dated, wherever they sort.
/// A failed POM request leaves its version unchecked and is asked again; a
/// version without a POM is dropped.
#[tokio::test]
async fn maven_dates_every_version_once_and_retries_failures() {
    let base = "https://repo.maven.apache.org/maven2/com/example/lib";
    let metadata = |versions: &[&str]| {
        let listed: String = versions
            .iter()
            .map(|v| format!("<version>{v}</version>"))
            .collect();
        response(
            200,
            &format!("<metadata><versioning><versions>{listed}</versions></versioning></metadata>"),
            &[("cache-control", "no-store")],
        )
    };
    let pom = |version: &str| format!("{base}/{version}/lib-{version}.pom");
    let dated = || {
        response(
            200,
            "",
            &[
                ("last-modified", "Wed, 01 Jan 2025 00:00:00 GMT"),
                ("cache-control", "no-store"),
            ],
        )
    };
    let mut first: Vec<String> = (0..12).map(|n| format!("3.{n}.0")).collect();
    first.push("3.3.1".into());
    let first: Vec<&str> = first.iter().map(String::as_str).collect();
    let mut steps = VecDeque::from([(format!("{base}/maven-metadata.xml"), metadata(&first))]);
    for version in &first {
        if *version == "3.3.1" {
            steps.push_back((pom(version), response(503, "", &[])));
        } else {
            steps.push_back((pom(version), dated()));
        }
    }
    let mut second = first.clone();
    second.push("3.3.2");
    second.push("3.3.3");
    steps.push_back((format!("{base}/maven-metadata.xml"), metadata(&second)));
    steps.push_back((pom("3.3.1"), dated()));
    steps.push_back((pom("3.3.2"), dated()));
    steps.push_back((pom("3.3.3"), response(404, "", &[])));
    let steps = Arc::new(Steps(Mutex::new(steps)));
    let dir = tempfile::tempdir().unwrap();
    let sources = client(steps.clone(), dir.path()).await;
    let id = patchula_config::PackageId::public(Source::Maven, "com.example:lib");
    let catalog = sources.lookup(&id).await.unwrap();
    let at = |catalog: &patchula_datasource::Catalog, version: &str| {
        catalog
            .versions
            .iter()
            .find(|v| v.version == version)
            .cloned()
    };
    let failed = at(&catalog, "3.3.1").unwrap();
    assert!(failed.unchecked && failed.published_at.is_none());
    assert_eq!(
        at(&catalog, "3.0.0").unwrap().published_at.as_deref(),
        Some("2025-01-01T00:00:00Z")
    );
    let catalog = sources.lookup(&id).await.unwrap();
    let retried = at(&catalog, "3.3.1").unwrap();
    assert!(!retried.unchecked && retried.published_at.is_some());
    assert!(at(&catalog, "3.3.2").unwrap().published_at.is_some());
    assert!(at(&catalog, "3.3.3").is_none());
    assert!(steps.0.lock().unwrap().is_empty());
}

/// Only the POMs of releases newer than the baseline are read, a patch on
/// the baseline's line included. A 429 leaves its version unchecked rather
/// than dated as undated, and is asked about again.
#[tokio::test]
async fn maven_dates_only_candidates_and_a_throttled_pom_stays_unchecked() {
    use patchula_datasource::Candidates;
    use patchula_words::Scheme;
    let base = "https://repo.maven.apache.org/maven2/com/example/lib";
    let metadata = response(
        200,
        "<metadata><versioning><versions><version>1.0.0</version><version>1.2.3</version>\
         <version>1.2.4</version><version>1.3.0-beta1</version><version>2.0.0</version>\
         </versions></versioning></metadata>",
        &[("cache-control", "no-store")],
    );
    let pom = |version: &str| format!("{base}/{version}/lib-{version}.pom");
    let dated = || {
        response(
            200,
            "",
            &[("last-modified", "Wed, 01 Jan 2025 00:00:00 GMT")],
        )
    };
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (format!("{base}/maven-metadata.xml"), metadata.clone()),
        (pom("1.2.4"), dated()),
        (pom("2.0.0"), response(429, "", &[("retry-after", "1")])),
        (format!("{base}/maven-metadata.xml"), metadata),
        (pom("2.0.0"), dated()),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let sources = client(steps.clone(), dir.path())
        .await
        .with_candidates(Candidates::after(
            Scheme::Maven,
            Source::Maven,
            "1.2.3",
            false,
        ));
    let id = patchula_config::PackageId::public(Source::Maven, "com.example:lib");
    let at = |catalog: &patchula_datasource::Catalog, version: &str| {
        catalog
            .versions
            .iter()
            .find(|v| v.version == version)
            .cloned()
            .unwrap()
    };
    let catalog = sources.lookup(&id).await.unwrap();
    assert_eq!(
        at(&catalog, "1.2.4").published_at.as_deref(),
        Some("2025-01-01T00:00:00Z")
    );
    let throttled = at(&catalog, "2.0.0");
    assert!(throttled.unchecked && throttled.published_at.is_none());
    for unasked in ["1.0.0", "1.2.3", "1.3.0-beta1"] {
        assert!(at(&catalog, unasked).unchecked, "{unasked}");
    }
    let catalog = sources.lookup(&id).await.unwrap();
    let retried = at(&catalog, "2.0.0");
    assert!(!retried.unchecked && retried.published_at.is_some());
    assert!(steps.0.lock().unwrap().is_empty());
}

/// When an operator mirror stands in for Maven Central, the POMs are read
/// from it, with its credentials, as the metadata is.
#[tokio::test]
async fn maven_reads_poms_through_the_mirror_that_serves_the_metadata() {
    let mirror = "https://acme.jfrog.io/maven/com/example/lib";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            format!("{mirror}/maven-metadata.xml"),
            response(
                200,
                "<metadata><versioning><versions><version>1.0.0</version></versions></versioning></metadata>",
                &[],
            ),
        ),
        (
            format!("{mirror}/1.0.0/lib-1.0.0.pom"),
            response(
                200,
                "",
                &[("last-modified", "Wed, 01 Jan 2025 00:00:00 GMT")],
            ),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let github = patchula_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        steps.clone(),
    )
    .unwrap();
    let entry = patchula_http::Credential::new(
        "https://acme.jfrog.io/maven",
        patchula_http::Auth::Bearer {
            token: "tok".into(),
        },
    )
    .unwrap()
    .mirroring("https://repo.maven.apache.org/maven2")
    .unwrap();
    let sources = Client::new(
        github,
        Cached::with_options(
            cache.clone(),
            steps.clone(),
            patchula_http::Options {
                registry_retries: 0,
                credentials: vec![entry],
                ..patchula_datasource::http_options()
            },
        )
        .unwrap(),
        cache,
    )
    .with_versions_host(false);
    let catalog = sources
        .lookup(&patchula_config::PackageId::public(
            Source::Maven,
            "com.example:lib",
        ))
        .await
        .unwrap();
    assert_eq!(
        catalog.versions[0].published_at.as_deref(),
        Some("2025-01-01T00:00:00Z")
    );
    assert!(steps.0.lock().unwrap().is_empty());
}

/// A linked registration page whose versions all sit at or below the
/// candidates' floor is not read; its versions are listed unchecked. A page
/// that may hold a candidate is read, and so is one without bounds.
#[tokio::test]
async fn nuget_skips_registration_pages_below_the_candidates() {
    use patchula_datasource::Candidates;
    use patchula_words::Scheme;
    let older = "https://api.nuget.org/v3/registration5-gz-semver2/lib/page-5.json";
    let current = "https://api.nuget.org/v3/registration5-gz-semver2/lib/page-6.json";
    let unbounded = "https://api.nuget.org/v3/registration5-gz-semver2/lib/page-x.json";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            "https://api.nuget.org/v3-flatcontainer/lib/index.json".into(),
            response(
                200,
                r#"{"versions":["5.0.0","5.1.0","6.0.0","6.0.1","7.0.0"]}"#,
                &[],
            ),
        ),
        (
            "https://api.nuget.org/v3/registration5-gz-semver2/lib/index.json".into(),
            response(
                200,
                &format!(
                    r#"{{"items":[
                        {{"@id":"{older}","count":2,"lower":"5.0.0","upper":"5.1.0"}},
                        {{"@id":"{current}","count":2,"lower":"6.0.0","upper":"6.0.1"}},
                        {{"@id":"{unbounded}","count":1}}
                    ]}}"#
                ),
                &[],
            ),
        ),
        (
            current.into(),
            response(
                200,
                r#"{"items":[
                    {"catalogEntry":{"version":"6.0.0","published":"2023-01-01T00:00:00Z","listed":true}},
                    {"catalogEntry":{"version":"6.0.1","published":"2024-07-01T00:00:00Z","listed":false,"deprecation":{"reasons":["Legacy"]}}}
                ]}"#,
                &[],
            ),
        ),
        (
            unbounded.into(),
            response(
                200,
                r#"{"items":[
                    {"catalogEntry":{"version":"7.0.0","published":"2025-01-01T00:00:00Z"}}
                ]}"#,
                &[],
            ),
        ),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let catalog = client(steps.clone(), dir.path())
        .await
        .with_candidates(Candidates::after(
            Scheme::Nuget,
            Source::Nuget,
            "6.0.0",
            false,
        ))
        .lookup(&patchula_config::PackageId::public(Source::Nuget, "lib"))
        .await
        .unwrap();
    let at = |version: &str| {
        catalog
            .versions
            .iter()
            .find(|v| v.version == version)
            .unwrap()
    };
    assert!(at("5.0.0").unchecked && at("5.1.0").unchecked);
    let patch = at("6.0.1");
    assert!(patch.yanked && patch.deprecated && !patch.unchecked);
    assert_eq!(
        at("7.0.0").published_at.as_deref(),
        Some("2025-01-01T00:00:00Z")
    );
    assert!(!at("7.0.0").yanked);
    assert!(
        steps.0.lock().unwrap().is_empty(),
        "the older page is never read"
    );
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
