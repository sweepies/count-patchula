//! Scheduled lockfile maintenance: a refresh plan over the lockfiles the
//! enabled ecosystems claim, held between windows or after a completed run.
use crate::{Hold, Plan, Snapshot, branch_name};
use patchula_cache::State;
use patchula_config::Config;
use patchula_lockfiles::refresh_jobs;
use patchula_manager::hash;
use patchula_report::{BranchAction, BranchStatus, Repository};
use serde::{Deserialize, Serialize};

/// Keep the scheduled branch in desired names between windows, so ordinary
/// dependency runs do not close an outstanding maintenance PR.
pub fn maintenance(
    repository: &Repository,
    config: &Config,
    snapshot: &Snapshot,
    since: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Plan>, String> {
    use chrono::Datelike;
    if !config.lockfiles.refresh {
        return Ok(None);
    }
    let jobs = refresh_jobs(config, &snapshot.paths);
    if jobs.is_empty() {
        return Ok(None);
    }
    let due = patchula_config::due(&config.maintenance_windows(), since, now);
    let period = format!("{}-{:02}", now.iso_week().year(), now.iso_week().week());
    let fingerprint = hash(
        serde_json::to_string(&(period, &jobs, &config.lockfiles))
            .unwrap()
            .as_bytes(),
    );
    let mut plan = Plan::maintenance(
        BranchAction {
            repository: repository.repository.clone(),
            branch: branch_name(&config.prs.branch, "lock-file-maintenance")?,
            base_commit: repository.commit.clone().ok_or("missing base")?,
            status: BranchStatus::Planned,
            reason: None,
            message: format!("{} lock file maintenance", config.defaults.commit_prefix),
            previous_commit: None,
            commit: None,
            verified: None,
            lockfile_pending: false,
            files: Vec::new(),
        },
        jobs.into_iter().collect(),
        fingerprint,
        snapshot.public_only.clone(),
    );
    if !due {
        plan.hold(Hold::MaintenanceWindow);
    }
    Ok(Some(plan))
}

/// When a repository's lockfile maintenance last completed.
#[derive(Serialize, Deserialize)]
pub struct MaintenanceSuccess(pub i64);
impl State for MaintenanceSuccess {
    const NAMESPACE: &'static str = "maintenance-success";
    const VERSION: u32 = 1;
    type Scope = patchula_cache::RepositoryScope;
}
pub fn refresh_scope(repo: &str) -> patchula_cache::RepositoryScope {
    patchula_cache::RepositoryScope {
        repository: repo.into(),
    }
}
pub async fn defer_completed_refresh(
    cache: &patchula_cache::Cache,
    plan: &mut Plan,
    config: &Config,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    use chrono::Datelike;
    if !plan.is_maintenance() || plan.action().status != BranchStatus::Planned {
        return Ok(());
    }
    if let Some(last) = cache
        .state::<MaintenanceSuccess>(&refresh_scope(&plan.action().repository))
        .await
        .map_err(|e| e.to_string())?
        && let Some(last) = chrono::DateTime::<chrono::Utc>::from_timestamp(last.0, 0)
        && (last.iso_week() == now.iso_week()
            || now - last < chrono::Duration::hours(i64::from(config.lockfiles.wait_hours)))
    {
        plan.hold(Hold::MaintenanceDone);
    }
    Ok(())
}
pub async fn record_refresh(
    cache: &patchula_cache::Cache,
    plans: &[Plan],
    actions: &[patchula_report::PullRequestAction],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    for plan in plans
        .iter()
        .filter(|p| p.is_maintenance() && p.lockfiles_ready())
    {
        if plan.held(&Hold::LockfilesCurrent)
            || actions.iter().any(|a| {
                a.branch == plan.action().branch
                    && matches!(
                        a.status,
                        patchula_report::PullRequestStatus::Created
                            | patchula_report::PullRequestStatus::Updated
                            | patchula_report::PullRequestStatus::Unchanged
                    )
            })
        {
            cache
                .set_state(
                    &refresh_scope(&plan.action().repository),
                    &MaintenanceSuccess(now.timestamp()),
                )
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
