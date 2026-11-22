use crate::{Error, HttpEntry, Opened, Options, Origin, Release, ReleaseList, Result};

/// Each migration sets `user_version` to its own number; a cache at version
/// N runs the ones after it.
pub(crate) const MIGRATIONS: [&str; 17] = [
    include_str!("migrations/001.sql"),
    include_str!("migrations/002.sql"),
    include_str!("migrations/003.sql"),
    include_str!("migrations/004.sql"),
    include_str!("migrations/005.sql"),
    include_str!("migrations/006.sql"),
    include_str!("migrations/007.sql"),
    include_str!("migrations/008.sql"),
    include_str!("migrations/009.sql"),
    include_str!("migrations/010.sql"),
    include_str!("migrations/011.sql"),
    include_str!("migrations/012.sql"),
    include_str!("migrations/013.sql"),
    include_str!("migrations/014.sql"),
    include_str!("migrations/015.sql"),
    include_str!("migrations/016.sql"),
    include_str!("migrations/017.sql"),
];
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

/// The state rows under a tabled name (`?1`: `[name, current key]` pairs)
/// whose namespace is not the name's current key, returning each deleted
/// row's key.
pub(crate) const PRUNE_STATE: &str = "\
DELETE FROM state WHERE rowid IN (
  SELECT state.rowid FROM json_each(?1) AS row JOIN state
    ON state.namespace >= json_extract(row.value, '$[0]') || '@v'
   AND state.namespace < json_extract(row.value, '$[0]') || '@w'
  WHERE state.namespace <> json_extract(row.value, '$[1]')
) RETURNING namespace";

/// The HTTP rows whose derived representation is under a tabled name
/// (`?1`: `[name, current key]` pairs) but is not its current key,
/// returning each deleted row's representation.
pub(crate) const PRUNE_HTTP: &str = "\
DELETE FROM http WHERE rowid IN (
  SELECT http.rowid FROM json_each(?1) AS row JOIN http
    ON http.representation >= json_extract(row.value, '$[0]') || '@v'
   AND http.representation < json_extract(row.value, '$[0]') || '@w'
  WHERE http.representation <> json_extract(row.value, '$[1]')
) RETURNING representation";
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
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "journal_size_limit", 16 * 1024 * 1024)?;
    // The version is read under the write lock, so processes opening a new
    // cache together migrate it once.
    let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > MIGRATIONS.len() as i64 {
        return Err(Error::FutureSchema(version));
    }
    for migration in MIGRATIONS.iter().skip(version.max(0) as usize) {
        transaction.execute_batch(migration)?;
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
    /// Open the cache at `path` and report what was found: where it came
    /// from, what the prune and the budget dropped, what is left, and how
    /// long since the cache was last opened.
    pub fn open(path: PathBuf, options: Options) -> Result<(Self, Opened)> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut origin = if path == Path::new(":memory:") {
            Origin::Memory
        } else if std::fs::metadata(&path).is_ok_and(|file| file.len() > 0) {
            Origin::Existing
        } else {
            Origin::Created
        };
        let conn = match Connection::open(&path)
            .map_err(Error::from)
            .and_then(initialize)
        {
            Ok(conn) => conn,
            Err(error) if is_corrupt(&error) => {
                origin = Origin::Rebuilt;
                Self::rebuild(&path)?
            }
            Err(error) => return Err(error),
        };
        if !conn.path().is_some_and(|path| !path.is_empty()) {
            origin = Origin::Memory;
        }
        let store = Self {
            conn,
            path,
            options,
        };
        let superseded = store.prune()?;
        let evicted = store.evict()?;
        let opened = store.survey(origin, superseded, evicted)?;
        Ok((store, opened))
    }

    /// What is left after the prune and eviction, and the time since the
    /// cache was last opened, which this open then records. Every count
    /// reads an index or a trigger-kept row: the HTTP timestamps sit after
    /// each body, so reading them would page through every body.
    fn survey(
        &self,
        origin: Origin,
        superseded: std::collections::BTreeMap<String, u64>,
        evicted: u64,
    ) -> Result<Opened> {
        let count = |sql: &str| -> Result<u64> {
            let count: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
            Ok(u64::try_from(count).unwrap_or(0))
        };
        let now = now();
        let last: Option<i64> = self
            .conn
            .query_row("SELECT at FROM opened", [], |row| row.get(0))
            .optional()?;
        self.conn.execute(
            "INSERT INTO opened(id, at) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET at=excluded.at",
            [now],
        )?;
        Ok(Opened {
            origin,
            superseded,
            evicted,
            responses: count("SELECT COUNT(*) FROM http")?,
            bytes: count("SELECT total FROM http_bytes")?,
            values: count(
                "SELECT (SELECT COUNT(*) FROM state) + (SELECT COUNT(*) FROM release_heads)",
            )?,
            idle: last.and_then(|last| u64::try_from(now - last).ok()),
        })
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
        self.evict().map(|_| ())
    }

    /// Write a 304's metadata over the entry, leaving its body untouched.
    pub fn http_refresh(&mut self, key: &str, entry: &HttpEntry) -> Result<()> {
        self.conn.execute(
            "UPDATE http SET etag=?2,last_modified=?3,headers=?4,fetched_at=?5,expires_at=?6,\
             cache_control=?7 WHERE key=?1",
            params![
                key,
                entry.etag,
                entry.last_modified,
                serde_json::to_string(&entry.headers)?,
                entry.fetched_at,
                entry.expires_at,
                entry.cache_control
            ],
        )?;
        Ok(())
    }

    pub fn http_remove(&mut self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM http WHERE key = ?1", [key])?;
        Ok(())
    }

    pub fn state(&self, namespace: &str, scope: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM state WHERE namespace=?1 AND scope=?2",
                params![namespace, scope],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_state(&mut self, namespace: &str, scope: &str, value: &[u8]) -> Result<()> {
        self.conn.execute(
            "INSERT INTO state(namespace,scope,value) VALUES (?1,?2,?3) \
             ON CONFLICT(namespace,scope) DO UPDATE SET value=excluded.value",
            params![namespace, scope, value],
        )?;
        Ok(())
    }

    pub fn set_states(&mut self, namespace: &str, rows: &[(String, Vec<u8>)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (scope, value) in rows {
            tx.execute(
                "INSERT INTO state(namespace,scope,value) VALUES (?1,?2,?3) \
                 ON CONFLICT(namespace,scope) DO UPDATE SET value=excluded.value",
                params![namespace, scope, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn clear_state(&mut self, namespace: &str, scope: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM state WHERE namespace=?1 AND scope=?2",
            params![namespace, scope],
        )?;
        Ok(())
    }

    /// Delete every state row and derived HTTP body kept under a tabled
    /// name's key other than the one this build reads: an older version or
    /// an older dependency fingerprint is never read again. One indexed
    /// join per table: each name's keys are the range `name@v..name@w`
    /// (names hold no `@`), found through the state primary key and the
    /// `http_representation` index.
    fn prune(&self) -> Result<std::collections::BTreeMap<String, u64>> {
        let mut superseded = std::collections::BTreeMap::new();
        for (sql, rows) in [
            (PRUNE_STATE, crate::STATE_ROWS),
            (PRUNE_HTTP, crate::DERIVED_ROWS),
        ] {
            let mut statement = self.conn.prepare(sql)?;
            let mut deleted = statement.query([crate::current_keys(rows)])?;
            while let Some(row) = deleted.next()? {
                let key: String = row.get(0)?;
                let name = key.split_once("@v").map_or(key.as_str(), |(name, _)| name);
                *superseded.entry(name.to_owned()).or_default() += 1;
            }
        }
        if !superseded.is_empty() {
            tracing::debug!(?superseded, "pruned superseded cache keys");
        }
        Ok(superseded)
    }

    /// Evict the least recently used HTTP rows until the bodies fit the
    /// budget. The total is the trigger-kept `http_bytes` row (migration
    /// 016), read again after each delete, so no write scans the table and
    /// a concurrent process's writes count too.
    fn evict(&self) -> Result<u64> {
        let mut evicted = 0;
        loop {
            let total: i64 = self
                .conn
                .query_row("SELECT total FROM http_bytes", [], |row| row.get(0))?;
            if u64::try_from(total).unwrap_or(0) <= self.options.http_body_budget {
                return Ok(evicted);
            }
            let Some(key) = self
                .conn
                .query_row(
                    "SELECT key FROM http ORDER BY last_access,key LIMIT 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(evicted);
            };
            self.conn.execute("DELETE FROM http WHERE key=?1", [key])?;
            evicted += 1;
        }
    }

    /// The refresh boundary, without materializing the cached history.
    pub fn release_head(&self, source: &str, package: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT newest FROM release_heads WHERE source=?1 AND package=?2",
                params![source, package],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn releases(&self, source: &str, package: &str) -> Result<ReleaseList> {
        let newest = self.release_head(source, package)?;
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

/// UTC Unix seconds.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}
