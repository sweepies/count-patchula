//! Public endpoints recorded by this implementation; normal tests only replay.
use rustovate_cache::{Cache, Options as CacheOptions};
use rustovate_config::Source;
use rustovate_datasource::Client;
use rustovate_github::Options as GithubOptions;
use rustovate_http::{Cached, Network};
use rustovate_testkit::{Options, Redactor};
use std::{path::Path, sync::Arc};

#[tokio::test]
async fn public_sources_cold_and_conditional_warm() {
    let token = if std::env::var("RUSTOVATE_RECORD").as_deref() == Ok("1") {
        std::env::var("GH_TOKEN")
            .or_else(|_| std::env::var("GITHUB_TOKEN"))
            .expect("recording needs token")
    } else {
        "offline-public-token".into()
    };
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/datasource/public-sources.json");
    let transport = rustovate_testkit::from_env(
        &path,
        Options {
            redactor: Redactor::default().with_literals([token.clone()]),
            ..Options::default()
        },
        || Ok(Arc::new(Network::new()?)),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::open(dir.path().join("cache.sqlite"), CacheOptions::default())
        .await
        .unwrap();
    let github = rustovate_github::Client::with_transport(
        &token,
        cache.clone(),
        GithubOptions::default(),
        transport.clone(),
    )
    .unwrap();
    let http = Cached::new(cache.clone(), transport, 4).unwrap();
    let client = Client::new(github.clone(), http.clone(), cache);
    let packages = [
        (Source::Npm, "tiny-tarball"),
        (Source::Crates, "heck"),
        (Source::Go, "rsc.io/quote"),
        (Source::GithubReleases, "jdx/mr-boxington"),
        (Source::GithubTags, "actions/hello-world-javascript-action"),
    ];
    let start = std::time::Instant::now();
    let mut first = Vec::new();
    for (source, package) in packages {
        let catalog = client.lookup(source, package).await.unwrap();
        assert!(!catalog.versions.is_empty(), "{package}");
        first.push(catalog);
    }
    let sha = client
        .digest("actions/hello-world-javascript-action", "main")
        .await
        .unwrap();
    assert_eq!(sha.len(), 40);
    let cold = start.elapsed();
    let before_g = github.stats().await;
    let before_h = http.stats();
    let start = std::time::Instant::now();
    for ((source, package), catalog) in packages.into_iter().zip(first) {
        let next = client.lookup(source, package).await.unwrap();
        assert_eq!(
            serde_json::to_value(catalog).unwrap(),
            serde_json::to_value(next).unwrap()
        );
    }
    assert_eq!(
        sha,
        client
            .digest("actions/hello-world-javascript-action", "main")
            .await
            .unwrap()
    );
    let after_g = github.stats().await;
    let after_h = http.stats();
    assert_eq!(
        after_g.requests - before_g.requests,
        3,
        "warm GitHub only probes and ref, no GraphQL history"
    );
    assert_eq!(after_g.not_modified - before_g.not_modified, 3);
    assert_eq!(
        after_h.requests - before_h.requests,
        2,
        "Go .info history is cached"
    );
    assert_eq!(after_h.not_modified - before_h.not_modified, 2);
    eprintln!(
        "sources cold={cold:?} warm={:?} cold_requests={} warm_requests=5 warm_304=5",
        start.elapsed(),
        before_g.requests + before_h.requests
    );
}
