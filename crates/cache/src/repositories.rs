//! The server's record of each repository it runs, for its read-only API.
use crate::{Cache, Result};
use rusqlite::{OptionalExtension, params};

/// One repository. `summary` and `snapshot` are the server's own JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub repo: String,
    pub installation: u64,
    pub private: bool,
    /// UTC Unix seconds of the last recorded run.
    pub ran_at: Option<i64>,
    pub summary: Option<Vec<u8>>,
    /// Only [`Cache::repository`] reads it.
    pub snapshot: Option<Vec<u8>>,
    /// When queued work for the repository is due, in UTC Unix milliseconds.
    pub queued_at: Option<i64>,
    /// Whether a batch has claimed that work.
    pub running: bool,
}

/// A recorded run of one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub installation: u64,
    pub repo: String,
    pub private: bool,
    pub ran_at: i64,
    pub summary: Vec<u8>,
    pub snapshot: Vec<u8>,
}

const COLUMNS: &str =
    "r.repo,r.installation,r.private,r.ran_at,r.summary,w.available_at,w.claimed IS NOT NULL";
const QUEUE: &str = "LEFT JOIN server_work w ON w.installation=r.installation AND w.repo=r.repo";

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok(Row {
        repo: row.get(0)?,
        installation: row.get::<_, i64>(1)? as u64,
        private: row.get(2)?,
        ran_at: row.get(3)?,
        summary: row.get(4)?,
        snapshot: None,
        queued_at: row.get(5)?,
        running: row.get::<_, Option<bool>>(6)?.unwrap_or(false),
    })
}

impl Cache {
    /// Sets an installation's repositories to a discovery listing of
    /// `(repo, private)`: new ones are added, visibility is refreshed, and
    /// ones no longer listed are removed with their runs.
    pub async fn repositories_listed(
        &self,
        installation: u64,
        listed: Vec<(String, bool)>,
    ) -> Result<()> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            let known = {
                let mut query =
                    tx.prepare("SELECT repo FROM server_repositories WHERE installation=?1")?;
                query
                    .query_map([installation as i64], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for repo in known
                .iter()
                .filter(|repo| !listed.iter().any(|(name, _)| name == *repo))
            {
                tx.execute("DELETE FROM server_repositories WHERE repo=?1", [repo])?;
            }
            for (repo, private) in &listed {
                tx.execute(
                    "INSERT INTO server_repositories (repo,installation,private) VALUES (?1,?2,?3) \
                     ON CONFLICT(repo) DO UPDATE SET installation=excluded.installation,private=excluded.private",
                    params![repo, installation as i64, private],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Records a run, replacing the repository's previous one.
    pub async fn repository_ran(&self, run: Run) -> Result<()> {
        self.call(move |store| {
            store.conn.execute(
                "INSERT INTO server_repositories VALUES (?1,?2,?3,?4,?5,?6) \
                 ON CONFLICT(repo) DO UPDATE SET installation=excluded.installation,private=excluded.private,\
                 ran_at=excluded.ran_at,summary=excluded.summary,snapshot=excluded.snapshot",
                params![
                    run.repo,
                    run.installation as i64,
                    run.private,
                    run.ran_at,
                    run.summary,
                    run.snapshot
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Removes the repositories of installations not in `installations`.
    pub async fn repositories_retain(&self, installations: Vec<u64>) -> Result<()> {
        self.call(move |store| {
            let ids = installations
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            store.conn.execute(
                &format!("DELETE FROM server_repositories WHERE installation NOT IN ({ids})"),
                [],
            )?;
            Ok(())
        })
        .await
    }

    /// Every repository in name order, without snapshots.
    pub async fn repositories(&self) -> Result<Vec<Row>> {
        self.read(|store| {
            let mut query = store.conn.prepare(&format!(
                "SELECT {COLUMNS} FROM server_repositories r {QUEUE} ORDER BY r.repo"
            ))?;
            let rows = query
                .query_map([], row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// One repository, with its snapshot.
    pub async fn repository(&self, repo: &str) -> Result<Option<Row>> {
        let repo = repo.to_owned();
        self.read(move |store| {
            Ok(store
                .conn
                .query_row(
                    &format!(
                        "SELECT {COLUMNS},r.snapshot FROM server_repositories r {QUEUE} WHERE r.repo=?1"
                    ),
                    [&repo],
                    |r| {
                        Ok(Row {
                            snapshot: r.get(7)?,
                            ..row(r)?
                        })
                    },
                )
                .optional()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Work;

    fn run(repo: &str, private: bool) -> Run {
        Run {
            installation: 1,
            repo: repo.into(),
            private,
            ran_at: 100,
            summary: b"{}".to_vec(),
            snapshot: b"{\"full\":true}".to_vec(),
        }
    }

    #[tokio::test]
    async fn listings_add_refresh_and_remove_repositories_and_keep_runs() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        cache
            .repositories_listed(
                1,
                vec![("acme/app".into(), false), ("acme/gone".into(), false)],
            )
            .await
            .unwrap();
        cache.repository_ran(run("acme/app", false)).await.unwrap();
        cache
            .repository_ran(run("acme/other", false))
            .await
            .unwrap();
        cache
            .repositories_listed(
                1,
                vec![("acme/app".into(), true), ("acme/new".into(), false)],
            )
            .await
            .unwrap();
        let rows = cache.repositories().await.unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(names, ["acme/app", "acme/new"]);
        assert!(rows[0].private);
        assert_eq!(rows[0].ran_at, Some(100));
        assert_eq!(rows[0].snapshot, None);
        assert_eq!(rows[1].summary, None);
        let app = cache.repository("acme/app").await.unwrap().unwrap();
        assert_eq!(app.snapshot.as_deref(), Some(&b"{\"full\":true}"[..]));
        assert_eq!(cache.repository("acme/gone").await.unwrap(), None);
    }

    #[tokio::test]
    async fn rows_show_queued_work_and_uninstalled_repositories_go() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Default::default())
            .await
            .unwrap();
        cache.repository_ran(run("acme/app", false)).await.unwrap();
        cache
            .repository_ran(Run {
                installation: 2,
                ..run("acme/elsewhere", false)
            })
            .await
            .unwrap();
        cache
            .enqueue(None, vec![Work::repository(1, "acme/app")], 5_000, 0)
            .await
            .unwrap();
        let app = cache.repository("acme/app").await.unwrap().unwrap();
        assert_eq!((app.queued_at, app.running), (Some(5_000), false));
        cache.claim_batch(5_000, 10).await.unwrap();
        assert!(cache.repository("acme/app").await.unwrap().unwrap().running);
        cache.repositories_retain(vec![1]).await.unwrap();
        let names: Vec<_> = cache
            .repositories()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.repo)
            .collect();
        assert_eq!(names, ["acme/app"]);
        cache.repositories_retain(Vec::new()).await.unwrap();
        assert!(cache.repositories().await.unwrap().is_empty());
    }
}
