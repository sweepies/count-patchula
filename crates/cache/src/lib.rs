//! Batch-shared SQLite cache. All database/compression work runs on one thread.
#![forbid(unsafe_code)]

mod store;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
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
    #[error("cache schema {0} is newer than supported schema 2")]
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
    jobs: mpsc::Sender<Job>,
}

impl Cache {
    pub async fn open(path: impl AsRef<Path>, options: Options) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let (jobs, mut receiver) = mpsc::channel::<Job>(128);
        let (ready, initialized) = oneshot::channel();
        std::thread::Builder::new()
            .name("rustovate-cache".into())
            .spawn(move || match Store::open(path, options) {
                Ok(mut store) => {
                    if ready.send(Ok(())).is_err() {
                        return;
                    }
                    while let Some(job) = receiver.blocking_recv() {
                        job(&mut store);
                    }
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })?;
        initialized.await.map_err(|_| Error::Worker)??;
        Ok(Self { jobs })
    }

    async fn call<T: Send + 'static>(
        &self,
        mut operation: impl FnMut(&mut Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, response) = oneshot::channel();
        self.jobs
            .send(Box::new(move |store| {
                let mut result = operation(store);
                if result.as_ref().is_err_and(store::is_corrupt) {
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
        self.call(move |store| store.http_get(&key)).await
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
        self.call(move |store| store.releases(&source, &package))
            .await
    }
    /// Read the refresh boundary without materializing the cached history.
    pub async fn release_head(&self, source: &str, package: &str) -> Result<Option<String>> {
        let (source, package) = (source.to_owned(), package.to_owned());
        self.call(move |store| {
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
        self.call(move |store| store.extraction_get(&key)).await
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
