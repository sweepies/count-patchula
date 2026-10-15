//! Durable single-process queue and transactional webhook delivery deduplication.
use crate::{Cache, Result};
use rusqlite::{OptionalExtension, params};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Work {
    pub installation: u64,
    /// Empty means discover repositories for this installation (not an engine batch).
    pub repo: String,
}
#[derive(Clone, Debug)]
pub struct Claim {
    pub work: Work,
    generation: i64,
}

impl Cache {
    /// Atomically accept a delivery and persist its work before acknowledging it.
    /// Delivery IDs are retained for seven days, covering normal GitHub redelivery.
    pub async fn enqueue(
        &self,
        delivery: Option<String>,
        work: Vec<Work>,
        now: i64,
        coalesce_ms: i64,
    ) -> Result<bool> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            tx.execute("DELETE FROM server_deliveries WHERE received_at < ?1", [now - 7 * 86400 * 1000])?;
            if let Some(delivery) = &delivery {
                let inserted = tx.execute("INSERT OR IGNORE INTO server_deliveries(delivery,received_at) VALUES (?1,?2)", params![delivery, now])?;
                if inserted == 0 { return Ok(false); }
            }
            for item in &work {
                tx.execute("INSERT INTO server_work(installation,repo,available_at) VALUES (?1,?2,?3) ON CONFLICT(installation,repo) DO UPDATE SET generation=server_work.generation+1",
                    params![item.installation as i64, item.repo, now + coalesce_ms])?;
            }
            tx.commit()?;
            Ok(true)
        }).await
    }
    /// One installation per batch so every repository runs with its own App token.
    pub async fn claim_batch(&self, now: i64, limit: usize) -> Result<Vec<Claim>> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            let installation: Option<i64> = tx.query_row("SELECT installation FROM server_work WHERE claimed IS NULL AND available_at<=?1 ORDER BY available_at,installation,repo LIMIT 1", [now], |r| r.get(0)).optional()?;
            let Some(installation) = installation else { return Ok(Vec::new()); };
            let rows = {
                let mut query = tx.prepare("SELECT repo,generation FROM server_work WHERE claimed IS NULL AND available_at<=?1 AND installation=?2 ORDER BY available_at,repo LIMIT ?3")?;
                query.query_map(params![now, installation, limit as i64], |r| Ok(Claim {
                    work: Work { installation: installation as u64, repo: r.get(0)? }, generation: r.get(1)?,
                }))?.collect::<std::result::Result<Vec<_>, _>>()?
            };
            for claim in &rows {
                tx.execute("UPDATE server_work SET claimed=generation WHERE installation=?1 AND repo=?2", params![installation, claim.work.repo])?;
            }
            tx.commit()?;
            Ok(rows)
        }).await
    }
    pub async fn finish_work(
        &self,
        claims: Vec<Claim>,
        success: bool,
        now: i64,
        delay_ms: i64,
    ) -> Result<()> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            for claim in &claims {
                if success {
                    tx.execute("DELETE FROM server_work WHERE installation=?1 AND repo=?2 AND generation=?3 AND claimed=?3", params![claim.work.installation as i64, claim.work.repo, claim.generation])?;
                }
                tx.execute("UPDATE server_work SET claimed=NULL,available_at=?4 WHERE installation=?1 AND repo=?2 AND claimed=?3", params![claim.work.installation as i64, claim.work.repo, claim.generation, now + delay_ms])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }
    /// Called once under the server's exclusive process lock, including on restart.
    pub async fn recover_work(&self) -> Result<()> {
        self.call(|store| {
            store
                .conn
                .execute("UPDATE server_work SET claimed=NULL", [])?;
            Ok(())
        })
        .await
    }
    pub async fn queue_len(&self) -> Result<usize> {
        self.call(|store| {
            Ok(store
                .conn
                .query_row("SELECT count(*) FROM server_work", [], |r| {
                    r.get::<_, i64>(0)
                })? as usize)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn work() -> Vec<Work> {
        vec![Work {
            installation: 1,
            repo: "owner/repo".into(),
        }]
    }
    #[tokio::test]
    async fn dedupe_coalesce_and_generation_fence() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        assert!(
            cache
                .enqueue(Some("delivery".into()), work(), 100, 10)
                .await
                .unwrap()
        );
        assert!(
            !cache
                .enqueue(Some("delivery".into()), work(), 100, 10)
                .await
                .unwrap()
        );
        cache.enqueue(None, work(), 105, 10).await.unwrap();
        assert_eq!(cache.queue_len().await.unwrap(), 1);
        assert!(cache.claim_batch(109, 10).await.unwrap().is_empty());
        let batch = cache.claim_batch(110, 10).await.unwrap();
        cache.enqueue(None, work(), 111, 10).await.unwrap();
        cache.enqueue(None, work(), 112, 10).await.unwrap();
        assert!(cache.claim_batch(130, 10).await.unwrap().is_empty());
        cache.finish_work(batch, true, 120, 10).await.unwrap();
        assert!(cache.claim_batch(129, 10).await.unwrap().is_empty());
        let batch = cache.claim_batch(130, 10).await.unwrap();
        assert_eq!(batch.len(), 1);
        cache.finish_work(batch, true, 131, 10).await.unwrap();
        assert_eq!(cache.queue_len().await.unwrap(), 0);
    }
    #[tokio::test]
    async fn restart_recovers_claimed_work_and_delivery_ids() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let cache = Cache::open(&path, Default::default()).await.unwrap();
        cache
            .enqueue(Some("delivery".into()), work(), 0, 0)
            .await
            .unwrap();
        assert_eq!(cache.claim_batch(0, 10).await.unwrap().len(), 1);
        drop(cache);
        let cache = Cache::open(path, Default::default()).await.unwrap();
        cache.recover_work().await.unwrap();
        assert!(
            !cache
                .enqueue(Some("delivery".into()), work(), 0, 0)
                .await
                .unwrap()
        );
        let batch = cache.claim_batch(0, 10).await.unwrap();
        assert_eq!(batch.len(), 1);
        cache.finish_work(batch, false, 1, 10).await.unwrap();
        assert_eq!(cache.claim_batch(11, 10).await.unwrap().len(), 1);
    }
}
