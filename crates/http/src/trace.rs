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
            span: tracing::debug_span!(target: "patchula_http", "http", method, host = url.host_str().unwrap_or(""), path, protocol = tracing::field::Empty, freshness = tracing::field::Empty, ttl_seconds = tracing::field::Empty),
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
        tracing::debug!(target: "patchula_http", phase = name, "phase.start");
        Self {
            name,
            started: Instant::now(),
        }
    }
}
impl Drop for PhaseTrace {
    fn drop(&mut self) {
        tracing::debug!(target: "patchula_http", phase = self.name, total_ms = self.started.elapsed().as_secs_f64() * 1000.0, "phase.end");
    }
}
