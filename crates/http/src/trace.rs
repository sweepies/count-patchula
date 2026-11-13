//! Explicit allowlist of diagnostic fields. Never format a request or error.
use crate::{Response, Transport};
use reqwest::{Request, StatusCode, Url};
use std::time::{Duration, Instant};
use tracing::Instrument;

/// Prefixes that mark credential material: the token shapes tracing and
/// test recordings both redact on. `v1.` keeps its own pattern in recordings:
/// only a long run after it is a token, so versions such as v1.2.3 survive.
pub const TOKEN_SHAPES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "ghr_",
    "github_pat_",
    "npm_",
    "sk-",
    "xoxb-",
    "xoxp-",
];

/// Whether `text` holds a token shape at a word boundary: `sk-` inside
/// `task-1` is not a key.
fn shaped(text: &str) -> bool {
    TOKEN_SHAPES.iter().any(|shape| {
        text.match_indices(shape).any(|(at, _)| {
            !text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        })
    })
}

/// Words that mark credential material in a path without being a token
/// shape: the schemes authorization headers use and credential query
/// parameters. Tracing only; recordings get them from their own rules.
const PATH_MARKERS: &[&str] = &["bearer", "authorization", "token="];

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
                if shaped(&lower) || PATH_MARKERS.iter().any(|marker| lower.contains(marker)) {
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
        self.finish(
            Some((response.status, Some(response.body.len()))),
            "fresh-hit",
            Duration::ZERO,
        );
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
        self.finish(
            result.as_ref().ok().map(|r| (r.status, Some(r.body.len()))),
            outcome,
            queued,
        );
        result
    }

    /// The same exchange with the body still streaming: the record logs when
    /// headers arrive, so it carries no byte count.
    pub async fn send_streaming(
        &self,
        transport: &dyn Transport,
        request: Request,
        queued: Duration,
        cached: bool,
    ) -> Result<crate::Streaming, crate::Error> {
        let result = transport
            .send_streaming(request)
            .instrument(self.span.clone())
            .await;
        let outcome = match &result {
            Ok(response) if response.status == 304 => "304",
            Ok(_) if !cached => "miss",
            Ok(response) if response.status == 200 => "200",
            _ => "miss",
        };
        self.finish(
            result.as_ref().ok().map(|r| (r.status, None)),
            outcome,
            queued,
        );
        result
    }

    fn finish(
        &self,
        response: Option<(StatusCode, Option<usize>)>,
        outcome: &str,
        queued: Duration,
    ) {
        tracing::debug!(parent: &self.span,
            status = response.map_or(0, |(status, _)| status.as_u16()), cache = outcome,
            bytes = response.and_then(|(_, bytes)| bytes).unwrap_or(0),
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

#[cfg(test)]
mod shape_tests {
    use super::shaped;

    #[test]
    fn token_shapes_count_only_at_a_word_boundary() {
        assert!(shaped("ghp_abc123"));
        assert!(shaped("x-npm_abc"));
        assert!(!shaped("task-1"));
        assert!(!shaped("v1.0.0.info"));
        assert!(!shaped("@v"));
    }
}
