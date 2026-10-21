//! Native auto-merge, or conservative later-run merging backed by write receipts.
use crate::{Inventory, Plan, plan::in_window, pull_requests::owned_pr};
use chrono::{DateTime, Duration, Utc};
use patchula_cache::{Cache, ExtractionKey};
use patchula_config::Config;
use patchula_github::{Client, GraphqlPage};
use patchula_report::{ActionStatus, PullRequestAction, Repository};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Default, Serialize, Deserialize)]
struct History {
    actor: String,
    prs: Vec<Value>,
}
fn history_key(repo: &str) -> ExtractionKey {
    ExtractionKey {
        repo: repo.into(),
        default_branch_commit: String::new(),
        config_hash: "auto-merge-history-v1".into(),
    }
}
pub(crate) async fn load_history(
    cache: &Cache,
    repo: &str,
    actor: &str,
) -> Result<Vec<Value>, String> {
    Ok(cache
        .extraction_get(&history_key(repo))
        .await
        .map_err(|e| e.to_string())?
        .and_then(|bytes| serde_json::from_slice::<History>(&bytes).ok())
        .filter(|h| h.actor == actor)
        .map(|h| h.prs)
        .unwrap_or_default())
}
async fn save_history(
    cache: &Cache,
    repo: &str,
    inventory: &Inventory,
    plans: &[Plan],
) -> Result<(), String> {
    let merged: Vec<_> = plans
        .iter()
        .filter(|p| p.action.status == ActionStatus::Merged)
        .filter_map(|p| p.cached_pr.clone())
        .collect();
    if merged.is_empty() {
        return Ok(());
    }
    let mut prs = inventory.merged_prs.clone();
    for pr in merged {
        prs.retain(|p| p["number"] != pr["number"]);
        prs.push(pr);
    }
    prs.sort_by_key(|p| p["number"].as_u64());
    if prs.len() > 20 {
        prs.drain(..prs.len() - 20);
    }
    cache
        .extraction_put(
            &history_key(repo),
            &serde_json::to_vec(&History {
                actor: inventory.actor.clone(),
                prs,
            })
            .map_err(|e| e.to_string())?,
        )
        .await
        .map_err(|e| e.to_string())
}

pub const CHECKS_QUERY: &str = r#"query($owner:String!,$name:String!,$oid:GitObjectID!,$cursor:String){
 repository(owner:$owner,name:$name){object(oid:$oid){... on Commit{
  oid statusCheckRollup{contexts(first:100,after:$cursor){pageInfo{hasNextPage endCursor} nodes{
   __typename ... on CheckRun{status conclusion} ... on StatusContext{state}
  }}}
 }}}
}"#;

#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub repository_id: String,
    pub native: bool,
    pub method: Option<String>,
    pub delete_branch: bool,
}
impl Settings {
    pub(crate) fn from_graphql(repo: &Value) -> Self {
        Self {
            repository_id: repo["id"].as_str().unwrap_or_default().into(),
            native: repo["autoMergeAllowed"] == true,
            method: method(
                repo,
                [
                    "squashMergeAllowed",
                    "mergeCommitAllowed",
                    "rebaseMergeAllowed",
                ],
            ),
            delete_branch: repo["deleteBranchOnMerge"] == true,
        }
    }
    pub(crate) fn from_rest(repo: &Value) -> Self {
        Self {
            repository_id: repo["node_id"].as_str().unwrap_or_default().into(),
            native: repo["allow_auto_merge"] == true,
            method: method(
                repo,
                [
                    "allow_squash_merge",
                    "allow_merge_commit",
                    "allow_rebase_merge",
                ],
            ),
            delete_branch: repo["delete_branch_on_merge"] == true,
        }
    }
}
fn method(repo: &Value, keys: [&str; 3]) -> Option<String> {
    keys.into_iter()
        .zip(["squash", "merge", "rebase"])
        .find_map(|(key, method)| (repo[key] == true).then(|| method.into()))
}
pub(crate) fn requested(plan: &Plan, config: &Config) -> bool {
    !plan.action.lockfile_pending
        && if plan.maintenance {
            config.defaults.merge
        } else {
            !plan.changes.is_empty() && plan.changes.iter().all(|c| c.merge)
        }
}
fn policy_allows(repo: &Repository, plan: &Plan, config: &Config, now: DateTime<Utc>) -> bool {
    if !repo.warnings.is_empty() {
        return false;
    }
    if plan.maintenance {
        let windows = config
            .defaults
            .windows
            .iter()
            .map(|w| patchula_report::Window {
                cron: w.cron.clone(),
                zone: w.zone.clone(),
            })
            .collect::<Vec<_>>();
        return in_window(&windows, now);
    }
    plan.changes.iter().all(|c| {
        let mut updates = repo
            .package_files
            .iter()
            .filter(|f| f.path == c.file)
            .flat_map(|f| &f.dependencies)
            .filter(|d| {
                d.package == c.package && d.metadata.as_ref().is_some_and(|m| m.source == c.source)
            })
            .flat_map(|d| &d.updates)
            .filter(|u| {
                u.new_value == c.value
                    && u.new_version == c.version
                    && u.new_digest == c.digest
                    && u.update_type == c.kind
            })
            .peekable();
        updates.peek().is_some()
            && updates.all(|u| {
                u.policy.as_ref().is_some_and(|p| {
                    p.merge
                        && in_window(&p.windows, now)
                        && u.release_age.as_ref().is_none_or(|age| {
                            DateTime::parse_from_rfc3339(&age.published_at).is_ok_and(|published| {
                                published
                                    .with_timezone(&Utc)
                                    .checked_add_signed(Duration::hours(i64::from(p.wait_hours)))
                                    .is_some_and(|eligible| eligible <= now)
                            })
                        })
                })
            })
    })
}
async fn green(client: &Client, repo: &str, head: &str) -> Result<bool, String> {
    let (owner, name) = repo.split_once('/').ok_or("invalid repository")?;
    let checks: Vec<Value> = client
        .graphql_pages(
            CHECKS_QUERY,
            json!({"owner":owner,"name":name,"oid":head}),
            |data| {
                if data["repository"]["object"]["oid"] != head {
                    return Err(patchula_github::Error::Json);
                }
                if data["repository"]["object"]["statusCheckRollup"].is_null() {
                    return Ok(GraphqlPage {
                        nodes: Vec::new(),
                        has_next_page: false,
                        end_cursor: None,
                    });
                }
                GraphqlPage::from_connection(&data, "/repository/object/statusCheckRollup/contexts")
            },
            |_| false,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(!checks.is_empty()
        && checks.iter().all(|c| match c["__typename"].as_str() {
            Some("CheckRun") => {
                c["status"] == "COMPLETED"
                    && matches!(
                        c["conclusion"].as_str(),
                        Some("SUCCESS" | "NEUTRAL" | "SKIPPED")
                    )
            }
            Some("StatusContext") => c["state"] == "SUCCESS",
            _ => false,
        }))
}
fn same_head(pr: &Value, proof: &Value, actor: &str, repo: &str) -> bool {
    owned_pr(pr, actor, repo)
        && pr["state"] == "open"
        && pr["merged_at"].is_null()
        && pr["node_id"] == proof["node_id"]
        && pr["head"]["sha"] == proof["head"]["sha"]
        && pr["head"]["ref"] == proof["head"]["ref"]
        && pr["base"]["ref"] == proof["base"]["ref"]
        && pr["body"] == proof["body"]
}
async fn one(
    client: &Client,
    inventory: &Inventory,
    repo: &Repository,
    pr: &mut Value,
) -> Result<(ActionStatus, String), String> {
    if pr["base"]["ref"].as_str() != repo.default_branch.as_deref() {
        return Ok((
            ActionStatus::Protected,
            "auto-merge blocked: PR no longer targets the default branch".into(),
        ));
    }
    let settings = &inventory.merge_settings;
    let method = settings
        .method
        .as_deref()
        .ok_or("repository has no enabled merge method")?;
    if settings.native {
        if pr["auto_merge"].is_null() {
            let result = client.graphql(crate::pull_requests::ENABLE_AUTO_MERGE,
                json!({"input":{"pullRequestId":pr["node_id"],"mergeMethod":method.to_uppercase(),"expectedHeadOid":pr["head"]["sha"]}})).await.map_err(|e| e.to_string())?;
            if !result["enablePullRequestAutoMerge"]["pullRequest"].is_object() {
                return Err("GitHub did not acknowledge native auto-merge".into());
            }
            pr["auto_merge"] = json!({"enabled":true});
        }
        return Ok((ActionStatus::Unchanged, "native auto-merge enabled".into()));
    }
    let number = pr["number"].as_u64().ok_or("missing PR number")?;
    let head = pr["head"]["sha"].as_str().ok_or("missing receipt head")?;
    let path = format!("repos/{}/pulls/{number}", repo.repository);
    let fresh = client.get_fresh(&path).await.map_err(|e| e.to_string())?;
    if !same_head(&fresh, pr, &inventory.actor, &repo.repository) {
        return Ok((
            ActionStatus::Protected,
            "auto-merge blocked: ownership or receipt head changed".into(),
        ));
    }
    if fresh["draft"] != false || fresh["mergeable"] != true || fresh["mergeable_state"] != "clean"
    {
        return Ok((
            ActionStatus::Skipped,
            "auto-merge waiting: GitHub mergeability/protection is not clean".into(),
        ));
    }
    if !green(client, &repo.repository, head).await? {
        return Ok((
            ActionStatus::Skipped,
            "auto-merge waiting: at least one check is required and all checks/statuses must pass"
                .into(),
        ));
    }
    // Recheck ownership/protections after pagination, then use a head lease on the
    // normal merge API. No admin/bypass option, direct base write or force merge.
    let fresh = client.get_fresh(&path).await.map_err(|e| e.to_string())?;
    if !same_head(&fresh, pr, &inventory.actor, &repo.repository)
        || fresh["draft"] != false
        || fresh["mergeable"] != true
        || fresh["mergeable_state"] != "clean"
    {
        return Ok((
            ActionStatus::Protected,
            "auto-merge blocked: PR changed during check inspection".into(),
        ));
    }
    let result = client
        .put(
            &format!("{path}/merge"),
            json!({"sha":head,"merge_method":method}),
        )
        .await
        .map_err(|e| e.to_string())?;
    if result["merged"] != true {
        return Ok((
            ActionStatus::Skipped,
            "auto-merge refused by GitHub; retry on a later run".into(),
        ));
    }
    let mut reason = format!("merged by patchula ({method}); all current-head checks passed");
    if !settings.delete_branch {
        let cleanup = async {
            if settings.repository_id.is_empty() {
                return Err("missing repository ID".into());
            }
            crate::delete_owned(
                client,
                &settings.repository_id,
                repo.default_branch
                    .as_deref()
                    .ok_or("missing default branch")?,
                result["sha"].as_str().ok_or("missing merged base SHA")?,
                pr["head"]["ref"].as_str().ok_or("missing head branch")?,
                head,
            )
            .await
        }
        .await;
        if let Err(error) = cleanup {
            reason.push_str(&format!("; branch cleanup deferred: {error}"));
        }
    }
    pr["state"] = json!("closed");
    pr["merged_at"] = json!("merged by patchula");
    Ok((ActionStatus::Merged, reason))
}

/// Only `cached_pr` restored from a prior verified receipt is eligible. A newly
/// written head must wait for another run, even if checks finish immediately.
pub async fn reconcile(
    client: &Client,
    cache: &Cache,
    repo: &Repository,
    config: &Config,
    inventory: &Inventory,
    plans: &mut [Plan],
    now: DateTime<Utc>,
) -> Vec<PullRequestAction> {
    let mut actions = Vec::new();
    for plan in plans.iter_mut() {
        if !requested(plan, config) || plan.action.status != ActionStatus::Unchanged {
            continue;
        }
        let Some(pr) = &mut plan.cached_pr else {
            continue;
        };
        // Check policy independently of manual create/schedule/age permits.
        let proof = pr.clone();
        let allowed = policy_allows(repo, plan, config, now);
        let pr = plan.cached_pr.as_mut().expect("restored receipt");
        let (status, reason) = if !allowed {
            (ActionStatus::Skipped, "auto-merge waiting: release age, schedule or lookup evidence does not permit merging".into())
        } else {
            one(client, inventory, repo, pr)
                .await
                .unwrap_or_else(|e| (ActionStatus::Error, format!("auto-merge: {e}")))
        };
        pr["patchula_merge"] = json!(reason);
        if status == ActionStatus::Merged {
            plan.action.status = ActionStatus::Merged;
        }
        actions.push(PullRequestAction {
            repository: repo.repository.clone(),
            branch: plan.action.branch.clone(),
            number: proof["number"].as_u64(),
            url: proof["html_url"].as_str().map(str::to_owned),
            status,
            reason: Some(reason),
        });
    }
    if !actions.is_empty()
        && let Err(error) = crate::receipts::save_merge_states(cache, repo, config, plans).await
    {
        for action in &mut actions {
            action
                .reason
                .get_or_insert_default()
                .push_str(&format!("; merge receipt cache: {error}"));
        }
    }
    if let Err(error) = save_history(cache, &repo.repository, inventory, plans).await {
        for action in &mut actions {
            if action.status == ActionStatus::Merged {
                action
                    .reason
                    .get_or_insert_default()
                    .push_str(&format!("; merge history cache: {error}"));
            }
        }
    }
    actions
}

pub fn dashboard_prs(inventory: &Inventory, repo: &str, plans: &[Plan]) -> Vec<Value> {
    dashboard_history(inventory, inventory.pull_requests(repo), plans)
}
pub fn dashboard_history(inventory: &Inventory, mut prs: Vec<Value>, plans: &[Plan]) -> Vec<Value> {
    // Untracked historical/native merges are not presented as fallback outcomes.
    prs.retain(|p| p["merged_at"].is_null() || !p["patchula_merge"].is_null());
    for pr in &inventory.merged_prs {
        if !prs.iter().any(|p| p["number"] == pr["number"]) {
            prs.push(pr.clone());
        }
    }
    for pr in plans.iter().filter_map(|p| p.cached_pr.as_ref()) {
        if let Some(old) = prs.iter_mut().find(|p| p["number"] == pr["number"]) {
            *old = pr.clone();
        } else if !pr["merged_at"].is_null() {
            prs.push(pr.clone());
        }
    }
    prs
}
