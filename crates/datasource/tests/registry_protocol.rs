//! Independently simulated protocol edge cases, never live HTTP.
use rustovate_cache::Cache;
use rustovate_config::Source;
use rustovate_datasource::Client;
use rustovate_http::{Cached, Exchange, Response, Transport};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

struct Steps(Mutex<VecDeque<(String, Response)>>);
impl Transport for Steps {
    fn send(&self, request: reqwest::Request) -> Exchange<'_> {
        let (url, response) = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected network read");
        assert_eq!(request.url().as_str(), url);
        if response.status == 304 {
            assert_eq!(request.headers()["if-none-match"], "\"tags\"");
        }
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
    }
}
async fn client(transport: Arc<Steps>, dir: &std::path::Path) -> Client {
    let cache = Cache::open(dir.join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let github = rustovate_github::Client::with_transport(
        "offline",
        cache.clone(),
        Default::default(),
        transport.clone(),
    )
    .unwrap();
    Client::new(
        github,
        Cached::new(cache.clone(), transport, 4).unwrap(),
        cache,
    )
}
#[tokio::test]
async fn lexical_pagination_is_complete_and_conditional_links_survive_304() {
    let a = "https://ghcr.io/v2/spec/image/tags/list?n=1000";
    let b = "https://ghcr.io/v2/spec/image/tags/list?n=1000&last=1.0";
    let steps = Arc::new(Steps(Mutex::new(VecDeque::from([
        (
            a.into(),
            response(
                200,
                r#"{"tags":["1.0"]}"#,
                &[
                    ("etag", "\"tags\""),
                    (
                        "link",
                        "</v2/spec/image/tags/list?n=1000&last=1.0>; rel=\"next\"",
                    ),
                ],
            ),
        ),
        (
            b.into(),
            response(
                200,
                r#"{"tags":["2.0-alpine","2.0-bookworm"]}"#,
                &[("etag", "\"tags\"")],
            ),
        ),
        (a.into(), response(304, "", &[])),
        (b.into(), response(304, "", &[])),
    ]))));
    let dir = tempfile::tempdir().unwrap();
    let client = client(steps.clone(), dir.path()).await;
    for _ in 0..2 {
        assert_eq!(
            client
                .lookup(Source::Docker, "ghcr.io/spec/image")
                .await
                .unwrap()
                .versions
                .len(),
            3
        );
    }
    assert!(steps.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn rate_limit_and_foreign_pagination_are_explicit() {
    for (response, expected) in [
        (
            response(429, "{}", &[("retry-after", "42")]),
            "retry after 42",
        ),
        (
            response(
                200,
                r#"{"tags":["1.0"]}"#,
                &[(
                    "link",
                    "<https://other.invalid/v2/spec/image/tags/list>; rel=\"next\"",
                )],
            ),
            "invalid package",
        ),
    ] {
        let steps = Arc::new(Steps(Mutex::new(VecDeque::from([(
            "https://ghcr.io/v2/spec/image/tags/list?n=1000".into(),
            response,
        )]))));
        let dir = tempfile::tempdir().unwrap();
        assert!(
            client(steps, dir.path())
                .await
                .lookup(Source::Docker, "ghcr.io/spec/image")
                .await
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
}
