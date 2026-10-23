use crate::{Error, ExtractionKey, HttpEntry, Options, Release, ReleaseList, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) struct Store {
    pub conn: Connection,
    path: PathBuf,
    options: Options,
}

pub(super) fn is_corrupt(error: &Error) -> bool {
    match error {
        Error::Corrupt => true,
        Error::Database(error) => matches!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase)
        ),
        _ => false,
    }
}

fn initialize(mut conn: Connection) -> Result<Connection> {
    conn.busy_timeout(Duration::from_secs(5))?;
    let integrity: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(Error::Corrupt);
    }
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > 8 {
        return Err(Error::FutureSchema(version));
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "journal_size_limit", 16 * 1024 * 1024)?;
    let transaction = conn.transaction()?;
    if version < 1 {
        transaction.execute_batch(include_str!("migrations/001.sql"))?;
    }
    if version < 2 {
        transaction.execute_batch(include_str!("migrations/002.sql"))?;
    }
    if version < 3 {
        transaction.execute_batch(include_str!("migrations/003.sql"))?;
    }
    if version < 4 {
        transaction.execute_batch(include_str!("migrations/004.sql"))?;
    }
    if version < 5 {
        transaction.execute_batch(include_str!("migrations/005.sql"))?;
    }
    if version < 6 {
        transaction.execute_batch(include_str!("migrations/006.sql"))?;
    }
    if version < 7 {
        transaction.execute_batch(include_str!("migrations/007.sql"))?;
    }
    if version < 8 {
        transaction.execute_batch(include_str!("migrations/008.sql"))?;
    }
    transaction.commit()?;
    Ok(conn)
}

impl Store {
    pub fn reader(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(Self {
            conn,
            path: path.into(),
            options: Options::default(),
        })
    }

    pub fn touch(&mut self, keys: &HashSet<String>) -> Result<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for key in keys {
            tx.execute("UPDATE access_clock SET value=value+1", [])?;
            tx.execute(
                "UPDATE http SET last_access=(SELECT value FROM access_clock) WHERE key=?1",
                [key],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn open(path: PathBuf, options: Options) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let conn = match Connection::open(&path)
            .map_err(Error::from)
            .and_then(initialize)
        {
            Ok(conn) => conn,
            Err(error) if is_corrupt(&error) => Self::rebuild(&path)?,
            Err(error) => return Err(error),
        };
        let store = Self {
            conn,
            path,
            options,
        };
        store.evict()?;
        Ok(store)
    }

    fn rebuild(path: &Path) -> Result<Connection> {
        tracing::warn!("corrupt cache detected; quarantining and rebuilding");
        let quarantine = quarantine(path);
        match quarantine
            .and_then(|()| Connection::open(path).map_err(Error::from))
            .and_then(initialize)
        {
            Ok(conn) => Ok(conn),
            Err(_) => {
                // Cache is expendable. Preserve the damaged file and keep the batch alive.
                tracing::warn!("cache rebuild failed; continuing with an in-memory cache");
                initialize(Connection::open_in_memory()?)
            }
        }
    }

    pub fn recover(&mut self) -> Result<()> {
        let temporary = Connection::open_in_memory()?;
        let old = std::mem::replace(&mut self.conn, temporary);
        drop(old); // close WAL before moving the database and its sidecars
        self.conn = Self::rebuild(&self.path)?;
        Ok(())
    }

    pub fn http_get(&mut self, key: &str) -> Result<Option<HttpEntry>> {
        let entry = self.conn.query_row("SELECT status, etag, last_modified, headers, body, fetched_at, expires_at, cache_control FROM http WHERE key=?1", [key], |row| {
            Ok((row.get::<_, u16>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?, row.get::<_, Vec<u8>>(4)?, row.get::<_, i64>(5)?, row.get::<_, Option<i64>>(6)?, row.get::<_, Option<String>>(7)?))
        }).optional()?;
        let Some((
            status,
            etag,
            last_modified,
            headers,
            body,
            fetched_at,
            expires_at,
            cache_control,
        )) = entry
        else {
            return Ok(None);
        };
        let body = match zstd::stream::decode_all(body.as_slice()) {
            Ok(body) => body,
            Err(_) => {
                tracing::warn!("invalid compressed HTTP cache entry; treating as a miss");
                return Ok(None);
            }
        };
        Ok(Some(HttpEntry {
            status,
            etag,
            last_modified,
            headers: serde_json::from_str(&headers)?,
            body,
            fetched_at,
            expires_at,
            cache_control,
        }))
    }

    pub fn http_put(&mut self, key: &str, entry: &HttpEntry) -> Result<()> {
        let body = zstd::stream::encode_all(entry.body.as_slice(), 3)?;
        let headers = serde_json::to_string(&entry.headers)?;
        let tx = self.conn.transaction()?;
        tx.execute("UPDATE access_clock SET value=value+1", [])?;
        tx.execute("INSERT INTO http(key,status,etag,last_modified,headers,body,fetched_at,expires_at,cache_control,last_access) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,(SELECT value FROM access_clock)) ON CONFLICT(key) DO UPDATE SET status=excluded.status,etag=excluded.etag,last_modified=excluded.last_modified,headers=excluded.headers,body=excluded.body,fetched_at=excluded.fetched_at,expires_at=excluded.expires_at,cache_control=excluded.cache_control,last_access=excluded.last_access",
            params![key, entry.status, entry.etag, entry.last_modified, headers, body, entry.fetched_at, entry.expires_at, entry.cache_control])?;
        tx.commit()?;
        self.evict()
    }

    fn evict(&self) -> Result<()> {
        let mut total: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(length(body)),0) FROM http",
            [],
            |row| row.get(0),
        )?;
        while total as u64 > self.options.http_body_budget {
            let (key, size): (String, i64) = self.conn.query_row(
                "SELECT key,length(body) FROM http ORDER BY last_access,key LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            self.conn.execute("DELETE FROM http WHERE key=?1", [key])?;
            total = total.saturating_sub(size);
        }
        Ok(())
    }

    pub fn releases(&self, source: &str, package: &str) -> Result<ReleaseList> {
        let newest = self
            .conn
            .query_row(
                "SELECT newest FROM release_heads WHERE source=?1 AND package=?2",
                params![source, package],
                |row| row.get(0),
            )
            .optional()?;
        let mut query = self.conn.prepare("SELECT version,published_at,digest FROM releases WHERE source=?1 AND package=?2 ORDER BY position")?;
        let releases = query
            .query_map(params![source, package], |row| {
                Ok(Release {
                    version: row.get(0)?,
                    published_at: row.get(1)?,
                    digest: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        Ok(ReleaseList { newest, releases })
    }

    pub fn merge_releases(
        &mut self,
        source: &str,
        package: &str,
        page: &[Release],
        after: Option<&str>,
    ) -> Result<bool> {
        // Keep upstream order; semver sorting loses dates, tags and prerelease identity.
        let tx = self.conn.transaction()?;
        let mut reached_known = false;
        let mut seen = HashSet::new();
        let mut incoming = Vec::new();
        for release in page {
            let known: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM releases WHERE source=?1 AND package=?2 AND version=?3)", params![source,package,release.version], |row| row.get(0))?;
            if known {
                reached_known = true;
                tx.execute("UPDATE releases SET published_at=COALESCE(?4,published_at),digest=COALESCE(?5,digest) WHERE source=?1 AND package=?2 AND version=?3", params![source,package,release.version,release.published_at,release.digest])?;
                break;
            }
            if seen.insert(&release.version) {
                incoming.push(release);
            }
        }
        let start = if let Some(anchor) = after {
            let position: i64 = tx
                .query_row(
                    "SELECT position FROM releases WHERE source=?1 AND package=?2 AND version=?3",
                    params![source, package, anchor],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(Error::ReleaseAnchor)?;
            tx.execute("UPDATE releases SET position=position+?3 WHERE source=?1 AND package=?2 AND position>?4", params![source,package,incoming.len() as i64,position])?;
            position + 1
        } else {
            let minimum: i64 = tx.query_row(
                "SELECT COALESCE(MIN(position),0) FROM releases WHERE source=?1 AND package=?2",
                params![source, package],
                |row| row.get(0),
            )?;
            minimum - incoming.len() as i64
        };
        for (index, release) in incoming.iter().enumerate() {
            tx.execute("INSERT INTO releases(source,package,version,published_at,digest,position) VALUES (?1,?2,?3,?4,?5,?6)", params![source,package,release.version,release.published_at,release.digest,start+index as i64])?;
        }
        let newest: Option<String> = tx.query_row("SELECT version FROM releases WHERE source=?1 AND package=?2 ORDER BY position LIMIT 1", params![source,package], |row| row.get(0)).optional()?;
        if let Some(newest) = newest {
            tx.execute("INSERT INTO release_heads(source,package,newest) VALUES (?1,?2,?3) ON CONFLICT(source,package) DO UPDATE SET newest=excluded.newest", params![source,package,newest])?;
        }
        tx.commit()?;
        Ok(reached_known)
    }

    pub fn extraction_get(&self, key: &ExtractionKey) -> Result<Option<Vec<u8>>> {
        Ok(self.conn.query_row("SELECT result FROM extraction WHERE repo=?1 AND branch_commit=?2 AND config_hash=?3", params![key.repo,key.default_branch_commit,key.config_hash], |row| row.get(0)).optional()?)
    }
}

fn quarantine(path: &Path) -> Result<()> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let backup = PathBuf::from(format!("{}.corrupt-{stamp}", path.display()));
    std::fs::rename(path, &backup)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        match std::fs::rename(sidecar, format!("{}{suffix}", backup.display())) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
