//! The write path's shared tail: auto-merge, the dashboard and the write
//! receipts, in that order, over the run's one authoritative inventory.
//! Both outcomes call it — plans settled from verified receipts and plans
//! that went through the full write — so the two can never drift apart.
use crate::{Inventory, Lifecycle, Plan, pull_requests::Tracked};
use chrono::{DateTime, Utc};
use patchula_cache::Cache;
use patchula_config::Config;
use patchula_github::Client;
use patchula_report::{PullRequestAction, Repository};
use patchula_words::Recheck;
use serde_json::Value;

/// What the shared tail produced, for the caller's report assembly.
pub struct Settlement {
    /// Auto-merge outcomes for this run's merge-eligible plans. On the full
    /// write path they are already inside the lifecycle's actions.
    pub merge_actions: Vec<PullRequestAction>,
    /// The dashboard reconcile's failure, if any. The caller fails the
    /// repository after whatever earlier failure it checks first.
    pub dashboard: Result<(), String>,
    /// Non-fatal reconcile warnings, already prefixed for the report.
    pub warnings: Vec<String>,
}

/// Reconciles one repository after its plans settled or wrote. `lifecycle`
/// is the REST view of a full write: its pull requests are authoritative
/// there, refreshed by publish, merge recording, batched closure and recheck
/// acknowledgement. The receipts path passes `None`; nothing was written,
/// so the fresh inventory's open owned pull requests stand in.
#[allow(clippy::too_many_arguments)]
pub async fn settle(
    client: &Client,
    cache: &Cache,
    repo: &Repository,
    config: &Config,
    inventory: &Inventory,
    plans: &mut [Plan],
    mut lifecycle: Option<&mut Lifecycle>,
    recheck: Recheck,
    server: Option<&str>,
    now: DateTime<Utc>,
) -> Settlement {
    let merge_actions =
        crate::automerge::reconcile(client, cache, repo, config, inventory, plans, now).await;
    let mut warnings = Vec::new();
    if let Some(state) = lifecycle.as_deref_mut() {
        state.record_merges(plans, merge_actions.clone());
        if let Err(error) = state.close_batched(client, plans).await {
            warnings.push(format!("batched PR closure: {error}"));
        }
        if let Err(error) = state.acknowledge_rechecks(client).await {
            warnings.push(format!("PR checkbox acknowledgement: {error}"));
        }
    }
    // The one pull-request set the dashboard renders: the lifecycle's fresh
    // REST view after a write, the inventory's on the receipts path.
    let prs = match lifecycle.as_deref() {
        Some(state) => state.dashboard_prs(),
        None => inventory.pull_requests(&repo.repository),
    };
    let history = crate::automerge::dashboard_history(inventory, prs, plans);
    // The renderer still takes one Value set until the dashboard commit;
    // the tracked PRs project down, the merged rows serialize back.
    let mut rows: Vec<Value> = history.open.iter().map(Tracked::value).collect();
    rows.extend(
        history
            .merged
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok()),
    );
    let body = crate::dashboard::render(repo, plans, &rows, recheck, server);
    let dashboard = crate::dashboard::reconcile(client, cache, repo, config, inventory, body).await;
    if let Some(state) = lifecycle {
        if let Err(error) =
            crate::maintenance::record_refresh(cache, plans, &state.actions, now).await
        {
            warnings.push(format!("maintenance cache: {error}"));
        }
        if let Err(error) = crate::save_receipts(cache, repo, config, plans, state).await {
            warnings.push(format!("write receipt cache: {error}"));
        }
    }
    Settlement {
        merge_actions,
        dashboard,
        warnings,
    }
}
