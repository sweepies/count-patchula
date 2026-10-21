//! Offline, fixed-workload GitHub/cache benchmark. No live transport is created.
#![forbid(unsafe_code)]

use patchula_cache::{Cache, HttpEntry, HttpKey, Options as CacheOptions};
use patchula_github::{Client, Options};
use patchula_testkit::{Options as FixtureOptions, Replay};
use std::{collections::BTreeMap, hint::black_box, path::Path, sync::Arc};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        CacheOptions::default(),
    )
    .await?;
    let cassette =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/github/conditional.json");
    for i in 0..25 {
        let replay = Arc::new(Replay::open(&cassette, FixtureOptions::default())?);
        // Unique synthetic credential scopes keep the first GET cold per replay,
        // while all clients use the same cache worker and database.
        let client = Client::with_transport(
            &format!("offline-benchmark-{i}"),
            cache.clone(),
            Options::default(),
            replay.clone(),
        )?;
        let first = client.get("conditional").await?;
        let second = client.get("conditional").await?;
        if first.body != second.body || client.stats().await.not_modified != 1 {
            return Err("conditional replay did not preserve the cached body".into());
        }
        black_box(second);
        replay.assert_finished()?;
    }
    let key = HttpKey::new("GET", "https://benchmark.invalid/cache", BTreeMap::new())?;
    let body = vec![b'x'; 4096];
    cache
        .http_put(
            &key,
            &HttpEntry {
                status: 200,
                etag: None,
                last_modified: None,
                headers: BTreeMap::new(),
                body: body.clone(),
                fetched_at: 0,
                expires_at: None,
                cache_control: None,
            },
        )
        .await?;
    for _ in 0..1000 {
        let entry = cache
            .http_get(&key)
            .await?
            .ok_or("expected warm cache hit")?;
        if entry.body != body {
            return Err("warm cache body mismatch".into());
        }
        black_box(entry);
    }
    println!("25 offline 200/304 pairs; 1000 warm 4 KiB cache reads");
    Ok(())
}
