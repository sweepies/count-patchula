use super::*;
use patchula_http::{Exchange, Response};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize};

#[derive(Default)]
struct Registry {
    requests: AtomicUsize,
    phase: AtomicUsize,
    failed: AtomicBool,
}
impl Transport for Registry {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async move {
            assert_eq!(request.url().host_str(), Some("registry.npmjs.org"));
            assert!(!request.headers().contains_key("authorization"));
            self.requests.fetch_add(1, Ordering::SeqCst);
            if self.failed.load(Ordering::SeqCst) {
                return Err(patchula_http::Error::Fixture(
                    "offline connection failure".into(),
                ));
            }
            let phase = self.phase.load(Ordering::SeqCst);
            let etag = format!("\"phase-{phase}\"");
            let conditional = request
                .headers()
                .get("if-none-match")
                .is_some_and(|value| value == etag.as_str());
            let newest = if phase == 0 { "2.0.0" } else { "3.0.0" };
            let body = json!({"dist-tags":{"latest":newest},"versions":{"1.0.0":{},newest:{}},"time":{"1.0.0":"2020-01-01T00:00:00Z",newest:"2026-10-14T13:00:00Z"}});
            Ok(Response {
                status: if conditional {
                    reqwest::StatusCode::NOT_MODIFIED
                } else {
                    reqwest::StatusCode::OK
                },
                headers: [
                    (reqwest::header::ETAG, etag.parse().unwrap()),
                    (
                        reqwest::header::CACHE_CONTROL,
                        "max-age=0, must-revalidate".parse().unwrap(),
                    ),
                ]
                .into_iter()
                .collect(),
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    }
}
fn engine(cache: Cache, registry: Arc<Registry>, token: &str) -> Engine {
    Engine::with_transport(token, cache, 4, registry)
        .unwrap()
        .with_versions_host(false)
}
fn key(package: &str) -> LookupKey {
    ("false:npm".into(), package.into())
}
fn now() -> DateTime<Utc> {
    "2026-10-15T12:00:00Z".parse().unwrap()
}
async fn read(engine: &Engine, wait: u32, now: DateTime<Utc>, force: bool) -> (Catalogs, Evidence) {
    let key = key("fixture");
    engine
        .age_catalogs(
            BTreeSet::from([key.clone()]),
            &BTreeMap::from([(key.clone(), wait)]),
            &if force {
                BTreeSet::from([key])
            } else {
                BTreeSet::new()
            },
            &engine.sources.catalog_batch(),
            now,
        )
        .await
        .unwrap()
}
async fn memory() -> Cache {
    Cache::open(":memory:", Default::default()).await.unwrap()
}

#[test]
fn cache_windows_are_clamped_zero_disables_and_expiry_is_strict() {
    assert_eq!(reuse_hours(0), None);
    assert_eq!(reuse_hours(1), Some(1));
    assert_eq!(reuse_hours(2), Some(1));
    assert_eq!(reuse_hours(24), Some(23));
    assert_eq!(reuse_hours(25), Some(24));
    assert_eq!(reuse_hours(u32::MAX), Some(24));
    assert!(eligible(1, 3_600_000, 1));
    assert!(!eligible(1, 3_600_001, 1));
    assert!(!eligible(1, 0, 24));
}

#[tokio::test]
async fn warm_catalogs_survive_reopen_and_expire_at_the_effective_wait_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.sqlite");
    let registry = Arc::new(Registry::default());
    for (hours, expected_reads, reused, version) in [
        (0, 1, false, "2.0.0"),
        (22, 1, true, "2.0.0"),
        (23, 2, false, "3.0.0"),
    ] {
        if hours != 0 {
            registry.phase.store(1, Ordering::SeqCst);
        }
        let cache = Cache::open(&path, Default::default()).await.unwrap();
        let engine = engine(cache, registry.clone(), "offline");
        let (catalogs, evidence) =
            read(&engine, 24, now() + chrono::Duration::hours(hours), false).await;
        assert_eq!(registry.requests.load(Ordering::SeqCst), expected_reads);
        assert_eq!(evidence[&key("fixture")].reused, reused);
        assert!(
            catalogs[&key("fixture")]
                .as_ref()
                .unwrap()
                .versions
                .iter()
                .any(|v| v.version == version)
        );
    }
}

#[tokio::test]
async fn zero_wait_and_forced_reads_keep_freshness_and_credentials_do_not_share_views() {
    let cache = memory().await;
    let registry = Arc::new(Registry::default());
    for (wait, force, token, reused) in [
        (24, false, "a", false),
        (24, false, "a", true),
        (0, false, "a", false),
        (24, true, "a", false),
        (24, false, "b", false),
    ] {
        let before = registry.requests.load(Ordering::SeqCst);
        let engine = engine(cache.clone(), registry.clone(), token);
        let (_, evidence) = read(&engine, wait, now(), force).await;
        assert_eq!(
            registry.requests.load(Ordering::SeqCst) - before,
            usize::from(!reused)
        );
        assert_eq!(evidence[&key("fixture")].reused, reused);
    }
}

fn config() -> Config {
    patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"\n".into(),
        )]),
    )
    .unwrap()
}
fn dependencies(config: &Config) -> Vec<Dependency> {
    Scanner::new(config)
        .unwrap()
        .extract(&BTreeMap::from([(
            "package.json".into(),
            r#"{"dependencies":{"fixture":"1.0.0","other":"1.0.0"}}"#.into(),
        )]))
        .unwrap()
        .dependencies
}

#[tokio::test]
async fn cached_held_back_versions_become_eligible_without_a_catalog_request() {
    let cache = memory().await;
    let registry = Arc::new(Registry::default());
    let config = config();
    let dep = dependencies(&config).remove(0);
    let eval = Evaluator::new(&config).unwrap();
    let first = engine(cache.clone(), registry.clone(), "offline");
    let (catalogs, evidence) = read(&first, 24, now(), false).await;
    let catalog = catalogs[&key("fixture")].as_ref().unwrap();
    let held = eval.evaluate_at(&dep, catalog, now()).unwrap();
    assert_eq!(held.held_back[0].version, "2.0.0");
    assert_eq!(
        evidence[&key("fixture")].fetched_at,
        "2026-10-15T12:00:00.000Z"
    );
    let second = engine(cache, registry.clone(), "offline");
    let (catalogs, evidence) = read(&second, 24, now() + chrono::Duration::hours(2), false).await;
    let eligible = eval
        .evaluate_at(
            &dep,
            catalogs[&key("fixture")].as_ref().unwrap(),
            now() + chrono::Duration::hours(2),
        )
        .unwrap();
    assert!(eligible.held_back.is_empty());
    assert!(
        eligible
            .updates
            .iter()
            .any(|update| update.new_version.as_deref() == Some("2.0.0"))
    );
    assert!(evidence[&key("fixture")].reused);
    assert_eq!(
        evidence[&key("fixture")].fetched_at,
        "2026-10-15T12:00:00.000Z"
    );
    assert_eq!(registry.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn checkbox_controls_refresh_only_the_selected_dependency_and_failures_do_not_become_views() {
    let cache = memory().await;
    let registry = Arc::new(Registry::default());
    let engine = engine(cache, registry.clone(), "offline");
    let config = config();
    let dependencies = dependencies(&config);
    let first = patchula_update::dashboard::extracted_key(&dependencies[0]);
    let mapping = Controls {
        branches: BTreeMap::from([(
            "patchula/fixture-2.x".into(),
            BTreeSet::from([first.clone()]),
        )]),
        targets: BTreeMap::from([("target".into(), BTreeSet::from([first.clone()]))]),
    };
    engine
        .cache
        .extraction_put(
            &engine.catalog_controls_key("demo/app"),
            &serde_json::to_vec(&mapping).unwrap(),
        )
        .await
        .unwrap();
    for mode in [
        "retry",
        "pr",
        "changed-pr",
        "dashboard",
        "recreate",
        "repository",
        "unknown",
    ] {
        let mut controls = patchula_update::dashboard::Controls::default();
        match mode {
            "retry" => {
                controls.retry.insert(first.clone());
            }
            "pr" | "changed-pr" => {
                controls.branches.insert("patchula/fixture-2.x".into());
            }
            "dashboard" => {
                controls.create.insert("target".into());
            }
            "recreate" => {
                controls.recreate.insert("target".into());
            }
            "repository" => controls.recheck = true,
            _ => {
                controls.branches.insert("unknown".into());
            }
        }
        let mut current = dependencies.clone();
        if mode == "changed-pr" {
            current[0].current = "1.1.0".into();
        }
        let work = Work {
            report: repository("demo/app", Status::Complete, None),
            config: Some(config.clone()),
            dependencies: current,
            snapshot: Default::default(),
            inventory: None,
            controls,
        };
        let forced = engine.forced_catalog_keys(&[work]).await.unwrap();
        assert!(forced.contains(&engine.catalog_key(&dependencies[0], Some(&config))));
        assert_eq!(
            forced.len(),
            if matches!(mode, "repository" | "unknown" | "changed-pr") {
                2
            } else {
                1
            }
        );
    }
    read(&engine, 24, now(), false).await;
    registry.failed.store(true, Ordering::SeqCst);
    let (failed, _) = read(&engine, 24, now(), true).await;
    assert!(failed[&key("fixture")].is_err());
    let (cached, evidence) = read(&engine, 24, now(), false).await;
    assert!(cached[&key("fixture")].is_ok());
    assert!(evidence[&key("fixture")].reused);
}

#[test]
fn effective_rules_include_zero_age_exceptions() {
    let config = patchula_config::load_files("/repo/patchula.pkl",BTreeMap::from([("/repo/patchula.pkl".into(),"amends \"PatchulaConfig.pkl\"\ndefaults { waitHours = 72 }\nrules = new Listing { new Rule { select { packages = new Listing { \"fixture\" }; updates = new Listing { \"minor\" } }; apply { waitHours = 0 } } }\n".into())])).unwrap();
    let eval = Evaluator::new(&config).unwrap();
    for dep in dependencies(&config) {
        assert_eq!(
            eval.catalog_wait_hours(&dep),
            if dep.name == "fixture" { 0 } else { 72 }
        );
    }
}
