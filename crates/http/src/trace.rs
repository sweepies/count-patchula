//! Explicit allowlist of diagnostic fields. Never format a request or error.
use crate::{Response, Transport};
use reqwest::{Request, Url};
use std::time::{Duration, Instant};
use tracing::Instrument;

pub struct RequestTrace {
    span: tracing::Span,
    started: Instant,
}

impl RequestTrace {
    pub fn new(method: &str, url: &Url) -> Self {
        // Queries, userinfo, headers and bodies are never passed to tracing.
        // Also redact recognizable credentials accidentally placed in paths.
        let path = url
            .path()
            .split('/')
            .map(|part| {
                let lower = percent_encoding::percent_decode_str(part)
                    .decode_utf8_lossy()
                    .to_ascii_lowercase();
                if [
                    "ghp_",
                    "gho_",
                    "ghs_",
                    "ghu_",
                    "ghr_",
                    "github_pat_",
                    "npm_",
                    "bearer",
                    "authorization",
                    "token=",
                    "sk-",
                    "xoxb-",
                    "xoxp-",
                ]
                .iter()
                .any(|prefix| lower.contains(prefix))
                {
                    "[redacted]"
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        Self {
            span: tracing::debug_span!(target: "rustovate_http", "http", method, host = url.host_str().unwrap_or(""), path, protocol = tracing::field::Empty, freshness = tracing::field::Empty, ttl_seconds = tracing::field::Empty),
            started: Instant::now(),
        }
    }

    pub fn fresh(&self, response: &Response) {
        self.finish(Some(response), "fresh-hit", Duration::ZERO);
    }

    pub fn cache_policy(&self, reason: &'static str, ttl_seconds: Option<i64>) {
        self.span.record("freshness", reason);
        if let Some(ttl) = ttl_seconds {
            self.span.record("ttl_seconds", ttl);
        }
    }

    pub async fn send(
        &self,
        transport: &dyn Transport,
        request: Request,
        queued: Duration,
        cached: bool,
    ) -> Result<Response, crate::Error> {
        let result = transport.send(request).instrument(self.span.clone()).await;
        let outcome = match &result {
            Ok(response) if response.status == 304 => "304",
            Ok(_) if !cached => "miss",
            Ok(response) if response.status == 200 => "200",
            _ => "miss",
        };
        self.finish(result.as_ref().ok(), outcome, queued);
        result
    }

    fn finish(&self, response: Option<&Response>, outcome: &str, queued: Duration) {
        tracing::debug!(parent: &self.span,
            status = response.map_or(0, |r| r.status.as_u16()), cache = outcome,
            bytes = response.map_or(0, |r| r.body.len()),
            queued_ms = queued.as_secs_f64() * 1000.0,
            total_ms = self.started.elapsed().as_secs_f64() * 1000.0,
            connection_reuse = "unavailable", connect_tls_ms = "unavailable",
            "request"
        );
    }
}

/// Scope guard so errors also close phase spans; safe across async suspension.
pub struct PhaseTrace {
    name: &'static str,
    started: Instant,
}
impl PhaseTrace {
    pub fn new(name: &'static str) -> Self {
        tracing::debug!(target: "rustovate_http", phase = name, "phase.start");
        Self {
            name,
            started: Instant::now(),
        }
    }
}
impl Drop for PhaseTrace {
    fn drop(&mut self) {
        tracing::debug!(target: "rustovate_http", phase = self.name, total_ms = self.started.elapsed().as_secs_f64() * 1000.0, "phase.end");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        fn send(&self, request: Request) -> crate::Exchange<'_> {
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
}
