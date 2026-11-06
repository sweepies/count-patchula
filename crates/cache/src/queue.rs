//! Durable single-process queue and transactional webhook delivery deduplication.
use crate::{Cache, Result};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Work {
    pub installation: u64,
    /// Empty means discover repositories for this installation (not an engine batch).
    pub repo: String,
    /// None runs unconditionally; Some verifies at least one owned PR first.
    pub check_prs: Option<BTreeSet<u64>>,
}
impl Work {
    pub fn repository(installation: u64, repo: impl Into<String>) -> Self {
        Self {
            installation,
            repo: repo.into(),
            check_prs: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Claim {
    pub work: Work,
    pub attribution: Attribution,
    generation: i64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Attribution {
    pub reasons: BTreeSet<String>,
    pub deliveries: BTreeSet<String>,
}
impl Attribution {
    pub fn reason(reason: &str) -> Self {
        Self {
            reasons: BTreeSet::from([reason.into()]),
            deliveries: BTreeSet::new(),
        }
    }
    pub fn merge(&mut self, other: &Self) {
        self.reasons.extend(other.reasons.iter().cloned());
        self.deliveries.extend(other.deliveries.iter().cloned());
    }
}

/// How a claimed repository's run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    /// It ran; its work is done.
    Done,
    /// It failed, and runs again after a backoff that grows with each
    /// consecutive failure.
    Failed,
    /// It must run again, without having failed: its default branch moved
    /// while the run was writing.
    Requeue,
}
impl Finish {
    /// Every claim ended the same way.
    pub fn all(self, claims: Vec<Claim>) -> Vec<(Claim, Finish)> {
        claims.into_iter().map(|claim| (claim, self)).collect()
    }
}
/// The delay before a first retry, and before a requeued run.
pub const RETRY_MS: i64 = 60_000;
/// The longest retry backoff.
pub const MAX_RETRY_MS: i64 = 3_600_000;
/// The backoff after `failures` consecutive failures: one minute, doubling up
/// to an hour.
pub fn backoff_ms(failures: i64) -> i64 {
    RETRY_MS
        .saturating_mul(1 << (failures - 1).clamp(0, 6))
        .min(MAX_RETRY_MS)
}
/// A repository's retry delay after `failures` consecutive failures: the
/// backoff plus up to a tenth more, always the same share for the same
/// repository, so repositories that failed together do not retry in step.
pub fn retry_delay_ms(installation: u64, repo: &str, failures: i64) -> i64 {
    let backoff = backoff_ms(failures);
    // FNV-1a: stable across processes and releases, unlike std's hasher.
    let hash = installation
        .to_le_bytes()
        .iter()
        .chain(repo.as_bytes())
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    backoff + (hash % (backoff as u64 / 10 + 1)) as i64
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
        let mut attribution = Attribution::reason(if delivery.is_some() {
            "webhook trigger"
        } else {
            "unspecified"
        });
        attribution.deliveries.extend(delivery.iter().cloned());
        self.enqueue_attributed(delivery, work, now, coalesce_ms, attribution)
            .await
    }
    pub async fn enqueue_attributed(
        &self,
        delivery: Option<String>,
        work: Vec<Work>,
        now: i64,
        coalesce_ms: i64,
        attribution: Attribution,
    ) -> Result<bool> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            tx.execute("DELETE FROM server_deliveries WHERE received_at < ?1", [now - 7 * 86400 * 1000])?;
            if let Some(delivery) = &delivery {
                let inserted = tx.execute("INSERT OR IGNORE INTO server_deliveries(delivery,received_at) VALUES (?1,?2)", params![delivery, now])?;
                if inserted == 0 { return Ok(false); }
            }
            // A new trigger brings a backed-off retry forward; coalescing
            // still waits for the window the first event opened.
            for item in &work {
                let previous: Option<Option<String>> = tx.query_row("SELECT check_prs FROM server_work WHERE installation=?1 AND repo=?2", params![item.installation as i64, item.repo], |r| r.get(0)).optional()?;
                let checks = match (&item.check_prs, previous) {
                    (Some(incoming), Some(Some(previous))) => {
                        let mut merged: BTreeSet<u64> = serde_json::from_str(&previous)?;
                        merged.extend(incoming);
                        Some(serde_json::to_string(&merged)?)
                    }
                    (Some(incoming), None) => Some(serde_json::to_string(incoming)?),
                    _ => None, // A push/checkbox/sweep takes precedence over check hints.
                };
                tx.execute("INSERT INTO server_work(installation,repo,available_at,check_prs) VALUES (?1,?2,?3,?4) ON CONFLICT(installation,repo) DO UPDATE SET generation=server_work.generation+1,check_prs=excluded.check_prs,available_at=MIN(server_work.available_at,excluded.available_at)",
                    params![item.installation as i64, item.repo, now + coalesce_ms, checks])?;
                tx.execute("INSERT INTO server_work_causes SELECT installation,repo,generation,?3 FROM server_work WHERE installation=?1 AND repo=?2", params![item.installation as i64, item.repo, serde_json::to_string(&attribution)?])?;
            }
            tx.commit()?;
            Ok(true)
        }).await
    }
    /// One installation per batch so every repository runs with its own App
    /// token, and never one whose earlier batch is still running, so batches
    /// of different installations can run at once.
    pub async fn claim_batch(&self, now: i64, limit: usize) -> Result<Vec<Claim>> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            let installation: Option<i64> = tx.query_row("SELECT installation FROM server_work WHERE claimed IS NULL AND available_at<=?1 AND installation NOT IN (SELECT installation FROM server_work WHERE claimed IS NOT NULL) ORDER BY available_at,installation,repo LIMIT 1", [now], |r| r.get(0)).optional()?;
            let Some(installation) = installation else { return Ok(Vec::new()); };
            let rows = {
                let mut query = tx.prepare("SELECT repo,generation,check_prs FROM server_work WHERE claimed IS NULL AND available_at<=?1 AND installation=?2 ORDER BY available_at,repo LIMIT ?3")?;
                let rows = query.query_map(params![now, installation, limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
                rows.into_iter().map(|(repo, generation, checks)| Ok(Claim {
                    work: Work { installation: installation as u64, repo, check_prs: checks.map(|text| serde_json::from_str(&text)).transpose()? }, generation, attribution: Attribution::default(),
                })).collect::<Result<Vec<_>>>()?
            };
            let mut rows = rows;
            for claim in &mut rows {
                let mut query = tx.prepare("SELECT attribution FROM server_work_causes WHERE installation=?1 AND repo=?2 AND generation<=?3")?;
                let causes = query.query_map(params![installation, claim.work.repo, claim.generation], |r| r.get::<_, String>(0))?;
                for cause in causes {
                    claim.attribution.merge(&serde_json::from_str::<Attribution>(&cause?)?);
                }
                if claim.attribution.reasons.is_empty() {
                    claim.attribution.reasons.insert("recovered work".into());
                }
                tx.execute("UPDATE server_work SET claimed=generation WHERE installation=?1 AND repo=?2", params![installation, claim.work.repo])?;
            }
            tx.commit()?;
            Ok(rows)
        }).await
    }
    pub async fn finish_work(
        &self,
        finished: Vec<(Claim, Finish)>,
        now: i64,
        coalesce_ms: i64,
    ) -> Result<()> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            for (claim, finish) in &finished {
                let key = params![claim.work.installation as i64, claim.work.repo, claim.generation];
                // Work that arrived during the run waits only for coalescing.
                let state: Option<(i64, i64)> = tx.query_row("SELECT generation,failures FROM server_work WHERE installation=?1 AND repo=?2 AND claimed=?3", key, |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                let Some((generation, failures)) = state else { continue };
                let newer = generation != claim.generation;
                let (failures, delay) = match finish {
                    Finish::Done => {
                        tx.execute("DELETE FROM server_work_causes WHERE installation=?1 AND repo=?2 AND generation<=?3", key)?;
                        tx.execute("DELETE FROM server_work WHERE installation=?1 AND repo=?2 AND generation=?3 AND claimed=?3", key)?;
                        (0, coalesce_ms)
                    }
                    Finish::Failed | Finish::Requeue => {
                        // One `retry` cause per generation, however often it fails.
                        let retry = serde_json::to_string(&Attribution::reason("retry"))?;
                        tx.execute("INSERT INTO server_work_causes SELECT ?1,?2,?3,?4 WHERE NOT EXISTS (SELECT 1 FROM server_work_causes WHERE installation=?1 AND repo=?2 AND generation=?3 AND attribution=?4)",
                            params![claim.work.installation as i64, claim.work.repo, claim.generation, retry])?;
                        let failures = failures + i64::from(*finish == Finish::Failed);
                        let delay = if newer { coalesce_ms } else if *finish == Finish::Failed { retry_delay_ms(claim.work.installation, &claim.work.repo, failures) } else { RETRY_MS };
                        (failures, delay)
                    }
                };
                tx.execute("UPDATE server_work SET claimed=NULL,available_at=?4,failures=?5 WHERE installation=?1 AND repo=?2 AND claimed=?3",
                    params![claim.work.installation as i64, claim.work.repo, claim.generation, now + delay, failures])?;
            }
            tx.commit()?;
            Ok(())
        }).await
    }
    /// Releases the claims of runs a shutdown cut short, to run again without
    /// counting as failures.
    pub async fn release_work(&self) -> Result<()> {
        self.call(|store| {
            store
                .conn
                .execute_batch("INSERT INTO server_work_causes SELECT installation,repo,generation,'{\"reasons\":[\"retry\"],\"deliveries\":[]}' FROM server_work WHERE claimed IS NOT NULL; UPDATE server_work SET claimed=NULL;")?;
            Ok(())
        })
        .await
    }
    /// Called once under the server's exclusive process lock, on start. A
    /// shutdown releases its claims, so a claim left over is a run that died
    /// with the process, such as by an OOM kill: it counts as a failure and
    /// backs off, so a run that kills the server cannot do so at full rate.
    pub async fn recover_work(&self, now: i64) -> Result<()> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            let claimed = {
                let mut query = tx.prepare("SELECT installation,repo,claimed,failures FROM server_work WHERE claimed IS NOT NULL")?;
                query.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))?.collect::<std::result::Result<Vec<_>, _>>()?
            };
            let retry = serde_json::to_string(&Attribution::reason("retry"))?;
            for (installation, repo, claimed, failures) in claimed {
                tx.execute("INSERT INTO server_work_causes SELECT ?1,?2,?3,?4 WHERE NOT EXISTS (SELECT 1 FROM server_work_causes WHERE installation=?1 AND repo=?2 AND generation=?3 AND attribution=?4)",
                    params![installation, repo, claimed, retry])?;
                let failures = failures + 1;
                let delay = retry_delay_ms(installation as u64, &repo, failures);
                tx.execute("UPDATE server_work SET claimed=NULL,failures=?3,available_at=?4 WHERE installation=?1 AND repo=?2",
                    params![installation, repo, failures, now + delay])?;
            }
            tx.commit()?;
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
    #[tokio::test]
    async fn attribution_survives_coalescing_followups_retries_and_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let cache = Cache::open(&path, Default::default()).await.unwrap();
        cache
            .enqueue_attributed(None, work(), 0, 0, Attribution::reason("startup discovery"))
            .await
            .unwrap();
        for delivery in ["first", "second"] {
            cache
                .enqueue(Some(delivery.into()), work(), 0, 0)
                .await
                .unwrap();
        }
        let claims = cache.claim_batch(0, 1).await.unwrap();
        assert_eq!(
            claims[0].attribution.deliveries,
            BTreeSet::from(["first".into(), "second".into()])
        );
        assert!(claims[0].attribution.reasons.contains("startup discovery"));
        cache
            .enqueue(Some("followup".into()), work(), 0, 0)
            .await
            .unwrap();
        cache
            .finish_work(Finish::Done.all(claims), 0, 0)
            .await
            .unwrap();
        let claims = cache.claim_batch(0, 1).await.unwrap();
        assert_eq!(
            claims[0].attribution.deliveries,
            BTreeSet::from(["followup".into()])
        );
        cache
            .finish_work(Finish::Failed.all(claims), 0, 0)
            .await
            .unwrap();
        drop(cache);
        let cache = Cache::open(path, Default::default()).await.unwrap();
        cache.recover_work(0).await.unwrap();
        let claims = cache
            .claim_batch(retry_delay_ms(1, "owner/repo", 1), 1)
            .await
            .unwrap();
        assert!(claims[0].attribution.reasons.contains("retry"));
        assert_eq!(
            claims[0].attribution.deliveries,
            BTreeSet::from(["followup".into()])
        );
        cache
            .finish_work(Finish::Done.all(claims), 0, 0)
            .await
            .unwrap();
        cache
            .enqueue_attributed(None, work(), 0, 0, Attribution::reason("sweep"))
            .await
            .unwrap();
        let claims = cache.claim_batch(0, 1).await.unwrap();
        assert_eq!(
            claims[0].attribution.reasons,
            BTreeSet::from(["sweep".into()])
        );
        assert!(claims[0].attribution.deliveries.is_empty());
    }
    fn work() -> Vec<Work> {
        vec![Work::repository(1, "owner/repo")]
    }
    #[tokio::test]
    async fn check_candidates_merge_survive_restart_and_yield_to_other_triggers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cache.sqlite");
        let cache = Cache::open(&path, Default::default()).await.unwrap();
        let candidate = |number| Work {
            installation: 1,
            repo: "owner/repo".into(),
            check_prs: Some(BTreeSet::from([number])),
        };
        cache.enqueue(None, vec![candidate(1)], 0, 0).await.unwrap();
        let claims = cache.claim_batch(0, 1).await.unwrap();
        cache.enqueue(None, vec![candidate(2)], 0, 0).await.unwrap();
        cache
            .finish_work(Finish::Done.all(claims), 0, 0)
            .await
            .unwrap();
        drop(cache);
        let cache = Cache::open(path, Default::default()).await.unwrap();
        cache.recover_work(0).await.unwrap();
        let claims = cache.claim_batch(0, 1).await.unwrap();
        assert_eq!(claims[0].work.check_prs, Some(BTreeSet::from([1, 2])));
        cache.enqueue(None, work(), 0, 0).await.unwrap();
        cache
            .finish_work(Finish::Done.all(claims), 0, 0)
            .await
            .unwrap();
        assert_eq!(
            cache.claim_batch(0, 1).await.unwrap()[0].work.check_prs,
            None
        );
    }
    #[tokio::test]
    async fn a_running_installation_is_skipped_while_another_is_claimed() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        for work in [Work::repository(1, "one/a"), Work::repository(2, "two/a")] {
            cache.enqueue(None, vec![work], 0, 0).await.unwrap();
        }
        let first = cache.claim_batch(0, 1).await.unwrap();
        assert_eq!(first[0].work.installation, 1);
        // Installation 1 has more work, but its batch is still running.
        cache
            .enqueue(None, vec![Work::repository(1, "one/b")], 0, 0)
            .await
            .unwrap();
        let second = cache.claim_batch(0, 10).await.unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].work.installation, 2);
        assert!(cache.claim_batch(0, 10).await.unwrap().is_empty());
        cache
            .finish_work(Finish::Done.all(first), 0, 0)
            .await
            .unwrap();
        let third = cache.claim_batch(0, 10).await.unwrap();
        assert_eq!(third[0].work.repo, "one/b");
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
        cache
            .finish_work(Finish::Done.all(batch), 120, 10)
            .await
            .unwrap();
        assert!(cache.claim_batch(129, 10).await.unwrap().is_empty());
        let batch = cache.claim_batch(130, 10).await.unwrap();
        assert_eq!(batch.len(), 1);
        cache
            .finish_work(Finish::Done.all(batch), 131, 10)
            .await
            .unwrap();
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
        cache.recover_work(0).await.unwrap();
        assert!(
            !cache
                .enqueue(Some("delivery".into()), work(), 0, 0)
                .await
                .unwrap()
        );
        // The run died with the process: a failure, so it backs off.
        let first = retry_delay_ms(1, "owner/repo", 1);
        assert!(cache.claim_batch(first - 1, 10).await.unwrap().is_empty());
        let batch = cache.claim_batch(first, 10).await.unwrap();
        assert_eq!(batch.len(), 1);
        cache
            .finish_work(Finish::Failed.all(batch), first, 10)
            .await
            .unwrap();
        let second = first + retry_delay_ms(1, "owner/repo", 2);
        assert!(cache.claim_batch(second - 1, 10).await.unwrap().is_empty());
        assert_eq!(cache.claim_batch(second, 10).await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn a_shutdown_releases_its_claims_without_counting_a_failure() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        cache.enqueue(None, work(), 0, 0).await.unwrap();
        assert_eq!(cache.claim_batch(0, 10).await.unwrap().len(), 1);
        cache.release_work().await.unwrap();
        let claims = cache.claim_batch(0, 10).await.unwrap();
        assert!(claims[0].attribution.reasons.contains("retry"));
        // No failure was counted: the next one is the first.
        cache
            .finish_work(Finish::Failed.all(claims), 0, 0)
            .await
            .unwrap();
        assert_eq!(
            cache
                .claim_batch(retry_delay_ms(1, "owner/repo", 1), 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }
    async fn causes(cache: &Cache) -> i64 {
        cache
            .call(|store| {
                Ok(store
                    .conn
                    .query_row("SELECT count(*) FROM server_work_causes", [], |r| r.get(0))?)
            })
            .await
            .unwrap()
    }
    #[test]
    fn backoff_doubles_from_a_minute_to_an_hour() {
        let minutes: Vec<_> = (1..=9).map(|n| backoff_ms(n) / 60_000).collect();
        assert_eq!(minutes, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }
    #[tokio::test]
    async fn only_failed_repositories_retry_and_their_backoff_grows_until_success() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        let repos = [
            Work::repository(1, "o/broken"),
            Work::repository(1, "o/fine"),
        ];
        cache.enqueue(None, repos.to_vec(), 0, 0).await.unwrap();
        let mut now = 0;
        for failures in 1..=8 {
            let claims = cache.claim_batch(now, 10).await.unwrap();
            let finished = claims
                .into_iter()
                .map(|claim| {
                    let finish = if claim.work.repo == "o/broken" {
                        Finish::Failed
                    } else {
                        Finish::Done
                    };
                    (claim, finish)
                })
                .collect();
            cache.finish_work(finished, now, 0).await.unwrap();
            assert_eq!(cache.queue_len().await.unwrap(), 1);
            let delay = retry_delay_ms(1, "o/broken", failures);
            assert!(
                cache
                    .claim_batch(now + delay - 1, 10)
                    .await
                    .unwrap()
                    .is_empty()
            );
            now += delay;
        }
        // However often it failed, its causes are the trigger and one retry.
        assert_eq!(causes(&cache).await, 2);
        let claims = cache.claim_batch(now, 10).await.unwrap();
        assert_eq!(claims.len(), 1);
        cache
            .finish_work(Finish::Done.all(claims), now, 0)
            .await
            .unwrap();
        assert_eq!(cache.queue_len().await.unwrap(), 0);
        assert_eq!(causes(&cache).await, 0);
        // A later failure starts over at a minute.
        cache
            .enqueue(None, vec![repos[0].clone()], now, 0)
            .await
            .unwrap();
        let claims = cache.claim_batch(now, 10).await.unwrap();
        cache
            .finish_work(Finish::Failed.all(claims), now, 0)
            .await
            .unwrap();
        assert_eq!(
            cache
                .claim_batch(now + retry_delay_ms(1, "o/broken", 1), 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn a_new_trigger_brings_a_backed_off_retry_forward() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        cache.enqueue(None, work(), 0, 0).await.unwrap();
        let mut now = 0;
        for failures in 1..=4 {
            let claims = cache.claim_batch(now, 10).await.unwrap();
            cache
                .finish_work(Finish::Failed.all(claims), now, 0)
                .await
                .unwrap();
            now += retry_delay_ms(1, "owner/repo", failures);
        }
        // Eight minutes of backoff remain; a push waits only for coalescing.
        let pushed = now - retry_delay_ms(1, "owner/repo", 4) + 1_000;
        cache.enqueue(None, work(), pushed, 10).await.unwrap();
        assert!(cache.claim_batch(pushed + 9, 10).await.unwrap().is_empty());
        assert_eq!(cache.claim_batch(pushed + 10, 10).await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn work_that_arrives_during_a_failed_run_waits_only_for_coalescing() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        cache.enqueue(None, work(), 0, 0).await.unwrap();
        let claims = cache.claim_batch(0, 10).await.unwrap();
        cache.enqueue(None, work(), 5, 10).await.unwrap();
        cache
            .finish_work(Finish::Failed.all(claims), 20, 10)
            .await
            .unwrap();
        assert_eq!(cache.claim_batch(30, 10).await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn a_requeue_retries_after_a_minute_without_counting_as_a_failure() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        cache.enqueue(None, work(), 0, 0).await.unwrap();
        let claims = cache.claim_batch(0, 10).await.unwrap();
        cache
            .finish_work(Finish::Failed.all(claims), 0, 0)
            .await
            .unwrap();
        let mut now = RETRY_MS;
        for _ in 0..3 {
            let claims = cache.claim_batch(now, 10).await.unwrap();
            cache
                .finish_work(Finish::Requeue.all(claims), now, 0)
                .await
                .unwrap();
            now += RETRY_MS;
        }
        // Still one failure: the next one backs off two minutes.
        let claims = cache.claim_batch(now, 10).await.unwrap();
        cache
            .finish_work(Finish::Failed.all(claims), now, 0)
            .await
            .unwrap();
        assert!(
            cache
                .claim_batch(now + retry_delay_ms(1, "owner/repo", 2) - 1, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            cache
                .claim_batch(now + retry_delay_ms(1, "owner/repo", 2), 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn repositories_retry_apart_by_at_most_a_tenth_of_the_backoff() {
        for failures in 1..=8 {
            let backoff = backoff_ms(failures);
            let delays: BTreeSet<_> = (0..20)
                .map(|n| retry_delay_ms(1, &format!("owner/repo-{n}"), failures))
                .collect();
            assert!(delays.len() > 10, "{delays:?}");
            assert!(
                delays
                    .iter()
                    .all(|d| (backoff..=backoff + backoff / 10).contains(d))
            );
            assert_eq!(
                retry_delay_ms(1, "owner/repo-1", failures),
                retry_delay_ms(1, "owner/repo-1", failures)
            );
        }
    }
    #[tokio::test]
    async fn a_success_resets_the_backoff_of_work_that_arrived_during_it() {
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        cache.enqueue(None, work(), 0, 0).await.unwrap();
        let mut now = 0;
        for failures in 1..=3 {
            let claims = cache.claim_batch(now, 10).await.unwrap();
            cache
                .finish_work(Finish::Failed.all(claims), now, 0)
                .await
                .unwrap();
            now += retry_delay_ms(1, "owner/repo", failures);
        }
        let claims = cache.claim_batch(now, 10).await.unwrap();
        cache.enqueue(None, work(), now, 0).await.unwrap();
        cache
            .finish_work(Finish::Done.all(claims), now, 0)
            .await
            .unwrap();
        // The newer work survives the success, which reset its failures.
        let claims = cache.claim_batch(now, 10).await.unwrap();
        cache
            .finish_work(Finish::Failed.all(claims), now, 0)
            .await
            .unwrap();
        let first = now + retry_delay_ms(1, "owner/repo", 1);
        assert!(cache.claim_batch(first - 1, 10).await.unwrap().is_empty());
        assert_eq!(cache.claim_batch(first, 10).await.unwrap().len(), 1);
    }
}
