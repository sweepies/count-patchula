//! Native auto-merge, or conservative later-run merging backed by write receipts.
use crate::{
    Inventory, Plan,
    pull_requests::{Tracked, body, body_hash, owned_pr},
};
use chrono::{DateTime, Utc};
use patchula_cache::{Cache, State};
use patchula_config::Config;
use patchula_github::{Client, GraphqlPage, model};
use patchula_report::{BranchStatus, PullRequestAction, PullRequestStatus, Repository};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One auto-merge history row: the fields the dashboard's merged section,
/// the server's public page and [`merged`] read, persisted under
/// `auto-merge-history` v1. Every field is `Option` so a row missing any
/// of them still decodes the way the old full-REST rows did; a row that
/// is not an object at all is dropped at decode, as invisible as the
/// fieldless `Value` it was before.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergedPr {
    /// `number` — rows dedupe and sort by it.
    pub number: Option<u64>,
    /// `title`.
    pub title: Option<String>,
    /// `html_url`.
    pub html_url: Option<String>,
    /// `merged_at` — `date-time` verbatim; absent rows are invisible.
    pub merged_at: Option<String>,
    /// The Count's merge note, under its persisted `patchula_merge` key.
    #[serde(rename = "patchula_merge")]
    pub merge_note: Option<String>,
}
impl MergedPr {
    /// Project a lifecycle PR down to its history row.
    pub(crate) fn from_tracked(tracked: &Tracked) -> Self {
        Self {
            number: Some(tracked.pr.number),
            title: Some(tracked.pr.title.clone()),
            html_url: Some(tracked.pr.html_url.clone()),
            merged_at: tracked.pr.merged_at.clone(),
            merge_note: tracked.merge_note().map(str::to_owned),
        }
    }
}
/// What the dashboard renders beside the live PR list: the tracked PRs
/// and the merge rows [`dashboard_history`] computes for them.
pub struct DashboardPrs {
    /// Live PRs — open ones, plus merged ones whose note keeps them
    /// rendered.
    pub open: Vec<Tracked>,
    /// The merged section's rows: live merged PRs, recorded history and
    /// cached merges, in that order.
    pub merged: Vec<MergedPr>,
}
#[derive(Default, Serialize, Deserialize)]
struct History {
    actor: String,
    #[serde(default, deserialize_with = "merged_prs")]
    prs: Vec<MergedPr>,
}
/// Element-wise row decode: a row that fails is invisible, never fatal to
/// the rest — the same leeway `Vec<Value>` gave.
fn merged_prs<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<MergedPr>, D::Error> {
    Ok(Vec::<Value>::deserialize(deserializer)?
        .into_iter()
        .filter_map(|row| MergedPr::deserialize(row).ok())
        .collect())
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
) -> Result<Vec<MergedPr>, String> {
    Ok(cache
        .state::<History>(&history_scope(repo))
        .await
        .map_err(|e| e.to_string())?
        .filter(|h| h.actor == actor)
        .map(|h| h.prs)
        .unwrap_or_default())
}
/// The PRs the Count last recorded merging in `repo`, oldest first, at most 20.
pub async fn merged(cache: &Cache, repo: &str) -> Vec<MergedPr> {
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
        .filter_map(|p| p.cached_pr.clone())
        .collect();
    if merged.is_empty() {
        return Ok(());
    }
    let mut prs = inventory.merged_prs.clone();
    for tracked in merged {
        let row = MergedPr::from_tracked(&tracked);
        prs.retain(|p| p.number != row.number);
        prs.push(row);
    }
    prs.sort_by_key(|p| p.number);
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
/// Whether a fresh REST read still is the PR a verified receipt saw:
/// every compared field is required, so a PR missing one cannot reach
/// this comparison — the decode failed it first.
fn same_head(
    fresh: &model::PullRequest,
    proof: &model::PullRequest,
    actor: &str,
    repo: &str,
) -> bool {
    owned_pr(fresh, actor, repo)
        && fresh.state == "open"
        && fresh.merged_at.is_none()
        && fresh.node_id == proof.node_id
        && fresh.head.sha == proof.head.sha
        && fresh.head.branch == proof.head.branch
        && fresh.base.branch == proof.base.branch
        && body_hash(body(fresh)) == body_hash(body(proof))
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
    pr: &mut Tracked,
) -> Result<(PullRequestStatus, String), String> {
    let head_oid = json!(pr.pr.head.sha);
    if pr.pr.base.branch != repo.default_branch.as_deref().unwrap_or_default() {
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
        if pr.pr.auto_merge.is_none()
            && enable_native(client, &pr.pr.node_id, method, &head_oid).await? == Native::Enabled
        {
            pr.set_auto_merge(true);
        }
        if pr.pr.auto_merge.is_some() {
            return Ok((
                PullRequestStatus::Unchanged,
                "native auto-merge enabled".into(),
            ));
        }
    }
    let number = pr.pr.number;
    let head = pr.pr.head.sha.clone();
    let path = format!("repos/{}/pulls/{number}", repo.repository);
    let fresh = client
        .get_fresh::<model::PullRequest>(&path)
        .await
        .map_err(|e| e.to_string())?;
    if !same_head(&fresh, &pr.pr, &inventory.actor, &repo.repository) {
        return Ok((
            PullRequestStatus::Protected,
            "auto-merge blocked: ownership or receipt head changed".into(),
        ));
    }
    if fresh.draft != Some(false)
        || fresh.mergeable != Some(true)
        || fresh.mergeable_state.as_deref() != Some("clean")
    {
        return Ok((
            PullRequestStatus::Skipped,
            "auto-merge waiting: GitHub mergeability/protection is not clean".into(),
        ));
    }
    if !green(client, &repo.repository, &head).await? {
        return Ok((
            PullRequestStatus::Skipped,
            "auto-merge waiting: at least one check is required and all checks/statuses must pass"
                .into(),
        ));
    }
    // Recheck ownership/protections after pagination, then use a head lease on the
    // normal merge API. No admin/bypass option, direct base write or force merge.
    let fresh = client
        .get_fresh::<model::PullRequest>(&path)
        .await
        .map_err(|e| e.to_string())?;
    if !same_head(&fresh, &pr.pr, &inventory.actor, &repo.repository)
        || fresh.draft != Some(false)
        || fresh.mergeable != Some(true)
        || fresh.mergeable_state.as_deref() != Some("clean")
    {
        return Ok((
            PullRequestStatus::Protected,
            "auto-merge blocked: PR changed during check inspection".into(),
        ));
    }
    let result = client
        .put::<model::MergeResult>(
            &format!("{path}/merge"),
            json!({"sha":head,"merge_method":method}),
        )
        .await
        .map_err(|e| e.to_string())?;
    if !result.merged {
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
                &pr.pr.head.branch,
                &head,
            )
            .await
        }
        .await;
        if let Err(error) = cleanup {
            reason.push_str(&format!("; branch cleanup deferred: {error}"));
        }
    }
    // The cached PR keeps GitHub's shape: merged_at is when it merged.
    pr.mark_merged(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
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
        let proof = pr.clone();
        let allowed = policy_allows(repo, plan, now);
        let pr = plan.cached_pr.as_mut().expect("restored receipt");
        let (status, reason) = if !allowed {
            (PullRequestStatus::Skipped, "auto-merge waiting: release age, schedule or lookup evidence does not permit merging".into())
        } else {
            one(client, inventory, repo, pr)
                .await
                .unwrap_or_else(|e| (PullRequestStatus::Error, format!("auto-merge: {e}")))
        };
        pr.set_merge_note(Some(crate::pull_requests::MergeNote::Reason(
            reason.clone(),
        )));
        if status == PullRequestStatus::Merged {
            plan.merged();
        }
        actions.push(PullRequestAction {
            repository: repo.repository.clone(),
            branch: plan.action().branch.clone(),
            number: Some(proof.pr.number),
            url: Some(proof.pr.html_url.clone()),
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

/// The dashboard's PR lists: the live pulls it renders, and the merged
/// rows beside them — live merged PRs, recorded history and cached
/// merges, deduplicated by number exactly as the combined list was.
pub fn dashboard_history(inventory: &Inventory, prs: Vec<Tracked>, plans: &[Plan]) -> DashboardPrs {
    // Untracked historical/native merges are not presented as fallback outcomes.
    let mut open: Vec<_> = prs
        .into_iter()
        .filter(|t| t.pr.merged_at.is_none() || t.merge_note().is_some())
        .collect();
    let mut merged: Vec<MergedPr> = open
        .iter()
        .filter(|t| t.pr.merged_at.is_some())
        .map(MergedPr::from_tracked)
        .collect();
    for row in &inventory.merged_prs {
        if !open.iter().any(|t| row.number == Some(t.pr.number))
            && !merged.iter().any(|m| m.number == row.number)
        {
            merged.push(row.clone());
        }
    }
    for tracked in plans.iter().filter_map(|p| p.cached_pr.as_ref()) {
        // A cached PR is the freshest picture of its number on either list.
        if let Some(old) = open.iter_mut().find(|t| t.pr.number == tracked.pr.number) {
            old.clone_from(tracked);
        }
        let row = MergedPr::from_tracked(tracked);
        if let Some(old) = merged.iter_mut().find(|m| m.number == row.number) {
            *old = row;
        } else if tracked.pr.merged_at.is_some() {
            merged.push(row);
        }
    }
    DashboardPrs { open, merged }
}

#[cfg(test)]
mod tests {
    use super::{already_mergeable, *};
    use patchula_cache::State;

    fn pull_request_json(number: u64) -> Value {
        json!({
            "node_id": format!("N{number}"), "number": number,
            "html_url": format!("https://github.com/o/r/pull/{number}"),
            "state": "open", "title": "chore(deps): bump serde",
            "body": format!("Bumps.\n\n<!-- patchula:{{\"version\":1,\"identity\":\"{}\",\"update\":\"{}\"}} -->",
                "a".repeat(64), "b".repeat(64)),
            "labels": [], "user": {"login": "patchula[bot]"},
            "head": {"ref": "patchula/serde-2.x", "sha": "s", "repo": {"full_name": "o/r"}},
            "base": {"ref": "main", "repo": {"full_name": "o/r"}}
        })
    }
    fn pull_request(number: u64) -> model::PullRequest {
        serde_json::from_value(pull_request_json(number)).expect("a complete REST pull request")
    }

    /// A row written before `MergedPr` — a whole REST pull object with the
    /// `patchula_merge` key smuggled in — decodes into the same row.
    #[tokio::test]
    async fn history_rows_written_as_rest_pulls_still_decode() {
        #[derive(Serialize, Deserialize)]
        struct OldHistory {
            actor: String,
            prs: Vec<Value>,
        }
        impl State for OldHistory {
            const NAMESPACE: &'static str = "auto-merge-history";
            const VERSION: u32 = 1;
            type Scope = HistoryScope;
        }
        let cache = Cache::open(":memory:", Default::default()).await.unwrap();
        cache
            .set_state(
                &history_scope("o/r"),
                &OldHistory {
                    actor: "patchula[bot]".into(),
                    prs: vec![
                        json!({"number":42,"node_id":"N42",
                            "html_url":"https://github.com/o/r/pull/42",
                            "state":"closed","title":"chore(deps): bump serde",
                            "merged_at":"2026-01-01T00:00:00Z",
                            "patchula_merge":"merged by patchula (squash)",
                            "user":{"login":"patchula[bot]"},"labels":[],
                            "head":{"ref":"patchula/serde-2.x","sha":"s",
                                "repo":{"full_name":"o/r"}},
                            "base":{"ref":"main","repo":{"full_name":"o/r"}}}),
                        // A row that is not an object is invisible, never
                        // fatal — the same leeway `Vec<Value>` gave.
                        json!("not a pull request"),
                    ],
                },
            )
            .await
            .unwrap();
        let rows = merged(&cache, "o/r").await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].number, Some(42));
        assert_eq!(rows[0].title.as_deref(), Some("chore(deps): bump serde"));
        assert_eq!(
            rows[0].merge_note.as_deref(),
            Some("merged by patchula (squash)")
        );
        // The dashboard's merged section renders the decoded row.
        let inventory = Inventory {
            merged_prs: rows,
            ..Default::default()
        };
        let history = dashboard_history(&inventory, Vec::new(), &[]);
        assert_eq!(history.merged.len(), 1);
        assert_eq!(history.merged[0].number, Some(42));
        let repo: Repository = serde_json::from_value(json!({
            "repository": "o/r", "status": "complete", "reason": null, "packageFiles": []
        }))
        .unwrap();
        let text = crate::dashboard::render(
            &repo,
            &[],
            &history.open,
            &history.merged,
            patchula_words::Recheck::NextRun,
            None,
        );
        // The line the old `Vec<Value>` rendering produced for this row.
        assert!(
            text.contains("- #42: chore(deps): bump serde — merged by patchula (squash)\n"),
            "{text}"
        );
    }

    /// Every field `same_head` compares is required in the type: a fresh
    /// read missing `node_id` never reaches the comparison — where two
    /// missing fields once compared equal, the decode now fails first.
    #[test]
    fn a_fresh_pr_missing_a_required_field_fails_closed() {
        let full = pull_request_json(7);
        for field in [
            "node_id", "number", "state", "title", "labels", "head", "base",
        ] {
            let mut pr = full.clone();
            pr.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<model::PullRequest>(pr).is_err(),
                "{field}"
            );
        }
    }

    /// The receipt comparison fails on any changed field, and a foreign or
    /// closed PR is never the one the receipt saw.
    #[test]
    fn same_head_compares_every_receipt_field() {
        let proof = pull_request(7);
        assert!(same_head(&proof, &proof, "patchula[bot]", "o/r"));
        let mut head_moved = pull_request(7);
        head_moved.head.sha = "other".into();
        assert!(!same_head(&head_moved, &proof, "patchula[bot]", "o/r"));
        let mut node_moved = pull_request(7);
        node_moved.node_id = "other".into();
        assert!(!same_head(&node_moved, &proof, "patchula[bot]", "o/r"));
        let mut base_moved = pull_request(7);
        base_moved.base.branch = "release".into();
        assert!(!same_head(&base_moved, &proof, "patchula[bot]", "o/r"));
        let mut closed = pull_request(7);
        closed.state = "closed".into();
        assert!(!same_head(&closed, &proof, "patchula[bot]", "o/r"));
        let mut merged = pull_request(7);
        merged.merged_at = Some("2026-01-01T00:00:00Z".into());
        assert!(!same_head(&merged, &proof, "patchula[bot]", "o/r"));
        let mut foreign = pull_request(7);
        foreign.head.repo = None;
        assert!(!same_head(&foreign, &proof, "patchula[bot]", "o/r"));
    }

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
