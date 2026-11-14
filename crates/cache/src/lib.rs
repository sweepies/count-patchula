//! Batch-shared SQLite cache with bounded parallel readers and one writer.
#![forbid(unsafe_code)]

pub mod installations;
pub mod queue;
pub mod repositories;
mod store;

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use store::Store;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cache database operation failed")]
    Database(#[from] rusqlite::Error),
    #[error("cache I/O failed")]
    Io(#[from] std::io::Error),
    #[error("cache serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("cache worker is unavailable")]
    Worker,
    #[error("cache URL must be absolute HTTP(S) without credentials")]
    Url,
    #[error("cache schema {0} is newer than supported schema {max}", max = store::MIGRATIONS.len())]
    FutureSchema(i64),
    #[error("cache integrity check failed")]
    Corrupt,
    #[error("release page anchor is not in cached history")]
    ReleaseAnchor,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Stable diagnostics without SQLite messages, paths or serialized contents.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Database(error) => match error.sqlite_error_code() {
                Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
                    "cache-busy"
                }
                _ => "cache-database",
            },
            Self::Io(_) => "cache-io",
            Self::Serialization(_) => "cache-serialization",
            Self::Worker => "cache-worker-unavailable",
            Self::Url => "cache-url",
            Self::FutureSchema(_) => "cache-future-schema",
            Self::Corrupt => "cache-corrupt",
            Self::ReleaseAnchor => "cache-release-anchor",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Budget for compressed HTTP bodies, not release/extraction data or SQLite pages.
    pub http_body_budget: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            http_body_budget: 256 * 1024 * 1024,
        }
    }
}

/// Canonical method, URL and vary-relevant request headers. Credentials must be
/// represented by a one-way namespace digest, never raw Authorization values.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct HttpKey {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    representation: Option<String>,
    /// The operator registry entry whose credentials fetched this response,
    /// by name. Responses read with credentials are never shared with reads
    /// without them, or with another entry's.
    #[serde(skip_serializing_if = "Option::is_none")]
    credential: Option<String>,
}

impl HttpKey {
    pub fn new(method: &str, url: &str, headers: BTreeMap<String, String>) -> Result<Self> {
        let mut url = url::Url::parse(url).map_err(|_| Error::Url)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::Url);
        }
        url.set_fragment(None);
        Ok(Self {
            representation: None,
            credential: None,
            method: method.to_ascii_uppercase(),
            url: url.into(),
            headers: headers
                .into_iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                .collect(),
        })
    }

    fn encoded(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Local derived format/version, never sent as an upstream request header.
    pub fn representation(mut self, format: &str) -> Self {
        self.representation = Some(format.into());
        self
    }

    /// Scope the key to an operator registry entry, by its name, never its secret.
    pub fn credential(mut self, id: &str) -> Self {
        self.credential = Some(id.into());
        self
    }
}

/// Timestamps are UTC Unix seconds; expiry is a hint, validators remain useful
/// after expiry. Evicted bodies are misses and never generate conditional GETs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpEntry {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub fetched_at: i64,
    pub expires_at: Option<i64>,
    pub cache_control: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Stable upstream identity (tag, release ID, or version); not semver-sorted.
    pub version: String,
    pub published_at: Option<String>,
    pub digest: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseList {
    pub newest: Option<String>,
    pub releases: Vec<Release>,
}

/// The scope of state kept per repository: one entry per `owner/name`.
#[derive(serde::Serialize)]
pub struct RepositoryScope {
    /// The repository the state is about.
    pub repository: String,
}

/// A value the Count keeps between runs, under its own namespace.
pub trait State: serde::Serialize + serde::de::DeserializeOwned {
    /// Names the stored shape. Never reused for a different shape.
    const NAMESPACE: &'static str;
    /// Bump when the stored shape or its meaning changes. For a
    /// code-versioned value, this names the code that derives it instead.
    const VERSION: u32;
    /// Whether the value is derived from an immutable input by this
    /// build's code (decoding, validation, extraction semantics), so
    /// `VERSION` names that code: the row in `tests/state-versions.tsv`
    /// hashes the sources and locked dependencies the derivation reads,
    /// and the owning crate's parity test fails on a change until the
    /// version is bumped. Entries survive releases whose derivation code
    /// did not change.
    const CODE_VERSIONED: bool = false;
    /// What one entry is about: a small serializable struct with named fields.
    type Scope: serde::Serialize;
}

/// Whether a name — a `State::NAMESPACE` or a derived representation's name —
/// is nonempty and leaves `@` and `+` to the key: `@v` starts the version,
/// and `+` marks the retired per-build keys migration 014 deletes.
pub const fn plain(name: &str) -> bool {
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' || bytes[i] == b'+' {
            return false;
        }
        i += 1;
    }
    !bytes.is_empty()
}

/// The `namespace` column of a state row: the shape's name and version —
/// the deriving code's version for a code-versioned state.
fn state_namespace<T: State>() -> String {
    const {
        assert!(
            plain(T::NAMESPACE),
            "a state namespace is nonempty and has no @ or +"
        )
    };
    format!("{}@v{}", T::NAMESPACE, T::VERSION)
}

type Job = Box<dyn FnOnce(&mut Store) + Send>;

/// Clone once per caller/task; a bounded queue prevents unlimited pending work.
#[derive(Clone)]
pub struct Cache {
    workspace_dir: Option<std::path::PathBuf>,
    jobs: mpsc::Sender<Job>,
    readers: Arc<Vec<mpsc::Sender<Job>>>,
    next_reader: Arc<AtomicUsize>,
    /// Whether reads may use the reader connections. Cleared for good once the
    /// store is found corrupt or falls back to memory: a rebuild moves the old
    /// file aside, and the readers still hold it open, so after that only the
    /// writer's connection sees the live database.
    readers_valid: Arc<AtomicBool>,
    touches: Arc<Mutex<HashSet<String>>>,
}

impl Cache {
    pub async fn open(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let workspace_dir = (path != Path::new(":memory:")).then(|| {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir().unwrap_or_default().join(&path)
            };
            absolute.with_file_name("worktrees")
        });
        let reader_path = path.clone();
        let touches = Arc::new(Mutex::new(HashSet::new()));
        let writer_touches = touches.clone();
        let readers_valid = Arc::new(AtomicBool::new(true));
        let writer_readers_valid = readers_valid.clone();
        let (jobs, mut receiver) = mpsc::channel::<Job>(128);
        let (ready, initialized) = oneshot::channel();
        std::thread::Builder::new()
            .name("patchula-cache".into())
            .spawn(move || match Store::open(path, options) {
                Ok(mut store) => {
                    let persistent = store.conn.path().is_some_and(|path| !path.is_empty());
                    if ready.send(Ok(persistent)).is_err() {
                        return;
                    }
                    while let Some(job) = receiver.blocking_recv() {
                        let touched =
                            std::mem::take(&mut *writer_touches.lock().expect("cache touches"));
                        if let Err(error) = store.touch(&touched) {
                            tracing::warn!(%error, "cache access accounting failed");
                        }
                        job(&mut store);
                        if !store.conn.path().is_some_and(|path| !path.is_empty()) {
                            writer_readers_valid.store(false, Ordering::Relaxed);
                        }
                    }
                    let touched =
                        std::mem::take(&mut *writer_touches.lock().expect("cache touches"));
                    let _ = store.touch(&touched);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })?;
        let persistent = initialized.await.map_err(|_| Error::Worker)??;
        let mut readers = Vec::new();
        // Read-only WAL connections neither migrate nor quarantine. Errors are
        // retried on the writer, which remains the sole recovery coordinator.
        // In-memory/fallback databases use the writer's connection instead.
        if persistent && reader_path != Path::new(":memory:") && reader_path.exists() {
            for index in 0..4 {
                let path = reader_path.clone();
                let (sender, mut receiver) = mpsc::channel::<Job>(32);
                let (ready, initialized) = oneshot::channel();
                std::thread::Builder::new()
                    .name(format!("patchula-cache-read-{index}"))
                    .spawn(move || {
                        let mut store = match Store::reader(&path) {
                            Ok(store) => store,
                            Err(_) => {
                                let _ = ready.send(false);
                                return;
                            }
                        };
                        if ready.send(true).is_err() {
                            return;
                        }
                        while let Some(job) = receiver.blocking_recv() {
                            job(&mut store);
                        }
                    })?;
                if !initialized.await.map_err(|_| Error::Worker)? {
                    readers.clear();
                    break;
                }
                readers.push(sender);
            }
        }
        Ok(Self {
            workspace_dir,
            jobs,
            readers: Arc::new(readers),
            next_reader: Arc::new(AtomicUsize::new(0)),
            readers_valid,
            touches,
        })
    }

    /// Opens a cache directory, creating it: `cache.sqlite` with `worktrees/`
    /// and the lockfile tool store, `tools/`, beside it, and `cache.lock`.
    pub async fn open_dir(dir: impl AsRef<Path>, options: Options) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        // Commands in other checkouts share the directory; opening checks and
        // migrates the database, so one process opens it at a time.
        let lock = std::fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("cache.lock"))?;
        let _lock = tokio::task::spawn_blocking(move || lock.lock().map(|()| lock))
            .await
            .map_err(|_| Error::Worker)??;
        Self::open(dir.join("cache.sqlite"), options).await
    }

    /// Persistent tool checkouts: `worktrees/` beside the SQLite file.
    pub fn workspace_dir(&self) -> Option<&Path> {
        self.workspace_dir.as_deref()
    }

    async fn read<T: Send + 'static>(
        &self,
        operation: impl Fn(&mut Store) -> Result<T> + Send + Sync + 'static,
    ) -> Result<T> {
        let operation = Arc::new(operation);
        if !self.readers.is_empty() && self.readers_valid.load(Ordering::Relaxed) {
            let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
            let (reply, response) = oneshot::channel();
            let read = operation.clone();
            match Self::dispatch(
                &self.readers[index],
                Box::new(move |store| {
                    let _ = reply.send(read(store));
                }),
                response,
            )
            .await
            {
                Ok(value) => return Ok(value),
                Err(error) if store::is_corrupt(&error) => {
                    self.readers_valid.store(false, Ordering::Relaxed);
                }
                Err(error) => return Err(error),
            }
        }
        self.call(move |store| operation(store)).await
    }

    async fn call<T: Send + 'static>(
        &self,
        mut operation: impl FnMut(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, response) = oneshot::channel();
        let readers_valid = self.readers_valid.clone();
        Self::dispatch(
            &self.jobs,
            Box::new(move |store| {
                let mut result = operation(store);
                if result.as_ref().is_err_and(store::is_corrupt) {
                    readers_valid.store(false, Ordering::Relaxed);
                    result = store.recover().and_then(|()| operation(store));
                }
                let _ = reply.send(result);
            }),
            response,
        )
        .await
    }

    /// Send `job` to a worker's channel and await its reply.
    ///
    /// Under `paused-clock` — the feature dev-dependencies of the crates
    /// whose tests pin run depth enable — the send and the reply wait run
    /// inside `tokio::task::spawn_blocking` instead. The workers are
    /// `std::thread`s the runtime does not count as work, so a plain
    /// `await` on the reply leaves Tokio's paused clock looking idle and
    /// free to auto-advance: whether a later request then lands before or
    /// after an earlier request's latency depends on real scheduling, and
    /// a measured wave slips either way. A blocking task on a
    /// current-thread runtime inhibits auto-advance, which holds the
    /// clock still until the reply arrives and keeps a run's depth exact.
    async fn dispatch<T: Send + 'static>(
        channel: &mpsc::Sender<Job>,
        job: Job,
        response: oneshot::Receiver<Result<T>>,
    ) -> Result<T> {
        #[cfg(feature = "paused-clock")]
        {
            let channel = channel.clone();
            tokio::task::spawn_blocking(move || -> Result<T> {
                channel.blocking_send(job).map_err(|_| Error::Worker)?;
                response.blocking_recv().map_err(|_| Error::Worker)?
            })
            .await
            .map_err(|_| Error::Worker)?
        }
        #[cfg(not(feature = "paused-clock"))]
        {
            channel.send(job).await.map_err(|_| Error::Worker)?;
            response.await.map_err(|_| Error::Worker)?
        }
    }

    pub async fn http_get(&self, key: &HttpKey) -> Result<Option<HttpEntry>> {
        let key = key.encoded()?;
        let read_key = key.clone();
        let result = self.read(move |store| store.http_get(&read_key)).await?;
        if result.is_some() {
            self.touches.lock().expect("cache touches").insert(key);
        }
        Ok(result)
    }

    pub async fn http_put(&self, key: &HttpKey, entry: &HttpEntry) -> Result<()> {
        let key = key.encoded()?;
        let entry = entry.clone();
        self.call(move |store| store.http_put(&key, &entry)).await
    }

    /// A 304 changes metadata, never the compressed body. Avoid recompressing
    /// and rewriting large registry documents after conditional validation.
    pub async fn http_refresh(&self, key: &HttpKey, entry: &HttpEntry) -> Result<()> {
        let key = key.encoded()?;
        let entry = HttpEntry {
            status: entry.status,
            etag: entry.etag.clone(),
            last_modified: entry.last_modified.clone(),
            headers: entry.headers.clone(),
            body: Vec::new(),
            fetched_at: entry.fetched_at,
            expires_at: entry.expires_at,
            cache_control: entry.cache_control.clone(),
        };
        self.call(move |store| store.http_refresh(&key, &entry))
            .await
    }

    pub async fn http_remove(&self, key: &HttpKey) -> Result<()> {
        let key = key.encoded()?;
        self.call(move |store| store.http_remove(&key)).await
    }

    /// Merge a newest-first page. Return true at the first item already present
    /// before this page, allowing the caller to stop pagination immediately.
    pub async fn merge_releases(
        &self,
        source: &str,
        package: &str,
        page: &[Release],
    ) -> Result<bool> {
        self.merge_release_page(source, package, page, None).await
    }

    /// `after` is the last inserted version from the preceding page of this
    /// refresh. None starts a refresh at the head, including a disjoint new page.
    pub async fn merge_release_page(
        &self,
        source: &str,
        package: &str,
        page: &[Release],
        after: Option<&str>,
    ) -> Result<bool> {
        let (source, package, page, after) = (
            source.to_owned(),
            package.to_owned(),
            page.to_vec(),
            after.map(str::to_owned),
        );
        self.call(move |store| store.merge_releases(&source, &package, &page, after.as_deref()))
            .await
    }

    pub async fn releases(&self, source: &str, package: &str) -> Result<ReleaseList> {
        let (source, package) = (source.to_owned(), package.to_owned());
        self.read(move |store| store.releases(&source, &package))
            .await
    }
    /// Read the refresh boundary without materializing the cached history.
    pub async fn release_head(&self, source: &str, package: &str) -> Result<Option<String>> {
        let (source, package) = (source.to_owned(), package.to_owned());
        self.read(move |store| store.release_head(&source, &package))
            .await
    }

    /// The value kept for `scope`, or None when there is none or it no longer
    /// decodes.
    pub async fn state<T: State + Send + 'static>(&self, scope: &T::Scope) -> Result<Option<T>> {
        let namespace = state_namespace::<T>();
        let scope = serde_json::to_string(scope)?;
        self.read(move |store| {
            let Some(bytes) = store.state(&namespace, &scope)? else {
                return Ok(None);
            };
            match serde_json::from_slice(&bytes) {
                Ok(value) => Ok(Some(value)),
                Err(_) => {
                    tracing::debug!(%namespace, "cache state does not decode; treating it as a miss");
                    Ok(None)
                }
            }
        })
        .await
    }

    /// Keep `value` for `scope`, replacing any earlier value.
    pub async fn set_state<T: State>(&self, scope: &T::Scope, value: &T) -> Result<()> {
        let namespace = state_namespace::<T>();
        let scope = serde_json::to_string(scope)?;
        let value = serde_json::to_vec(value)?;
        self.call(move |store| store.set_state(&namespace, &scope, &value))
            .await
    }

    /// Forget the value kept for `scope`.
    pub async fn clear_state<T: State>(&self, scope: &T::Scope) -> Result<()> {
        let namespace = state_namespace::<T>();
        let scope = serde_json::to_string(scope)?;
        self.call(move |store| store.clear_state(&namespace, &scope))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_fresh_cache_is_at_the_latest_schema() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let version = cache
            .call(|store| {
                Ok(store
                    .conn
                    .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))?)
            })
            .await
            .unwrap();
        assert_eq!(version, store::MIGRATIONS.len() as i64);
    }

    /// Migration 014 drops state rows keyed under the retired per-build
    /// scheme — a namespace ending `+<build>` — and keeps `name@v` entries.
    #[tokio::test]
    async fn migration_014_drops_per_build_state() {
        #[derive(Serialize, Deserialize)]
        struct Parsed(u32);
        #[derive(Serialize)]
        struct Scope {
            key: String,
        }
        impl State for Parsed {
            const NAMESPACE: &'static str = "test-parsed";
            const VERSION: u32 = 1;
            type Scope = Scope;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let scope = Scope { key: "k".into() };
        let cache = Cache::open(&path, Options::default()).await.unwrap();
        cache.set_state(&scope, &Parsed(1)).await.unwrap();
        cache
            .call(|store| {
                store.conn.execute(
                    "INSERT INTO state(namespace, scope, value) VALUES ('test-parsed@v1+0.0.1', '{}', '1'), ('plain@v1', '{}', '1')",
                    [],
                )?;
                store.conn.pragma_update(None, "user_version", 13)?;
                Ok(())
            })
            .await
            .unwrap();
        drop(cache);
        let cache = Cache::open(&path, Options::default()).await.unwrap();
        assert_eq!(
            cache.state::<Parsed>(&scope).await.unwrap().map(|p| p.0),
            Some(1)
        );
        let namespaces = cache
            .read(|store| {
                let mut statement = store
                    .conn
                    .prepare("SELECT namespace FROM state ORDER BY namespace")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap();
        assert_eq!(
            namespaces,
            ["plain@v1".to_string(), "test-parsed@v1".to_string()]
        );
    }

    /// What release A writes under a code-versioned namespace, release B
    /// reads back: the key carries `name@v` and no build stamp. A release
    /// that bumped the version — which the `state-versions.tsv` guard
    /// forces on a derivation change — misses as it must.
    #[tokio::test]
    async fn code_versioned_state_survives_a_release() {
        #[derive(Serialize)]
        struct Scope {
            key: String,
        }
        macro_rules! release {
            ($name:ident, $version:expr) => {
                #[derive(Serialize, Deserialize)]
                struct $name(u32);
                impl State for $name {
                    const NAMESPACE: &'static str = "test-derived";
                    const VERSION: u32 = $version;
                    const CODE_VERSIONED: bool = true;
                    type Scope = Scope;
                }
            };
        }
        release!(ReleaseA, 1);
        release!(ReleaseB, 1);
        release!(ReleaseC, 2);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let scope = Scope { key: "k".into() };
        let cache = Cache::open(&path, Options::default()).await.unwrap();
        cache.set_state(&scope, &ReleaseA(1)).await.unwrap();
        drop(cache);
        let cache = Cache::open(&path, Options::default()).await.unwrap();
        assert_eq!(
            cache.state::<ReleaseB>(&scope).await.unwrap().map(|v| v.0),
            Some(1)
        );
        assert_eq!(
            cache.state::<ReleaseC>(&scope).await.unwrap().map(|v| v.0),
            None
        );
        let namespaces = cache
            .read(|store| {
                let mut statement = store
                    .conn
                    .prepare("SELECT namespace FROM state ORDER BY namespace")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap();
        assert_eq!(namespaces, ["test-derived@v1".to_string()]);
    }

    /// Migration 014 drops derived bodies keyed under the retired per-build
    /// scheme — a representation ending `+<build>` — and keeps `@v`-versioned
    /// entries and `+` elsewhere in a key.
    #[tokio::test]
    async fn migration_014_drops_per_build_http_representations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let cache = Cache::open(&path, Options::default()).await.unwrap();
        let entry = HttpEntry {
            status: 200,
            etag: None,
            last_modified: None,
            headers: BTreeMap::new(),
            body: b"body".to_vec(),
            fetched_at: 0,
            expires_at: None,
            cache_control: None,
        };
        fn key(representation: Option<&str>, credential: Option<&str>) -> String {
            let mut key = HttpKey::new("GET", "https://example.org/a+b", BTreeMap::new()).unwrap();
            if let Some(representation) = representation {
                key = key.representation(representation);
            }
            if let Some(credential) = credential {
                key = key.credential(credential);
            }
            key.encoded().unwrap()
        }
        cache
            .call(move |store| {
                // The retired scheme, and the schemes in use.
                for key in [
                    key(Some("doc+0.0.1"), None),
                    key(Some("doc@v1"), None),
                    key(Some("doc@v1"), Some("a+b")),
                    key(None, None),
                ] {
                    store.http_put(&key, &entry)?;
                }
                store.conn.pragma_update(None, "user_version", 13)?;
                Ok(())
            })
            .await
            .unwrap();
        drop(cache);
        let cache = Cache::open(&path, Options::default()).await.unwrap();
        let keys = cache
            .read(|store| {
                let mut statement = store.conn.prepare("SELECT key FROM http ORDER BY key")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
            })
            .await
            .unwrap();
        let mut expected = [
            key(Some("doc@v1"), None),
            key(Some("doc@v1"), Some("a+b")),
            key(None, None),
        ];
        expected.sort();
        assert_eq!(keys, expected);
    }

    #[tokio::test]
    async fn state_that_does_not_decode_is_a_miss() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Parsed(u32);
        #[derive(Serialize)]
        struct Scope {
            key: String,
        }
        impl State for Parsed {
            const NAMESPACE: &'static str = "test-corrupt";
            const VERSION: u32 = 1;
            type Scope = Scope;
        }
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Options::default())
            .await
            .unwrap();
        let scope = Scope { key: "k".into() };
        cache.set_state(&scope, &Parsed(1)).await.unwrap();
        cache
            .call(|store| {
                // Bytes that are not the value's JSON, stored as a blob the
                // way set_state writes them.
                store.conn.execute(
                    "UPDATE state SET value=x'6e6f74206a736f6e' WHERE namespace=?1",
                    rusqlite::params!["test-corrupt@v1"],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(cache.state::<Parsed>(&scope).await.unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn commands_opening_a_new_cache_together_all_succeed() {
        for _ in 0..10 {
            let directory = tempfile::tempdir().unwrap();
            let opens: Vec<_> = (0..8)
                .map(|_| {
                    let dir = directory.path().to_owned();
                    tokio::spawn(async move { Cache::open_dir(dir, Options::default()).await })
                })
                .collect();
            for open in opens {
                open.await.unwrap().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn read_decoders_run_concurrently_while_writer_stays_available() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Options::default())
            .await
            .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let cache = cache.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                cache
                    .read(move |store| {
                        // Each read holds its worker until all four have started.
                        barrier.wait();
                        Ok(store
                            .conn
                            .query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?)
                    })
                    .await
                    .unwrap()
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            #[derive(Serialize, Deserialize)]
            struct Probe(String);
            #[derive(Serialize)]
            struct ProbeScope {
                key: String,
            }
            impl State for Probe {
                const NAMESPACE: &'static str = "test-probe";
                const VERSION: u32 = 1;
                type Scope = ProbeScope;
            }
            cache
                .set_state(&ProbeScope { key: "data".into() }, &Probe("value".into()))
                .await
                .unwrap();
            for task in tasks {
                assert_eq!(task.await.unwrap(), 1);
            }
        })
        .await
        .expect("read workers must not serialize or block the writer");
    }
}
