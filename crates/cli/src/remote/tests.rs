use super::*;
use crate::{App, run};
use patchula_http::{Exchange, Response};
use std::sync::{Arc, Mutex};

const SERVER: &str = "https://patchula.acme.dev";

/// The server and GitHub's login endpoints. `revoked` tokens get a 401 from the server.
#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<(String, String, String, String)>>,
    pending: Mutex<u32>,
    polls: Mutex<u32>,
    revoked: Vec<&'static str>,
}
impl Transport for Fake {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let url = request.url().to_string();
        let auth = request
            .headers()
            .get(header::AUTHORIZATION)
            .map(|value| {
                assert!(value.is_sensitive());
                value.to_str().unwrap().to_owned()
            })
            .unwrap_or_default();
        let body = request
            .body()
            .and_then(|b| b.as_bytes())
            .map(|b| String::from_utf8(b.to_vec()).unwrap())
            .unwrap_or_default();
        self.requests.lock().unwrap().push((
            request.method().to_string(),
            url.clone(),
            auth.clone(),
            body.clone(),
        ));
        assert!(!url.contains("ghr_"), "a refresh token reached a URL");
        let (status, value) = match (request.method().as_str(), url.as_str()) {
            ("GET", "https://patchula.acme.dev/api/v1/auth") => {
                (200, json!({"clientId": "Iv23client"}))
            }
            ("POST", DEVICE_CODE_URL) => {
                assert_eq!(body, "client_id=Iv23client");
                (
                    200,
                    json!({"device_code": "dev", "user_code": "WDJB-MJHT",
                        "verification_uri": "https://github.com/login/device",
                        "expires_in": 900, "interval": 5}),
                )
            }
            ("POST", ACCESS_TOKEN_URL) if body.contains("grant_type=refresh_token") => {
                assert!(body.contains("refresh_token=ghr_old"));
                (
                    200,
                    json!({"access_token": "ghu_refreshed", "expires_in": 28800,
                        "refresh_token": "ghr_new", "refresh_token_expires_in": 15897600}),
                )
            }
            ("POST", ACCESS_TOKEN_URL) => {
                assert!(body.contains("device_code=dev"));
                let mut pending = self.pending.lock().unwrap();
                if *pending > 0 {
                    *pending -= 1;
                    (200, json!({"error": "authorization_pending"}))
                } else {
                    (
                        200,
                        json!({"access_token": "ghu_device", "expires_in": 28800,
                            "refresh_token": "ghr_device", "refresh_token_expires_in": 15897600}),
                    )
                }
            }
            ("POST", "https://patchula.acme.dev/api/v1/runs") => {
                if self
                    .revoked
                    .iter()
                    .any(|token| auth == format!("Bearer {token}"))
                {
                    (
                        401,
                        json!({"errorKind": "unauthorized", "error": "bad token"}),
                    )
                } else {
                    let body: Value = serde_json::from_str(&body).unwrap();
                    assert_eq!(body["repositories"], json!(["acme/app"]));
                    (202, json!({"id": "job-1"}))
                }
            }
            ("GET", "https://patchula.acme.dev/api/v1/runs/job-1") => {
                let mut polls = self.polls.lock().unwrap();
                *polls += 1;
                if *polls < 2 {
                    (200, json!({"status": "running"}))
                } else {
                    (
                        200,
                        json!({"status": "complete", "report": {
                            "schemaVersion": patchula_report::SCHEMA_VERSION,
                            "repositories": [], "stats": null, "actions": null}}),
                    )
                }
            }
            other => panic!("unexpected request {other:?}"),
        };
        Box::pin(async move {
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: Default::default(),
                body: serde_json::to_vec(&value).unwrap(),
            })
        })
    }
}

fn services(fake: Arc<Fake>, dir: &std::path::Path) -> Services {
    Services {
        transport: Some(fake),
        config_dir: Some(dir.to_owned()),
        ..Default::default()
    }
}
fn stored(dir: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("servers.json")).unwrap()).unwrap()
}
fn seed(dir: &std::path::Path, login: Value) {
    std::fs::write(
        dir.join("servers.json"),
        json!({ SERVER: login }).to_string(),
    )
    .unwrap();
}
fn app(args: &[&str]) -> App {
    App::parse_from(&args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>()).unwrap()
}
fn methods(fake: &Fake) -> Vec<String> {
    fake.requests
        .lock()
        .unwrap()
        .iter()
        .map(|(method, url, ..)| format!("{method} {url}"))
        .collect()
}

#[test]
fn server_urls_are_https_or_localhost() {
    assert_eq!(
        server(Some("https://patchula.acme.dev/"))
            .unwrap()
            .as_deref(),
        Some(SERVER)
    );
    assert_eq!(
        server(Some("http://localhost:8080")).unwrap().as_deref(),
        Some("http://localhost:8080")
    );
    for bad in [
        "http://patchula.acme.dev",
        "https://patchula.acme.dev/?x=1",
        "https://user@patchula.acme.dev",
        "patchula.acme.dev",
    ] {
        assert_eq!(server(Some(bad)).unwrap_err().0, 2, "{bad}");
    }
}

#[tokio::test(start_paused = true)]
async fn first_run_signs_in_with_the_device_flow_and_stores_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake {
        pending: Mutex::new(1),
        ..Default::default()
    });
    let services = services(fake.clone(), dir.path());
    let app = app(&["run", "--dry-run", "--server", SERVER, "acme/app", "--json"]);
    assert_eq!(run(&app, &services).await.unwrap(), 0);
    let requests = fake.requests.lock().unwrap().clone();
    let start = requests
        .iter()
        .find(|(method, url, ..)| method == "POST" && url.ends_with("/api/v1/runs"))
        .unwrap();
    assert_eq!(start.2, "Bearer ghu_device");
    assert_eq!(
        serde_json::from_str::<Value>(&start.3).unwrap()["mode"],
        "dry-run"
    );
    let login = &stored(dir.path())[SERVER];
    assert_eq!(login["accessToken"], "ghu_device");
    assert_eq!(login["refreshToken"], "ghr_device");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("servers.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The stored token is reused: no second sign-in.
    fake.requests.lock().unwrap().clear();
    *fake.polls.lock().unwrap() = 0;
    assert_eq!(run(&app, &services).await.unwrap(), 0);
    assert!(
        !methods(&fake)
            .iter()
            .any(|r| r.contains("github.com/login")),
        "{:?}",
        methods(&fake)
    );
}

#[tokio::test(start_paused = true)]
async fn an_expired_token_is_refreshed_without_a_new_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        json!({"accessToken": "ghu_old", "expiresAt": 1, "refreshToken": "ghr_old",
            "refreshExpiresAt": 4_000_000_000_i64}),
    );
    let fake = Arc::new(Fake::default());
    let services = services(fake.clone(), dir.path());
    assert_eq!(
        run(&app(&["lookup", "--server", SERVER, "acme/app"]), &services)
            .await
            .unwrap(),
        0
    );
    assert!(!methods(&fake).iter().any(|r| r.contains("device/code")));
    assert_eq!(stored(dir.path())[SERVER]["accessToken"], "ghu_refreshed");
    assert_eq!(stored(dir.path())[SERVER]["refreshToken"], "ghr_new");
}

#[tokio::test(start_paused = true)]
async fn a_revoked_token_signs_in_again_once() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), json!({"accessToken": "ghu_revoked"}));
    let fake = Arc::new(Fake {
        revoked: vec!["ghu_revoked"],
        ..Default::default()
    });
    let services = services(fake.clone(), dir.path());
    assert_eq!(
        run(&app(&["run", "--server", SERVER, "acme/app"]), &services)
            .await
            .unwrap(),
        0
    );
    assert!(methods(&fake).iter().any(|r| r.contains("device/code")));
    assert_eq!(stored(dir.path())[SERVER]["accessToken"], "ghu_device");
}

#[tokio::test]
async fn local_only_flags_are_refused_with_a_server() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let services = services(fake.clone(), dir.path());
    for args in [
        &["lookup", "--server", SERVER, "--local", "."][..],
        &[
            "lookup", "--server", SERVER, "--config", "x.pkl", "acme/app",
        ],
        &["lookup", "--server", SERVER, "--all-repos"],
        &[
            "run",
            "--server",
            SERVER,
            "--allow-lockfile-pending",
            "acme/app",
        ],
    ] {
        let Err((code, error)) = run(&app(args), &services).await else {
            panic!("{args:?}")
        };
        assert_eq!(code, 2, "{args:?}");
        assert!(error.to_string().contains("only works on this machine"));
    }
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn auth_login_and_logout_manage_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let services = services(fake.clone(), dir.path());
    assert_eq!(
        run(&app(&["auth", "login", "--server", SERVER]), &services)
            .await
            .unwrap(),
        0
    );
    assert_eq!(stored(dir.path())[SERVER]["accessToken"], "ghu_device");
    assert_eq!(
        run(&app(&["auth", "logout", "--server", SERVER]), &services)
            .await
            .unwrap(),
        0
    );
    assert_eq!(stored(dir.path()), json!({}));
}
