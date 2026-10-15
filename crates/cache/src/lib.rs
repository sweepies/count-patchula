//! Batch-shared SQLite cache with bounded parallel readers and one writer.
#![forbid(unsafe_code)]

pub mod queue;
mod store;

use rusqlite::OptionalExtension;
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
    #[error("cache schema {0} is newer than supported schema 5")]
    FutureSchema(i64),
    #[error("cache integrity check failed")]
    Corrupt,
    #[error("release page anchor is not in cached history")]
    ReleaseAnchor,
}

pub type Result<T> = std::result::Result<T, Error>;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractionKey {
    pub repo: String,
    pub default_branch_commit: String,
    pub config_hash: String,
}

type Job = Box<dyn FnOnce(&mut Store) + Send>;

/// Clone once per caller/task; a bounded queue prevents unlimited pending work.
#[derive(Clone)]
pub struct Cache {
    workspace_dir: Option<std::path::PathBuf>,
    jobs: mpsc::Sender<Job>,
    readers: Arc<Vec<mpsc::Sender<Job>>>,
    next_reader: Arc<AtomicUsize>,
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
            absolute.with_extension("worktrees")
        });
        let reader_path = path.clone();
        let touches = Arc::new(Mutex::new(HashSet::new()));
        let writer_touches = touches.clone();
        let readers_valid = Arc::new(AtomicBool::new(true));
        let writer_readers_valid = readers_valid.clone();
        let (jobs, mut receiver) = mpsc::channel::<Job>(128);
        let (ready, initialized) = oneshot::channel();
        std::thread::Builder::new()
            .name("rustovate-cache".into())
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
                    .name(format!("rustovate-cache-read-{index}"))
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

    /// Persistent tool checkouts sit beside the SQLite cache, never in a repo.
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
            self.readers[index]
                .send(Box::new(move |store| {
                    let _ = reply.send(read(store));
                }))
                .await
                .map_err(|_| Error::Worker)?;
            match response.await.map_err(|_| Error::Worker)? {
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
        self.jobs
            .send(Box::new(move |store| {
                let mut result = operation(store);
                if result.as_ref().is_err_and(store::is_corrupt) {
                    readers_valid.store(false, Ordering::Relaxed);
                    result = store.recover().and_then(|()| operation(store));
                }
                let _ = reply.send(result);
            }))
            .await
            .map_err(|_| Error::Worker)?;
        response.await.map_err(|_| Error::Worker)?
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
        self.call(move |store| {
            store.conn.execute("UPDATE http SET etag=?2,last_modified=?3,headers=?4,fetched_at=?5,expires_at=?6,cache_control=?7 WHERE key=?1",
                rusqlite::params![key,entry.etag,entry.last_modified,serde_json::to_string(&entry.headers)?,entry.fetched_at,entry.expires_at,entry.cache_control])?;
            Ok(())
        }).await
    }

    pub async fn http_remove(&self, key: &HttpKey) -> Result<()> {
        let key = key.encoded()?;
        self.call(move |store| {
            store
                .conn
                .execute("DELETE FROM http WHERE key = ?1", [&key])?;
            Ok(())
        })
        .await
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
        self.read(move |store| {
            Ok(store
                .conn
                .query_row(
                    "SELECT newest FROM release_heads WHERE source=?1 AND package=?2",
                    rusqlite::params![source, package],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    pub async fn extraction_get(&self, key: &ExtractionKey) -> Result<Option<Vec<u8>>> {
        let key = key.clone();
        self.read(move |store| store.extraction_get(&key)).await
    }

    pub async fn extraction_put(&self, key: &ExtractionKey, serialized: &[u8]) -> Result<()> {
        let (key, serialized) = (key.clone(), serialized.to_vec());
        self.call(move |store| {
            store.conn.execute("INSERT INTO extraction(repo, branch_commit, config_hash, result) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(repo, branch_commit, config_hash) DO UPDATE SET result=excluded.result",
                rusqlite::params![key.repo, key.default_branch_commit, key.config_hash, serialized])?;
            Ok(())
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            cache
                .extraction_put(
                    &ExtractionKey {
                        repo: "repo".into(),
                        default_branch_commit: "sha".into(),
                        config_hash: "config".into(),
                    },
                    b"data",
                )
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
