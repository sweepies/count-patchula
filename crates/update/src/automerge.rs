//! Native auto-merge, or conservative later-run merging backed by write receipts.
use crate::{Inventory, Plan, pull_requests::value_owned_pr};
use chrono::{DateTime, Utc};
use patchula_cache::{Cache, State};
use patchula_config::Config;
use patchula_github::{Client, GraphqlPage, model};
use patchula_report::{BranchStatus, PullRequestAction, PullRequestStatus, Repository};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Default, Serialize, Deserialize)]
struct History {
    actor: String,
    prs: Vec<Value>,
}
#[derive(Serialize)]
struct HistoryScope {
    repository: String,
}
fn history_scope(repo: &str) -> HistoryScope {
    HistoryScope {
        repository: repo.into(),
    }
}
impl State for History {
    const NAMESPACE: &'static str = "auto-merge-history";
    const VERSION: u32 = 1;
    type Scope = HistoryScope;
}
pub(crate) async fn load_history(
    cache: &Cache,
    repo: &str,
    actor: &str,
) -> Result<Vec<Value>, String> {
    Ok(cache
        .state::<History>(&history_scope(repo))
        .await
        .map_err(|e| e.to_string())?
        .filter(|h| h.actor == actor)
        .map(|h| h.prs)
        .unwrap_or_default())
}
/// The PRs the Count last recorded merging in `repo`, oldest first, at most 20.
pub async fn merged(cache: &Cache, repo: &str) -> Vec<Value> {
    cache
        .state::<History>(&history_scope(repo))
        .await
        .ok()
        .flatten()
        .map(|h| h.prs)
        .unwrap_or_default()
}
async fn save_history(
    cache: &Cache,
    repo: &str,
    inventory: &Inventory,
    plans: &[Plan],
) -> Result<(), String> {
    let merged: Vec<_> = plans
        .iter()
        .filter(|p| p.action().status == BranchStatus::Merged)
        .filter_map(|p| p.cached_pr.clone().map(|t| t.value()))
        .collect();
    if merged.is_empty() {
        return Ok(());
    }
    let mut prs = inventory.merged_prs.clone();
    for pr in merged {
        let number = crate::pull_requests::Pr::read(&pr).number;
        prs.retain(|p| crate::pull_requests::Pr::read(p).number != number);
        prs.push(pr);
    }
    prs.sort_by_key(|p| crate::pull_requests::Pr::read(p).number);
    if prs.len() > 20 {
        prs.drain(..prs.len() - 20);
    }
    cache
        .set_state(
            &history_scope(repo),
            &History {
                actor: inventory.actor.clone(),
                prs,
            },
        )
        .await
        .map_err(|e| e.to_string())
}

#[allow(dead_code)]
mod checks {
    use graphql_client::GraphQLQuery;

    /// `GitObjectID` scalar the generated variables resolve as
    /// `super::GitObjectID`.
    type GitObjectID = String;

    /// `checks.graphql`, checked against the vendored GitHub schema.
    #[derive(GraphQLQuery)]
    #[graphql(
        schema_path = "../github/graphql/schema.docs.graphql",
        query_path = "src/checks.graphql",
        response_derives = "Debug"
    )]
    pub struct ChecksQuery;
}
use checks::checks_query::{
    ChecksQueryRepositoryObject as ChecksObject,
    ChecksQueryRepositoryObjectOnCommitStatusCheckRollupContextsNodes as ChecksContext,
};

/// The checks document, exported for tests that compare request bodies.
pub const CHECKS_QUERY: &str = checks::checks_query::QUERY;

#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub repository_id: String,
    pub native: bool,
    pub method: Option<String>,
    pub delete_branch: bool,
}
impl Settings {
    /// The inventory `repository`'s merge settings — every flag is a
    /// schema `Boolean!`/`ID!`.
    pub(crate) fn from_graphql(repo: &crate::receipts::InventoryRepository) -> Self {
        Self {
            repository_id: repo.id.clone(),
            native: repo.auto_merge_allowed,
            method: [
                (repo.squash_merge_allowed, "squash"),
                (repo.merge_commit_allowed, "merge"),
                (repo.rebase_merge_allowed, "rebase"),
            ]
            .into_iter()
            .find_map(|(allowed, method)| allowed.then(|| method.into())),
            delete_branch: repo.delete_branch_on_merge,
        }
    }
    pub(crate) fn from_rest(repo: &model::Repository) -> Self {
        Self {
            repository_id: repo.node_id.clone(),
            native: repo.allow_auto_merge == Some(true),
            method: [
                (repo.allow_squash_merge, "squash"),
                (repo.allow_merge_commit, "merge"),
                (repo.allow_rebase_merge, "rebase"),
            ]
            .into_iter()
            .find_map(|(allowed, method)| (allowed == Some(true)).then(|| method.into())),
            delete_branch: repo.delete_branch_on_merge == Some(true),
        }
    }
}
pub(crate) fn requested(plan: &Plan, config: &Config) -> bool {
    crate::pull_requests::merge_requested(plan, &config.defaults)
}
fn policy_allows(repo: &Repository, plan: &Plan, now: DateTime<Utc>) -> bool {
    if repo.degraded() {
        return false;
    }
    // Windows decide when updates are proposed, not when they merge: a PR whose
    // checks pass after its window still merges.
    if plan.maintenance {
        return true;
    }
    plan.changes.iter().all(|c| {
        let mut updates = repo
            .package_files
            .iter()
            .flat_map(|f| f.dependencies.iter().map(move |d| (f.path.as_str(), d)))
            .filter(|&(path, d)| {
                d.metadata
                    .as_ref()
                    .is_some_and(|m| c.touches(path, m.source, &d.package, d.registry.as_deref()))
            })
            .flat_map(|(_, d)| &d.updates)
            .filter(|u| c.same_target(u))
            .peekable();
        updates.peek().is_some()
            && updates.all(|u| {
                u.policy.as_ref().is_some_and(|p| {
                    p.merge
                        && u.release_age.as_ref().is_none_or(|age| {
                            DateTime::parse_from_rfc3339(&age.published_at).is_ok_and(|published| {
                                patchula_lookup::eligible_at(
                                    published.with_timezone(&Utc),
                                    p.wait_hours,
                                )
                                .is_some_and(|eligible| eligible <= now)
                            })
                        })
                })
            })
    })
}
async fn green(client: &Client, repo: &str, head: &str) -> Result<bool, String> {
    let (owner, name) = repo.split_once('/').ok_or("invalid repository")?;
    let checks: Vec<Option<ChecksContext>> = client
        .graphql_pages(
            CHECKS_QUERY,
            json!({"owner":owner,"name":name,"oid":head}),
            |data| {
                let object: Option<ChecksObject> =
                    serde_json::from_value(data["repository"]["object"].clone())
                        .map_err(|_| patchula_github::Error::Json)?;
                let Some(ChecksObject::Commit(commit)) = object else {
                    return Err(patchula_github::Error::Json);
                };
                if commit.oid != head {
                    return Err(patchula_github::Error::Json);
                }
                let Some(rollup) = commit.status_check_rollup else {
                    return Ok(GraphqlPage {
                        nodes: Vec::new(),
                        has_next_page: false,
                        end_cursor: None,
                    });
                };
                let contexts = rollup.contexts;
                Ok(GraphqlPage {
                    nodes: contexts.nodes.unwrap_or_default(),
                    has_next_page: contexts.page_info.has_next_page,
                    end_cursor: contexts.page_info.end_cursor,
                })
            },
            |_| false,
        )
        .await
        .map_err(|e| e.to_string())?;
    // A null or unreadable context node reads as not-green, as it did when
    // the nodes were `Value`s without a `__typename`.
    Ok(!checks.is_empty()
        && checks.iter().all(|c| match c {
            Some(ChecksContext::CheckRun(run)) => {
                matches!(
                    run.status,
                    checks::checks_query::CheckStatusState::COMPLETED
                ) && matches!(
                    run.conclusion,
                    Some(
                        checks::checks_query::CheckConclusionState::SUCCESS
                            | checks::checks_query::CheckConclusionState::NEUTRAL
                            | checks::checks_query::CheckConclusionState::SKIPPED
                    )
                )
            }
            Some(ChecksContext::StatusContext(context)) => {
                matches!(context.state, checks::checks_query::StatusState::SUCCESS)
            }
            _ => false,
        }))
}
fn same_head(pr: &Value, proof: &Value, actor: &str, repo: &str) -> bool {
    let fields = crate::pull_requests::Pr::read(pr);
    let proof_fields = crate::pull_requests::Pr::read(proof);
    value_owned_pr(pr, actor, repo)
        && fields.state.as_deref() == Some("open")
        && fields.merged_at.is_none()
        && pr["node_id"] == proof["node_id"]
        && fields.head.sha == proof_fields.head.sha
        && fields.head.branch == proof_fields.head.branch
        && pr["base"]["ref"] == proof["base"]["ref"]
        && crate::pull_requests::body_hash(fields.body.as_deref().unwrap_or(""))
            == crate::pull_requests::body_hash(proof_fields.body.as_deref().unwrap_or(""))
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Native {
    Enabled,
    /// GitHub refused because nothing blocks the merge: no required check or
    /// review stands in the way, so there is nothing for auto-merge to wait for.
    AlreadyMergeable,
}
/// Whether GitHub's refusal says the PR is already mergeable. It names the PR's
/// `MergeStateStatus` ("Pull request is in unstable status"). CLEAN (checks
/// pass), UNSTABLE (only non-required checks fail or are pending) and HAS_HOOKS
/// (pre-receive hooks) all mean nothing required stands in the way, which is
/// what `gh pr merge --auto` also takes for mergeable now. BLOCKED, BEHIND and
/// DIRTY are other states: auto-merge can wait for the first two, and the last
/// is a real refusal.
fn already_mergeable(message: &str) -> bool {
    message.match_indices(" is in ").any(|(at, found)| {
        let rest = &message[at + found.len()..];
        rest.split_once(" status").is_some_and(|(state, _)| {
            ["clean", "unstable", "has_hooks"].contains(&state.to_ascii_lowercase().as_str())
        })
    })
}
/// Turns on GitHub's auto-merge for a PR, pinned to its head commit. GitHub
/// offers auto-merge only on a PR that cannot be merged yet, and refuses one
/// that is mergeable now. That refusal is not a failure, and does not entitle
/// anything to merge: the caller's own merge rules apply.
pub(crate) async fn enable_native(
    client: &Client,
    id: &str,
    method: &str,
    head: &Value,
) -> Result<Native, String> {
    let input = json!({"input":{"pullRequestId":id,"mergeMethod":method.to_uppercase(),"expectedHeadOid":head}});
    match client
        .graphql::<crate::pull_requests::EnableAutoMergeData>(
            crate::pull_requests::ENABLE_AUTO_MERGE,
            input,
        )
        .await
    {
        Ok(result)
            if result
                .enable_pull_request_auto_merge
                .as_ref()
                .is_some_and(|payload| payload.pull_request.is_some()) =>
        {
            Ok(Native::Enabled)
        }
        Ok(_) => Err("GitHub did not acknowledge native auto-merge".into()),
        Err(patchula_github::Error::Graphql(message)) if already_mergeable(&message) => {
            Ok(Native::AlreadyMergeable)
        }
        Err(error) => Err(error.to_string()),
    }
}
async fn one(
    client: &Client,
    inventory: &Inventory,
    repo: &Repository,
    pr: &mut Value,
) -> Result<(PullRequestStatus, String), String> {
    let fields = crate::pull_requests::Pr::read(pr);
    let head_oid = fields
        .head
        .sha
        .as_ref()
        .map_or(Value::Null, |sha| json!(sha));
    if pr["base"]["ref"].as_str() != repo.default_branch.as_deref() {
        return Ok((
            PullRequestStatus::Protected,
            "auto-merge blocked: PR no longer targets the default branch".into(),
        ));
    }
    let settings = &inventory.merge_settings;
    let method = settings
        .method
        .as_deref()
        .ok_or("repository has no enabled merge method")?;
    // A PR that GitHub already finds mergeable cannot take native auto-merge;
    // it falls through to the Count's own merge, behind the same gate.
    if settings.native {
        if pr["auto_merge"].is_null()
            && enable_native(
                client,
                pr["node_id"].as_str().unwrap_or_default(),
                method,
                &head_oid,
            )
            .await?
                == Native::Enabled
        {
            pr["auto_merge"] = json!({"enabled":true});
        }
        if !pr["auto_merge"].is_null() {
            return Ok((
                PullRequestStatus::Unchanged,
                "native auto-merge enabled".into(),
            ));
        }
    }
    let number = fields.number.ok_or("missing PR number")?;
    let head = fields.head.sha.as_deref().ok_or("missing receipt head")?;
    let path = format!("repos/{}/pulls/{number}", repo.repository);
    let fresh = client
        .get_fresh::<Value>(&path)
        .await
        .map_err(|e| e.to_string())?;
    if !same_head(&fresh, pr, &inventory.actor, &repo.repository) {
        return Ok((
            PullRequestStatus::Protected,
            "auto-merge blocked: ownership or receipt head changed".into(),
        ));
    }
    if fresh["draft"].as_bool() != Some(false)
        || fresh["mergeable"].as_bool() != Some(true)
        || fresh["mergeable_state"] != "clean"
    {
        return Ok((
            PullRequestStatus::Skipped,
            "auto-merge waiting: GitHub mergeability/protection is not clean".into(),
        ));
    }
    if !green(client, &repo.repository, head).await? {
        return Ok((
            PullRequestStatus::Skipped,
            "auto-merge waiting: at least one check is required and all checks/statuses must pass"
                .into(),
        ));
    }
    // Recheck ownership/protections after pagination, then use a head lease on the
    // normal merge API. No admin/bypass option, direct base write or force merge.
    let fresh = client
        .get_fresh::<Value>(&path)
        .await
        .map_err(|e| e.to_string())?;
    if !same_head(&fresh, pr, &inventory.actor, &repo.repository)
        || fresh["draft"].as_bool() != Some(false)
        || fresh["mergeable"].as_bool() != Some(true)
        || fresh["mergeable_state"] != "clean"
    {
        return Ok((
            PullRequestStatus::Protected,
            "auto-merge blocked: PR changed during check inspection".into(),
        ));
    }
    let result = client
        .put::<Value>(
            &format!("{path}/merge"),
            json!({"sha":head,"merge_method":method}),
        )
        .await
        .map_err(|e| e.to_string())?;
    if result["merged"].as_bool() != Some(true) {
        return Ok((
            PullRequestStatus::Skipped,
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
                fields.head.branch.as_deref().ok_or("missing head branch")?,
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
    // The cached PR keeps GitHub's shape: merged_at is when it merged.
    pr["merged_at"] = json!(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    Ok((PullRequestStatus::Merged, reason))
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
        if !requested(plan, config) || plan.action().status != BranchStatus::Unchanged {
            continue;
        }
        let Some(pr) = &mut plan.cached_pr else {
            continue;
        };
        // Check policy independently of manual create/schedule/age permits.
        let proof = pr.value();
        let allowed = policy_allows(repo, plan, now);
        let pr = plan.cached_pr.as_mut().expect("restored receipt");
        let mut value = pr.value();
        let (status, reason) = if !allowed {
            (PullRequestStatus::Skipped, "auto-merge waiting: release age, schedule or lookup evidence does not permit merging".into())
        } else {
            one(client, inventory, repo, &mut value)
                .await
                .unwrap_or_else(|e| (PullRequestStatus::Error, format!("auto-merge: {e}")))
        };
        pr.resync(value);
        pr.set_merge_note(Some(crate::pull_requests::MergeNote::Reason(
            reason.clone(),
        )));
        if status == PullRequestStatus::Merged {
            plan.merged();
        }
        let proof_fields = crate::pull_requests::Pr::read(&proof);
        actions.push(PullRequestAction {
            repository: repo.repository.clone(),
            branch: plan.action().branch.clone(),
            number: proof_fields.number,
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
            if action.status == PullRequestStatus::Merged {
                action
                    .reason
                    .get_or_insert_default()
                    .push_str(&format!("; merge history cache: {error}"));
            }
        }
    }
    actions
}

pub fn dashboard_history(inventory: &Inventory, mut prs: Vec<Value>, plans: &[Plan]) -> Vec<Value> {
    // Untracked historical/native merges are not presented as fallback outcomes.
    prs.retain(|p| {
        crate::pull_requests::Pr::read(p).merged_at.is_none()
            || crate::pull_requests::merge_note(p).is_some()
    });
    for pr in &inventory.merged_prs {
        let number = crate::pull_requests::Pr::read(pr).number;
        if !prs
            .iter()
            .any(|p| crate::pull_requests::Pr::read(p).number == number)
        {
            prs.push(pr.clone());
        }
    }
    for tracked in plans.iter().filter_map(|p| p.cached_pr.as_ref()) {
        let pr = tracked.value();
        let fields = crate::pull_requests::Pr::read(&pr);
        if let Some(old) = prs
            .iter_mut()
            .find(|p| crate::pull_requests::Pr::read(p).number == fields.number)
        {
            *old = pr;
        } else if fields.merged_at.is_some() {
            prs.push(pr);
        }
    }
    prs
}

#[cfg(test)]
mod tests {
    use super::already_mergeable;

    /// GitHub's refusal names the PR's merge state; only the states in which
    /// nothing required blocks the merge mean "mergeable now".
    #[test]
    fn a_refusal_naming_a_mergeable_state_is_not_an_error() {
        for state in [
            "clean",
            "unstable",
            "has_hooks",
            "CLEAN",
            "UNSTABLE",
            "HAS_HOOKS",
        ] {
            assert!(
                already_mergeable(&format!("Pull request Pull request is in {state} status")),
                "{state}"
            );
        }
        assert!(already_mergeable("one; Pull request is in unstable status"));
        for message in [
            "Pull request Pull request is in dirty status",
            "Pull request is in unknown status",
            "Pull request is in clean",
            "Auto merge is not allowed for this repository",
            "",
        ] {
            assert!(!already_mergeable(message), "{message}");
        }
    }
}
