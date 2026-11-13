//! The quiet sweep: a batch the schedule alone enqueued may finish a
//! repository whose stored answer still stands without a GitHub request of
//! its own. The repository's lookup runs from the extraction the last
//! complete run cached — no head check, no inventory — and the row it
//! produces is hashed the way the stored one was. A match means nothing
//! changed; anything unknown, due or stale fails toward running in full.
use super::lookup::Scanned;
use super::*;
use chrono::{DateTime, Utc};

/// Every repository still runs in full at least this often: a webhook that
/// never arrived or a change no subscribed event reports heals within the
/// bound rather than at an unknown later run.
const FULL_RUN: chrono::Duration = chrono::Duration::hours(6);

/// The answer a complete run stored for the sweep to reproduce.
#[derive(Serialize, Deserialize)]
struct QuietRun {
    /// The default-branch head the stored extraction and answer belong to.
    commit: String,
    /// The default branch the rebuilt report row names.
    branch: String,
    /// [`hash`] of the run's report row.
    report_hash: String,
    /// The earliest millisecond instant time alone can change the answer —
    /// `None` when the run saw nothing time-dependent, `Some` of the run's
    /// own time when one could not be computed so the repository always
    /// runs.
    next_due: Option<i64>,
    /// When the storing run ran, in milliseconds.
    ran_at: i64,
}
impl State for QuietRun {
    const NAMESPACE: &'static str = "quiet-run";
    const VERSION: u32 = 1;
    type Scope = CredentialScope;
}

/// The report row as the sweep compares it: a hash without the fields time
/// alone fills — evaluation and fetch clocks — and the tooling the write
/// path alone computes.
pub(super) fn hash(repo: &Repository) -> Result<String> {
    let mut row = repo.clone();
    row.tooling.clear();
    for dep in row
        .package_files
        .iter_mut()
        .flat_map(|file| &mut file.dependencies)
    {
        for update in &mut dep.updates {
            if let Some(age) = &mut update.release_age {
                age.evaluated_at.clear();
            }
        }
        if let Some(lookup) = dep
            .metadata
            .as_mut()
            .and_then(|metadata| metadata.lookup.as_mut())
        {
            lookup.evaluated_at.clear();
            lookup.catalog = None;
        }
    }
    Ok(patchula_manager::hash(&serde_json::to_vec(&row)?))
}

/// The earliest instant time alone can change this repository's answer: a
/// held-back release's eligibility, the next schedule-window time, an
/// update's merge-age gate or a completed lockfile refresh's defer. `None`
/// means the run saw nothing time-dependent; an input that cannot be
/// computed makes the answer `Some(now)` — the repository always runs.
fn next_due(
    repo: &Repository,
    config: &Config,
    maintenance_success: Option<i64>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let parse = |value: &str| -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|parsed| parsed.with_timezone(&Utc))
    };
    let mut due: Option<DateTime<Utc>> = None;
    macro_rules! fold {
        ($instant:expr) => {
            let instant = $instant;
            if instant > now {
                due = Some(due.map_or(instant, |earlier| earlier.min(instant)));
            }
        };
    }
    for dep in repo
        .package_files
        .iter()
        .flat_map(|file| &file.dependencies)
    {
        if let Some(metadata) = &dep.metadata {
            for held in &metadata.held_back {
                for value in
                    std::iter::once(&held.eligible_at).chain(held.target_eligible_at.iter())
                {
                    let Some(instant) = parse(value) else {
                        return Some(now);
                    };
                    fold!(instant);
                }
            }
        }
        for update in &dep.updates {
            let wait = update.policy.as_ref().map_or(0, |policy| policy.wait_hours);
            if let Some(age) = &update.release_age
                && wait > 0
            {
                let Some(published) = parse(&age.published_at) else {
                    return Some(now);
                };
                fold!(published + chrono::Duration::hours(i64::from(wait)));
            }
        }
    }
    // Every window a plan consults opens at its next cron time: proposed
    // updates carry their effective policy's, and a refresh-enabled
    // repository's lockfile maintenance consults `maintenance_windows`.
    let maintenance_windows = config
        .lockfiles
        .refresh
        .then(|| config.maintenance_windows());
    for window in repo
        .package_files
        .iter()
        .flat_map(|file| &file.dependencies)
        .flat_map(|dep| &dep.updates)
        .flat_map(|update| update.policy.iter().flat_map(|policy| &policy.windows))
        .chain(maintenance_windows.iter().flatten())
    {
        if let Some(instant) = patchula_config::window_after(window, now) {
            fold!(instant);
        }
    }
    // A completed refresh stays deferred until both its week has ended and
    // its wait has elapsed, so it can next matter at the later of the two.
    if config.lockfiles.refresh
        && let Some(last) =
            maintenance_success.and_then(|last| chrono::DateTime::<Utc>::from_timestamp(last, 0))
    {
        let lift = last
            .date_naive()
            .week(chrono::Weekday::Mon)
            .first_day()
            .checked_add_days(chrono::Days::new(7))
            .and_then(|monday| monday.and_hms_opt(0, 0, 0))
            .map(|start| {
                start
                    .and_utc()
                    .max(last + chrono::Duration::hours(i64::from(config.lockfiles.wait_hours)))
            })
            .unwrap_or(now);
        fold!(lift);
    }
    due
}

impl Engine {
    /// The repositories of a sweep batch whose stored answer still stands.
    /// Each candidate ran its lookup from the extraction the last complete
    /// run cached — no head check, no inventory — and a repository whose
    /// new row hashes to the stored hash is quiet. Missing or stale state,
    /// a due time that has passed, the six-hour bound or a changed hash all
    /// keep the repository running in full.
    pub async fn sweep_quiet(
        &self,
        repositories: &[String],
        now: DateTime<Utc>,
    ) -> Result<BTreeSet<String>> {
        let now_ms = now.timestamp_millis();
        let credential_scope = self.github.credential_scope().to_owned();
        let mut candidates = Vec::new();
        for repo in repositories {
            let scope = CredentialScope {
                repository: repo.clone(),
                credential_scope: credential_scope.clone(),
            };
            let Some(state) = self.cache.state::<QuietRun>(&scope).await? else {
                continue;
            };
            if now_ms - state.ran_at >= FULL_RUN.num_milliseconds()
                || state.next_due.is_some_and(|due| now_ms >= due)
            {
                continue;
            }
            candidates.push((repo.clone(), state));
        }
        if candidates.is_empty() {
            return Ok(BTreeSet::new());
        }
        let options = Options {
            repositories: candidates.iter().map(|(repo, _)| repo.clone()).collect(),
            ..Options::default()
        };
        let discovery = self.discover(&options).await?;
        // The scan stage's answer from state alone: the stored commit's
        // cached config and extraction. A repository missing either is left
        // out of the check and runs in full.
        let mut work = Vec::new();
        for (repo, state) in &candidates {
            if let Some(scanned) = self.quiet_work(repo, state, &discovery.schema_hash).await? {
                work.push(scanned);
            }
        }
        if work.is_empty() {
            return Ok(BTreeSet::new());
        }
        let sent = std::sync::Mutex::new(BTreeSet::<Sent>::new());
        let scanned = Scanned {
            work,
            extraction: 0,
            prefetch: 0,
            digests: self.prefetch_digests(&discovery, &sent).await,
        };
        let tail = self.lookup_tail(&discovery, scanned, now, &sent).await?;
        let mut quiet = BTreeSet::new();
        for work in tail.work {
            let name = &work.report.repository;
            if let Some((_, state)) = candidates.iter().find(|(repo, _)| repo == name)
                && work.lookup_hash == state.report_hash
            {
                quiet.insert(name.clone());
            }
        }
        Ok(quiet)
    }

    /// `scan`'s cached answer for a stored commit: its saved config and
    /// extraction rebuilt into a `Work` without a head check. `None` when a
    /// piece is missing — the repository runs in full.
    async fn quiet_work(
        &self,
        repo: &str,
        state: &QuietRun,
        schema_hash: &str,
    ) -> Result<Option<Work>> {
        let scope = RepoConfigScope {
            repository: repo.into(),
            commit: state.commit.clone(),
            schema_hash: schema_hash.into(),
        };
        let Some(config) = self
            .cache
            .state::<RepoConfig>(&scope)
            .await?
            .and_then(|saved| saved.0)
        else {
            return Ok(None);
        };
        let scope = ExtractionScope {
            repository: repo.into(),
            commit: state.commit.clone(),
            config_hash: patchula_manager::hash(&serde_json::to_vec(&config)?),
        };
        let Some(cached) = self.cache.state::<CachedExtraction>(&scope).await? else {
            return Ok(None);
        };
        self.extraction_hits.fetch_add(1, Ordering::Relaxed);
        let mut report = repo_report(repo, Status::Complete, None);
        report.default_branch = Some(state.branch.clone());
        report.commit = Some(state.commit.clone());
        report.warnings = cached.extraction.warnings.clone();
        report.skipped = skipped(&cached.extraction.skipped);
        Ok(Some(Work {
            dependencies: cached
                .extraction
                .dependencies
                .into_iter()
                .map(Dep::new)
                .collect(),
            locked: cached.extraction.locked,
            snapshot: cached.snapshot,
            ..Work::empty(report, Some(config))
        }))
    }

    /// A complete run's stored answer for the sweep: the row's lookup hash,
    /// the head and branch it belongs to, its next due instant and when it
    /// ran. A repository missing any piece stores nothing and always runs.
    pub(crate) async fn record_quiet(&self, work: &Work, now: DateTime<Utc>) -> Result<()> {
        let (Some(commit), Some(branch), Some(config)) = (
            &work.report.commit,
            &work.report.default_branch,
            &work.config,
        ) else {
            return Ok(());
        };
        if work.lookup_hash.is_empty() {
            return Ok(());
        }
        let maintenance_success = if config.lockfiles.refresh {
            self.cache
                .state::<patchula_update::maintenance::MaintenanceSuccess>(
                    &patchula_update::maintenance::refresh_scope(&work.report.repository),
                )
                .await?
                .map(|success| success.0)
        } else {
            None
        };
        let scope = CredentialScope {
            repository: work.report.repository.clone(),
            credential_scope: self.github.credential_scope().into(),
        };
        self.cache
            .set_state(
                &scope,
                &QuietRun {
                    commit: commit.clone(),
                    branch: branch.clone(),
                    report_hash: work.lookup_hash.clone(),
                    next_due: next_due(&work.report, config, maintenance_success, now)
                        .map(|instant| instant.timestamp_millis()),
                    ran_at: now.timestamp_millis(),
                },
            )
            .await?;
        Ok(())
    }
}
