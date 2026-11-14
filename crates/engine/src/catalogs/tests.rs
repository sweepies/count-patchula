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
                return Err(patchula_http::Error::Network);
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
                stale: None,
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
    (
        false,
        PackageId::public(Source::Npm, package),
        MiseAccess::Run,
    )
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
            &engine.sources,
            &MiseVias::new(),
            now,
        )
        .await
        .map(|(catalogs, evidence, _)| (catalogs, evidence))
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
    let eval = Evaluator::new(&config, Default::default()).unwrap();
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
    let first = patchula_update::identity::extracted_key(&dependencies[0]);
    let mapping = Controls {
        branches: BTreeMap::from([(
            "patchula/fixture-2.x".into(),
            BTreeSet::from([first.clone()]),
        )]),
        targets: BTreeMap::from([("target".into(), BTreeSet::from([first.clone()]))]),
    };
    engine
        .cache
        .set_state(&engine.credential_scope("demo/app"), &mapping)
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
                controls
                    .retry
                    .insert(patchula_update::identity::extracted_retry_key(
                        &dependencies[0],
                    ));
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
            dependencies: current.into_iter().map(Dep::new).collect(),
            controls,
            ..Work::empty(
                crate::repo_report("demo/app", Status::Complete, None),
                Some(config.clone()),
            )
        };
        let retried = engine.retried(&work).await.unwrap();
        assert!(retried[0]);
        assert_eq!(
            retried.iter().filter(|retried| **retried).count(),
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

/// A plan that fails keeps the saved plan-derived controls — saving an
/// empty set would un-retry every box the next run reads them for — and
/// still saves this run's held-back targets.
#[tokio::test]
async fn a_failed_plan_keeps_the_saved_controls() {
    let cache = memory().await;
    let engine = engine(cache, Arc::new(Registry::default()), "offline");
    let config = config();
    let dependencies = dependencies(&config);
    let first = patchula_update::identity::extracted_key(&dependencies[0]);
    let saved = Controls {
        branches: BTreeMap::from([(
            "patchula/fixture-2.x".into(),
            BTreeSet::from([first.clone()]),
        )]),
        targets: BTreeMap::new(),
    };
    engine
        .cache
        .set_state(&engine.credential_scope("demo/app"), &saved)
        .await
        .unwrap();
    // An update with no extraction commit: planning it fails.
    let mut report = crate::repo_report("demo/app", Status::Complete, None);
    report.package_files = serde_json::from_value(json!([{"path":"package.json","dependencies":[{
        "manager":"npm","name":"fixture","package":"fixture",
        "current":{"value":"1.0.0","version":"1.0.0","digest":null},
        "updates":[{"newValue":"2.0.0","newVersion":"2.0.0","newDigest":null,
            "updateType":"major","group":null}],
        "metadata":{"source":"npm","kinds":[],"locations":[],"stale":{},"heldBack":[{
            "version":"3.0.0","updateType":"major","publishedAt":"2026-10-15T00:00:00Z",
            "eligibleAt":"2026-10-16T00:00:00Z"}]}
    }]}]))
    .unwrap();
    let report_dep = report.package_files[0].dependencies[0].clone();
    let work = Work {
        dependencies: dependencies.into_iter().map(Dep::new).collect(),
        ..Work::empty(report, Some(config))
    };
    assert!(
        patchula_update::plan(
            &work.report,
            work.config.as_ref().unwrap(),
            &work
                .dependencies
                .iter()
                .map(|dep| dep.dependency.clone())
                .collect::<Vec<_>>(),
            &work.snapshot,
            false,
            None,
            now(),
        )
        .is_err()
    );
    engine.save_catalog_controls(&[work], now()).await.unwrap();
    let kept = engine
        .cache
        .state::<Controls>(&engine.credential_scope("demo/app"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.branches, saved.branches);
    let held = patchula_update::identity::held_target(
        &patchula_update::identity::dependency_key("package.json", &report_dep),
        "3.0.0",
        "major",
    );
    assert!(kept.targets.contains_key(&held), "{:?}", kept.targets);
}

#[test]
fn effective_rules_include_zero_age_exceptions() {
    let config = patchula_config::load_files("/repo/patchula.pkl",BTreeMap::from([("/repo/patchula.pkl".into(),"amends \"PatchulaConfig.pkl\"\ndefaults { waitHours = 72 }\nrules = new Listing { new Rule { select { packages = new Listing { \"fixture\" }; updates = new Listing { \"minor\" } }; apply { waitHours = 0 } } }\n".into())])).unwrap();
    let eval = Evaluator::new(&config, Default::default()).unwrap();
    for dep in dependencies(&config) {
        assert_eq!(
            eval.catalog_wait_hours(&dep),
            Ok(if dep.name == "fixture" { 0 } else { 72 })
        );
    }
}

/// A rule the evaluator cannot apply to a dependency reuses no catalog for
/// it, and its evaluation reports the rule error as the lookup failure.
#[tokio::test]
async fn a_rule_error_reuses_no_catalog_and_the_evaluation_reports_it() {
    let engine = engine(memory().await, Arc::new(Registry::default()), "offline");
    let config = patchula_config::load_files(
        "/repo/patchula.pkl",
        BTreeMap::from([(
            "/repo/patchula.pkl".into(),
            "amends \"PatchulaConfig.pkl\"\ndefaults { waitHours = 72 }\nrules = new Listing { new Rule { select { current = \"semver:>>1\" }; apply { waitHours = 72 } } }\n".into(),
        )]),
    )
    .unwrap();
    let dep = Scanner::new(&config)
        .unwrap()
        .extract(&BTreeMap::from([(
            "Cargo.toml".into(),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = \"1.39.0\"\n"
                .into(),
        )]))
        .unwrap()
        .dependencies
        .remove(0);
    let eval = Evaluator::new(&config, Default::default()).unwrap();
    assert!(eval.catalog_wait_hours(&dep).is_err());
    let work = Work {
        evaluator: Some(eval),
        ..Work::empty(
            crate::repo_report("demo/app", Status::Complete, None),
            Some(config.clone()),
        )
    };
    let needs = engine.needs(&work, &dep, false);
    assert!(needs.versions);
    assert_eq!(reuse_hours(needs.wait_hours), None);
    let catalog = Catalog {
        versions: Vec::new(),
        dist_tags: Default::default(),
    };
    let Err(error) = work
        .evaluator
        .as_ref()
        .unwrap()
        .evaluate_at(&dep, &catalog, now())
    else {
        panic!("the rule error is the evaluation's error");
    };
    assert!(error.contains(">>1"), "{error}");
}

/// Installation credentials whose repository ids a test can move.
#[derive(Default)]
struct Installation(std::sync::Mutex<BTreeMap<String, u64>>);
impl patchula_github::Credentials for Installation {
    fn scope(&self) -> &str {
        "installation"
    }
    fn token(&self) -> patchula_github::Token<'_> {
        Box::pin(async { Ok("installation-token".to_owned()) })
    }
    fn mints_scoped_tokens(&self) -> bool {
        true
    }
    fn scoped_token(
        &self,
        repository: &str,
        _: patchula_github::Permissions,
    ) -> Option<patchula_github::Token<'_>> {
        let token = format!("scoped-{repository}");
        Some(Box::pin(async move { Ok(token) }))
    }
    fn repository_id(&self, repository: &str) -> Option<patchula_github::RepositoryId<'_>> {
        let mut ids = self.0.lock().unwrap();
        let next = 100 + ids.len() as u64;
        let id = *ids.entry(repository.into()).or_insert(next);
        Some(Box::pin(async move { Ok(id) }))
    }
}

/// One package's views under `Run`, `Public` and a repository's own access
/// are separate entries: none is reused for another, each is reused for
/// itself, and a repository's view is not reused for a new repository
/// under its old name.
#[tokio::test]
async fn views_are_kept_per_access_and_a_repository_view_by_its_id() {
    let credentials = Arc::new(Installation::default());
    let engine = Engine::from_settings(
        &patchula_config::settings::Settings::default(),
        Vec::new(),
        Auth::Credentials(credentials.clone()),
        4,
        patchula_words::Recheck::NextRun,
        memory().await,
        Arc::new(Registry::default()),
    )
    .unwrap();
    let at = |access: MiseAccess| (false, PackageId::public(Source::Npm, "fixture"), access);
    let keys = [
        at(MiseAccess::Run),
        at(MiseAccess::Public),
        at(MiseAccess::Repository("acme/app".into())),
        at(MiseAccess::Repository("acme/api".into())),
    ];
    let read = |key: LookupKey| {
        let engine = &engine;
        async move {
            let (catalogs, evidence, _) = engine
                .age_catalogs(
                    BTreeSet::from([key.clone()]),
                    &BTreeMap::from([(key.clone(), 24)]),
                    &BTreeSet::new(),
                    &engine.sources,
                    &MiseVias::new(),
                    now(),
                )
                .await
                .unwrap();
            assert!(catalogs[&key].is_ok());
            evidence[&key].reused
        }
    };
    for key in &keys {
        assert!(!read(key.clone()).await, "{key:?} reused another's view");
    }
    for key in &keys {
        assert!(read(key.clone()).await, "{key:?} not reused");
    }
    credentials.0.lock().unwrap().insert("acme/app".into(), 2);
    assert!(!read(keys[2].clone()).await);
    assert!(read(keys[3].clone()).await);
}

/// The second fetch pass computes its vias from the work as it stands, so
/// a lockfile package promoted after the first pass is looked up for the
/// repository that asked for it.
#[test]
fn vias_follow_promoted_dependencies() {
    let mise = |package: &str, access: MiseAccess| {
        let dependency = Dependency {
            package: package.into(),
            ..patchula_manager::testing::dependency(
                patchula_words::Manager::Mise,
                DependencyKind::Tool,
                Source::Mise,
            )
        };
        let key = (false, dependency.id(), access);
        Dep {
            dependency,
            needs: Needs {
                catalog_key: Some(key),
                ..Needs::default()
            },
            evaluation: Evaluation::default(),
        }
    };
    let mut work = vec![
        Work {
            private: Some(true),
            ..Work::empty(repo_report("acme/app", Status::Complete, None), None)
        },
        Work {
            private: Some(false),
            ..Work::empty(repo_report("acme/site", Status::Complete, None), None)
        },
    ];
    work[1].dependencies.push(mise("jq", MiseAccess::Public));
    work[1].dependencies.push(mise("node", MiseAccess::Run));
    let first = crate::lookup::mise_vias(&work);
    assert_eq!(first.len(), 1);
    // Promoted after the first pass: acme/app now asks for jq too, and for
    // a tool of its own.
    work[0].dependencies.push(mise("jq", MiseAccess::Public));
    work[0].dependencies.push(mise(
        "go:example.com/tool",
        MiseAccess::Repository("acme/app".into()),
    ));
    let second = crate::lookup::mise_vias(&work);
    let names: Vec<_> = second.values().map(|via| via.name.as_str()).collect();
    assert_eq!(names, ["acme/app", "acme/app"]);
    assert_eq!(second.len(), 2);
}

/// A public key's token is minted for a repository whose own entry asked
/// for exactly that tool and options: options are part of the package, so
/// one repository's options never send another repository's token to the
/// host they name.
#[test]
fn a_public_listing_with_options_is_fetched_for_a_repository_that_wrote_them() {
    let mise = |package: &str| {
        let dependency = Dependency {
            package: package.into(),
            ..patchula_manager::testing::dependency(
                patchula_words::Manager::Mise,
                DependencyKind::Tool,
                Source::Mise,
            )
        };
        let key = (false, dependency.id(), MiseAccess::Public);
        Dep {
            dependency,
            needs: Needs {
                catalog_key: Some(key),
                ..Needs::default()
            },
            evaluation: Evaluation::default(),
        }
    };
    const PLAIN: &str = "github:acme/tool";
    const ELSEWHERE: &str = r#"github:acme/tool[api_url="https://ghe.example/api/v3"]"#;
    let repo = |name: &str| Work {
        private: Some(false),
        ..Work::empty(repo_report(name, Status::Complete, None), None)
    };
    let mut work = vec![repo("acme/first"), repo("evil/second")];
    work[0].dependencies.push(mise(PLAIN));
    work[1].dependencies.push(mise(ELSEWHERE));
    work[1].dependencies.push(mise(PLAIN));
    let vias = crate::lookup::mise_vias(&work);
    let via = |package: &str| {
        let key = (
            false,
            patchula_config::PackageId::public(Source::Mise, package),
            MiseAccess::Public,
        );
        vias[&key].name.clone()
    };
    assert_eq!(vias.len(), 2);
    assert_eq!(via(PLAIN), "acme/first");
    // Only evil/second wrote these options, so only its token can go there.
    assert_eq!(via(ELSEWHERE), "evil/second");
}
