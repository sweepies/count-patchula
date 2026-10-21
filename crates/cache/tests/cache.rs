#![forbid(unsafe_code)]

use patchula_cache::{Cache, ExtractionKey, HttpEntry, HttpKey, Options, Release};
use std::collections::BTreeMap;

fn key(name: &str) -> HttpKey {
    HttpKey::new(
        "get",
        &format!("https://example.com/{name}"),
        BTreeMap::new(),
    )
    .unwrap()
}
fn entry(body: Vec<u8>) -> HttpEntry {
    HttpEntry {
        status: 200,
        etag: Some("\"v1\"".into()),
        last_modified: Some("Wed, 14 Oct 2026 00:00:00 GMT".into()),
        headers: BTreeMap::from([(
            "link".into(),
            "<https://example.com/?page=2>; rel=\"next\"".into(),
        )]),
        body,
        fetched_at: 100,
        expires_at: Some(200),
        cache_control: Some("max-age=100".into()),
    }
}
fn releases(versions: &[&str]) -> Vec<Release> {
    versions
        .iter()
        .map(|version| Release {
            version: (*version).into(),
            published_at: Some("2026-10-14T00:00:00Z".into()),
            digest: Some(format!("digest-{version}")),
        })
        .collect()
}
async fn cache(directory: &tempfile::TempDir) -> Cache {
    Cache::open(directory.path().join("cache.sqlite"), Options::default())
        .await
        .unwrap()
}

#[tokio::test]
async fn conditional_refresh_preserves_compressed_body_and_updates_only_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(&directory).await;
    let key = key("large-registry");
    let original = entry(vec![b'x'; 1024 * 1024]);
    cache.http_put(&key, &original).await.unwrap();
    let conn = rusqlite::Connection::open(directory.path().join("cache.sqlite")).unwrap();
    let body = || {
        conn.query_row("SELECT body FROM http", [], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
    };
    let before = body();
    let mut refreshed = original.clone();
    refreshed.body = b"a 304 has no replacement body".to_vec();
    refreshed.etag = Some("new-validator".into());
    refreshed.fetched_at = 300;
    refreshed.expires_at = Some(400);
    cache.http_refresh(&key, &refreshed).await.unwrap();
    assert_eq!(body(), before);
    refreshed.body = original.body;
    assert_eq!(cache.http_get(&key).await.unwrap(), Some(refreshed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_readers_and_writers_share_persistent_wal() {
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(&directory).await;
    let mut tasks = Vec::new();
    for index in 0..48 {
        let cache = cache.clone();
        tasks.push(tokio::spawn(async move {
            let key = key(&format!("package-{index}"));
            let entry = entry(format!("body-{index}").into_bytes());
            cache.http_put(&key, &entry).await.unwrap();
            assert_eq!(cache.http_get(&key).await.unwrap(), Some(entry));
            let extraction = ExtractionKey {
                repo: format!("owner/repo-{index}"),
                default_branch_commit: "commit".into(),
                config_hash: "config".into(),
            };
            cache
                .extraction_put(&extraction, b"{\"dependencies\":[]}")
                .await
                .unwrap();
            assert_eq!(
                cache.extraction_get(&extraction).await.unwrap().unwrap(),
                b"{\"dependencies\":[]}"
            );
            let changed = ExtractionKey {
                config_hash: "changed".into(),
                ..extraction
            };
            assert!(cache.extraction_get(&changed).await.unwrap().is_none());
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let other = Cache::open(directory.path().join("cache.sqlite"), Options::default())
        .await
        .unwrap();
    assert!(other.http_get(&key("package-47")).await.unwrap().is_some());
    let conn = rusqlite::Connection::open(directory.path().join("cache.sqlite")).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let compressed: Vec<u8> = conn
        .query_row("SELECT body FROM http LIMIT 1", [], |row| row.get(0))
        .unwrap();
    assert_eq!(&compressed[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
}

#[tokio::test]
async fn merge_pages_preserves_order_and_stops_at_first_known_item() {
    let directory = tempfile::tempdir().unwrap();
    let cache = cache(&directory).await;
    assert!(
        !cache
            .merge_releases("github", "pkg", &releases(&["3", "2"]))
            .await
            .unwrap()
    );
    assert!(
        !cache
            .merge_release_page("github", "pkg", &releases(&["1", "0"]), Some("2"))
            .await
            .unwrap()
    );
    // More than one new page can precede old history: no overlap is not an append.
    assert!(
        !cache
            .merge_releases("github", "pkg", &releases(&["7", "6"]))
            .await
            .unwrap()
    );
    assert!(
        cache
            .merge_release_page(
                "github",
                "pkg",
                &releases(&["5", "4", "3", "ignored"]),
                Some("6")
            )
            .await
            .unwrap()
    );
    let list = cache.releases("github", "pkg").await.unwrap();
    assert_eq!(list.newest.as_deref(), Some("7"));
    assert_eq!(
        list.releases
            .iter()
            .map(|r| r.version.as_str())
            .collect::<Vec<_>>(),
        ["7", "6", "5", "4", "3", "2", "1", "0"]
    );
    assert!(
        cache
            .merge_releases("github", "pkg", &releases(&["7", "ignored"]))
            .await
            .unwrap()
    );
    assert_eq!(cache.releases("github", "pkg").await.unwrap(), list);
    assert!(
        cache
            .releases("other", "pkg")
            .await
            .unwrap()
            .releases
            .is_empty()
    );
    assert!(
        cache
            .merge_release_page("github", "pkg", &releases(&["oops"]), Some("absent"))
            .await
            .is_err()
    );
    assert_eq!(cache.releases("github", "pkg").await.unwrap(), list);
}

#[tokio::test]
async fn corrupt_file_is_quarantined_and_rebuilt_without_losing_the_batch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.sqlite");
    std::fs::write(&path, b"this is not a SQLite database").unwrap();
    let cache = cache(&directory).await;
    assert!(cache.http_get(&key("x")).await.unwrap().is_none());
    cache
        .http_put(&key("x"), &entry(b"fresh".to_vec()))
        .await
        .unwrap();
    assert!(cache.http_get(&key("x")).await.unwrap().is_some());
    let backups: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains(".corrupt-"))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(
        std::fs::read(&backups[0]).unwrap(),
        b"this is not a SQLite database"
    );
}

#[tokio::test]
async fn migrates_previous_schema_without_discarding_http_or_release_data() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../src/migrations/001.sql"))
            .unwrap();
        let compressed = zstd::stream::encode_all(&b"old body"[..], 3).unwrap();
        // The key representation is intentionally stable across schema versions.
        let encoded = r#"{"method":"GET","url":"https://example.com/old","headers":{}}"#;
        conn.execute(
            "INSERT INTO http(key,status,headers,body,fetched_at) VALUES (?1,200,'{}',?2,123)",
            rusqlite::params![encoded, compressed],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO releases(source,package,version,position) VALUES ('github','pkg','1',0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO release_heads(source,package,newest) VALUES ('github','pkg','1')",
            [],
        )
        .unwrap();
    }
    let cache = cache(&directory).await;
    assert_eq!(
        cache.http_get(&key("old")).await.unwrap().unwrap().body,
        b"old body"
    );
    assert_eq!(
        cache
            .releases("github", "pkg")
            .await
            .unwrap()
            .newest
            .as_deref(),
        Some("1")
    );
    let conn = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        7
    );
}

#[tokio::test]
async fn eviction_uses_compressed_body_budget_and_recent_access() {
    let directory = tempfile::tempdir().unwrap();
    let body: Vec<_> = (0..2048).map(|i| ((i * 17 + i / 11) % 251) as u8).collect();
    let size = zstd::stream::encode_all(body.as_slice(), 3).unwrap().len() as u64;
    let cache = Cache::open(
        directory.path().join("cache.sqlite"),
        Options {
            http_body_budget: size * 2,
        },
    )
    .await
    .unwrap();
    cache
        .http_put(&key("one"), &entry(body.clone()))
        .await
        .unwrap();
    cache
        .http_put(&key("two"), &entry(body.clone()))
        .await
        .unwrap();
    cache.http_get(&key("one")).await.unwrap();
    cache
        .merge_releases("github", "pkg", &releases(&["1"]))
        .await
        .unwrap();
    cache.http_put(&key("three"), &entry(body)).await.unwrap();
    assert!(cache.http_get(&key("two")).await.unwrap().is_none());
    assert!(cache.http_get(&key("one")).await.unwrap().is_some());
    assert!(cache.http_get(&key("three")).await.unwrap().is_some());
    assert_eq!(
        cache
            .releases("github", "pkg")
            .await
            .unwrap()
            .releases
            .len(),
        1
    );
    let conn = rusqlite::Connection::open(directory.path().join("cache.sqlite")).unwrap();
    conn.execute("UPDATE http SET body=x'000102'", []).unwrap();
    assert!(
        cache.http_get(&key("one")).await.unwrap().is_none(),
        "bad compressed entries are misses"
    );
}

#[test]
fn http_keys_normalize_authority_method_fragment_and_headers_but_keep_query_order() {
    assert!(HttpKey::new("get", "https://user:secret@example.com/", BTreeMap::new()).is_err());
    let a = HttpKey::new(
        "get",
        "https://EXAMPLE.COM:443/a/../b?x=1&x=2#fragment",
        BTreeMap::from([("Accept".into(), " application/json ".into())]),
    )
    .unwrap();
    let b = HttpKey::new(
        "GET",
        "https://example.com/b?x=1&x=2",
        BTreeMap::from([("accept".into(), "application/json".into())]),
    )
    .unwrap();
    assert!(a == b);
    let reversed = HttpKey::new(
        "GET",
        "https://example.com/b?x=2&x=1",
        BTreeMap::from([("accept".into(), "application/json".into())]),
    )
    .unwrap();
    assert!(a != reversed);
    assert!(b.clone().representation("catalog-v1") != b);
    assert!(b.clone().representation("catalog-v1") != b.representation("catalog-v2"));
}

#[tokio::test]
async fn future_schema_is_preserved_instead_of_being_treated_as_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.sqlite");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE future_data(value TEXT); INSERT INTO future_data VALUES ('keep'); PRAGMA user_version=99;").unwrap();
    }
    assert!(matches!(
        Cache::open(&path, Options::default()).await,
        Err(patchula_cache::Error::FutureSchema(99))
    ));
    let conn = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        conn.query_row("SELECT value FROM future_data", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
}

#[tokio::test]
async fn migration_retires_raw_registries_and_old_content_hashed_catalogs() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache.sqlite");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("../src/migrations/001.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../src/migrations/002.sql"))
        .unwrap();
    for url in [
        "https://registry.npmjs.org/vite",
        "https://index.crates.io/he/ck/heck",
        "https://pypi.org/pypi/pip/json",
        "https://api.github.com/repos/example/releases",
    ] {
        let key = serde_json::json!({"method":"GET","url":url,"headers":{}}).to_string();
        conn.execute(
            "INSERT INTO http(key,status,headers,body,fetched_at) VALUES (?1,200,'{}',?2,0)",
            rusqlite::params![key, zstd::stream::encode_all(&b"old body"[..], 3).unwrap()],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO extraction VALUES ('catalog:npm:vite','hash','catalog-v1',X'00')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO extraction VALUES ('owner/repo','sha','config',X'01')",
        [],
    )
    .unwrap();
    let _cache = Cache::open(&path, Options::default()).await.unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM http", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT repo FROM extraction", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "owner/repo"
    );
}
