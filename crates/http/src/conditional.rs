//! The conditional-GET protocol the registry cache and the GitHub client
//! share: request keys, stored entries, validators, 304 merges, `Retry-After`
//! and bounded decoding. Each client keeps its own freshness policy, sender
//! and error mapping; only this protocol is common.
use crate::Response;
use futures_util::StreamExt;
use patchula_cache::{HttpEntry, HttpKey};
use reqwest::{
    Request, StatusCode,
    header::{self, HeaderValue},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

/// Failures building requests or responses from cached metadata, and running
/// a bounded decode. Each client maps these into its own error enum.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid cached HTTP metadata")]
    Metadata,
    #[error("conditional decode worker failed")]
    Worker,
    /// A streamed body failed where a buffered exchange would already have
    /// failed the request: the transport error reaches `derive` between
    /// chunks and is handed back here.
    #[error(transparent)]
    Stream(#[from] crate::Error),
}

/// Unix seconds now, for `entry`'s fetch time and freshness decisions.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// The only `HttpKey` builder. `sent` is the representation-selecting headers
/// the client always sends; authorization and cookies never enter it. The
/// scope goes through `HttpKey::credential`, so responses are never shared
/// between credentials.
pub fn key(
    method: &str,
    url: &str,
    sent: &[(&str, &str)],
    representation: Option<&str>,
    scope: Option<&str>,
) -> patchula_cache::Result<HttpKey> {
    let mut key = HttpKey::new(
        method,
        url,
        sent.iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<BTreeMap<_, _>>(),
    )?;
    if let Some(format) = representation {
        key = key.representation(format);
    }
    if let Some(scope) = scope {
        key = key.credential(scope);
    }
    Ok(key)
}

/// One freshness rule — `max-age` less the response's `Age`, else `Expires` —
/// and the union of response headers an entry keeps.
pub fn entry(response: &Response, now: i64) -> HttpEntry {
    let cache_control = comma_header(response, "cache-control");
    let max_age = cache_control
        .as_deref()
        .into_iter()
        .flat_map(|value| value.split(','))
        .find_map(|part| {
            let (name, value) = part.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("max-age")
                .then(|| value.trim().trim_matches('"').parse::<i64>().ok())?
        })
        .filter(|age| *age >= 0);
    let age = text(response, "age")
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let expires_at = max_age
        .map(|max_age| now.saturating_add(max_age.saturating_sub(age).max(0)))
        .or_else(|| {
            httpdate::parse_http_date(text(response, "expires")?)
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|value| value.as_secs() as i64)
        });
    // Only the headers needed to rebuild the response; never cookies or credentials.
    let headers = [
        "etag",
        "last-modified",
        "link",
        "cache-control",
        "expires",
        "vary",
        "content-type",
        "docker-content-digest",
    ]
    .into_iter()
    .filter_map(|name| comma_header(response, name).map(|value| (name.into(), value)))
    .collect();
    HttpEntry {
        status: response.status.as_u16(),
        etag: text(response, "etag").map(str::to_owned),
        last_modified: text(response, "last-modified").map(str::to_owned),
        headers,
        body: response.body.clone(),
        fetched_at: now,
        expires_at,
        cache_control,
    }
}

/// Whether the response may be stored under its key. A client sends a fixed
/// set of request headers, so a `Vary` on a header the key holds, on a header
/// the client never sends, or on `Accept-Encoding` (bodies are stored decoded)
/// selects the same response every time. `no-store`, `Vary: *` and `Vary: User-Agent` (the
/// agent names this build) are uncacheable, and so is a `Vary` on
/// authorization unless `authorization_in_key`: the key's credential scope
/// names whoever authorized the request.
pub fn storable(response: &Response, authorization_in_key: bool) -> bool {
    let no_store = comma_header(response, "cache-control").is_some_and(|control| {
        control
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("no-store"))
    });
    let varies = comma_header(response, "vary").unwrap_or_default();
    let vary_ok = varies
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .all(|name| {
            match name.to_ascii_lowercase().as_str() {
                "*" => false,
                // The key carries who authorized only when the caller says so.
                "authorization" | "proxy-authorization" | "cookie" => authorization_in_key,
                // The agent names this build, which the key does not.
                "user-agent" => false,
                // In the key, decoded away (accept-encoding), or never sent.
                _ => true,
            }
        });
    !no_store && vary_ok
}

/// Send the entry's validators on a revalidating request.
pub fn add_validators(request: &mut Request, entry: &HttpEntry) -> Result<(), Error> {
    for (name, value) in [
        (header::IF_NONE_MATCH, &entry.etag),
        (header::IF_MODIFIED_SINCE, &entry.last_modified),
    ] {
        if let Some(value) = value {
            request.headers_mut().insert(
                name,
                HeaderValue::from_str(value).map_err(|_| Error::Metadata)?,
            );
        }
    }
    Ok(())
}

/// A 304 keeps the cached status and body; response headers win and cached
/// headers fill in what the response omitted.
pub fn not_modified(mut response: Response, entry: HttpEntry) -> Result<Response, Error> {
    response.status = StatusCode::from_u16(entry.status).map_err(|_| Error::Metadata)?;
    response.body = entry.body;
    for (name, value) in entry.headers {
        let name = header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Metadata)?;
        if !response.headers.contains_key(&name) {
            response.headers.insert(
                name,
                HeaderValue::from_str(&value).map_err(|_| Error::Metadata)?,
            );
        }
    }
    Ok(response)
}

/// Rebuild the response a cached entry stands for.
pub fn response(entry: &HttpEntry) -> Result<Response, Error> {
    let mut headers = header::HeaderMap::new();
    for (name, value) in &entry.headers {
        headers.insert(
            header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| Error::Metadata)?,
            HeaderValue::from_str(value).map_err(|_| Error::Metadata)?,
        );
    }
    Ok(Response {
        status: StatusCode::from_u16(entry.status).map_err(|_| Error::Metadata)?,
        headers,
        body: entry.body.clone(),
        stale: None,
    })
}

/// `Retry-After` as delta-seconds or an HTTP date. Callers keep their own
/// defaults on top.
pub fn retry_after(response: &Response) -> Option<Duration> {
    let value = text(response, "retry-after")?;
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
        })
}

/// The body a successful read derives from: decoded bytes, buffered or still
/// arriving chunk by chunk.
pub enum Body {
    /// A whole body, for reads whose transport buffers: fakes, replays and
    /// clients that inspect the bytes anyway.
    Buffered(Vec<u8>),
    /// Body chunks as the transport yields them.
    Streaming(crate::BodyStream),
}

/// Read one JSON document as it arrives, the way `serde_json::from_slice`
/// reads a slice: one value, trailing data refused.
pub fn json_document<T: serde::de::DeserializeOwned>(
    body: &mut dyn std::io::Read,
) -> serde_json::Result<T> {
    let mut deserializer = serde_json::Deserializer::from_reader(body);
    let value = T::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

/// Run `derive` on the blocking pool. A buffered body decodes under a
/// `decoding` permit, bounding how many whole documents are worked on at
/// once; a streamed body is already bounded by its exchange's host permit
/// and feeds the worker through a bounded channel it reads as `io::Read`, so
/// the raw document is never held whole. The inner result is the derive
/// function's own verdict, so a document it rejects never looks like a
/// transport failure; a body that fails mid-stream is the outer
/// `Error::Stream`, carrying the transport error itself.
pub async fn derive<F, E>(
    body: Body,
    derive: F,
    decoding: &Arc<Semaphore>,
) -> Result<std::result::Result<Vec<u8>, E>, Error>
where
    F: FnOnce(&mut dyn std::io::Read) -> std::result::Result<Vec<u8>, E> + Send + 'static,
    E: Send + 'static,
{
    match body {
        Body::Buffered(bytes) => {
            let permit = decoding
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| Error::Worker)?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                derive(&mut &bytes[..])
            })
            .await
            .map_err(|_| Error::Worker)
        }
        Body::Streaming(body) => {
            // Chunks in flight between the reader task and the worker bound
            // what a fast network can buffer while a slow parse holds back.
            let (sender, receiver) = tokio::sync::mpsc::channel(4);
            let failed = Arc::new(Mutex::new(None));
            let pump = crate::AbortOnDrop(tokio::spawn(async move {
                let mut body = body;
                while let Some(chunk) = body.next().await {
                    if sender.send(chunk).await.is_err() {
                        break; // the worker is done: it errored, or its caller is gone
                    }
                }
            }));
            let mut reader = Chunks {
                receiver,
                current: std::io::Cursor::new(crate::Bytes::new()),
                failed: failed.clone(),
                stopped: false,
            };
            let joined = tokio::task::spawn_blocking(move || derive(&mut reader))
                .await
                .map_err(|_| Error::Worker)?;
            drop(pump);
            match (joined, failed.lock().unwrap().take()) {
                (result, None) => Ok(result),
                // The stream failed mid-document: the derive error is the
                // truncation it saw; the transport error is the one to keep.
                (_, Some(transport)) => Err(Error::Stream(transport)),
            }
        }
    }
}

/// The worker side of `derive`'s channel: chunk after chunk as `io::Read`,
/// the stream's error remembered once the reader reports its end as `Err`.
struct Chunks {
    receiver: tokio::sync::mpsc::Receiver<Result<crate::Bytes, crate::Error>>,
    current: std::io::Cursor<crate::Bytes>,
    failed: Arc<Mutex<Option<crate::Error>>>,
    stopped: bool,
}

impl std::io::Read for Chunks {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.stopped {
            return Err(std::io::Error::other(
                "the response body stopped mid-document",
            ));
        }
        loop {
            let read = self.current.read(buffer)?;
            if read > 0 {
                return Ok(read);
            }
            match self.receiver.blocking_recv() {
                Some(Ok(chunk)) => self.current = std::io::Cursor::new(chunk),
                Some(Err(error)) => {
                    self.stopped = true;
                    *self.failed.lock().unwrap() = Some(error);
                    return Err(std::io::Error::other(
                        "the response body stopped mid-document",
                    ));
                }
                None => return Ok(0),
            }
        }
    }
}

fn text<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers.get(name)?.to_str().ok()
}

fn comma_header(response: &Response, name: &str) -> Option<String> {
    let values: Vec<_> = response
        .headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&'static str, &'static str)]) -> Response {
        Response {
            status: StatusCode::from_u16(status).unwrap(),
            headers: headers
                .iter()
                .map(|&(name, value)| {
                    (
                        header::HeaderName::from_static(name),
                        HeaderValue::from_static(value),
                    )
                })
                .collect::<header::HeaderMap>(),
            body: b"body".to_vec(),
            stale: None,
        }
    }

    #[test]
    fn entry_subtracts_age_from_max_age() {
        let entry = entry(
            &response(
                200,
                &[("cache-control", "public, max-age=300"), ("age", "120")],
            ),
            1000,
        );
        assert_eq!(entry.expires_at, Some(1000 + 180));
        assert_eq!(entry.fetched_at, 1000);
    }

    #[test]
    fn entry_falls_back_to_expires_without_max_age() {
        let entry = entry(
            &response(200, &[("expires", "Wed, 14 Oct 2026 00:00:00 GMT")]),
            1000,
        );
        assert_eq!(entry.expires_at, Some(1791936000));
    }

    #[test]
    fn entry_keeps_the_union_of_replay_headers() {
        let entry = entry(
            &response(
                200,
                &[
                    ("etag", "\"v\""),
                    ("last-modified", "Wed, 14 Oct 2026 00:00:00 GMT"),
                    ("link", "<https://a/>; rel=\"next\""),
                    ("cache-control", "public, max-age=300"),
                    ("expires", "Wed, 14 Oct 2026 00:00:00 GMT"),
                    ("vary", "Accept"),
                    ("content-type", "application/json"),
                    ("docker-content-digest", "sha256:abc"),
                    ("set-cookie", "session=secret"),
                    ("authorization", "Bearer hidden"),
                ],
            ),
            1000,
        );
        for name in [
            "etag",
            "last-modified",
            "link",
            "cache-control",
            "expires",
            "vary",
            "content-type",
            "docker-content-digest",
        ] {
            assert!(entry.headers.contains_key(name), "{name}");
        }
        assert!(!entry.headers.contains_key("set-cookie"));
        assert!(!entry.headers.contains_key("authorization"));
    }

    #[test]
    fn storable_follows_what_the_key_holds() {
        let stored = |headers: &[(&'static str, &'static str)], authorization_in_key| {
            storable(&response(200, headers), authorization_in_key)
        };
        assert!(!stored(&[("cache-control", "private, no-store")], true));
        assert!(!stored(&[("vary", "Accept, *")], true));
        assert!(!stored(&[("vary", "User-Agent")], true));
        assert!(stored(
            &[("vary", "Accept, Accept-Encoding, X-Requested-With")],
            false
        ));
        assert!(stored(&[("vary", "Accept, Authorization, Cookie")], true));
        assert!(!stored(&[("vary", "Authorization")], false));
        assert!(stored(&[], false));
    }

    #[test]
    fn max_age_is_read_whatever_its_case() {
        let entry = entry(
            &response(200, &[("cache-control", "public, Max-Age = 300")]),
            1000,
        );
        assert_eq!(entry.expires_at, Some(1300));
    }

    #[test]
    fn key_rejects_a_url_the_cache_key_rejects() {
        assert!(key("GET", "not a url", &[], None, None).is_err());
    }

    #[test]
    fn retry_after_reads_an_http_date() {
        let date = Box::leak(
            httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(120)).into_boxed_str(),
        );
        let delay = retry_after(&response(429, &[("retry-after", date)])).unwrap();
        assert!(delay > Duration::from_secs(60) && delay <= Duration::from_secs(120));
    }

    /// A body stream over a `Vec` of chunk results, flagging its drop so a
    /// test can see the reader side let go.
    struct Flagged {
        dropped: Arc<std::sync::atomic::AtomicBool>,
        chunks: std::vec::IntoIter<Result<crate::Bytes, crate::Error>>,
    }
    impl futures_util::Stream for Flagged {
        type Item = Result<crate::Bytes, crate::Error>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Ready(self.chunks.next())
        }
    }
    impl Drop for Flagged {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn flagged(
        chunks: Vec<Result<crate::Bytes, crate::Error>>,
    ) -> (Body, Arc<std::sync::atomic::AtomicBool>) {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        (
            Body::Streaming(Box::pin(Flagged {
                dropped: dropped.clone(),
                chunks: chunks.into_iter(),
            })),
            dropped,
        )
    }

    fn chunk(bytes: &[u8]) -> Result<crate::Bytes, crate::Error> {
        Ok(crate::Bytes::copy_from_slice(bytes))
    }

    fn echo(body: &mut dyn std::io::Read) -> Result<Vec<u8>, ()> {
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).map_err(|_| ())?;
        Ok(bytes)
    }

    /// Chunks and a whole body feed a derive the same bytes.
    #[tokio::test]
    async fn streamed_chunks_derive_the_same_as_a_buffer() {
        let decoding = Arc::new(Semaphore::new(4));
        let document = br#"{"versions":{"1.0.0":{},"2.0.0":{"deprecated":"old"}}} tail"#;
        let (body, _) = flagged(vec![
            chunk(&document[..7]),
            chunk(&document[7..40]),
            chunk(&document[40..]),
        ]);
        let streamed = derive(body, echo, &decoding).await.unwrap().unwrap();
        let buffered = derive(Body::Buffered(document.to_vec()), echo, &decoding)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(streamed, buffered);
    }

    /// A mid-stream failure is the transport error, the same one a buffered
    /// exchange would have returned before any derive ran.
    #[tokio::test]
    async fn a_mid_stream_failure_is_the_transport_error() {
        let decoding = Arc::new(Semaphore::new(4));
        let (body, _) = flagged(vec![chunk(br#"{"versions":"#), Err(crate::Error::Network)]);
        let result = derive(body, echo, &decoding).await;
        assert!(matches!(result, Err(Error::Stream(crate::Error::Network))));
    }

    /// A body that just stops is the derive's own verdict — a truncated
    /// document, not a transport failure — so the caller keeps mapping it to
    /// its document error.
    #[tokio::test]
    async fn a_truncated_stream_is_the_derive_error() {
        fn document(body: &mut dyn std::io::Read) -> Result<Vec<u8>, ()> {
            serde_json::to_vec(&json_document::<serde_json::Value>(body).map_err(|_| ())?)
                .map_err(|_| ())
        }
        let decoding = Arc::new(Semaphore::new(4));
        let (body, _) = flagged(vec![chunk(br#"{"versions":{"1.0.0":"#)]);
        let result = derive(body, document, &decoding).await.unwrap();
        assert!(result.is_err());
    }

    /// A derive that stops reading early lets the stream go: the pump cannot
    /// send into a closed channel and the body is dropped.
    #[tokio::test]
    async fn an_early_derive_exit_drops_the_stream() {
        fn one_byte(body: &mut dyn std::io::Read) -> Result<Vec<u8>, ()> {
            let mut byte = [0];
            body.read_exact(&mut byte).map_err(|_| ())?;
            Err(())
        }
        let decoding = Arc::new(Semaphore::new(4));
        let (body, dropped) = flagged((0..64).map(|_| chunk(b"0123456789abcdef")).collect());
        derive(body, one_byte, &decoding)
            .await
            .unwrap()
            .unwrap_err();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !dropped.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the stream is dropped once its reader is done");
    }

    /// `json_document` reads exactly one document: trailing whitespace is
    /// fine, trailing data is refused, as `from_slice` would.
    #[test]
    fn json_document_reads_one_document() {
        let mut reader: &[u8] = br#"{"a": 1}   "#;
        assert_eq!(
            json_document::<serde_json::Value>(&mut reader).unwrap()["a"],
            1
        );
        let mut reader: &[u8] = br#"{"a": 1} {}"#;
        assert!(json_document::<serde_json::Value>(&mut reader).is_err());
        let mut reader: &[u8] = br#"{"a": 1"#;
        assert!(json_document::<serde_json::Value>(&mut reader).is_err());
    }
}
