//! The derive engine every cached compaction runs on: the body a successful
//! read derives from, bounded so a large document is never held whole. Its
//! text is hashed into every transform's row in `tests/derived-versions.tsv`.
use crate::conditional::Error;
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
