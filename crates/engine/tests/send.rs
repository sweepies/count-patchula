//! The server boxes engine futures as `Send`; engine tests run on a
//! current-thread runtime, so pin the bound down here.
use patchula_cache::Cache;
use patchula_engine::{Engine, Options};
use patchula_http::{Exchange, Transport};
use std::sync::Arc;

struct Noop;
impl Transport for Noop {
    fn send(&self, _request: reqwest::Request) -> Exchange<'_> {
        Box::pin(async { Err(patchula_http::Error::Network) })
    }
}

fn need_send<T: Send>(_: T) {}

#[tokio::test]
async fn public_futures_stay_send() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::open(dir.path().join("cache.sqlite"), Default::default())
        .await
        .unwrap();
    let engine = Engine::with_transport("offline", cache, 4, Arc::new(Noop)).unwrap();
    need_send(engine.lookup(Options::default()));
    need_send(engine.run(Options::default(), false, false));
}
