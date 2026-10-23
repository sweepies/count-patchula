//! The server's record of each repository it runs, for its read-only API.
use crate::{Cache, Result};
use rusqlite::{OptionalExtension, params};

/// One repository. `summary` and `snapshot` are the server's own JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub repo: String,
    pub installation: u64,
    pub private: bool,
    /// UTC Unix seconds when the last recorded run started.
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
    /// UTC Unix seconds when the run started.
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

const ADD: &str = "INSERT INTO server_repositories (repo,installation,private) VALUES (?1,?2,?3) \
    ON CONFLICT(repo) DO UPDATE SET installation=excluded.installation,private=excluded.private";

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
                tx.execute(ADD, params![repo, installation as i64, private])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Adds `(repo, private)` repositories to an installation, keeping any runs.
    pub async fn repositories_added(
        &self,
        installation: u64,
        added: Vec<(String, bool)>,
    ) -> Result<()> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            for (repo, private) in &added {
                tx.execute(ADD, params![repo, installation as i64, private])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Removes repositories with their runs.
    pub async fn repositories_removed(&self, removed: Vec<String>) -> Result<()> {
        self.call(move |store| {
            let tx = store.conn.transaction()?;
            for repo in &removed {
                tx.execute("DELETE FROM server_repositories WHERE repo=?1", [repo])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Sets a known repository's visibility.
    pub async fn repository_visibility(&self, repo: &str, private: bool) -> Result<()> {
        let repo = repo.to_owned();
        self.call(move |store| {
            store.conn.execute(
                "UPDATE server_repositories SET private=?2 WHERE repo=?1",
                params![repo, private],
            )?;
            Ok(())
        })
        .await
    }

    /// Records a run of a known repository in its installation, unless a run
    /// that started later is already recorded. Returns whether it was recorded.
    /// A run can make a repository private but never public: its visibility
    /// was read before a webhook may have changed it.
    pub async fn repository_ran(&self, run: Run) -> Result<bool> {
        self.call(move |store| {
            Ok(store.conn.execute(
                "UPDATE server_repositories SET private=MAX(private,?3),ran_at=?4,summary=?5,snapshot=?6 \
                 WHERE repo=?1 AND installation=?2 AND (ran_at IS NULL OR ran_at<=?4)",
                params![
                    run.repo,
                    run.installation as i64,
                    run.private,
                    run.ran_at,
                    run.summary,
                    run.snapshot
                ],
            )? > 0)
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
    use crate::{Options, queue::Work};

    fn run(repo: &str, ran_at: i64) -> Run {
        Run {
            installation: 1,
            repo: repo.into(),
            private: false,
            ran_at,
            summary: b"{}".to_vec(),
            snapshot: format!("{{\"at\":{ran_at}}}").into_bytes(),
        }
    }
    async fn names(cache: &Cache) -> Vec<String> {
        cache
            .repositories()
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.repo)
            .collect()
    }

    #[tokio::test]
    async fn listings_add_refresh_and_remove_repositories_and_keep_runs() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Options::default())
            .await
            .unwrap();
        let listed = |repos: &[(&str, bool)]| {
            repos
                .iter()
                .map(|(repo, private)| ((*repo).to_owned(), *private))
                .collect::<Vec<_>>()
        };
        cache
            .repositories_listed(1, listed(&[("acme/app", false), ("acme/gone", false)]))
            .await
            .unwrap();
        assert!(cache.repository_ran(run("acme/app", 100)).await.unwrap());
        assert!(
            !cache
                .repository_ran(run("acme/unknown", 100))
                .await
                .unwrap(),
            "only known repositories are recorded"
        );
        cache
            .repositories_listed(1, listed(&[("acme/app", true), ("acme/new", false)]))
            .await
            .unwrap();
        let rows = cache.repositories().await.unwrap();
        assert_eq!(names(&cache).await, ["acme/app", "acme/new"]);
        assert!(rows[0].private);
        assert_eq!(rows[0].ran_at, Some(100));
        assert_eq!(rows[0].snapshot, None);
        assert_eq!(rows[1].summary, None);
        let app = cache.repository("acme/app").await.unwrap().unwrap();
        assert_eq!(app.snapshot.as_deref(), Some(&b"{\"at\":100}"[..]));
        assert_eq!(cache.repository("acme/gone").await.unwrap(), None);
        assert!(
            !cache.repository_ran(run("acme/gone", 200)).await.unwrap(),
            "a removed repository does not come back from a run"
        );
    }

    #[tokio::test]
    async fn an_earlier_run_never_replaces_a_later_one() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Options::default())
            .await
            .unwrap();
        cache
            .repositories_added(1, vec![("acme/app".into(), false)])
            .await
            .unwrap();
        assert!(cache.repository_ran(run("acme/app", 200)).await.unwrap());
        assert!(!cache.repository_ran(run("acme/app", 100)).await.unwrap());
        assert!(
            !cache
                .repository_ran(Run {
                    installation: 2,
                    ..run("acme/app", 300)
                })
                .await
                .unwrap(),
            "another installation's run is not this repository's"
        );
        let app = cache.repository("acme/app").await.unwrap().unwrap();
        assert_eq!(app.ran_at, Some(200));
        assert_eq!(app.snapshot.as_deref(), Some(&b"{\"at\":200}"[..]));
        cache.repository_visibility("acme/app", true).await.unwrap();
        assert!(cache.repository_ran(run("acme/app", 400)).await.unwrap());
        assert!(
            cache.repository("acme/app").await.unwrap().unwrap().private,
            "a run read as public does not undo a later privatized webhook"
        );
    }

    #[tokio::test]
    async fn webhooks_change_visibility_and_membership() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Options::default())
            .await
            .unwrap();
        cache
            .repositories_added(
                1,
                vec![("acme/app".into(), false), ("acme/b".into(), false)],
            )
            .await
            .unwrap();
        cache.repository_ran(run("acme/app", 100)).await.unwrap();
        cache.repository_visibility("acme/app", true).await.unwrap();
        cache
            .repository_visibility("acme/unknown", false)
            .await
            .unwrap();
        let app = cache.repository("acme/app").await.unwrap().unwrap();
        assert!(app.private);
        assert_eq!(app.ran_at, Some(100), "visibility keeps the run");
        cache
            .repositories_added(1, vec![("acme/app".into(), false)])
            .await
            .unwrap();
        assert_eq!(
            cache.repository("acme/app").await.unwrap().unwrap().ran_at,
            Some(100),
            "adding again keeps the run"
        );
        cache
            .repositories_removed(vec!["acme/b".into()])
            .await
            .unwrap();
        assert_eq!(names(&cache).await, ["acme/app"]);
    }

    #[tokio::test]
    async fn rows_show_queued_work_and_uninstalled_repositories_go() {
        let directory = tempfile::tempdir().unwrap();
        let cache = Cache::open(directory.path().join("cache.sqlite"), Options::default())
            .await
            .unwrap();
        cache
            .repositories_added(1, vec![("acme/app".into(), false)])
            .await
            .unwrap();
        cache
            .repositories_added(2, vec![("acme/elsewhere".into(), false)])
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
        assert_eq!(names(&cache).await, ["acme/app"]);
        cache.repositories_retain(Vec::new()).await.unwrap();
        assert!(names(&cache).await.is_empty());
    }
}
