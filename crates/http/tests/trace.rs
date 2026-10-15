// Keep this capture test in its own process: concurrent untraced unit tests
// can first register the shared callsites with tracing's no-subscriber dispatch
// while the scoped subscriber is being installed, caching Interest::never.
use reqwest::{Request, Url};
use rustovate_http::{RequestTrace, Response, Transport};
use std::time::Duration;
use std::{
    io::Write,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct SecretResponse;
impl Transport for SecretResponse {
    fn send(&self, request: Request) -> rustovate_http::Exchange<'_> {
        assert!(request.headers().contains_key("authorization"));
        Box::pin(async {
            Ok(Response {
                status: reqwest::StatusCode::OK,
                headers: reqwest::header::HeaderMap::from_iter([(
                    "authorization".parse().unwrap(),
                    "Bearer response-secret".parse().unwrap(),
                )]),
                body: b"ghp_responseSecret".to_vec(),
            })
        })
    }
}

#[test]
fn traced_request_never_formats_credentials_headers_queries_or_bodies() {
    let output = Output::default();
    let writer = output.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let mut request = Request::new(reqwest::Method::GET,
                Url::parse("https://user:password@example.com/repos/ghp_pathSecret/%67%68%70%5fencodedSecret?token=query-secret&Authorization=hidden").unwrap());
            request.headers_mut().insert("authorization", "Bearer github_pat_headerSecret".parse().unwrap());
            *request.body_mut() = Some("npm_bodySecret".into());
            let trace = RequestTrace::new(request.method().as_str(), request.url());
            let response = trace.send(&SecretResponse, request, Duration::from_millis(12), false).await.unwrap();
            trace.fresh(&response);
        });
    });
    let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
    assert_eq!(log.lines().count(), 2, "{log}");
    for forbidden in [
        "Authorization",
        "authorization",
        "Bearer",
        "ghp_",
        "github_pat_",
        "npm_",
        "query-secret",
        "response-secret",
        "password",
        "user:",
        "encodedSecret",
        "%67",
    ] {
        assert!(!log.contains(forbidden), "{log}");
    }
    for expected in [
        "GET",
        "example.com",
        "/repos/[redacted]",
        "status=200",
        "cache=\"miss\"",
        "fresh-hit",
        "bytes=",
        "queued_ms=12",
        "total_ms=",
        "connection_reuse=",
    ] {
        assert!(log.contains(expected), "{expected}: {log}");
    }
}
