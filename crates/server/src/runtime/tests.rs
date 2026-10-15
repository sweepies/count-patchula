use super::*;
use axum::{body::Body, http::Request};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);
static LOG_TEST_LOCK: Mutex<()> = Mutex::const_new(());
impl std::io::Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Logs {
    fn events(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["fields"].clone())
            .collect()
    }
    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync + 'static {
        let logs = self.clone();
        tracing_subscriber::fmt()
            .json()
            .with_writer(move || logs.clone())
            .finish()
    }
}

#[tokio::test]
async fn webhook_logs_one_decision_per_delivery_without_sensitive_data() {
    let _capture = LOG_TEST_LOCK.lock().await;
    use tracing::instrument::WithSubscriber;
    let logs = Logs::default();
    async {
        let state = state().await;
        let router = router(state.clone(), Arc::new(AtomicBool::new(true)));
        let mut value = payload();
        value["sensitive"] = json!("body-must-not-appear");
        for (id, outcome) in [
            ("delivery", "accepted/enqueued"),
            ("delivery", "duplicate delivery ignored"),
        ] {
            router
                .clone()
                .oneshot(request(serde_json::to_vec(&value).unwrap(), "push", id))
                .await
                .unwrap();
            let event = logs.events().pop().unwrap();
            assert_eq!(event["outcome"], outcome);
            assert_eq!(event["event"], "push");
            assert_eq!(event["delivery_id"], id);
            assert_eq!(event["repository"], "owner/repo");
            assert_eq!(event["installation"], 1);
        }
        value["sender"]["login"] = json!("rustovate[bot]");
        router
            .clone()
            .oneshot(request(serde_json::to_vec(&value).unwrap(), "push", "own"))
            .await
            .unwrap();
        assert_eq!(logs.events().last().unwrap()["reason"], "own-bot ignored");
        value["sender"]["login"] = json!("human");
        let mut settings = state.settings.current();
        settings.triggers.push = false;
        let disabled = router_for_settings(&state, settings.clone());
        disabled
            .oneshot(request(
                serde_json::to_vec(&value).unwrap(),
                "push",
                "disabled",
            ))
            .await
            .unwrap();
        assert_eq!(logs.events().last().unwrap()["reason"], "trigger disabled");
        settings.triggers.push = true;
        settings.allowed_repositories = vec!["other/repo".into()];
        router_for_settings(&state, settings)
            .oneshot(request(
                serde_json::to_vec(&value).unwrap(),
                "push",
                "disallowed",
            ))
            .await
            .unwrap();
        assert_eq!(logs.events().last().unwrap()["reason"], "not allowlisted");
        let mut bad = request(b"secret-body".to_vec(), "secret-event", "secret-delivery");
        bad.headers_mut()
            .insert("x-hub-signature-256", "secret-signature".parse().unwrap());
        router.clone().oneshot(bad).await.unwrap();
        assert_eq!(
            router
                .oneshot(request(vec![b'a'; BODY_LIMIT + 1], "push", "large"))
                .await
                .unwrap()
                .status(),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
    .with_subscriber(logs.subscriber())
    .await;
    let events = logs.events();
    assert_eq!(events.len(), 7);
    assert_eq!(
        events[5],
        json!({"message":"webhook received", "outcome":"bad signature"})
    );
    assert_eq!(
        events[6],
        json!({"message":"webhook received", "outcome":"body rejected", "status":413})
    );
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    for forbidden in [
        "secret-body",
        "secret-event",
        "secret-delivery",
        "secret-signature",
        "body-must-not-appear",
        "test-only-secret",
    ] {
        assert!(!text.contains(forbidden), "{text}");
    }
}
fn router_for_settings(state: &Webhook, settings: Settings) -> Router {
    let mut state = state.clone();
    state.settings = Reload::new(settings);
    router(state, Arc::new(AtomicBool::new(true)))
}

struct InstallationLookup {
    requests: std::sync::atomic::AtomicUsize,
    status: std::sync::atomic::AtomicU16,
}
impl Transport for InstallationLookup {
    fn send(&self, request: reqwest::Request) -> rustovate_http::Exchange<'_> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.method(), reqwest::Method::GET);
        assert!(matches!(
            request.url().path(),
            "/repos/owner/repo/installation" | "/orgs/owner/installation"
        ));
        let authorization = request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .unwrap();
        assert!(authorization.is_sensitive());
        assert_eq!(
            authorization
                .to_str()
                .unwrap()
                .strip_prefix("Bearer ")
                .unwrap()
                .split('.')
                .count(),
            3
        );
        let status = reqwest::StatusCode::from_u16(self.status.load(Ordering::SeqCst)).unwrap();
        Box::pin(async move {
            Ok(rustovate_http::Response {
                status,
                headers: Default::default(),
                body: serde_json::to_vec(&json!({"id":1})).unwrap(),
            })
        })
    }
}
fn lookup_app(transport: Arc<dyn Transport>, id: u64) -> Arc<App> {
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
    static PEM: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let pem = PEM.get_or_init(|| {
        rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
            .unwrap()
            .to_pkcs1_pem(LineEnding::LF)
            .unwrap()
            .to_string()
    });
    App::new(id, pem.as_bytes(), transport).unwrap()
}

#[tokio::test]
async fn repository_and_org_webhooks_resolve_installations_and_ping_does_no_work() {
    use tracing::instrument::WithSubscriber;
    let _capture = LOG_TEST_LOCK.lock().await;
    let logs = Logs::default();
    let transport = Arc::new(InstallationLookup {
        requests: 0.into(),
        status: 200.into(),
    });
    let app = lookup_app(transport.clone(), 1);
    let state = state().await.with_app(app.clone());
    let router = router(state.clone(), Arc::new(AtomicBool::new(true)));
    let mut value = payload();
    value.as_object_mut().unwrap().remove("installation");
    // Ping is verified but requires neither an installation lookup nor queue rows.
    let response = router
        .clone()
        .oneshot(request(serde_json::to_vec(&value).unwrap(), "ping", "ping"))
        .with_subscriber(logs.subscriber())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(transport.requests.load(Ordering::SeqCst), 0);
    assert_eq!(state.cache.queue_len().await.unwrap(), 0);
    let mut bad_ping = request(b"{}".to_vec(), "ping", "bad-ping");
    bad_ping.headers_mut().remove("x-hub-signature-256");
    assert_eq!(
        router.clone().oneshot(bad_ping).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(transport.requests.load(Ordering::SeqCst), 0);
    let response = router
        .clone()
        .oneshot(request(
            serde_json::to_vec(&value).unwrap(),
            "push",
            "resolved",
        ))
        .with_subscriber(logs.subscriber())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
    let claims = state.cache.claim_batch(i64::MAX, 10).await.unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].work, Work::repository(1, "owner/repo"));
    state.cache.finish_work(claims, true, 0, 0).await.unwrap();
    // Apply installation and repository allowlists after resolution, including cache hits.
    let mut denied = state.settings.current();
    denied.allowed_installations = vec![2];
    router_for_settings(&state, denied)
        .oneshot(request(
            serde_json::to_vec(&value).unwrap(),
            "push",
            "denied-installation",
        ))
        .with_subscriber(logs.subscriber())
        .await
        .unwrap();
    assert_eq!(state.cache.queue_len().await.unwrap(), 0);
    assert_eq!(logs.events().last().unwrap()["reason"], "not allowlisted");
    assert_eq!(
        logs.events().last().unwrap()["installation_resolution"],
        "installation-resolved"
    );
    let mut denied = state.settings.current();
    denied.allowed_repositories = vec!["other/repo".into()];
    router_for_settings(&state, denied)
        .oneshot(request(
            serde_json::to_vec(&value).unwrap(),
            "push",
            "denied-repository",
        ))
        .await
        .unwrap();
    assert_eq!(state.cache.queue_len().await.unwrap(), 0);
    assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
    let org = json!({"organization":{"login":"owner"}, "action":"added", "repositories_added":[{"full_name":"owner/repo"}]});
    assert_eq!(
        router
            .clone()
            .oneshot(request(
                serde_json::to_vec(&org).unwrap(),
                "installation_repositories",
                "org"
            ))
            .with_subscriber(logs.subscriber())
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
    let claims = state.cache.claim_batch(i64::MAX, 10).await.unwrap();
    assert_eq!(claims[0].work, Work::repository(1, "owner/repo"));
    state.cache.finish_work(claims, true, 0, 0).await.unwrap();
    // A new resolver with an empty cache gets a 404: accept/ignore, no org fallback.
    let unknown = self::state().await.with_app(app.clone());
    transport.status.store(404, Ordering::SeqCst);
    value["organization"] = json!({"login":"owner"});
    let unknown_router = super::router(unknown.clone(), Arc::new(AtomicBool::new(true)));
    assert_eq!(
        unknown_router
            .oneshot(request(
                serde_json::to_vec(&value).unwrap(),
                "push",
                "unknown"
            ))
            .with_subscriber(logs.subscriber())
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(unknown.cache.queue_len().await.unwrap(), 0);
    assert_eq!(transport.requests.load(Ordering::SeqCst), 3);
    assert_eq!(
        logs.events().last().unwrap()["reason"],
        "installation-not-found"
    );
    // Transient failures remain retryable, and do not consume the delivery ID.
    transport.status.store(500, Ordering::SeqCst);
    let unavailable = self::state().await.with_app(app);
    let unavailable_router = super::router(unavailable.clone(), Arc::new(AtomicBool::new(true)));
    let body = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        unavailable_router
            .clone()
            .oneshot(request(body.clone(), "push", "retry"))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    transport.status.store(200, Ordering::SeqCst);
    assert_eq!(
        unavailable_router
            .oneshot(request(body, "push", "retry"))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(unavailable.cache.queue_len().await.unwrap(), 1);
    let decisions = logs.events();
    assert_eq!(decisions.len(), 5);
    assert_eq!(decisions[1]["reason"], "installation-resolved");
    assert_eq!(decisions[1]["installation"], 1);
}

#[tokio::test]
async fn installation_cache_uses_memory_disk_ttl_and_invalidates_on_not_found() {
    use crate::installations::{Resolver, TTL_SECONDS};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.sqlite");
    let cache = Cache::open(&path, Default::default()).await.unwrap();
    let transport = Arc::new(InstallationLookup {
        requests: 0.into(),
        status: 200.into(),
    });
    let app = lookup_app(transport.clone(), 1);
    let value = json!({"repository":{"full_name":"OWNER/Repo"}});
    let resolver = Resolver::new(app.clone(), cache.clone());
    assert_eq!(resolver.resolve_at(&value, 100).await.unwrap(), Some(1));
    cache
        .installation_remove(1, "repos/owner/repo".into())
        .await
        .unwrap();
    assert_eq!(resolver.resolve_at(&value, 101).await.unwrap(), Some(1));
    assert_eq!(transport.requests.load(Ordering::SeqCst), 1);
    assert_eq!(
        resolver
            .resolve_at(&value, 100 + TTL_SECONDS)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
    drop(resolver);
    drop(cache);
    let cache = Cache::open(&path, Default::default()).await.unwrap();
    let resolver = Resolver::new(app.clone(), cache.clone());
    assert_eq!(
        resolver
            .resolve_at(&value, 101 + TTL_SECONDS)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(transport.requests.load(Ordering::SeqCst), 2);
    // TTL is not extended by a disk or memory hit. A 404 removes the SQLite row too.
    transport.status.store(404, Ordering::SeqCst);
    assert_eq!(
        resolver
            .resolve_at(&value, 100 + 2 * TTL_SECONDS)
            .await
            .unwrap(),
        None
    );
    assert_eq!(transport.requests.load(Ordering::SeqCst), 3);
    assert!(
        cache
            .installation_get(1, "repos/owner/repo".into(), 0)
            .await
            .unwrap()
            .is_none()
    );
    // Do not negative-cache a 404: a new installation can take effect immediately.
    transport.status.store(200, Ordering::SeqCst);
    assert_eq!(
        resolver
            .resolve_at(&value, 101 + 2 * TTL_SECONDS)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(transport.requests.load(Ordering::SeqCst), 4);
    // Another App must resolve its own installation, even when sharing SQLite.
    let other = Resolver::new(lookup_app(transport.clone(), 2), cache);
    assert_eq!(
        other
            .resolve_at(&value, 101 + 2 * TTL_SECONDS)
            .await
            .unwrap(),
        Some(1)
    );
    assert_eq!(transport.requests.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn discovery_skip_is_explicit_and_each_repository_report_gets_a_file() {
    let _capture = LOG_TEST_LOCK.lock().await;
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
    use tracing::instrument::WithSubscriber;
    let logs = Logs::default();
    let directory = tempfile::tempdir().unwrap();
    let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let pem = key.to_pkcs1_pem(LineEnding::LF).unwrap();
    let transport: Arc<dyn Transport> = Arc::new(DiscoveryAndMissingRepository);
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let runner = Engines {
        app: App::new(1, pem.as_bytes(), transport.clone()).unwrap(),
        transport,
        cache: cache.clone(),
        engines: Mutex::new(BTreeMap::new()),
        recheck: None,
        bot_login: "rustovate[bot]".into(),
    };
    let mut settings = settings();
    settings.report_directory = Some(directory.path().to_owned());
    let attribution = Attribution::reason("startup discovery");
    runner
        .run(&[Work::repository(1, "")], &settings, &attribution)
        .with_subscriber(logs.subscriber())
        .await
        .unwrap();
    let events = logs.events();
    let skipped = events
        .iter()
        .find(|event| event["message"] == "batch report skipped")
        .unwrap();
    assert_eq!(skipped["discovery_only"], true);
    assert_eq!(skipped["repository_runs"], 0);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    let claims = cache.claim_batch(i64::MAX, 10).await.unwrap();
    assert_eq!(claims.len(), 1);
    assert!(claims[0].attribution.reasons.contains("startup discovery"));
    for _ in 0..2 {
        assert!(
            runner
                .run(&[claims[0].work.clone()], &settings, &claims[0].attribution)
                .await
                .is_err()
        );
    }
    let paths = std::fs::read_dir(directory.path())
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(paths.len(), 2);
    for path in paths {
        let actual: Report = serde_json::from_slice(&std::fs::read(path.path()).unwrap()).unwrap();
        assert_eq!(actual.repositories.len(), 1);
        assert_eq!(actual.repositories[0].repository, "owner/repo");
        assert_eq!(actual.repositories[0].status, Status::Error);
    }
}

struct DiscoveryAndMissingRepository;
impl Transport for DiscoveryAndMissingRepository {
    fn send(&self, request: reqwest::Request) -> rustovate_http::Exchange<'_> {
        let (status, value) = match request.url().path() {
            "/app/installations/1/access_tokens" => (
                reqwest::StatusCode::CREATED,
                json!({"token":"offline-test-token", "expires_at":(Utc::now() + chrono::Duration::hours(1)).to_rfc3339()}),
            ),
            "/installation/repositories" => (
                reqwest::StatusCode::OK,
                json!({"repositories":[{"full_name":"owner/repo"}]}),
            ),
            "/graphql" => (
                reqwest::StatusCode::NOT_FOUND,
                json!({"message":"Not Found"}),
            ),
            path if path.starts_with("/repos/owner/repo") => (
                reqwest::StatusCode::NOT_FOUND,
                json!({"message":"Not Found"}),
            ),
            path => panic!("unexpected offline request: {path}"),
        };
        Box::pin(async move {
            Ok(rustovate_http::Response {
                status,
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
            })
        })
    }
}

fn settings() -> Settings {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.pkl");
    std::fs::write(&path, "amends \"RustovateServer.pkl\"\ncoalescingMs = 0\n").unwrap();
    Settings::load(&path).unwrap()
}
async fn state() -> Webhook {
    Webhook::new(
        Cache::open(":memory:", Default::default()).await.unwrap(),
        Reload::new(settings()),
        "rustovate[bot]".into(),
        b"test-only-secret".to_vec(),
    )
}
fn payload() -> Value {
    json!({"installation":{"id":1},"sender":{"login":"human"},"repository":{"id":100,"full_name":"owner/repo","default_branch":"main"},"ref":"refs/heads/main"})
}
fn pr() -> Value {
    json!({"number":1,"body":format!("<!-- rustovate:{} -->\n- [x] any wording <!-- rustovate-recheck -->", json!({"version":1,"identity":"a".repeat(64),"update":"b".repeat(64)})),"user":{"login":"rustovate[bot]"},"head":{"ref":"custom-deps/pkg-2.x","repo":{"full_name":"owner/repo"}},"base":{"repo":{"full_name":"owner/repo"}}})
}
fn signature(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"test-only-secret").unwrap();
    mac.update(body);
    format!(
        "sha256={}",
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn request(body: Vec<u8>, event: &str, id: &str) -> Request<Body> {
    Request::post("/webhook")
        .header("x-hub-signature-256", signature(&body))
        .header("x-github-event", event)
        .header("x-github-delivery", id)
        .body(Body::from(body))
        .unwrap()
}
#[tokio::test]
async fn signature_precedes_json_parse_and_duplicate_delivery_never_enqueues_twice() {
    let state = state().await;
    let router = router(state.clone(), Arc::new(AtomicBool::new(true)));
    let mut bad = request(b"not JSON".to_vec(), "push", "bad");
    bad.headers_mut().insert(
        "x-hub-signature-256",
        "sha256=0000000000000000000000000000000000000000000000000000000000000000"
            .parse()
            .unwrap(),
    );
    assert_eq!(
        router.clone().oneshot(bad).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(state.cache.queue_len().await.unwrap(), 0);
    assert_eq!(
        router
            .clone()
            .oneshot(request(b"not JSON".to_vec(), "push", "bad"))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    let body = serde_json::to_vec(&payload()).unwrap();
    assert_eq!(
        router
            .clone()
            .oneshot(request(body.clone(), "push", "delivery"))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    let claims = state.cache.claim_batch(i64::MAX, 10).await.unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(
        router
            .clone()
            .oneshot(request(body, "push", "delivery"))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    state.cache.finish_work(claims, true, 0, 0).await.unwrap();
    assert_eq!(state.cache.queue_len().await.unwrap(), 0);
    let large = request(vec![b'a'; BODY_LIMIT + 1], "push", "large");
    assert_eq!(
        router.clone().oneshot(large).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let mut own = payload();
    own["sender"]["login"] = json!("rustovate[bot]");
    assert_eq!(
        router
            .oneshot(request(serde_json::to_vec(&own).unwrap(), "push", "own"))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(state.cache.queue_len().await.unwrap(), 0);
}
#[test]
fn every_trigger_and_switch_and_filters() {
    let settings = settings();
    let mut events = Vec::new();
    events.push(("push", payload(), "push"));
    let mut dashboard = payload();
    dashboard["action"] = json!("edited");
    let checkbox = format!(
        "- [x] Any text <!-- rustovate-action:{} -->",
        json!({"action":"recheck","target":"a".repeat(64)})
    );
    dashboard["issue"] = json!({"user":{"login":"rustovate[bot]"},"body":format!("{}\n{checkbox}", rustovate_update::dashboard::MARKER)});
    dashboard["changes"] =
        json!({"body":{"from":dashboard["issue"]["body"].as_str().unwrap().replace("[x]", "[ ]")}});
    events.push(("issues", dashboard, "dashboard"));
    let mut edited = payload();
    edited["action"] = json!("edited");
    edited["pull_request"] = pr();
    edited["changes"] =
        json!({"body":{"from":pr()["body"].as_str().unwrap().replace("[x]", "[ ]")}});
    events.push(("pull_request", edited.clone(), "pullRequestEdited"));
    edited["action"] = json!("closed");
    events.push(("pull_request", edited, "pullRequestClosed"));
    let mut install = payload();
    install["action"] = json!("created");
    install["repositories"] = json!([{"full_name":"owner/repo"}]);
    events.push(("installation", install.clone(), "installation"));
    install["action"] = json!("added");
    install["repositories_added"] = install["repositories"].clone();
    events.push(("installation_repositories", install, "installation"));
    for event in ["check_suite", "check_run"] {
        let mut checks = payload();
        checks["action"] = json!("completed");
        checks[event] = json!({"pull_requests":[{"number":1,"head":{"ref":"custom-deps/pkg","repo":{"id":100}},"base":{"repo":{"id":100}}}]});
        events.push((event, checks, "checks"));
    }
    for (event, payload, switch) in events {
        let work = crate::webhook::classify(event, &payload, &settings, "rustovate[bot]");
        assert_eq!(
            work,
            vec![Work {
                installation: 1,
                repo: "owner/repo".into(),
                check_prs: (switch == "checks").then(|| std::collections::BTreeSet::from([1])),
            }],
            "{event}"
        );
        let mut value = serde_json::to_value(&settings).unwrap();
        value["triggers"][switch] = json!(false);
        let disabled: Settings = serde_json::from_value(value).unwrap();
        assert!(
            crate::webhook::classify(event, &payload, &disabled, "rustovate[bot]").is_empty(),
            "{event} disabled"
        );
        let mut filtered = settings.clone();
        filtered.allowed_installations = vec![2];
        assert!(crate::webhook::classify(event, &payload, &filtered, "rustovate[bot]").is_empty());
        filtered.allowed_installations.clear();
        filtered.allowed_repositories = vec!["other/repo".into()];
        assert!(crate::webhook::classify(event, &payload, &filtered, "rustovate[bot]").is_empty());
        let mut own = payload.clone();
        own["sender"]["login"] = json!("rustovate[bot]");
        assert!(crate::webhook::classify(event, &own, &settings, "rustovate[bot]").is_empty());
    }
    let mut feature = payload();
    feature["ref"] = json!("refs/heads/feature");
    assert!(crate::webhook::classify("push", &feature, &settings, "rustovate[bot]").is_empty());
    let mut human_pr = payload();
    human_pr["action"] = json!("closed");
    human_pr["pull_request"] = pr();
    human_pr["pull_request"]["user"]["login"] = json!("human");
    assert!(
        crate::webhook::classify("pull_request", &human_pr, &settings, "rustovate[bot]").is_empty()
    );
}
#[tokio::test]
async fn only_api_endpoints_and_readiness_tracks_settings_and_auth() {
    let state = state().await;
    let ready = Arc::new(AtomicBool::new(false));
    let router = router(state.clone(), ready.clone());
    for (path, expected) in [
        ("/healthz", 200),
        ("/readyz", 503),
        ("/", 404),
        ("/settings", 404),
    ] {
        assert_eq!(
            router
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
                .as_u16(),
            expected
        );
    }
    ready.store(true, Ordering::Release);
    assert_eq!(
        router
            .clone()
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    state
        .settings
        .refresh(std::path::Path::new("/nonexistent/server.pkl"));
    assert_eq!(
        router
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
struct BlockingBatch {
    started: tokio::sync::Notify,
    finish: tokio::sync::Notify,
}
impl Batch for BlockingBatch {
    fn run<'a>(
        &'a self,
        _: &'a [Work],
        _: &'a Settings,
        _: &'a Attribution,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.started.notify_one();
            self.finish.notified().await;
            Ok(())
        })
    }
}
#[tokio::test]
async fn shutdown_finishes_batch_or_requeues_at_deadline() {
    for finish in [false, true] {
        let state = state().await;
        state
            .cache
            .enqueue(None, vec![Work::repository(1, "owner/repo")], 0, 0)
            .await
            .unwrap();
        let runner = Arc::new(BlockingBatch {
            started: tokio::sync::Notify::new(),
            finish: tokio::sync::Notify::new(),
        });
        let (stop, stopped) = watch::channel(false);
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = tokio::spawn(run_until(
            listener,
            state.clone(),
            Arc::new(AtomicBool::new(true)),
            runner.clone(),
            async {
                let _ = shutdown_rx.await;
            },
            Duration::from_millis(30),
            stop,
            stopped,
        ));
        runner.started.notified().await;
        shutdown.send(()).unwrap();
        if finish {
            runner.finish.notify_one();
        }
        server.await.unwrap().unwrap();
        assert!(!state.accepting.load(Ordering::Acquire));
        assert_eq!(state.cache.queue_len().await.unwrap(), usize::from(!finish));
        if !finish {
            assert_eq!(state.cache.claim_batch(i64::MAX, 1).await.unwrap().len(), 1);
        }
    }
}
#[cfg(unix)]
#[tokio::test]
async fn sigterm_during_batch_retains_unfinished_work() {
    let mut term =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let state = state().await;
    state
        .cache
        .enqueue(None, vec![Work::repository(1, "owner/repo")], 0, 0)
        .await
        .unwrap();
    let runner = Arc::new(BlockingBatch {
        started: tokio::sync::Notify::new(),
        finish: tokio::sync::Notify::new(),
    });
    let (stop, stopped) = watch::channel(false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = tokio::spawn(run_until(
        listener,
        state.clone(),
        Arc::new(AtomicBool::new(true)),
        runner.clone(),
        async move {
            term.recv().await;
        },
        Duration::from_millis(10),
        stop,
        stopped,
    ));
    runner.started.notified().await;
    nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::Signal::SIGTERM).unwrap();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!state.accepting.load(Ordering::Acquire));
    assert_eq!(state.cache.claim_batch(i64::MAX, 1).await.unwrap().len(), 1);
}
#[test]
fn cron_obeys_timezone_and_can_be_disabled() {
    let mut settings = settings();
    settings.sweep_cron = Some("0 0 9 * * *".into());
    settings.sweep_timezone = "America/New_York".into();
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-15T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        sweep_after(&settings, now).unwrap().to_rfc3339(),
        "2026-10-15T13:00:00+00:00"
    );
    settings.sweep_cron = None;
    assert!(sweep_after(&settings, now).is_none());
}
#[tokio::test]
async fn report_directory_preserves_versioned_contract() {
    let dir = tempfile::tempdir().unwrap();
    let report = Report {
        schema_version: SCHEMA_VERSION,
        repositories: Vec::new(),
        stats: None,
        actions: None,
    };
    output_report(&report, Some(dir.path())).await.unwrap();
    let path = std::fs::read_dir(dir.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let decoded: Report = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(decoded, report);
}

struct PrDetails(Value);
impl Transport for PrDetails {
    fn send(&self, request: reqwest::Request) -> rustovate_http::Exchange<'_> {
        assert_eq!(request.url().path(), "/repos/owner/repo/pulls/1");
        let body = serde_json::to_vec(&self.0).unwrap();
        Box::pin(async move {
            Ok(rustovate_http::Response {
                status: reqwest::StatusCode::OK,
                headers: Default::default(),
                body,
            })
        })
    }
}
#[tokio::test]
async fn check_events_verify_real_ownership_with_custom_branch_prefixes() {
    let mut cases = vec![(pr(), true)];
    let mut human = pr();
    human["user"]["login"] = json!("human");
    cases.push((human, false));
    let mut unmarked = pr();
    unmarked["body"] = json!("ordinary PR");
    cases.push((unmarked, false));
    let mut fork = pr();
    fork["head"]["repo"]["full_name"] = json!("other/repo");
    cases.push((fork, false));
    for (pr, expected) in cases {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let client = Client::with_transport(
            "offline-token",
            cache,
            Default::default(),
            Arc::new(PrDetails(pr)),
        )
        .unwrap();
        let candidate = Work {
            installation: 1,
            repo: "owner/repo".into(),
            check_prs: Some(std::collections::BTreeSet::from([1])),
        };
        assert_eq!(
            check_candidate(&client, &candidate, "rustovate[bot]")
                .await
                .unwrap(),
            expected
        );
    }
}
