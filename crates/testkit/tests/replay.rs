#![forbid(unsafe_code)]

use patchula_http::{Exchange, Response, Transport};
use patchula_testkit::{Options, Recorder, Redactor, Replay, check_directory};
use reqwest::{Method, Request, header::HeaderValue};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}
fn request() -> Request {
    let mut request = Request::new(
        Method::POST,
        "https://offline.invalid/graphql".parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("content-type", HeaderValue::from_static("application/json"));
    *request.body_mut() =
        Some(r#"{"variables":{"cursor":null},"query":"query { viewer { login } }"}"#.into());
    request
}

#[tokio::test]
async fn replay_matches_body_headers_method_and_url_without_a_network_client() {
    let path = fixtures().join("testkit/smoke.json");
    for change in 0..4 {
        let replay = Replay::open(&path, Options::default()).unwrap();
        let mut request = request();
        match change {
            0 => *request.method_mut() = Method::GET,
            1 => request.url_mut().set_path("/other"),
            2 => {
                request.headers_mut().clear();
            }
            _ => *request.body_mut() = Some("{}".into()),
        }
        let error = replay.send(request).await.unwrap_err().to_string();
        assert!(
            error.contains("unmatched") && error.contains("replay never accesses the network"),
            "{error}"
        );
    }
    let replay = Replay::open(&path, Options::default()).unwrap();
    assert!(replay.assert_finished().is_err());
    assert_eq!(replay.send(request()).await.unwrap().status, 200);
    replay.assert_finished().unwrap();
    assert!(
        replay.send(request()).await.is_err(),
        "interactions are consumed once"
    );
    if std::env::var("PATCHULA_RECORD").as_deref() != Ok("1") {
        let transport = patchula_testkit::from_env(&path, Options::default(), || {
            panic!("replay must never construct a network client")
        })
        .unwrap();
        transport.send(request()).await.unwrap();
    }
}

struct Echo;
impl Transport for Echo {
    fn send(&self, request: Request) -> Exchange<'_> {
        Box::pin(async move {
            let mut headers = request.headers().clone();
            headers.insert("set-cookie", HeaderValue::from_static("session=private"));
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers,
                body: request.body().unwrap().as_bytes().unwrap().to_vec(),
            })
        })
    }
}

#[tokio::test]
async fn recording_redacts_before_atomic_write_and_replays_with_other_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("secrets.json");
    let redactor = Redactor::default()
        .with_literals(["custom-private-value".into()])
        .with_pattern("orgkey_[a-z]+")
        .unwrap();
    let options = Options {
        redactor: redactor.clone(),
        ..Options::default()
    };
    let recorder = Recorder::new(
        &path,
        options.clone(),
        Arc::new(Echo),
        "Simulated redaction test".into(),
    );
    let mut request = request();
    let url = "https://user:password@offline.invalid/graphql?access_token=private-query&safe=yes"; // betterleaks:allow
    *request.url_mut() = url.parse().unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_static("Bearer private-auth"),
    );
    request
        .headers_mut()
        .insert("cookie", HeaderValue::from_static("private-cookie"));
    *request.body_mut() = Some(r#"{"authorization":"private-json","nested":["ghp_abc123","ghs_123_eyJhbGci.payload-signature.last","gho_abc123","github_pat_abc123","v1.0123456789abcdef0123456789abcdef01234567","custom-private-value","orgkey_secret"],"url":"https://example.com/?token=private-link"}"#.into());
    recorder.send(request.try_clone().unwrap()).await.unwrap();
    let stored = std::fs::read_to_string(&path).unwrap();
    for private in [
        "private-query",
        "private-auth",
        "private-cookie",
        "private-json",
        "private-link",
        "session=private",
        "custom-private-value",
        "orgkey_secret",
        "ghp_abc123",
        "ghs_123_eyJhbGci",
        "payload-signature",
        "gho_abc123",
        "github_pat_abc123",
        "v1.0123456789abcdef0123456789abcdef01234567",
        "user:password",
    ] {
        assert!(!stored.contains(private), "secret was written");
    }
    assert!(stored.contains("REDACTED"));
    redactor.check(stored.as_bytes()).unwrap();
    assert_eq!(check_directory(directory.path(), &redactor).unwrap(), 1);
    let replay = Replay::open(&path, options).unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_static("Bearer other-auth"),
    );
    replay.send(request).await.unwrap();
    replay.assert_finished().unwrap();
}

#[test]
fn fixture_scanner_rejects_raw_escaped_url_and_custom_secrets() {
    let redactor = Redactor::default().with_literals(["extra-secret".into()]);
    for json in [
        r#"{"body":"ghp_secret"}"#,
        r#"{"body":"\u0067hp_secret"}"#,
        r#"{"body":"https://example.com/?access_token=anything"}"#,
        r#"{"body":"https://example.com/?auth=anything&key=private"}"#,
        r#"{"body":"https://example.com/path/%67hp%5Fsecret"}"#,
        r#"{"body":"https://example.com/?safe=%67hs%5Fsecret"}"#,
        r#"{"authorization":["anything"]}"#,
        r#"{"set-cookie":["session=anything"]}"#,
        r#"{"body":"extra-secret"}"#,
        r#"{"body":"v1.0123456789abcdef0123456789abcdef01234567"}"#,
    ] {
        assert!(
            redactor.check(json.as_bytes()).is_err(),
            "scanner accepted a secret"
        );
    }
    assert!(check_directory(&fixtures(), &redactor).unwrap() >= 1);
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("leak.json"),
        r#"{"body":"ghs_secret"}"#,
    )
    .unwrap();
    let error = check_directory(directory.path(), &redactor).unwrap_err();
    assert!(error.to_string().contains("leak.json"));
    assert!(!format!("{error:?}").contains("ghs_secret"));
}

#[test]
fn redaction_preserves_release_versions_but_removes_complete_stateless_tokens() {
    let redactor = Redactor::default();
    assert_eq!(
        redactor.text("release v1.24.0 https://example.com/v1.24.0"),
        "release v1.24.0 https://example.com/v1.24.0"
    );
    assert_eq!(
        redactor.text("ghs_1234_eyJhbGci.payload-with-dash.signature"),
        "[REDACTED]"
    );
}

#[test]
fn a_url_with_nothing_to_redact_keeps_its_bytes_however_it_serializes() {
    let redactor = Redactor::default();
    let kept = "see https://example.com/a?x=a%20b&y=1 and https://example.com/p%2Fq";
    assert_eq!(redactor.text(kept), kept);
    assert_eq!(
        redactor.text("https://example.com/a?x=a%20b&token=abc"),
        "https://example.com/a?x=a+b&token=%5BREDACTED%5D"
    );
}
