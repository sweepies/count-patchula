use super::*;
use axum::{body::Body, http::Request};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;

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
    fn run<'a>(&'a self, _: &'a [Work], _: &'a Settings) -> BoxFuture<'a, Result<(), Error>> {
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
