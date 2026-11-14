#![forbid(unsafe_code)]
//! The repository-scoped installation-token mint against the fake GitHub,
//! plus an `#[ignore]`d proof against a real App installation.

use patchula_github::app::App;
use patchula_http::{Exchange, Transport};
use patchula_testkit::{github::FakeGitHub, transcripts};
use reqwest::{
    Method, Request, Url,
    header::{self, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

fn snapshot() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/transcripts.tsv")
}

fn pem() -> String {
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
    rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .unwrap()
        .to_pkcs1_pem(LineEnding::LF)
        .unwrap()
        .to_string()
}

fn app(transport: Arc<dyn Transport>) -> Arc<App> {
    App::new(
        1,
        pem().as_bytes(),
        transport,
        Url::parse("https://api.github.com").unwrap(),
    )
    .unwrap()
}

/// Records each access-token mint's request and response bodies, so tests
/// assert what was sent and answered — the fake's transcript pins requests
/// only.
struct Recorded {
    inner: Arc<dyn Transport>,
    mints: Mutex<Vec<(Value, Value)>>,
}
impl Transport for Recorded {
    fn send(&self, request: Request) -> Exchange<'_> {
        let mint =
            request.method() == Method::POST && request.url().path().ends_with("/access_tokens");
        let body = request
            .body()
            .and_then(|body| body.as_bytes())
            .and_then(|body| serde_json::from_slice(body).ok())
            .unwrap_or(Value::Null);
        Box::pin(async move {
            let response = self.inner.send(request).await?;
            if mint {
                self.mints.lock().unwrap().push((
                    body,
                    serde_json::from_slice(&response.body).unwrap_or(Value::Null),
                ));
            }
            Ok(response)
        })
    }
}

/// A scoped mint reads the repository's numeric id with the installation
/// token, posts `repository_ids` + `metadata: read`, caches per repository
/// and leaves the full mint body-less — the transcript pins every request
/// byte for byte.
#[tokio::test]
async fn a_scoped_mint_posts_repository_ids_and_metadata_read() {
    let github = Arc::new(FakeGitHub::new("demo/app"));
    github.add_repository("demo/worker", true);
    let app = app(github.clone());
    let credentials = app.installation(10);
    let id = github.state().repo().id;

    let token = credentials
        .scoped_token("demo/app")
        .expect("an installation mints scoped tokens")
        .await
        .unwrap();

    // The full mint for the id lookup's bearer stayed body-less; the
    // scoped mint posted exactly `repository_ids` + `metadata: read`.
    let mints: Vec<Value> = github
        .state()
        .writes
        .iter()
        .filter(|(path, _)| path == "/app/installations/10/access_tokens")
        .map(|(_, body)| body.clone())
        .collect();
    assert_eq!(
        mints,
        vec![
            Value::Null,
            json!({"repository_ids":[id],"permissions":{"metadata":"read"}}),
        ]
    );
    assert!(
        github
            .state()
            .requests
            .iter()
            .any(|(method, path)| method == "GET" && path == "/repos/demo/app"),
        "the numeric id is read with the installation token"
    );

    // Cached: the same repository does not mint again or read its id
    // again, and a different repository mints its own token.
    assert_eq!(
        credentials.scoped_token("demo/app").unwrap().await.unwrap(),
        token
    );
    assert_eq!(
        github
            .requests()
            .iter()
            .filter(|(method, path)| method == "GET" && path == "/repos/demo/app")
            .count(),
        1
    );
    let other = credentials
        .scoped_token("demo/worker")
        .unwrap()
        .await
        .unwrap();
    assert_ne!(other, token);
    assert_eq!(
        github
            .state()
            .writes
            .iter()
            .filter(|(path, _)| path == "/app/installations/10/access_tokens")
            .count(),
        3
    );
    transcripts::check(&snapshot());
}

/// GitHub answers a scoped mint with `permissions`, `repositories` and
/// `repository_selection`; a repository id the installation cannot see is
/// a 404, so the mint fails.
#[tokio::test]
async fn a_scoped_mint_answers_permissions_and_repositories() {
    let github = Arc::new(FakeGitHub::new("demo/app"));
    github.add_repository("demo/hidden", false);
    let recorded = Arc::new(Recorded {
        inner: github.clone(),
        mints: Mutex::new(Vec::new()),
    });
    let app = app(recorded.clone());
    let credentials = app.installation(10);

    credentials.scoped_token("demo/app").unwrap().await.unwrap();
    let (body, answer) = recorded.mints.lock().unwrap()[1].clone();
    assert_eq!(
        body,
        json!({"repository_ids":[github.state().repo().id],
               "permissions":{"metadata":"read"}})
    );
    assert_eq!(answer["permissions"], json!({"metadata":"read"}));
    assert_eq!(answer["repository_selection"], "selected");
    let repositories = answer["repositories"].as_array().unwrap();
    assert_eq!(repositories.len(), 1);
    assert_eq!(repositories[0]["full_name"], "demo/app");

    // `demo/hidden` is not installed: the mint's repository restriction
    // makes it a 404.
    assert!(
        credentials
            .scoped_token("demo/hidden")
            .unwrap()
            .await
            .is_err()
    );
    transcripts::check(&snapshot());
}

/// A real App installation proves the mint end to end. Set all of
/// `PATCHULA_TEST_APP_ID`, `PATCHULA_TEST_APP_KEY_FILE`,
/// `PATCHULA_TEST_INSTALLATION`, `PATCHULA_TEST_REPO` and
/// `PATCHULA_TEST_OTHER_PRIVATE_REPO`, then run
/// `mise exec -- cargo test -p patchula-github --test app -- --ignored`.
/// Nothing is recorded and no token is printed.
#[tokio::test]
#[ignore = "needs a real GitHub App installation; set PATCHULA_TEST_*"]
async fn a_real_installation_mints_a_metadata_read_token_for_one_repository() {
    let (Ok(id), Ok(key_file), Ok(installation), Ok(repo), Ok(other)) = (
        std::env::var("PATCHULA_TEST_APP_ID"),
        std::env::var("PATCHULA_TEST_APP_KEY_FILE"),
        std::env::var("PATCHULA_TEST_INSTALLATION"),
        std::env::var("PATCHULA_TEST_REPO"),
        std::env::var("PATCHULA_TEST_OTHER_PRIVATE_REPO"),
    ) else {
        panic!(
            "the real-installation proof needs PATCHULA_TEST_APP_ID, PATCHULA_TEST_APP_KEY_FILE, \
             PATCHULA_TEST_INSTALLATION, PATCHULA_TEST_REPO and PATCHULA_TEST_OTHER_PRIVATE_REPO"
        );
    };
    let network = Arc::new(patchula_http::Network::new().unwrap());
    let recorded = Arc::new(Recorded {
        inner: network.clone(),
        mints: Mutex::new(Vec::new()),
    });
    let app = App::new(
        id.parse().unwrap(),
        &std::fs::read(&key_file).unwrap(),
        recorded.clone(),
        Url::parse("https://api.github.com").unwrap(),
    )
    .unwrap();
    let token = app
        .installation(installation.parse().unwrap())
        .scoped_token(&repo)
        .expect("installations mint scoped tokens")
        .await
        .unwrap();

    // The scoped request body and the response's permissions and
    // repositories.
    let (body, answer) = {
        let mints = recorded.mints.lock().unwrap();
        mints
            .iter()
            .find(|(body, _)| body["repository_ids"].is_array())
            .expect("the scoped mint posted a body")
            .clone()
    };
    let repository = body["repository_ids"][0].as_u64().unwrap();
    assert_eq!(
        body,
        json!({"repository_ids":[repository],"permissions":{"metadata":"read"}})
    );
    assert_eq!(answer["permissions"], json!({"metadata":"read"}));
    assert_eq!(answer["repository_selection"], "selected");
    let repositories = answer["repositories"].as_array().unwrap();
    assert_eq!(repositories.len(), 1);
    assert!(
        repositories[0]["full_name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case(&repo)),
        "{repositories:?}"
    );
    assert_eq!(repositories[0]["id"], repository);

    // The token reads a public tag by REST and by GraphQL.
    let tag = get(&network, "repos/actions/checkout/git/ref/tags/v6", &token).await;
    assert_eq!(tag.status, 200);
    let graphql = post(
        &network,
        "graphql",
        &token,
        json!({"query":"{ repository(owner: \"actions\", name: \"checkout\") { name } }"}),
    )
    .await;
    assert_eq!(graphql.status, 200);
    let answer: Value = serde_json::from_slice(&graphql.body).unwrap();
    assert_eq!(answer["data"]["repository"]["name"], "checkout");

    // Another private repository of the installation is a 404, and a
    // write is refused.
    let hidden = get(&network, &format!("repos/{other}"), &token).await;
    assert_eq!(hidden.status, 404);
    let write = post(
        &network,
        &format!("repos/{repo}/issues"),
        &token,
        json!({"title": "x"}),
    )
    .await;
    assert_eq!(write.status, 403);
}

async fn request(
    network: &Arc<patchula_http::Network>,
    method: Method,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> patchula_http::Response {
    let mut request = Request::new(
        method,
        Url::parse(&format!("https://api.github.com/{path}")).unwrap(),
    );
    let mut auth = HeaderValue::from_str(&format!("Bearer {token}")).unwrap();
    auth.set_sensitive(true);
    request.headers_mut().insert(header::AUTHORIZATION, auth);
    request.headers_mut().insert(
        header::ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    request.headers_mut().insert(
        header::USER_AGENT,
        HeaderValue::from_static("patchula-test"),
    );
    request.headers_mut().insert(
        "x-github-api-version",
        HeaderValue::from_static("2022-11-28"),
    );
    if let Some(body) = body {
        request.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        *request.body_mut() = Some(serde_json::to_vec(&body).unwrap().into());
    }
    network.send(request).await.unwrap()
}

async fn get(
    network: &Arc<patchula_http::Network>,
    path: &str,
    token: &str,
) -> patchula_http::Response {
    request(network, Method::GET, path, token, None).await
}

async fn post(
    network: &Arc<patchula_http::Network>,
    path: &str,
    token: &str,
    body: Value,
) -> patchula_http::Response {
    request(network, Method::POST, path, token, Some(body)).await
}
