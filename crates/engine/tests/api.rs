//! Engine construction: invalid concurrency is refused at the API boundary,
//! and `from_settings` builds the same engine from the saved settings.
use patchula_cache::Cache;
use patchula_engine::Engine;
use patchula_http::{Exchange, Transport};
use std::sync::Arc;

struct Never;
impl Transport for Never {
    fn send(&self, _: reqwest::Request) -> Exchange<'_> {
        panic!("construction tests issue no requests")
    }
}

/// Concurrency outside 1..=99 is refused by every constructor, not carried
/// into a run.
#[tokio::test]
async fn invalid_concurrency_is_refused_by_every_constructor() {
    for concurrency in [0, 101] {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let Err(error) = Engine::with_transport("token", cache, concurrency, Arc::new(Never))
        else {
            panic!("the engine was built")
        };
        assert!(error.to_string().contains("concurrency"), "{error}");
    }
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let mut engine = Engine::with_transport("token", cache, 4, Arc::new(Never)).unwrap();
    let Err(error) = engine.set_batch_options(0, patchula_words::Recheck::NextRun) else {
        panic!("batch options were accepted")
    };
    assert!(error.to_string().contains("concurrency"), "{error}");
    engine
        .set_batch_options(4, patchula_words::Recheck::Now)
        .unwrap();
}

/// `from_settings` reads its settings once: it builds the same engine and
/// applies its own concurrency check.
#[tokio::test]
async fn from_settings_builds_the_configured_engine() {
    let settings = patchula_config::settings::Settings::default();
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let mut engine = Engine::from_settings(
        &settings,
        Vec::new(),
        patchula_engine::Auth::Token("t".into()),
        4,
        patchula_words::Recheck::NextRun,
        cache,
        Arc::new(Never),
    )
    .unwrap();
    let Err(error) = engine.set_batch_options(0, patchula_words::Recheck::NextRun) else {
        panic!("batch options were accepted")
    };
    assert!(error.to_string().contains("concurrency"));
    let cache = Cache::open(":memory:", Default::default()).await.unwrap();
    let Err(error) = Engine::from_settings(
        &settings,
        Vec::new(),
        patchula_engine::Auth::Token("t".into()),
        0,
        patchula_words::Recheck::NextRun,
        cache,
        Arc::new(Never),
    ) else {
        panic!("the engine was built")
    };
    assert!(error.to_string().contains("concurrency"), "{error}");
}
