use super::*;
use crate::{App, run};
use patchula_http::{Exchange, Response};
use serde_json::json;
use std::sync::{Arc, Mutex};

const SERVER: &str = "https://patchula.acme.dev";
const ISSUES: &str = "https://api.github.com/repos/acme/app/issues?state=open&sort=updated&direction=desc&per_page=100&page=";

/// The server and GitHub's login endpoints. `revoked` tokens get a 401 from the server.
#[derive(Default)]
struct Fake {
    requests: Mutex<Vec<(String, String, String, String)>>,
    pending: Mutex<u32>,
    polls: Mutex<u32>,
    revoked: Vec<&'static str>,
    /// Answer the issues listing with an element that cannot decode.
    broken_issues: bool,
    /// GitHub cannot describe the server's App.
    no_app: bool,
    /// The server names this client ID instead of its App's.
    client_id: Option<&'static str>,
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
            ("GET", issues) if issues.starts_with(ISSUES) => {
                let page: usize = issues[ISSUES.len()..].parse().unwrap();
                // A full first page of other items, then the markers.
                let items: Vec<Value> = if page == 1 {
                    vec![json!({"user": {"type": "User", "login": "human"}, "body": ""}); 100]
                } else {
                    // An unreadable item ahead of the server's own marker:
                    // skipping it once discovered the marker behind it.
                    let broken = self.broken_issues.then(|| json!({"user": {"type": "Bot"}}));
                    broken
                        .into_iter()
                        .chain([
                            // Anyone can write a marker; only the server's own bot counts.
                            json!({"user": {"type": "User", "login": "human"},
                            "body": "<!-- patchula-server:https://evil.acme.dev -->"}),
                            // Another App's bot copied a marker in: the
                            // attacker's server claims that bot but hands
                            // out the Count App's client ID.
                            json!({"user": {"type": "Bot", "login": "other[bot]"},
                            "body": "<!-- patchula-server:https://evil.acme.dev -->"}),
                            json!({"user": {"type": "Bot", "login": "imposter[bot]"},
                            "body": format!("<!-- patchula-server:{SERVER} -->")}),
                            json!({"user": {"type": "Bot", "login": "patchula[bot]"},
                            "body": format!("table\n<!-- patchula-server:{SERVER} -->\n")}),
                        ])
                        .collect()
                };
                (200, json!(items))
            }
            ("GET", "https://evil.acme.dev/api/v1/auth") => {
                (200, json!({"clientId": "Iv23client", "bot": "other[bot]"}))
            }
            ("GET", "https://patchula.acme.dev/api/v1/auth") => (
                200,
                json!({"clientId": self.client_id.unwrap_or("Iv23client"), "bot": "patchula[bot]"}),
            ),
            ("GET", "https://api.github.com/apps/other") => {
                (200, json!({"slug": "other", "client_id": "Iv23other"}))
            }
            ("GET", "https://api.github.com/apps/imposter") => {
                (404, json!({"message": "Not Found"}))
            }
            ("GET", "https://api.github.com/apps/patchula") if self.no_app => {
                (404, json!({"message": "Not Found"}))
            }
            ("GET", "https://api.github.com/apps/patchula") => {
                (200, json!({"slug": "patchula", "client_id": "Iv23client"}))
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
                headers: reqwest::header::HeaderMap::new(),
                body: serde_json::to_vec(&value).unwrap(),
                stale: None,
            })
        })
    }
}

fn services(fake: Arc<Fake>, dir: &std::path::Path) -> Services {
    Services {
        transport: Some(fake),
        config_dir: Some(dir.to_owned()),
        server: Some(SERVER.into()),
        ..Services::default()
    }
}
fn remembered_store(dir: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(dir.join("remotes.json")).unwrap()).unwrap()
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
    assert_eq!(parse("https://patchula.acme.dev/").unwrap(), SERVER);
    assert_eq!(
        parse("http://localhost:8080").unwrap(),
        "http://localhost:8080"
    );
    for bad in [
        "http://patchula.acme.dev",
        "https://patchula.acme.dev/?x=1",
        "https://user@patchula.acme.dev",
        "patchula.acme.dev",
    ] {
        assert_eq!(parse(bad).unwrap_err().0, 2, "{bad}");
    }
}

#[tokio::test(start_paused = true)]
async fn first_run_signs_in_with_the_device_flow_and_stores_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake {
        pending: Mutex::new(1),
        ..Fake::default()
    });
    let services = services(fake.clone(), dir.path());
    let app = app(&["remote", "run", "--dry-run", "acme/app", "--json"]);
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
        run(&app(&["remote", "lookup", "acme/app"]), &services)
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
        ..Fake::default()
    });
    let services = services(fake.clone(), dir.path());
    assert_eq!(
        run(&app(&["remote", "run", "acme/app"]), &services)
            .await
            .unwrap(),
        0
    );
    assert!(methods(&fake).iter().any(|r| r.contains("device/code")));
    assert_eq!(stored(dir.path())[SERVER]["accessToken"], "ghu_device");
}

#[tokio::test]
async fn remote_with_several_repositories_needs_a_configured_server() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let services = Services {
        server: None,
        ..services(fake.clone(), dir.path())
    };
    let Err((code, error)) = run(
        &app(&["remote", "lookup", "acme/app", "acme/api"]),
        &services,
    )
    .await
    else {
        panic!("ran without a server")
    };
    assert_eq!(code, 2);
    assert!(error.to_string().contains("PATCHULA_SERVER"), "{error}");
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn remote_discovers_and_remembers_a_single_named_repository() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let services = Services {
        server: None,
        ..services(fake.clone(), dir.path())
    };
    assert_eq!(
        run(&app(&["remote", "lookup", "acme/app"]), &services)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        remembered_store(dir.path()),
        json!({"acme/app": {"server": SERVER, "app": "Iv23client"}})
    );
    // Second run uses the remembered server without re-scanning GitHub issues.
    fake.requests.lock().unwrap().clear();
    *fake.polls.lock().unwrap() = 0;
    assert_eq!(
        run(&app(&["remote", "lookup", "acme/app"]), &services)
            .await
            .unwrap(),
        0
    );
    assert!(
        !methods(&fake)
            .iter()
            .any(|r| r.contains("api.github.com/repos/")),
        "{:?}",
        methods(&fake)
    );
}

#[tokio::test(start_paused = true)]
async fn remote_login_and_logout_manage_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let services = services(fake.clone(), dir.path());
    assert_eq!(
        run(&app(&["remote", "login", "--server", SERVER]), &services)
            .await
            .unwrap(),
        0
    );
    assert_eq!(stored(dir.path())[SERVER]["accessToken"], "ghu_device");
    assert_eq!(
        run(&app(&["remote", "logout", "--server", SERVER]), &services)
            .await
            .unwrap(),
        0
    );
    assert_eq!(stored(dir.path()), json!({}));
}

#[tokio::test]
async fn find_discovers_and_remember_and_logout_manage_the_pin() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let (found, newly_discovered) = find(&services(fake.clone(), dir.path()), "acme/app")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.url, SERVER);
    assert!(newly_discovered);
    // `find` alone does not write the pin until the caller completes login/run.
    assert!(!dir.path().join("remotes.json").exists());
    remember(&services(fake.clone(), dir.path()), "acme/app", &found).unwrap();
    assert_eq!(
        remembered_store(dir.path()),
        json!({"acme/app": {"server": SERVER, "app": "Iv23client"}})
    );
    // Remembered: no GitHub request on the next lookup.
    fake.requests.lock().unwrap().clear();
    let (again, newly_discovered) = find(&services(fake.clone(), dir.path()), "acme/app")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.url, SERVER);
    assert!(!newly_discovered);
    assert!(fake.requests.lock().unwrap().is_empty());
    // Signing out forgets the remembered repository too.
    run(
        &app(&["remote", "logout", "--server", SERVER]),
        &services(fake, dir.path()),
    )
    .await
    .unwrap();
    assert_eq!(remembered_store(dir.path()), json!({}));
}

#[tokio::test(start_paused = true)]
async fn legacy_repositories_json_does_not_block_logout_or_discovery() {
    let dir = tempfile::tempdir().unwrap();
    // Legacy file written by earlier releases contains `"remote": true|false`.
    std::fs::write(
        dir.path().join("repositories.json"),
        json!({"acme/app": {"server": SERVER, "remote": false, "app": "Iv23client"}}).to_string(),
    )
    .unwrap();
    seed(dir.path(), json!({"accessToken": "ghu_old"}));
    let fake = Arc::new(Fake::default());
    let services = Services {
        server: None,
        ..services(fake.clone(), dir.path())
    };
    assert_eq!(
        run(&app(&["remote", "logout", "--server", SERVER]), &services)
            .await
            .unwrap(),
        0
    );
    assert_eq!(stored(dir.path()), json!({}));
    assert_eq!(
        run(&app(&["remote", "lookup", "acme/app"]), &services)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        remembered_store(dir.path()),
        json!({"acme/app": {"server": SERVER, "app": "Iv23client"}})
    );
}

#[tokio::test(start_paused = true)]
async fn remote_login_and_logout_infer_checkout_server_and_failed_login_does_not_pin() {
    let dir = tempfile::tempdir().unwrap();
    // A failed login must not write a pin to remotes.json.
    let failing = Arc::new(Fake {
        client_id: Some("Iv23client"),
        ..Fake::default()
    });
    // Discovery succeeds for acme/app, then sign-in fails if device flow is disabled.
    struct DisabledDeviceFlow(Arc<Fake>);
    impl Transport for DisabledDeviceFlow {
        fn send(&self, request: reqwest::Request) -> Exchange<'_> {
            if request.url().as_str() == DEVICE_CODE_URL {
                return Box::pin(async {
                    Ok(Response {
                        status: reqwest::StatusCode::OK,
                        headers: reqwest::header::HeaderMap::new(),
                        body: serde_json::to_vec(&json!({"error": "device_flow_disabled"}))
                            .unwrap(),
                        stale: None,
                    })
                });
            }
            self.0.send(request)
        }
    }
    let failed_services = Services {
        transport: Some(Arc::new(DisabledDeviceFlow(failing))),
        config_dir: Some(dir.path().to_owned()),
        server: None,
        repository: Some("acme/app".into()),
        ..Services::default()
    };
    assert!(
        run(&app(&["remote", "login"]), &failed_services)
            .await
            .is_err()
    );
    assert!(!dir.path().join("remotes.json").exists());

    // A successful checkout login pins the server, and logout without --server clears both stores.
    let fake = Arc::new(Fake::default());
    let checkout_services = Services {
        server: None,
        repository: Some("acme/app".into()),
        ..services(fake.clone(), dir.path())
    };
    assert_eq!(
        run(&app(&["remote", "login"]), &checkout_services)
            .await
            .unwrap(),
        0
    );
    assert_eq!(stored(dir.path())[SERVER]["accessToken"], "ghu_device");
    assert_eq!(
        remembered_store(dir.path()),
        json!({"acme/app": {"server": SERVER, "app": "Iv23client"}})
    );
    fake.requests.lock().unwrap().clear();
    assert_eq!(
        run(&app(&["remote", "logout"]), &checkout_services)
            .await
            .unwrap(),
        0
    );
    // Logout reads remotes.json locally without hitting the network.
    assert!(fake.requests.lock().unwrap().is_empty());
    assert_eq!(stored(dir.path()), json!({}));
    assert_eq!(remembered_store(dir.path()), json!({}));
}

#[tokio::test]
async fn remote_target_precedence_and_repository_validation() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("remotes.json"),
        json!({"acme/app": {"server": "https://remembered.acme.dev", "app": "Iv23client"}})
            .to_string(),
    )
    .unwrap();
    let fake = Arc::new(Fake::default());

    // 1. Explicit --server beats PATCHULA_SERVER and remembered.
    let svc = services(fake.clone(), dir.path());
    let (server, repos, discovered) = crate::remote_target(
        Some("https://explicit.acme.dev"),
        &["acme/app".into()],
        &svc,
    )
    .await
    .unwrap();
    assert_eq!(server.url, "https://explicit.acme.dev");
    assert_eq!(repos, ["acme/app"]);
    assert!(!discovered);

    // 2. PATCHULA_SERVER beats remembered.
    let (server, _, discovered) = crate::remote_target(None, &["acme/app".into()], &svc)
        .await
        .unwrap();
    assert_eq!(server.url, SERVER);
    assert!(!discovered);

    // 3. With neither set, remembered wins without network calls.
    let no_env = Services {
        server: None,
        ..services(fake.clone(), dir.path())
    };
    let (server, _, discovered) = crate::remote_target(None, &["acme/app".into()], &no_env)
        .await
        .unwrap();
    assert_eq!(server.url, "https://remembered.acme.dev");
    assert!(!discovered);
    assert!(fake.requests.lock().unwrap().is_empty());

    // 4. Invalid repository slugs are rejected before any request.
    for bad in [
        "../../user",
        "acme/",
        "/app",
        "acme/app/extra",
        "acme/app?x=1",
        "acme/..",
    ] {
        let (code, _) = crate::remote_target(None, &[bad.into()], &no_env)
            .await
            .unwrap_err();
        assert_eq!(code, 2, "{bad}");
    }
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discover_checks_github_app_before_contacting_advertised_host_and_caps_probes() {
    struct ProbeFake {
        requests: Mutex<Vec<String>>,
    }
    impl Transport for ProbeFake {
        fn send(&self, request: reqwest::Request) -> Exchange<'_> {
            let url = request.url().to_string();
            self.requests.lock().unwrap().push(url.clone());
            let (status, value) = match url.as_str() {
                u if u.starts_with(ISSUES) => {
                    let mut items = vec![json!({
                        "user": {"type": "Bot", "login": "unknown[bot]"},
                        "body": "<!-- patchula-server:https://never-contact.acme.dev -->"
                    })];
                    // 7 distinct servers from a real App whose auth endpoints disagree,
                    // followed by the real server (beyond DISCOVERY_SERVER_LIMIT = 5).
                    for i in 1..=7 {
                        items.push(json!({
                            "user": {"type": "Bot", "login": "other[bot]"},
                            "body": format!("<!-- patchula-server:https://s{i}.acme.dev -->")
                        }));
                    }
                    items.push(json!({
                        "user": {"type": "Bot", "login": "patchula[bot]"},
                        "body": format!("<!-- patchula-server:{SERVER} -->")
                    }));
                    (200, json!(items))
                }
                "https://api.github.com/apps/unknown" => (404, json!({"message": "Not Found"})),
                "https://api.github.com/apps/other" => {
                    (200, json!({"slug": "other", "client_id": "Iv23other"}))
                }
                "https://api.github.com/apps/patchula" => {
                    (200, json!({"slug": "patchula", "client_id": "Iv23client"}))
                }
                u if u.ends_with("/api/v1/auth") => {
                    (200, json!({"clientId": "Iv23wrong", "bot": "other[bot]"}))
                }
                other => panic!("unexpected request {other}"),
            };
            Box::pin(async move {
                Ok(Response {
                    status: reqwest::StatusCode::from_u16(status).unwrap(),
                    headers: reqwest::header::HeaderMap::new(),
                    body: serde_json::to_vec(&value).unwrap(),
                    stale: None,
                })
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(ProbeFake {
        requests: Mutex::default(),
    });
    let svc = Services {
        transport: Some(fake.clone()),
        config_dir: Some(dir.path().to_owned()),
        ..Services::default()
    };
    assert_eq!(discover(&svc, "acme/app").await, None);
    let urls = fake.requests.lock().unwrap().clone();
    assert!(
        !urls.iter().any(|u| u.contains("never-contact.acme.dev")),
        "unknown bot's advertised server must never be contacted: {urls:?}"
    );
    let auth_probes: Vec<_> = urls
        .iter()
        .filter(|u| u.ends_with("/api/v1/auth"))
        .collect();
    assert_eq!(auth_probes.len(), DISCOVERY_SERVER_LIMIT, "{auth_probes:?}");
}

#[tokio::test]
async fn unreachable_remembered_server_suggests_remote_logout() {
    struct DeadTransport;
    impl Transport for DeadTransport {
        fn send(&self, _request: reqwest::Request) -> Exchange<'_> {
            Box::pin(async { Err(patchula_http::Error::Connect) })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    seed(
        dir.path(),
        json!({"accessToken": "ghu_stored", "expiresAt": 4_000_000_000_i64}),
    );
    let svc = Services {
        transport: Some(Arc::new(DeadTransport)),
        config_dir: Some(dir.path().to_owned()),
        ..Services::default()
    };
    let target = Server {
        url: SERVER.into(),
        app: Some("Iv23client".into()),
    };
    let (code, error) = report(&svc, &target, vec!["acme/app".into()], Mode::Lookup)
        .await
        .unwrap_err();
    assert_eq!(code, 1);
    let rendered = format!("{error:?}");
    assert!(rendered.contains("patchula remote logout"), "{rendered}");
}

#[tokio::test]
async fn a_marker_from_another_app_is_skipped_even_if_its_server_claims_that_bot() {
    // evil.acme.dev claims other[bot], which wrote its marker, and hands out
    // the Count App's client ID; GitHub says other[bot]'s App has another.
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    assert_eq!(
        discover(&services(fake.clone(), dir.path()), "acme/app").await,
        Some((SERVER.to_owned(), "Iv23client".to_owned()))
    );
    let requests = methods(&fake);
    let other_app = requests
        .iter()
        .position(|r| r == "GET https://api.github.com/apps/other")
        .unwrap();
    let evil_auth = requests
        .iter()
        .position(|r| r == "GET https://evil.acme.dev/api/v1/auth")
        .unwrap();
    assert!(
        other_app < evil_auth,
        "GitHub App must be resolved before probing advertised host"
    );
}

#[tokio::test]
async fn an_app_github_cannot_describe_discovers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake {
        no_app: true,
        ..Fake::default()
    });
    assert_eq!(
        discover(&services(fake, dir.path()), "acme/app").await,
        None
    );
}

#[tokio::test]
async fn a_server_whose_app_does_not_match_its_author_discovers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake {
        client_id: Some("Iv23other"),
        ..Fake::default()
    });
    assert_eq!(
        discover(&services(fake, dir.path()), "acme/app").await,
        None
    );
}

#[tokio::test]
async fn a_bot_login_that_is_not_an_app_slug_is_never_looked_up() {
    let fake = Fake::default();
    for login in [
        "../users/x[bot]",
        "[bot]",
        "patchula",
        "a/b[bot]",
        "a?b[bot]",
    ] {
        assert_eq!(app_client_id(&fake, None, login).await, None, "{login}");
    }
    assert!(fake.requests.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_remembered_server_that_names_another_app_is_refused_before_sign_in() {
    // Discovery verified Iv23client; the server now hands out another ID.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("remotes.json"),
        json!({"acme/app": {"server": SERVER, "app": "Iv23client"}}).to_string(),
    )
    .unwrap();
    let fake = Arc::new(Fake {
        client_id: Some("Iv23evil"),
        ..Fake::default()
    });
    let services = services(fake.clone(), dir.path());
    let (target, discovered) = find(&services, "acme/app").await.unwrap().unwrap();
    assert!(!discovered);
    let error = report(&services, &target, vec!["acme/app".into()], Mode::Lookup)
        .await
        .unwrap_err();
    assert!(
        error.1.to_string().contains("names a different GitHub App"),
        "{}",
        error.1
    );
    assert!(
        !methods(&fake)
            .iter()
            .any(|r| r.contains("github.com/login"))
    );
    // A server the user names is theirs to vouch for.
    let named = Server::named(SERVER.to_owned());
    assert_eq!(client_id(&*fake, &named).await.unwrap(), "Iv23evil");
}

#[tokio::test]
async fn an_unreadable_issues_page_discovers_nothing() {
    // An element that cannot decode abandons the page: a partial read must
    // not skip past a marker it could not see.
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake {
        broken_issues: true,
        ..Fake::default()
    });
    assert_eq!(
        discover(&services(fake, dir.path()), "acme/app").await,
        None
    );
}

/// A server that accepts the run and answers status polls from `replies`,
/// repeating the last; `post` overrides the POST /runs answer once.
#[derive(Default)]
struct RunFake {
    replies: Mutex<std::collections::VecDeque<Value>>,
    post: Mutex<Option<(u16, Value)>>,
}
impl Transport for RunFake {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let url = request.url().to_string();
        let (status, value) = match (request.method().as_str(), url.as_str()) {
            ("POST", "https://patchula.acme.dev/api/v1/runs") => self
                .post
                .lock()
                .unwrap()
                .clone()
                .unwrap_or((202, json!({"id": "job-1"}))),
            ("GET", "https://patchula.acme.dev/api/v1/runs/job-1") => {
                let mut replies = self.replies.lock().unwrap();
                let value = if replies.len() > 1 {
                    replies.pop_front().unwrap()
                } else {
                    replies
                        .front()
                        .cloned()
                        .unwrap_or_else(|| json!({"status": "running"}))
                };
                (200, value)
            }
            other => panic!("unexpected request {other:?}"),
        };
        Box::pin(async move {
            Ok(Response {
                status: reqwest::StatusCode::from_u16(status).unwrap(),
                headers: reqwest::header::HeaderMap::new(),
                body: serde_json::to_vec(&value).unwrap(),
                stale: None,
            })
        })
    }
}

/// Services with a stored, unexpired token so `report` goes straight to the run.
fn run_services(fake: Arc<RunFake>, dir: &std::path::Path) -> Services {
    seed(
        dir,
        json!({"accessToken": "ghu_stored", "expiresAt": 4_000_000_000_i64}),
    );
    Services {
        transport: Some(fake),
        config_dir: Some(dir.to_owned()),
        server: Some(SERVER.into()),
        ..Services::default()
    }
}

#[tokio::test(start_paused = true)]
async fn run_statuses_surface_their_own_failures() {
    let attempt = |replies: Vec<Value>| async move {
        let dir = tempfile::tempdir().unwrap();
        let fake = Arc::new(RunFake {
            replies: Mutex::new(replies.into()),
            ..RunFake::default()
        });
        report(
            &run_services(fake, dir.path()),
            &Server::named(SERVER.into()),
            vec!["acme/app".into()],
            Mode::Lookup,
        )
        .await
    };
    let complete = |report: Value| json!({"status": "complete", "report": report});
    let valid = json!({
        "schemaVersion": patchula_report::SCHEMA_VERSION,
        "repositories": [], "stats": null, "actions": null
    });

    // A finished report with another schema version names both versions.
    let Err((code, error)) = attempt(vec![complete(json!({
        "schemaVersion": patchula_report::SCHEMA_VERSION + 1,
        "repositories": [], "stats": null, "actions": null
    }))])
    .await
    else {
        panic!("a foreign schema version was accepted")
    };
    assert_eq!(code, 1);
    assert!(error.to_string().contains("sends report schema"), "{error}");

    // A report that is not a report at all.
    let Err((_, error)) = attempt(vec![complete(json!({"schemaVersion": "six"}))]).await else {
        panic!("a malformed report was accepted")
    };
    assert!(error.to_string().contains("cannot read"), "{error}");

    // A status the protocol does not know.
    let Err((_, error)) = attempt(vec![json!({"status": "confused"})]).await else {
        panic!("an unknown status was accepted")
    };
    assert!(error.to_string().contains("unknown run status"), "{error}");

    // The server's own failure reason reaches the user.
    let Err((_, error)) = attempt(vec![
        json!({"status": "failed", "error": "the Count slipped"}),
    ])
    .await
    else {
        panic!("a failed run was accepted")
    };
    assert!(error.to_string().contains("the Count slipped"), "{error}");

    // And a good report still comes back.
    let done = attempt(vec![complete(valid)]).await.unwrap();
    assert!(done.repositories.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_rejected_run_start_says_the_run_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(RunFake {
        post: Mutex::new(Some((
            404,
            json!({"errorKind": "unknown-run", "error": "no such run"}),
        ))),
        ..RunFake::default()
    });
    let Err((code, error)) = report(
        &run_services(fake, dir.path()),
        &Server::named(SERVER.into()),
        vec!["acme/app".into()],
        Mode::Lookup,
    )
    .await
    else {
        panic!("a rejected run was accepted")
    };
    assert_eq!(code, 1);
    let message = error.to_string();
    assert!(message.contains("the run is gone"), "{message}");
    assert!(message.contains("15 minutes"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn a_run_that_never_finishes_times_out_with_the_servers_limit() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(RunFake::default());
    let Err((code, error)) = report(
        &run_services(fake, dir.path()),
        &Server::named(SERVER.into()),
        vec!["acme/app".into()],
        Mode::Lookup,
    )
    .await
    else {
        panic!("a run that never finished was accepted")
    };
    assert_eq!(code, 1);
    let message = error.to_string();
    assert!(
        message.contains("did not finish within 6 hours"),
        "{message}"
    );
    assert!(message.contains("may still be running"), "{message}");
}
