use super::*;
use axum::{body::Body, http::Request};
use serde_json::Value;
use std::sync::Mutex as StdMutex;
use tower::ServiceExt;

/// GitHub as the API sees it: two users, two installations, one outsider repo.
struct GitHub;
impl Transport for GitHub {
    fn send(&self, request: reqwest::Request) -> patchula_http::Exchange<'_> {
        let path = request.url().path().to_owned();
        let auth = request
            .headers()
            .get(header::AUTHORIZATION)
            .map(|value| {
                assert!(value.is_sensitive());
                value.to_str().unwrap().to_owned()
            })
            .unwrap_or_default();
        let (status, body) = match path.as_str() {
            "/user" => match auth.as_str() {
                "Bearer ghu_writer" | "Bearer ghp_personal" => (200, json!({"login": "writer"})),
                "Bearer ghu_reader" => (200, json!({"login": "reader"})),
                "Bearer ghu_elsewhere" => (200, json!({"login": "writer"})),
                "Bearer ghu_limited" => (403, json!({})),
                _ => (401, json!({})),
            },
            // Only this App's user tokens can list its installations.
            "/user/installations" => match auth.as_str() {
                "Bearer ghu_writer" | "Bearer ghu_reader" => (
                    200,
                    json!({"total_count": 2, "installations": [{"id": 1}, {"id": 2}]}),
                ),
                "Bearer ghu_elsewhere" => {
                    (200, json!({"total_count": 1, "installations": [{"id": 2}]}))
                }
                _ => (403, json!({})),
            },
            "/repos/acme/app/installation" | "/repos/acme/web/installation" => {
                (200, json!({"id": 1}))
            }
            "/repos/other-org/app/installation" => (200, json!({"id": 2})),
            "/repos/stranger/app/installation" => (404, json!({})),
            "/app/installations/1/access_tokens" | "/app/installations/2/access_tokens" => (
                201,
                json!({"token": "ghs_installation", "expires_at": "2999-01-01T00:00:00Z"}),
            ),
            path if path.ends_with("/collaborators/writer/permission") => {
                (200, json!({"permission": "write"}))
            }
            path if path.ends_with("/collaborators/reader/permission") => {
                (200, json!({"permission": "read"}))
            }
            other => panic!("unexpected request {other}"),
        };
        let mut headers = reqwest::header::HeaderMap::new();
        if auth == "Bearer ghu_limited" {
            headers.insert("x-ratelimit-remaining", "0".parse().unwrap());
        }
        Box::pin(async move {
            Ok(patchula_http::Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers,
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    }
}

/// Installation, repositories, mode and login of one run.
type Call = (u64, Vec<String>, Mode, String);
#[derive(Default)]
struct Recorder {
    calls: StdMutex<Vec<Call>>,
    gate: Option<Arc<tokio::sync::Notify>>,
}
impl Runner for Recorder {
    fn run<'a>(
        &'a self,
        installation: u64,
        repositories: Vec<String>,
        mode: Mode,
        login: &'a str,
        _settings: &'a Settings,
    ) -> BoxFuture<'a, Result<Report, &'static str>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push((
                installation,
                repositories.clone(),
                mode,
                login.to_owned(),
            ));
            if let Some(gate) = &self.gate {
                gate.notified().await;
            }
            Ok(Report {
                schema_version: patchula_report::SCHEMA_VERSION,
                repositories: repositories
                    .into_iter()
                    .map(|repository| patchula_report::Repository {
                        repository,
                        status: patchula_report::Status::Complete,
                        reason: None,
                        package_files: Vec::new(),
                        warnings: Vec::new(),
                        skipped: Vec::new(),
                        notices: Vec::new(),
                        lookup_failures: Vec::new(),
                        default_branch: None,
                        commit: None,
                    })
                    .collect(),
                stats: None,
                actions: None,
            })
        })
    }
}

fn app() -> Arc<App> {
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
    static PEM: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let pem = PEM.get_or_init(|| {
        rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
            .unwrap()
            .to_pkcs1_pem(LineEnding::LF)
            .unwrap()
            .to_string()
    });
    App::new(7, pem.as_bytes(), Arc::new(GitHub)).unwrap()
}

async fn api(runner: Arc<Recorder>, client_id: Option<&str>, settings: &str) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.pkl");
    std::fs::write(&path, format!("amends \"PatchulaServer.pkl\"\n{settings}")).unwrap();
    routes(Api::new(Parts {
        app: app(),
        client_id: client_id.map(str::to_owned),
        bot: "patchula[bot]".into(),
        transport: Arc::new(GitHub),
        cache: Cache::open(":memory:", patchula_cache::Options::default())
            .await
            .unwrap(),
        settings: Reload::new(Settings::load(&path).unwrap()),
        accepting: Arc::new(AtomicBool::new(true)),
        runner,
    }))
}

async fn call(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}
fn start(token: &str, body: Value) -> Request<Body> {
    Request::post("/api/v1/runs")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
fn poll(token: &str, id: &str) -> Request<Body> {
    Request::get(format!("/api/v1/runs/{id}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn auth_publishes_the_client_id_only_when_known() {
    let router = api(Arc::new(Recorder::default()), Some("Iv23client"), "").await;
    let request = || Request::get("/api/v1/auth").body(Body::empty()).unwrap();
    assert_eq!(
        call(&router, request()).await,
        (
            StatusCode::OK,
            json!({"clientId": "Iv23client", "bot": "patchula[bot]"})
        )
    );
    let router = api(Arc::new(Recorder::default()), None, "").await;
    let (status, body) = call(&router, request()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["errorKind"], "no-client-id");
}

#[tokio::test]
async fn a_writer_runs_and_polls_their_own_job() {
    let runner = Arc::new(Recorder::default());
    let router = api(runner.clone(), Some("client"), "").await;
    let (status, body) = call(
        &router,
        start(
            "ghu_writer",
            json!({"mode": "dry-run", "repositories": ["Acme/App", "acme/web", "acme/app"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["id"].as_str().unwrap().to_owned();
    let report = loop {
        let (status, body) = call(&router, poll("ghu_writer", &id)).await;
        assert_eq!(status, StatusCode::OK);
        if body["status"] != "running" {
            assert_eq!(body["status"], "complete");
            break body["report"].clone();
        }
        tokio::task::yield_now().await;
    };
    assert_eq!(report["repositories"][0]["repository"], "acme/app");
    assert_eq!(
        *runner.calls.lock().unwrap(),
        [(
            1,
            vec!["acme/app".to_owned(), "acme/web".to_owned()],
            Mode::DryRun,
            "writer".to_owned()
        )]
    );
    // Another user cannot read the job.
    let (status, body) = call(&router, poll("ghu_reader", &id)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["errorKind"], "unknown-run");
}

#[tokio::test]
async fn rejections_name_the_reason() {
    let runner = Arc::new(Recorder::default());
    let router = api(
        runner.clone(),
        Some("client"),
        "allowedRepositories {\n  \"acme/app\"\n  \"other-org/app\"\n}\n",
    )
    .await;
    let run = |repos: Value| json!({"mode": "run", "repositories": repos});
    for (token, body, status, kind) in [
        ("ghu_nobody", run(json!(["acme/app"])), 401, "unauthorized"),
        // A personal token for a writer is not this App's sign-in.
        (
            "ghp_personal",
            run(json!(["acme/app"])),
            401,
            "unauthorized",
        ),
        // A rate limit keeps the token: it is not an authorization failure.
        (
            "ghu_limited",
            run(json!(["acme/app"])),
            503,
            "github-rate-limited",
        ),
        // This App's token, but it does not reach installation 1.
        (
            "ghu_elsewhere",
            run(json!(["acme/app"])),
            403,
            "not-installed",
        ),
        ("ghu_reader", run(json!(["acme/app"])), 403, "forbidden"),
        (
            "ghu_writer",
            run(json!(["stranger/app"])),
            403,
            "not-installed",
        ),
        ("ghu_writer", run(json!(["acme/web"])), 403, "not-installed"),
        (
            "ghu_writer",
            run(json!(["acme/app", "other-org/app"])),
            400,
            "mixed-installations",
        ),
        ("ghu_writer", run(json!([])), 400, "invalid-request"),
        ("ghu_writer", run(json!(["../x"])), 400, "invalid-request"),
        (
            "ghu_writer",
            json!({"mode": "everything", "repositories": ["acme/app"]}),
            400,
            "invalid-request",
        ),
    ] {
        let (actual, response) = call(&router, start(token, body.clone())).await;
        assert_eq!(actual.as_u16(), status, "{body} {response}");
        assert_eq!(response["errorKind"], kind, "{body}");
    }
    let unauthenticated = Request::post("/api/v1/runs")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(run(json!(["acme/app"])).to_string()))
        .unwrap();
    assert_eq!(
        call(&router, unauthenticated).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert!(runner.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn one_run_at_a_time_per_user() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let runner = Arc::new(Recorder {
        gate: Some(gate.clone()),
        ..Recorder::default()
    });
    let router = api(runner.clone(), Some("client"), "").await;
    let body = json!({"mode": "lookup", "repositories": ["acme/app"]});
    let (status, first) = call(&router, start("ghu_writer", body.clone())).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, second) = call(&router, start("ghu_writer", body.clone())).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(second["errorKind"], "job-running");
    while runner.calls.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    gate.notify_one();
    let id = first["id"].as_str().unwrap();
    while call(&router, poll("ghu_writer", id)).await.1["status"] == "running" {
        tokio::task::yield_now().await;
    }
    let (status, _) = call(&router, start("ghu_writer", body)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn drain_waits_for_runs_in_progress() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let runner = Arc::new(Recorder {
        gate: Some(gate.clone()),
        ..Recorder::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.pkl");
    std::fs::write(&path, "amends \"PatchulaServer.pkl\"\n").unwrap();
    let api = Api::new(Parts {
        app: app(),
        client_id: Some("client".into()),
        bot: "patchula[bot]".into(),
        transport: Arc::new(GitHub),
        cache: Cache::open(":memory:", patchula_cache::Options::default())
            .await
            .unwrap(),
        settings: Reload::new(Settings::load(&path).unwrap()),
        accepting: Arc::new(AtomicBool::new(true)),
        runner: runner.clone(),
    });
    let router = routes(api.clone());
    let body = json!({"mode": "run", "repositories": ["acme/app"]});
    assert_eq!(
        call(&router, start("ghu_writer", body)).await.0,
        StatusCode::ACCEPTED
    );
    while runner.calls.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    let drained = tokio::spawn({
        let api = api.clone();
        async move { api.drain().await }
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        !drained.is_finished(),
        "drain returned while a run was going"
    );
    gate.notify_one();
    drained.await.unwrap();
}
