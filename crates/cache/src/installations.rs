//! Installation identity cache, namespaced by App and target (repos/ or orgs/).
use crate::{Cache, Result};
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Copy)]
pub struct Mapping {
    pub installation: u64,
    /// UTC Unix seconds, shared by memory and disk without extending the TTL.
    pub expires_at: i64,
}
impl Cache {
    pub async fn installation_get(
        &self,
        app: u64,
        target: String,
        now: i64,
    ) -> Result<Option<Mapping>> {
        self.read(move |store| {
            Ok(store.conn.query_row(
                "SELECT installation,expires_at FROM server_installations WHERE app=?1 AND target=?2 AND expires_at>?3",
                params![app as i64, target, now],
                |row| Ok(Mapping { installation: row.get::<_, i64>(0)? as u64, expires_at: row.get(1)? }),
            ).optional()?)
        }).await
    }
    pub async fn installation_put(
        &self,
        app: u64,
        target: String,
        mapping: Mapping,
        now: i64,
    ) -> Result<()> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            tx.execute("DELETE FROM server_installations WHERE expires_at<=?1", [now])?;
            tx.execute("INSERT INTO server_installations VALUES (?1,?2,?3,?4) ON CONFLICT(app,target) DO UPDATE SET installation=excluded.installation,expires_at=excluded.expires_at", params![app as i64, target, mapping.installation as i64, mapping.expires_at])?;
            tx.commit()?;
            Ok(())
        }).await
    }
    pub async fn installation_remove(&self, app: u64, target: String) -> Result<()> {
        self.call(move |store| {
            store.conn.execute(
                "DELETE FROM server_installations WHERE app=?1 AND target=?2",
                params![app as i64, target],
            )?;
            Ok(())
        })
        .await
    }
}
